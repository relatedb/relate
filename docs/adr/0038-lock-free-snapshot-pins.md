---
type: adr
area: engine
date: 2026-09-01
deciders: MemoryDB maintainers
name: 0038-lock-free-snapshot-pins
status: accepted
tags:
  - perf
  - transactions
title: Lock-free snapshot pins; no database `RwLock` on the query path
REFERENCES:
  - "[[adr/0013-tuple-mvcc]]"
  - "[[adr/0018-concurrent-snapshot-reads]]"
  - "[[adr/0020-segmented-wal-concurrent-checkpoint]]"
  - "[[adr/0023-incremental-version-gc]]"
  - "[[adr/0111-engine-bounded-execution]]"
---

# 0038. Lock-free snapshot pins; no database `RwLock` on the query path

## Context

Identity read-only at scale 0.10 peaked at **8 workers (~108k ops/s)**
and **lost throughput** at 16/32/50 (p50 on bound reads ~3×). Two
process-wide synchronizations sit on every statement:

1. **`pin_now` / unpin** (`Mutex<HashMap<u64, u32>>`). [ADR-0023](./0023-incremental-version-gc.md) needs a
   horizon so incremental GC cannot drop a version a reader still sees.
   The RO mix never GCs, but every op still takes that mutex twice.
2. **`Shared.inner: RwLock`**. [ADR-0018](./0018-concurrent-snapshot-reads.md) took it in **read** mode for
   MATCH and `apply_redo` so checkpoint/GC could use the write side.
 [ADR-0023](./0023-incremental-version-gc.md) moved GC to the read side. Dump also takes read. The write
   side is unused on the query path (`register_procedure` and
   `persist_on_drop` only). Readers, apply, dump, and GC all share
   `.read()`; the lock no longer excludes GC. Pins do. The `RwLock`
   word still bounces on every statement.

`Graph` is already DashMap + atomics ([ADR-0018](./0018-concurrent-snapshot-reads.md)). [ADR-0023](./0023-incremental-version-gc.md) rejected
“epoch reclaim without a pin table” until the mutex horizon was not
enough. The worker sweep is that signal.

## Decision

**Pins.** Replace the pin `Mutex` with a fixed **slot array** of
`AtomicU64` (256 slots, cache-padded not required):

- `0` = free. `u64::MAX` = claiming (timestamp not stored yet).
- Occupied slot stores `snapshot_ts + 1` so `commit_ts == 0` is
  representable.
- Pin: CAS free → claiming, **then** `published_ts.load(Acquire)`, then
  store `ts + 1`. Unpin: store `0`.
- `gc_horizon`: if any slot is claiming, do not drop (horizon `0`).
  Else `min(commit_ts, min occupied ts)`.
- If every slot is occupied, spin + yield and retry (256 in-flight
  statements exceeds the loadtest). Do not fall back to a mutex.

Claim-before-load closes the race where GC samples an empty table,
then a delayed pin installs an older `ts`.

**Query path.** `Shared` owns `Graph` and `StoreTarget` without a
`RwLock`. `persist_on_drop` is an `AtomicBool`. `register_procedure`
inserts into the existing procedures DashMap. `query`, overlay
execute, `apply_redo`, dump, and GC take no database lock.

Checkpoint still pins `snapshot_ts` ([ADR-0020](./0020-segmented-wal-concurrent-checkpoint.md)). Isolation remains
`xmin`/`xmax` at the statement pin ([ADR-0013](./0013-tuple-mvcc.md)). WAL / group commit
unchanged.

## Alternatives considered

- **Keep the pin mutex; shard it** — less exclusive, still a lock on
  every read. Rejected; the horizon protocol does not need a map.
- **Skip pins when there are no writers** — the identity RO mix looks
  safe; a concurrent checkpoint is not. Rejected.
- **`pin_count` only, GC when zero** — at 100k ops/s the count is never
  zero. Need a min timestamp. Rejected.
- **`crossbeam-epoch`** — extra dependency for the same reclaim. Rejected
  until slots are not enough.
- **Keep `RwLock` “to freeze checkpoint”** — dump and GC already run
  under read and do not exclude MATCH. Rejected as a query-path lock.

## Consequences

- [ADR-0023](./0023-incremental-version-gc.md)’s pin table becomes lock-free slots; the horizon rule is
  unchanged.
- [ADR-0018](./0018-concurrent-snapshot-reads.md)’s database `RwLock` is gone from MATCH / apply / dump.
- 8→16 workers on a 35 MiB graph may still lose to LLC / DashMap
  contention; this ADR removes the two process-wide words, not cache
  ping-pong.
- Tests must hold a pin across `apply_redo` + `gc_versions` and show
  the old version remains, and hammer pin/unpin against GC.

## Later references

[ADR-0111](./0111-engine-bounded-execution.md) replaces the unbounded "spin + yield and retry" of `pin_now` when
every slot is occupied with a bounded wait that returns a typed "snapshot pin"
error. The slot array and lock-free protocol here are unchanged.
