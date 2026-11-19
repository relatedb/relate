---
type: adr
area: engine
date: 2026-08-30
deciders: MemoryDB maintainers
name: 0023-incremental-version-gc
status: accepted
tags:
  - storage
title: Incremental version GC does not take the exclusive graph lock
REFERENCES:
  - "[[adr/0018-concurrent-snapshot-reads]]"
  - "[[adr/0020-segmented-wal-concurrent-checkpoint]]"
  - "[[adr/0033-compact-live-records-typed-csr]]"
  - "[[adr/0038-lock-free-snapshot-pins]]"
SUPERSEDES:
  - "[[adr/0018-concurrent-snapshot-reads]]"
---

# 0023. Incremental version GC does not take the exclusive graph lock

## Context

[ADR-0018](./0018-concurrent-snapshot-reads.md) / [ADR-0020](./0020-segmented-wal-concurrent-checkpoint.md) dump Parquet under a graph **read** lock so
`apply_redo` continues. After the dump, `gc_versions` took the **write**
lock, dropped dead `xmin`/`xmax` versions, and called `rebuild_indexes()`
(full rescan). At fraud scale 0.30 that exclusive section stalled MATCH
and apply for seconds (one op measured 5.9 s) and cut throughput ~70%.

Indexes are already a **superset** ([ADR-0018](./0018-concurrent-snapshot-reads.md)): lookups re-check
`node_at`. A full rebuild is not required for correctness. A version is
safe to free when no pin has `snapshot_ts ∈ [xmin, xmax)`.

## Decision

1. **Pins.** `execute_read` and optimistic write execute register
   `snapshot_ts` on the graph (`pin_now` / drop). `gc_horizon` is
   `min(commit_ts, min in-flight pin)` (or `commit_ts` if none).
2. **Drop.** A version with `xmax ≠ 0` is dropped when `xmax ≤
   gc_horizon`. Live rows (`xmax == 0`) stay. Each id’s version vec is
   mutated under that DashMap entry only. Other ids and other statements
   proceed.
3. **Indexes.** When versions of a node are dropped, remove that id from
   label/property keys that **no remaining version** of the id still
   carries. No `label_index` / `prop_index` clear+rescan. Adjacency stays
   a superset (neighbors already filter `rel()`).
4. **Locks.** Checkpoint GC uses the graph **read** lock (same as dump /
   apply). The exclusive write lock is not taken for GC.

## Alternatives considered

- **Keep write lock + `rebuild_indexes`** — correct, O(graph) stall.
  Rejected; measured at scale 0.30.
- **Skip version GC** — indexes and chains grow forever. Rejected.
- **Epoch reclaim without a pin table** — more machinery. Rejected until
  the pin horizon is not enough.

## Consequences

- [ADR-0018](./0018-concurrent-snapshot-reads.md)’s “checkpoint/GC take the write side and rebuild indexes” is
 superseded for GC. Dump is still a read lock ([ADR-0020](./0020-segmented-wal-concurrent-checkpoint.md)).
- A statement must `pin_now` before it reads versions so GC cannot
  drop a row in `[xmin, xmax)` for that pin.
- Index entries may lag by one GC if a race leaves a stale id; lookups
  still filter visibility.

## Later references

[ADR-0033](./0033-compact-live-records-typed-csr.md): live records and typed adj are compact in RAM. GC still
drops per-id version chains and incremental index keys; adjacency
remains a superset.

[ADR-0038](./0038-lock-free-snapshot-pins.md): the pin table is a lock-free atomic slot array (claim, then
load `published_ts`). The horizon rule is unchanged. Epoch reclaim
without a pin table stays rejected.
