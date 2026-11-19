---
type: adr
area: engine
date: 2026-09-23
deciders: CypherLite maintainers
name: 0091-bundle-generation-page-quarantine
status: proposed
tags:
  - storage
title: Quarantine retired bundle pages across catalog publication
REFERENCES:
  - "[[adr/0074-single-file-bundle-vfs]]"
  - "[[adr/0077-embedded-durability-boundary]]"
  - "[[adr/0086-bounded-persistence-input]]"
  - "[[adr/0089-immutable-checkpoint-shard-publication]]"
  - "[[adr/0092-manifest-bound-pull-identity]]"
  - "[[adr/0093-bundle-wal-page-ownership]]"
---

# 0091. Quarantine retired bundle pages across catalog publication

## Context

[ADR-0074](./0074-single-file-bundle-vfs.md) recovers the higher valid `.cl` superblock and joins orphan pages to
the freelist. [ADR-0086](./0086-bounded-persistence-input.md) validates the selected catalog's ranges, and [ADR-0089](./0089-immutable-checkpoint-shard-publication.md)
publishes immutable snapshot shards. But `BundleVfs` can free an overwritten
manifest inode or the current catalog and immediately allocate its pages for
the next catalog or inode *before* the next superblock has been durably
published. An interrupted barrier then selects the older superblock, whose
catalog or manifest bytes have already been overwritten. Immutable virtual
names alone cannot protect the old physical pages.

This is an *allocator/replacement* hazard, distinct from the existing WAL
append path of [ADR-0074](./0074-single-file-bundle-vfs.md)/[ADR-0077](./0077-embedded-durability-boundary.md). `BundleVfs::write_bytes` reads and rewrites a
whole 4096-byte page for an append at a non-page-aligned EOF. Even though the
new bytes start past the committed logical WAL length, a torn host-page write
can damage an older committed prefix in that same page. [ADR-0086](./0086-bounded-persistence-input.md)'s torn-tail
rule can discard only an incomplete or corrupt final portion; it cannot repair
a previously committed frame damaged by such a write. Quarantining retired pages
neither introduces nor fixes this pre-existing write path; the same partial
page rewrite already occurs without catalog replacement.

## Decision

1. **Quarantine retired selected-generation ranges.** On open, validate the
 selected superblock, catalog and WAL-tail mapping per [ADR-0086](./0086-bounded-persistence-input.md). Track its
   catalog and inode physical ranges (including manifest, identity, shard and
   WAL inodes) to identify pages *freed or replaced* while publishing a new
   catalog. Freeing, truncating, deleting or replacing an inode, or retiring
   the old catalog, makes its selected-old pages *pending free*, even if the
   current in-memory catalog no longer names them. The allocator must not
   serve these pages from the freelist, map them to a new inode, or overwrite
   them with a new catalog or WAL extension while that old superblock could
   be selected on recovery. Validated old freelist ranges that are not owned
   by old live objects may still be used. Preserve the old superblock slot
   until replacement publication is durable. This pin is a restriction on
   *reallocation of retired pages*, not a promise that every byte of every
   still-live inode is immutable: ordinary WAL appends continue to use the
 existing [ADR-0074](./0074-single-file-bundle-vfs.md) WAL-tail shortcut, including an in-place rewrite of a
   live partial last page. Do not count a live WAL page as available for
   reallocation merely because its logical EOF precedes the page end.

2. **Barrier controls reclamation.** For catalog-changing operations, write
   new data and a new catalog on separate pages, sync their contents, then
   write the alternate superblock and pass its host-file durability barrier
 ([ADR-0077](./0077-embedded-durability-boundary.md)). The existing catalog-free WAL-tail shortcut is outside this
   replacement protocol. Only after the replacement barrier
   succeeds is the new generation selected and the previous generation's
   unreachable pages eligible for freelist reuse; establish the new selected
   read set for the next update. If a barrier fails or its completion is
   ambiguous, do **not** recycle quarantined pages or erase either possible
   catalog generation. Return an error and retain the pages until recovery
   reopens and validates the selected superblock, or a later proven barrier
   establishes an authoritative generation. Never treat a real catalog/page
   read error as an excuse to fall back and mutate the bundle. Recovery derives
   a safe freelist from validated page ownership rather than trusting a list
   that overlaps either still-selectable catalog's live extents.

3. **Virtual publication and compatibility.** Apply the same pin rule to
   `rename`, `remove_file`, `remove_dir_all`, catalog rewrites and checkpoint
   manifest replacement, not just to a snapshot writer. A pre-publication
   failure must not overwrite the previous manifest, its selected identity
 object ([ADR-0092](./0092-manifest-bound-pull-identity.md)), or any WAL extent *retired by this replacement*; the
   previous catalog must still map them. This does not guarantee recovery of
   WAL records already present in a live, partially filled physical page if a
   concurrent/later append tears that page. After success, reclaim unreachable
   previous pages without permanent growth. Keep the on-disk bundle version,
 superblock format and single host file of [ADR-0074](./0074-single-file-bundle-vfs.md); old bundles are
   validated and opened without eager rewrite. Memory and Native do not
   acquire `.cl`-style page generations.

## Alternatives considered

- **Immediately put freed inode/catalog pages into the freelist.** Rejected:
  allocation before a successful superblock barrier overwrites data selected
  by the old superblock after a crash.
- **Protect only `manifest.json`'s inode.** Rejected: the old catalog and
  other retired live inode extents (including WAL and shards) also form the
  old generation's reachable recovery graph.
- **Call an in-place append after logical WAL EOF safe because it does not
  change committed logical offsets.** Rejected: `write_bytes` rewrites the
  entire physical page, and torn writes can corrupt an older WAL frame.
- **Copy-on-write every partial WAL tail page in this replacement decision.**
  Rejected here: it requires splitting/remapping the WAL inode extent and
 writing a new catalog on commits that formerly used only [ADR-0074](./0074-single-file-bundle-vfs.md)'s tail
  superblock hint. It adds page-copy and catalog publication/barrier work to
  the commit path, not just to checkpoint replacement; such a change needs a
  separately measured durability decision and fault-tested protocol.
- **Retain every retired page forever.** Rejected: repeated checkpoints would
  grow a single-file bundle without bound even after durable publication.
- **Add host-side journal or temporary shard files.** Rejected: sidecars
 violate [ADR-0074](./0074-single-file-bundle-vfs.md) and duplicate the dual-superblock publication mechanism.
- **Fall back on any older checksum-valid superblock if the new one reads
  badly.** Rejected: an actual read error is not proof of corruption and
  fallback can discard a committed generation.

## Consequences

- Bundle allocation needs generation-aware quarantine even when the current
  virtual catalog has removed an inode. More pages may be held temporarily
  during a checkpoint, but a successful barrier releases unreachable pages.
- The partial-page WAL append tear remains a **high-severity unresolved
 durability risk** under [ADR-0074](./0074-single-file-bundle-vfs.md)/[ADR-0077](./0077-embedded-durability-boundary.md), not fixed by page quarantine. An
  old superblock's valid checksum does not imply that the WAL prefix in its
 live last page survived a torn append; [ADR-0086](./0086-bounded-persistence-input.md) must fail corruption rather
  than silently discard an affected committed frame. A separately decided
  COW/append protocol is needed before claiming a crash-proof old WAL prefix
  for `.cl` across arbitrary interrupted physical writes.
- Tests must fault new-catalog/superblock barriers after manifest replace and
  WAL activity, prove retired pages are not reused, and separately exercise
  torn partial-page appends as an *unresolved* corruption case rather than
  treating a successful old-superblock selection as a WAL-integrity proof.
  Test later reclamation and unchanged one-file compatibility on
  Linux/macOS/Windows.

## Later references

[ADR-0093](./0093-bundle-wal-page-ownership.md) proposes the separate partial-page WAL copy-on-write and catalog
publication protocol identified here as unresolved. Its selected-page remap
adds to this ADR's retired-page quarantine; the in-place WAL-tail exception
above describes the pre-[ADR-0093](./0093-bundle-wal-page-ownership.md) writer until that protocol is implemented.
