---
type: adr
area: engine
date: 2026-08-28
deciders: MemoryDB maintainers
name: 0020-segmented-wal-concurrent-checkpoint
status: accepted
tags:
  - checkpoint
  - wal
title: Checkpoint at a frozen LSN; writers append a new WAL segment
REFERENCES:
  - "[[adr/0012-wal-and-parquet-checkpoints]]"
  - "[[adr/0013-tuple-mvcc]]"
  - "[[adr/0018-concurrent-snapshot-reads]]"
  - "[[adr/0019-group-commit-shared-fsync]]"
  - "[[adr/0022-id-window-checkpoint-shards]]"
  - "[[adr/0023-incremental-version-gc]]"
  - "[[adr/0041-timed-checkpoint]]"
  - "[[adr/0045-cdc-replication-slots]]"
  - "[[adr/0574-withdraw-cdc-replication-slots]]"
---

# 0020. Checkpoint at a frozen LSN; writers append a new WAL segment

## Context

[ADR-0012](./0012-wal-and-parquet-checkpoints.md) checkpoints by holding the WAL mutex and the exclusive graph
lock, writing Parquet, then `truncate(true)` on the single
`wal/00000001.log`. Group commit ([ADR-0019](./0019-group-commit-shared-fsync.md)) therefore stalls on Parquet
IO. POSIX cannot cut a prefix out of a file that is still being
appended. Readers already pin `snapshot_ts` ([ADR-0013](./0013-tuple-mvcc.md), [ADR-0018](./0018-concurrent-snapshot-reads.md)); a
checkpoint can use the same visibility without freezing writers.

## Decision

Checkpoint is **rotate, dump, recycle** — not stop-the-world truncate.

1. **Segments.** WAL files are `wal/NNNNNNNN.log` (`00000001.log`, …).
   Append only hits the **open** segment. `should_checkpoint` (64 MiB or
   10 000 commits) **closes** that file (fsync, open the next number).
   `:memory:` keeps one record list; recycle drops `lsn <= checkpoint_lsn`.
2. **Pin.** After rotate, `checkpoint_lsn` is the max LSN in **closed**
   segments and `snapshot_ts` is `graph.commit_ts()` (those records are
   already applied). The WAL mutex is released.
3. **Dump.** Parquet is written from rows visible at `snapshot_ts` under
   a graph **read** lock. Manifest `commit_ts` / `checkpoint_lsn` match
   that pin. Writers continue on the open segment (`apply_redo` is a
   concurrent read lock).
4. **Recycle.** After the manifest is durable, delete closed segments
   with `max_lsn <= checkpoint_lsn`. The live file is never rewritten.
5. **GC.** Version GC stays under the exclusive graph lock **after**
 the dump, so in-flight readers drain first ([ADR-0018](./0018-concurrent-snapshot-reads.md)).

`Database::checkpoint()` and last-handle drop wait for an in-flight
dump, rotate if the open segment has records, dump, recycle, then GC.

Replay is unchanged: load Parquet, apply WAL records with
`lsn > checkpoint_lsn` across segments in id order.

## Alternatives considered

- **Keep wipe-under-lock ([ADR-0012](./0012-wal-and-parquet-checkpoints.md))** — simple. Rejected; Parquet IO
  blocks every commit.
- **Truncate the middle of one file while appending** — undefined with
  concurrent `write`. Rejected.
- **Copy the tail into a new file** — extra IO and a rewrite window.
  Rejected; closing a segment is enough.
- **Dump live `commit_ts` without a pin** — mixes rows committed after
  the cut. Rejected; recovery would not match `checkpoint_lsn`.

## Consequences

- [ADR-0012](./0012-wal-and-parquet-checkpoints.md)’s “truncate the WAL” after Parquet is replaced by deleting
  whole closed segments. Layout may contain several `*.log` files.
- Auto-checkpoint no longer holds the WAL mutex for the Parquet write.
- Explicit checkpoint still waits until the snapshot is on disk.
- Follow-on: a dedicated checkpointer thread is an implementation
  detail; v1 may dump on a spawned thread or inline after rotate.

## Later references

[ADR-0022](./0022-id-window-checkpoint-shards.md): the dump step writes only dirty id-window Parquet parts
(plus a new manifest). Rotate / pin / recycle are unchanged.

[ADR-0023](./0023-incremental-version-gc.md): the GC step after recycle does not take the exclusive graph
lock or rebuild indexes. Version drop is incremental under per-id maps.

[ADR-0041](./0041-timed-checkpoint.md): a background timer (default 300 s, `0` disables) also
rotates and dumps when the WAL has uncheckpointed work. Size and
commit-count triggers are unchanged. `:memory:` has no timer.

[ADR-0045](./0045-cdc-replication-slots.md) let optional CDC slots raise the recycle cut. [ADR-0574](./0574-withdraw-cdc-replication-slots.md)
withdraws those slots. Recycle stays `checkpoint_lsn`, as this ADR
decided.

[ADR-0643](./0643-checkpoint-cuts-before-the-dump.md): the dump no longer runs under the
checkpoint's busy flag with writers waiting it out. A checkpoint freezes
its pin, its snapshot pin and its store image under the publication write
guard, then dumps from that cut while commits proceed; the dirty ids are
taken at the cut rather than cleared after the dump. Rotate, pin and
recycle keep the shape decided here, and the dump still writes rows
visible at `snapshot_ts` — under a snapshot pin instead of a graph read
lock.
