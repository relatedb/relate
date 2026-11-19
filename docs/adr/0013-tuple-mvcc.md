---
type: adr
area: engine
date: 2026-08-28
deciders: MemoryDB maintainers
name: 0013-tuple-mvcc
status: accepted
tags:
  - transactions
title: Tuple-level MVCC for snapshot reads
REFERENCES:
  - "[[adr/0005-lpg-parquet-layout]]"
  - "[[adr/0010-label-property-indexes]]"
  - "[[adr/0011-snapshot-isolation]]"
  - "[[adr/0012-wal-and-parquet-checkpoints]]"
  - "[[adr/0015-optimistic-wal-commits]]"
  - "[[adr/0018-concurrent-snapshot-reads]]"
SUPERSEDES:
  - "[[adr/0011-snapshot-isolation]]"
---

# 0013. Tuple-level MVCC for snapshot reads

## Context

[ADR-0011](./0011-snapshot-isolation.md) gave readers snapshot isolation by cloning the entire in-memory
graph under a read lock. That clone is expensive on dataset-scale graphs
and still serializes the start of every read behind the `RwLock`. A WAL
([ADR-0012](./0012-wal-and-parquet-checkpoints.md)) makes optimistic rollback less necessary: durability is the
log, not undo of a published commit. Versioned tuples are now justified.

## Decision

- Each stored node and relationship version has **`xmin` / `xmax`**.
  `xmax = 0` means the version is live. `SET`/`REMOVE` install a new
  version of the same Cypher identity (`NodeId` / `RelId`). `DELETE`
  sets `xmax`.
- A **read** query pins `snapshot_ts = last_committed` and uses
  visibility `xmin <= snapshot_ts && (xmax == 0 || xmax > snapshot_ts)`.
  It does **not** clone the graph.
- **Writers serialize** (exclusive write lock). MVCC is for concurrent
  readers, not write-write concurrency.
- Indexes ([ADR-0010](./0010-label-property-indexes.md)) return candidate ids; the executor filters by
 visibility. Checkpoint ([ADR-0012](./0012-wal-and-parquet-checkpoints.md)) writes only rows visible at the
  checkpoint snapshot and GCs versions with `xmax <= checkpoint_ts`
  when no reader is pinned older than that.
- Public `Node` / `Relationship` values returned to clients do not
  expose xmin/xmax.

## Alternatives considered

- **Keep clone-on-read ([ADR-0011](./0011-snapshot-isolation.md))** — correct snapshots, O(graph) copy
  per MATCH. Rejected now that WAL exists and dataset graphs are large
  enough for the clone to matter.
- **Concurrent writers / SSI** — extra conflict detection. Rejected;
  writers stay serialized.
- **Store all versions in Parquet** — checkpoint stays a latest-visible
 snapshot ([ADR-0005](./0005-lpg-parquet-layout.md) / [ADR-0012](./0012-wal-and-parquet-checkpoints.md)). Rejected; MVCC is in RAM only.
- **Copy-on-write generation trees** — similar isolation, harder
  adjacency/index updates. Rejected in favor of xmin/xmax chains.

## Consequences

- Read queries no longer take a long-lived exclusive copy of `Graph`.
- Bolt BEGIN/COMMIT remain auto-commit no-ops ([ADR-0011](./0011-snapshot-isolation.md) consequence
  carried forward).
- Executor MATCH/expand/index scans must honor `snapshot_ts`.
- [ADR-0011](./0011-snapshot-isolation.md) is superseded.

## Later references

[ADR-0015](./0015-optimistic-wal-commits.md): writers no longer hold the exclusive lock for the whole
statement. Snapshot reads are unchanged; write-write conflicts are
checked when the WAL assigns `commit_ts`.

[ADR-0018](./0018-concurrent-snapshot-reads.md): `apply_redo` no longer needs the exclusive graph lock.
Snapshot queries and apply overlap; visibility is still `xmin`/`xmax`.
