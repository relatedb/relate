---
type: adr
area: engine
date: 2026-09-23
deciders: CypherLite maintainers
name: 0098-safe-atomic-replacement
status: proposed
tags:
  - storage
title: Exclusive temporary files and durable atomic replacement
REFERENCES:
  - "[[adr/0072-virtual-filesystem]]"
  - "[[adr/0074-single-file-bundle-vfs]]"
  - "[[adr/0077-embedded-durability-boundary]]"
  - "[[adr/0097-exclusive-storage-ownership]]"
SUPERSEDES:
  - "[[adr/0072-virtual-filesystem]]"
---

# 0098. Exclusive temporary files and durable atomic replacement

## Context

[ADR-0072](./0072-virtual-filesystem.md) introduced the synchronous VFS and retained a Native write through a
fixed `{path}.tmp`, followed by file `sync_all`, close, and rename. The current
`atomic_write` uses `create_truncate` on that predictable name. A pre-existing
symlink or file can therefore be opened and truncated before the replacement;
even a successful file sync does not make the renamed directory entry durable
after a crash. [ADR-0074](./0074-single-file-bundle-vfs.md) makes the temporary name a virtual inode inside a
single `.cl` host file. [ADR-0097](./0097-exclusive-storage-ownership.md) specifies exclusive ownership of persistent
stores and private creation of new Unix database files; ownership does not
remove the need for safe temporary-file creation or namespace persistence.

The Rust 1.88 `OpenOptions::create_new` contract provides an atomic
create-if-absent operation that fails even when the final component is a
dangling symlink. The storage-primitives review found no proven standard-library
Windows equivalent for syncing a containing directory: opening a directory
read-only and calling `sync_all` is not a supported `FlushFileBuffers`
guarantee. Replacement durability must distinguish the two platforms.

## Decision

1. **Exclusive, unpredictable temporary name.** For each `atomic_write`,
   choose a fresh name in the *destination's containing virtual directory*,
   derived from the destination only as a recognizable prefix and suffixed
   with at least 128 bits from an OS-backed cryptographically secure random
   source. Never use the fixed `{path}.tmp`, timestamps, process IDs, or a
   counter as the sole source of uniqueness. Attempt exclusive create without
   truncation or following an existing final-component symlink. Native uses
   `OpenOptions::create_new(true)` (and mode `0600` for new files on Unix per
 [ADR-0097](./0097-exclusive-storage-ownership.md)); Memory checks/inserts under its file-map lock; Bundle checks and
   allocates an inode under its bundle mutex. An already occupied name,
   including a symlink in Native, is a collision: generate a *different*
   random name and retry a bounded number of times, then report an error. Do
   not open, remove, or truncate the colliding name. Expose exclusive-create
   semantics through the VFS (an open option or dedicated method), not a
   separate Native-only write path. The temp name and handle belong to this
   operation only after exclusive creation succeeds.

2. **Write, publish, clean up.** Create destination parents as needed, write
   all bytes to the owned temp, sync that temp file, and **close its write
   handle before rename**, including on Windows. Rename within the same
   containing directory, replacing the destination as one VFS namespace
   operation; do not unlink the destination first. On a pre-publish write,
   sync, close, or rename failure, attempt to remove only the temp successfully
   created by this operation, after closing its handle. Surface the original
   failure (and cleanup failure if any); never delete or roll back the
   destination. A rename that reports an error may already have published on
   some backends: cleanup may target only the still-present owned temp name,
   never the destination. Once rename succeeds, the temp has been consumed;
   a subsequent durability error must not trigger temp/destination cleanup or
   an automatic rewrite.

3. **Native namespace persistence.** On Linux and macOS, after the successful
   same-directory rename, open the containing host directory and `sync_all`
   it; propagate any open/sync failure. This is in addition to syncing and
   closing the temp *before* rename. If `atomic_write` created new parent
   directories, sync each newly created directory's parent, from the first
   newly created component through the destination parent, so the new path
   components are also durable before reporting success. Do not promise this
   sequence for a filesystem that rejects directory sync: report its I/O
   failure. On Windows, sync the writable temp and close it before rename,
   but do not try `sync_all` on a read-only directory handle. Until a tested
   Windows-specific directory flush primitive is adopted, a successful
   rename supplies atomic replacement visibility but **does not promise that
   the renamed directory entry survives sudden power loss**. Failures of the
   file sync or rename still propagate. A later platform-specific decision
   may strengthen this boundary; a no-op Windows parent-sync implementation
   must not be described as a durable namespace barrier.

4. **Backend-specific persistence.** Memory retains the same virtual
   replacement behavior with exclusive temp names but has no host sync or
   crash-durability guarantee; its parent-sync operation is a no-op. Bundle
   keeps both the temp and destination as virtual inodes in its *single* host
 file ([ADR-0074](./0074-single-file-bundle-vfs.md)). Its temp-file sync and rename use the existing host-file
   catalog/superblock durability barriers: the rename persists the catalog
   pointer swap, so no separate host directory entry is renamed or synced for
   each virtual replacement. Bundle's virtual parent-sync operation is a
   no-op, not a second host-side temp file. This does not change the separate
   lifetime/durability of first creating the `.cl` host file itself.

5. **Post-rename sync failure.** A failed Native directory sync returns an
   I/O error even though the new bytes may already be visible. The old name
   cannot safely be restored and the caller must not infer either old or new
   data after a crash from that failed call. Retain the synced new destination
   in place and let recovery/readback establish current state; do not silently
   report success or automatically retry a potentially non-idempotent
   operation. Tests must fault-inject each stage and verify this ordering and
   distinction, as well as collision, symlink, cleanup, repeated-overwrite,
   and Native/Memory/Bundle roundtrips on supported platforms.

## Alternatives considered

- **Keep `{path}.tmp` with `create_truncate`.** Rejected: a pre-existing file
  or symlink can be followed and truncated before the operation has ownership
  of the temporary path.
- **Use a predictable name with `create_new`, or test `exists` before open.**
  Rejected: a predictable name permits trivial pre-creation/denial and an
  existence check is a race rather than an exclusive reservation.
- **Sync only the temp file, then rename.** Rejected: file-content sync alone
  does not persist the new Native directory entry on Linux/macOS, or newly
  created ancestor entries.
- **Open a Windows directory read-only and call `sync_all`, claiming equal
  power-loss durability on all platforms.** Rejected: no tested directory
  flush guarantee follows from that standard-library operation; the promise
  would exceed the verified primitive.
- **Create a host `.tmp` sidecar for Bundle, or use a host directory sync for
 every virtual rename.** Rejected: the first breaks [ADR-0074](./0074-single-file-bundle-vfs.md)'s single-file
  contract, and the second does not persist a virtual inode catalog swap.
- **Restore the old destination or retry automatically after a post-rename
  sync error.** Rejected: publication may already have occurred; rollback
  and blind retries can lose data or repeat a caller operation.

## Consequences

- Native replacement does not open or truncate an existing temp or follow
  its final-component symlink. Collision handling and cleanup are scoped to
  the file this write actually created; newly created Unix temp files inherit
 [ADR-0097](./0097-exclusive-storage-ownership.md)'s private `0600` creation mode.
- Successful Native replacement on Linux/macOS includes file sync, handle
  close, rename, and containing-directory sync (plus newly created ancestor
  persistence); directory-sync errors remain observable even after
  publication. Windows retains the close-before-rename rule and explicitly
  weaker power-loss namespace guarantee. These barriers complement the WAL
 durability boundary in [ADR-0077](./0077-embedded-durability-boundary.md) rather than changing commit semantics.
- Memory remains non-durable and `.cl` keeps one host file with its existing
 catalog/superblock sync contract ([ADR-0074](./0074-single-file-bundle-vfs.md)). VFS implementations need
  exclusive-create and parent-sync behavior, with fault-injection tests for
  operation ordering and failures before implementation can ship.
