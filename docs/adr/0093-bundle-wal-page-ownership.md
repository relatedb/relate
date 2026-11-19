---
type: adr
area: engine
date: 2026-09-24
deciders: CypherLite maintainers
name: 0093-bundle-wal-page-ownership
status: proposed
tags:
  - storage
  - wal
title: Copy-on-write the selected bundle WAL tail page
REFERENCES:
  - "[[adr/0070-docker-baseline-loadtest-environment]]"
  - "[[adr/0071-ci-owned-loadtest-freeze-fingerprint]]"
  - "[[adr/0074-single-file-bundle-vfs]]"
  - "[[adr/0077-embedded-durability-boundary]]"
  - "[[adr/0086-bounded-persistence-input]]"
  - "[[adr/0091-bundle-generation-page-quarantine]]"
SUPERSEDES:
  - "[[adr/0074-single-file-bundle-vfs]]"
---

# 0093. Copy-on-write the selected bundle WAL tail page

## Context

[ADR-0074](./0074-single-file-bundle-vfs.md) uses a WAL-tail shortcut: an append can publish the WAL inode's new
logical length in an alternate superblock without rewriting the catalog.
[ADR-0091](./0091-bundle-generation-page-quarantine.md) quarantines *retired* pages until their replacement generation is
published, but a still-live WAL page remains writable. `BundleVfs::write_bytes`
reads and writes an entire 4096-byte physical page even for an append past the
logical EOF. `Wal::append_record` separately writes CRC, length and payload.
For example, if the selected catalog maps `wal/00000001.log` bytes `[0,100)`
to page 3, each append at byte 100 can rewrite page 3. A torn host write can
damage the old committed prefix while the old superblock remains checksum-valid.
Reopening that generation cannot repair an earlier committed WAL frame via the
incomplete-active-tail rule of [ADR-0086](./0086-bounded-persistence-input.md). Page quarantine alone does not help
while the selected page has not yet been retired.

The v1 catalog already stores *ordered, discontiguous* inode extents and the
v1 superblock selects a catalog root/length as well as a WAL inode/length.
This permits page remapping without changing the on-disk format or adding a
host-file sidecar. The host barriers follow [ADR-0077](./0077-embedded-durability-boundary.md); on macOS its current
`F_BARRIERFSYNC` is an ordering barrier, **not** a guarantee of flush to
stable media (`F_FULLFSYNC` would be stronger).

## Decision

1. **Copy before touching selected bytes.** On the first write in an
   unbarriered group that would modify a physical WAL page reachable through
   the selected durable superblock (notably an append into a partially filled
   last page), read that page, allocate a different page outside every
   selected live range, copy and patch the new page, then map that logical
   page to the replacement. Split the WAL inode's ordered extent around that
   logical page if necessary, and coalesce physically adjacent extents where
   possible. Mark the mapping and logical length catalog-dirty; the remaining
   CRC/length/payload writes in the same group may modify this **new** page.
   New pages at a page boundary may be allocated normally, but their mapping
   also requires catalog publication. A read/copy failure must not write the
   selected page or substitute zeros. The previously selected WAL page and
 catalog stay protected by [ADR-0091](./0091-bundle-generation-page-quarantine.md)'s selected/retired quarantine.

2. **Publish the mapping before reuse.** Write the replacement WAL pages and
   a fresh v1 catalog containing the ordered extents and new inode length;
   the catalog-free WAL-tail shortcut is forbidden for a remap. Pass a first
   host-file contents barrier for the data **and catalog**, then write the
   alternate v1 superblock (new generation, catalog root/length, file page
   count, WAL inode/length) and pass a second host-file publication barrier.
   Never overwrite the prior selected superblock slot or reallocate pages it
   selects before that second barrier succeeds. A valid old superblock still
   maps its untouched old WAL prefix if the new page/catalog or superblock
   write tears; a checksum-valid new superblock may select only content
   ordered by the first barrier. After successful publication, select the
   new generation and reclaim now-unreachable old pages and catalog pages;
 on reopen, validate ownership and reclaim true orphans as in [ADR-0091](./0091-bundle-generation-page-quarantine.md).

3. **Fence uncertain writes.** An error after a potentially partial page
   write, catalog write, contents barrier, superblock write or publication
   barrier fences subsequent writes on that handle until reopen establishes
   the authoritative valid generation. In particular, never sync partially
   modified in-memory extents after an earlier failed append and never
   release quarantined pages on an ambiguous barrier. A checksum-invalid
   alternate superblock leaves the old generation selected; a fully written
   alternate superblock around an uncertain second barrier may select old or
   new, both of which must map intact committed prefixes. Real read I/O
   errors are errors, not a reason to fall back to and mutate another
   generation. A successful barrier has the host's platform-specific ordering
   semantics; this decision assumes neither atomic 4096-byte writes nor a
   macOS stable-media flush.

4. **Keep format-v1 readers.** Continue to open existing v1 bundles without
   an eager rewrite, including catalogs whose stored WAL length predates the
   selected superblock's WAL-tail hint (`apply_wal_tail`). New catalog extent
   mappings and superblock fields use that same v1 representation; new readers
   recover either valid selected generation. Previously shipped v1 writers
   can still read these bundles but can append to a selected partial page
   *unsafely*; this safety guarantee applies only to writers implementing
   this protocol. Preventing old-binary writes would require a separate
   incompatible version/feature gate decision.

5. **Exercise the crash protocol and cost.** Fault-inject the old-page
   read/copy; torn/failed first, second and third WAL `write_all` (CRC,
   length, payload); new catalog write; contents barrier; torn/fully written
   alternate superblock; and publication barrier before/after physical
   completion. Reopen each case and assert old raw page bytes and old WAL
   records survive pre-publication faults, and that a successfully selected
   new generation replays the new record exactly once. Exercise repeated
   appends, page-boundary crossings, rotation, catalog updates and old v1
   reopening. Instrument page reads/writes and host barriers for small
   commits/groups, then compare the frozen loadtest envelopes at the final
 gate ([ADR-0070](./0070-docker-baseline-loadtest-environment.md)/[ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md)).

## Alternatives considered

- **Keep in-place WAL-tail writes and rely on the superblock checksum or
  incomplete-tail tolerance.** Rejected: the checksum authenticates the
  superblock, not the page it already selects, and torn writes can corrupt
  a committed frame before logical EOF.
- **Quarantine only pages freed by catalog replacements ([ADR-0091](./0091-bundle-generation-page-quarantine.md)).**
  Rejected as a complete WAL fix: a live selected partial page is still the
  target of a full-page rewrite and cannot be repaired by delaying reuse.
- **Add a tail-page indirection/checksum in a new superblock format.**
  Rejected for this version: v1 has no authenticated spare superblock fields;
  mixed-version recovery, old-writer exclusion and folding the override at
  rotation/boundaries add more complexity. It would still need two ordering
  barriers per small commit to publish the new data safely.
- **Write a host-side WAL page or journal.** Rejected: a sidecar violates
 [ADR-0074](./0074-single-file-bundle-vfs.md)'s one-host-file container contract and adds another publication
  protocol.

## Consequences

- A small WAL frame fitting in the selected partial page previously cost
  approximately three full-page WAL writes and one alternate-superblock page
  write with one host barrier per durable group. The new path still makes
  those three WAL page writes (to the copied page), plus `C` catalog page
  writes and one superblock page write with **two** host barriers: `4 + C`
  page writes, normally five for a one-page catalog, plus one old-page read
  and one newly allocated page. `C` is bounded by the existing 16 MiB catalog
  cap (at most 4096 pages); groups amortize publication and boundary/rotation
  costs vary. The next code change must measure actual counts and compare
  frozen baseline workloads, rather than assert a throughput improvement.
- Extents may fragment and catalogs grow; coalesce when possible and reject
  an over-cap catalog before publication. Successful barriers release retired
  pages instead of leaking one page per commit. New writes also require
  fencing on partial `write_bytes` failures, not only on `persist` failures.
- This extends [ADR-0091](./0091-bundle-generation-page-quarantine.md)'s old-generation protection from *retired* pages to
 selected WAL pages touched by append, and supersedes [ADR-0074](./0074-single-file-bundle-vfs.md)'s catalog-free
  WAL-tail shortcut for those remaps. The one-file v1 read format remains.
