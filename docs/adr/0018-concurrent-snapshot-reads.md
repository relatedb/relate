---
type: adr
area: engine
date: 2026-08-28
deciders: MemoryDB maintainers
name: 0018-concurrent-snapshot-reads
status: accepted
tags:
  - transactions
title: Snapshot reads do not exclude apply_redo
REFERENCES:
  - "[[adr/0011-snapshot-isolation]]"
  - "[[adr/0012-wal-and-parquet-checkpoints]]"
  - "[[adr/0013-tuple-mvcc]]"
  - "[[adr/0014-single-wal-write-path]]"
  - "[[adr/0015-optimistic-wal-commits]]"
  - "[[adr/0016-write-overlay]]"
  - "[[adr/0019-group-commit-shared-fsync]]"
  - "[[adr/0020-segmented-wal-concurrent-checkpoint]]"
  - "[[adr/0023-incremental-version-gc]]"
  - "[[adr/0038-lock-free-snapshot-pins]]"
SUPERSEDES:
  - "[[adr/0016-write-overlay]]"
---

# 0018. Snapshot reads do not exclude apply_redo

## Context

[ADR-0013](./0013-tuple-mvcc.md) already pins `snapshot_ts` and filters `xmin`/`xmax`, so a later
commit is logically invisible to an in-flight MATCH. [ADR-0016](./0016-write-overlay.md) still held
the database `RwLock` for the whole query so `apply_redo` (`&mut Graph`)
could not run while those HashMaps were iterated. `detect_fanout` (~315 ms)
therefore blocked publish, and because the WAL mutex is held across apply,
it also blocked every other CREATE’s `sync_all`.

The version chains were MVCC; the maps were not.

## Decision

The live `Graph` is safe for concurrent snapshot readers and one
`apply_redo` (WAL still serializes apply):

1. **Pin.** A read or write execute records `snapshot_ts = commit_ts`
   once at start. MATCH/expand/index use that timestamp for the whole
   statement, not the advancing published `commit_ts`.
2. **Publish last.** `apply_redo` installs versions with `xmin =
   commit_ts`, then stores `commit_ts`. Readers pinned earlier never
   observe a torn apply.
3. **No exclusive graph lock for apply.** `apply_redo` takes `&Graph`
   (concurrent maps). The database `RwLock` is taken in **read** mode
   for queries and for apply. Checkpoint/GC still take the write side
   so they wait for in-flight statements, then rebuild indexes from
   versions live at the checkpoint timestamp.
4. **Indexes are a superset.** `SET`/`DELETE` do not remove index
   entries until GC. Lookups re-check labels and properties on the
   version visible at `snapshot_ts`.

WAL protocol ([ADR-0014](./0014-single-wal-write-path.md), [ADR-0015](./0015-optimistic-wal-commits.md)) is unchanged: append + `sync_all`,
then apply, first-committer-wins on the write set.

## Alternatives considered

- **Keep the exclusive apply lock ([ADR-0016](./0016-write-overlay.md))** — simple `&mut Graph`.
  Rejected; it is the fanout×WAL convoy measured on the fraud loadtest.
- **Clone the graph per read ([ADR-0011](./0011-snapshot-isolation.md))** — true isolation, O(graph).
  Rejected again; dataset graphs make the copy dominate.
- **Group commit / delayed fsync** — reduces WAL queue, not this lock.
 Still rejected as in [ADR-0012](./0012-wal-and-parquet-checkpoints.md); orthogonal to snapshot apply.
- **Release WAL before apply only** — unblocks the next fsync while a
  scan holds the write lock, but publish still waits for the scan.
  Rejected as the sole fix; both are wanted, and concurrent maps give
  both once apply is `&Graph`.

## Consequences

- [ADR-0016](./0016-write-overlay.md)’s “read lock freezes HashMaps for the statement” is
  superseded. The read lock no longer excludes apply; it only excludes
  checkpoint/GC.
- In-flight queries must not call `Graph::node()` / `neighbors()` at
  live `commit_ts`; they use the pinned timestamp (via `ExecGraph`).
- Version GC remains checkpoint-only, under the exclusive lock.

## Later references

[ADR-0019](./0019-group-commit-shared-fsync.md): waiting independent WAL records share one `sync_all`. That is
the group-commit protocol previously bundled with delayed fsync and
rejected here; delayed fsync remains rejected. Snapshot apply is
unchanged.

[ADR-0020](./0020-segmented-wal-concurrent-checkpoint.md): Parquet dump uses a graph read lock at a pinned `snapshot_ts`.
The exclusive lock remains for version GC after the dump, not for the
WAL cut.

[ADR-0023](./0023-incremental-version-gc.md): GC no longer takes the exclusive lock or `rebuild_indexes`.
Dead versions (`xmax ≤ min(pin, commit_ts)`) are dropped per id;
indexes shrink incrementally. Dump is unchanged.

[ADR-0038](./0038-lock-free-snapshot-pins.md): MATCH, apply, and dump no longer take the database `RwLock`.
`Graph` is reached without that lock; pins are lock-free slots.
