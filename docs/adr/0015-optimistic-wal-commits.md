---
type: adr
area: engine
date: 2026-08-28
deciders: MemoryDB maintainers
name: 0015-optimistic-wal-commits
status: accepted
tags:
  - transactions
  - wal
title: Optimistic writes; commit_ts assigned at WAL append
REFERENCES:
  - "[[adr/0012-wal-and-parquet-checkpoints]]"
  - "[[adr/0013-tuple-mvcc]]"
  - "[[adr/0014-single-wal-write-path]]"
  - "[[adr/0016-write-overlay]]"
  - "[[adr/0017-client-conflict]]"
  - "[[adr/0018-concurrent-snapshot-reads]]"
  - "[[adr/0019-group-commit-shared-fsync]]"
  - "[[adr/0028-user-visible-transactions]]"
  - "[[adr/0042-graph-memory-limit]]"
SUPERSEDES:
  - "[[adr/0013-tuple-mvcc]]"
---

# 0015. Optimistic writes; commit_ts assigned at WAL append

## Context

[ADR-0013](./0013-tuple-mvcc.md) serialized writers with an exclusive `RwLock` for the whole
statement: execute, discard unpublished versions, WAL `sync_all`,
`apply_redo`. Readers take the matching read lock for the entire query.
Loadtests with concurrent connections therefore queued on that lock —
including 0.2ms lookups behind a 23ms fsync — while wall-clock throughput
did not improve. MVCC already gives snapshot reads; the missing piece is
write-write concurrency until the durable serialize point.

## Decision

Writes are **optimistic**. The WAL append is the only serialize point that
assigns a commit timestamp.

1. **Txn id.** Each write attempt allocates a monotonically increasing
   `txn_id` (`AtomicU64`) at start. Unpublished versions on the private
   snapshot use that id as `xmin`.
2. **Private execute.** The writer clones the committed graph under a
   read lock, executes Cypher on that clone (MATCH+CREATE in one statement
   still sees its own writes), and drops the clone. The shared graph is
   not mutated and not exclusively locked during execute or fsync.
   Node/rel ids come from process-wide atomics so concurrent creates do
   not collide.
3. **Evaluate at WAL.** A commit mutex (the WAL) is acquired. If any
   committed version in the attempt’s **write set** has `xmin`/`xmax`
   newer than the attempt’s `snapshot_ts`, the attempt is discarded and
   the statement is retried on a fresh snapshot. Otherwise
   `commit_ts = max(last_committed + 1, txn_id)` (always increasing in
   WAL order), the record is appended and fsynced, then `apply_redo`
   publishes that `commit_ts` on the shared graph.
4. **Write set.** Physical node/rel identities in the redo: `SET` /
   `REMOVE` / `DELETE` / label changes conflict with any later version of
   that id. New `CREATE` ids conflict only if the id is already present.
   A new relationship conflicts if an endpoint visible at the snapshot
   is no longer live (deleted), not merely property-updated.

First committer on overlapping write sets wins. Disjoint creates (typical
payment edges) commit concurrently. Readers keep [ADR-0013](./0013-tuple-mvcc.md) visibility;
they are blocked only for the short `apply_redo`, not for fsync.

## Alternatives considered

- **Keep exclusive writers ([ADR-0013](./0013-tuple-mvcc.md))** — simple, no retries. Rejected;
  fsync and execute hold the lock that readers and other writers need.
- **Release the lock only around fsync, still serialize execute** —
  helps readers, not concurrent write CPU. Rejected as the durability
  protocol; execute stays private so two writers can run while a third
  fsyncs.
- **Full SSI / read-set validation** — predicate locks, write skew
  detection. Rejected; OpenCypher auto-commit has no uniqueness
  constraints, and the load mix is insert-heavy. Write-write on ids is
  enough.
- **In-place unpublished versions on the shared graph** — no clone.
  Rejected; `Graph` mutation is `&mut` and a single `in_write` flag.
  Concurrent execute would data-race HashMaps.
- **Group commit / delayed fsync** — already rejected in [ADR-0012](./0012-wal-and-parquet-checkpoints.md).

## Consequences

- [ADR-0013](./0013-tuple-mvcc.md)’s “writers serialize for the whole statement” is superseded;
  writers still serialize at WAL append + `apply_redo`.
- [ADR-0014](./0014-single-wal-write-path.md) is unchanged: commit is still WAL then `apply_redo`. Execute
  still uses unpublished versions, but only on the private snapshot;
  `abort_write` on the shared graph is not on the success path.
- Clone cost is O(graph) per write attempt. Acceptable versus fsync for
  dataset-scale graphs; an overlay buffer can replace the clone later
  without changing this protocol.
- Exhausted retries surface as a semantic error. Clients do not see
  `txn_id`.
- Bolt `BEGIN`/`COMMIT` remain auto-commit no-ops.

## Later references

[ADR-0016](./0016-write-overlay.md): write execute uses a private overlay on the shared snapshot
instead of cloning `Graph`. WAL commit protocol is unchanged.

[ADR-0017](./0017-client-conflict.md): the engine does not retry on write-write conflict; the client
sees `Error::Conflict`.

[ADR-0018](./0018-concurrent-snapshot-reads.md): `apply_redo` uses a shared graph read lock (concurrent maps).
In-flight MATCH no longer excludes publish.

[ADR-0019](./0019-group-commit-shared-fsync.md): independent waiters share one `sync_all`. `commit_ts` assignment
and first-committer-wins stay at WAL append; the fsync is no longer 1:1
with that assignment.

[ADR-0028](./0028-user-visible-transactions.md): an explicit transaction uses one `txn_id` and one write-set
check at `COMMIT`. Bolt `BEGIN` / `COMMIT` are no longer no-ops.
Auto-commit statements still commit per statement.

[ADR-0042](./0042-graph-memory-limit.md): `flush_group` also rejects growing writes that would exceed
the logical graph+index cap (`Error::Capacity`) before `wal.append_record`.
