---
type: adr
area: engine
date: 2026-09-23
deciders: CypherLite maintainers
name: 0089-immutable-checkpoint-shard-publication
status: proposed
tags:
  - checkpoint
  - storage
title: Publish checkpoint shards under immutable generation-unique paths
REFERENCES:
  - "[[adr/0014-single-wal-write-path]]"
  - "[[adr/0022-id-window-checkpoint-shards]]"
  - "[[adr/0072-virtual-filesystem]]"
  - "[[adr/0074-single-file-bundle-vfs]]"
  - "[[adr/0086-bounded-persistence-input]]"
  - "[[adr/0098-safe-atomic-replacement]]"
---

# 0089. Publish checkpoint shards under immutable generation-unique paths

## Context

[ADR-0022](./0022-id-window-checkpoint-shards.md) specifies copy-on-write dirty id windows and a manifest published
last, but also fixes each part's path at `nodes/{lo}-{hi}.parquet` or
`relationships/{lo}-{hi}.parquet`. Rewriting a dirty window replaces that
path before the new manifest is written. If a later part fails, the old
manifest can now point to new bytes while its old checkpoint LSN is retained;
recovery may apply the same committed WAL changes again or lose a previously
committed view. Splitting a window does not prevent collisions with an old
part. [ADR-0086](./0086-bounded-persistence-input.md) bounds both writer and reader; a late out-of-envelope part is
one ordinary cause of this partial-checkpoint failure.

The VFS of [ADR-0072](./0072-virtual-filesystem.md) serves Native directories, ephemeral Memory, and the
single-host-file Bundle of [ADR-0074](./0074-single-file-bundle-vfs.md). [ADR-0098](./0098-safe-atomic-replacement.md) proposes exclusive temporary
creation and file/namespace durability for atomic replacement. Those
guarantees protect *one* path, not a multi-part checkpoint whose existing
parts have already been overwritten. [ADR-0014](./0014-single-wal-write-path.md) requires the previously
published snapshot plus committed WAL to remain recoverable after a failed
checkpoint.

## Decision

1. **Immutable part identity.** Keep format_version 3 and its manifest part
   schema. The manifest's `path`, not the id window alone, identifies the
   exact bytes to load. Each new node/relationship leaf part written for a
   dirty or new window uses a fresh checkpoint generation identifier with at
   least 128 bits of OS-backed randomness plus the kind and half-open range:
   `nodes/{lo}-{hi}-{generation}.parquet` or
   `relationships/{lo}-{hi}-{generation}.parquet`. If multiple leaf parts
   require the same range within one generation, give each a distinct
   collision-resistant suffix. Reserve final names by exclusive VFS creation
   (including no-follow on Native), retry a collision with a new name, and
   never replace an existing final part. A checkpoint may retain unchanged
   manifest entries verbatim, whether their paths use the old fixed spelling
   or the new spelling. Rewritten/split entries always get fresh paths; no
   still-referenced shard is modified or deleted before publication.

2. **Publish only complete, reopenable parts.** Encode/check each leaf against
 [ADR-0086](./0086-bounded-persistence-input.md)'s file, page, row, and decoded-byte envelope before including it
   in the new manifest. Do not publish oversized intermediate windows while
   splitting or recursively re-encode every ancestor without a bound; produce
   final leaves with bounded work and report an unsplittable part as a failed
   checkpoint. Write every new part, sync its contents and close its handle,
 then persist its namespace entry as required by [ADR-0098](./0098-safe-atomic-replacement.md) *before*
   publishing a manifest that references it. Native Unix can batch directory
   syncs once per touched shard directory after all new files have been
   individually synced (and sync newly created ancestors). Bundle can batch
   a host-file barrier only if it also makes all new inode names and contents
   durable before the manifest pointer swap; otherwise use the existing
   per-part VFS syncs. Memory uses the same ordering without host fsync.
   These costs belong to checkpointing, never to each transaction commit.

3. **Manifest is the publication point.** After *both* kinds are ready, build
   the full manifest by reusing unchanged entries and adding only complete
   new parts. Atomically replace and durably publish `manifest.json` last via
 [ADR-0098](./0098-safe-atomic-replacement.md). Until that publication succeeds, the old manifest and every
   part it names stay untouched; preserve the entire committed WAL needed by
   the old manifest. A failed checkpoint must not clear dirty state or
   advance the recyclable WAL cut, even when a rename or directory-sync
   error leaves the new manifest visibly installed. The caller recycles WAL
   only after the checkpoint reports durable success. On Linux/macOS this
   means the manifest's containing directory sync has succeeded; on Windows
 [ADR-0098](./0098-safe-atomic-replacement.md) does not claim power-loss durability of a renamed directory
   entry, so neither does this decision.

4. **Cleanup without guessing publication.** Track the newly created part
   names for an in-flight attempt. If it fails *before* manifest publication,
   remove those owned, unreferenced new names on a best-effort basis; surface
   cleanup failures with the checkpoint error and leave the WAL intact.
   If manifest replacement or its durability barrier fails after it might
   have published, do not delete a new part merely because the call failed:
   inspect the readable manifest, and retain all parts it names. If the
   manifest cannot be read or durability is ambiguous, defer reclamation;
   never roll back a visible manifest by deleting its parts. On successful
   publication, retire paths no longer referenced by the durable manifest;
   cleanup failures there are deferred housekeeping, not a reversal of the
   published checkpoint. For Native Windows, defer retirement of old parts
   across a renamed manifest until a future proven namespace-durability
   barrier or offline maintenance; a subsequent power loss may restore an
   older manifest.

   After recovery has validated and loaded the selected manifest and replayed
   its WAL, sweep recognizable generation-named orphan parts not in that
   manifest, including leftovers of crashed/failed attempts and superseded
   generations. Do not sweep on a missing, unreadable, or invalid manifest
   until recovery can establish the authoritative reference set; do not
   treat a listed missing part as an empty shard. Keep legacy fixed-name
   objects out of automatic crash-orphan matching. A sweep must never delete
   a referenced path, must not follow a symlink masquerading as a shard, and
   may retry deletion at the next open/checkpoint if removal fails. Run
   reference-set construction and directory enumeration during open or
   checkpoint housekeeping, not on each transaction commit. Memory sweeps
   its virtual tree during its lifetime; Bundle sweeps virtual inodes inside
   the one `.cl` file, with no host sidecars. On Native Windows, where the
   recovered manifest entry may lack a proven power-loss namespace barrier,
   retain ambiguous unreferenced generations instead of sweeping a possible
   older-manifest fallback; clean only parts known to belong to an attempt
   that never published, or wait for an explicit durable maintenance path.

5. **Read compatibility.** Continue reading v1/v2 monolithic
   `nodes.parquet` and `relationships.parquet`, and v3 manifests containing
   fixed `nodes/{lo}-{hi}.parquet` / `relationships/{lo}-{hi}.parquet` paths.
   New v3 manifests may mix fixed and generation-unique paths while unchanged
   parts are reused. No eager rewrite on open and no new manifest version are
 required. New and legacy parts alike obey [ADR-0086](./0086-bounded-persistence-input.md)'s persisted-input
   limits; missing referenced objects are storage errors.

## Alternatives considered

- **Rewrite the fixed path in place and rely on manifest-last replacement.**
  Rejected: the old manifest still references that same path, so a later
  shard failure or crash exposes new bytes under the old checkpoint LSN.
- **Use a two-slot manifest without immutable parts.** Rejected: both slots
  can still refer to a path overwritten for the other slot; protecting the
  manifest alone does not preserve old bytes.
- **Rewrite every shard on every checkpoint under a new directory.**
  Rejected: it turns dirty-window copy-on-write into full-graph I/O and adds
  unnecessary per-part sync and encoding costs as the graph grows.
- **Delete all new parts on any checkpoint error.** Rejected: a manifest
  rename or directory-sync error can occur after publication; those parts
  may already be referenced by the visible or crash-recovered manifest.
- **Recycle the WAL after writing parts but before publishing the manifest.**
  Rejected: recovery with the old manifest would then lack committed changes
  beyond its checkpoint LSN.
- **Bump the manifest format or migrate fixed-name v3 parts on open.**
  Rejected: the existing v3 `path` field already supports both spellings;
  eager migration adds writes and a new failure boundary during recovery.

## Consequences

- Dirty checkpoint writes allocate new shard paths, and a failed later part
  leaves previously published bytes intact. The old manifest and WAL remain
  a complete recovery source; cleanup of unreferenced names is retryable.
- An unchanged window remains a reference to the same immutable object. The
  extra file/inode creation and sync work is proportional to new dirty leaf
  parts at checkpoint time; there is no all-shard rewrite or cleanup scan per
  commit. Native may coalesce shard-directory syncs; Bundle keeps all objects
  in one host file and Memory retains in-process ordering.
- Tests must inject failure after one new dirty part, manifest write/rename
  and post-rename sync uncertainty, and crash-orphan cleanup on Native,
  Memory, and Bundle. Recovery must prove that referenced old and mixed v3
  paths, legacy v1/v2 monoliths, and the required WAL remain usable. Public
 checkpoint/durability documentation must reflect [ADR-0098](./0098-safe-atomic-replacement.md) and [ADR-0086](./0086-bounded-persistence-input.md)'s
  platform and input limits.
