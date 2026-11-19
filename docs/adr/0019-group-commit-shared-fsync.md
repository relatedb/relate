---
type: adr
area: engine
date: 2026-08-28
deciders: MemoryDB maintainers
name: 0019-group-commit-shared-fsync
status: accepted
tags:
  - wal
title: Waiting independent WAL records share one fsync
REFERENCES:
  - "[[adr/0012-wal-and-parquet-checkpoints]]"
  - "[[adr/0014-single-wal-write-path]]"
  - "[[adr/0015-optimistic-wal-commits]]"
  - "[[adr/0017-client-conflict]]"
  - "[[adr/0028-user-visible-transactions]]"
  - "[[adr/0077-embedded-durability-boundary]]"
  - "[[adr/0094-preflight-wal-acceptance]]"
---

# 0019. Waiting independent WAL records share one fsync

## Context

[ADR-0012](./0012-wal-and-parquet-checkpoints.md) required a `sync_all` for every successful write so a crash
cannot lose a commit the client already saw. Combined with auto-commit
(one Cypher statement = one WAL record), concurrent connections each
pay a full fsync (~4 ms on a laptop APFS volume). The fraud loadtest
with `--workers` 1 vs 5 vs 10 therefore sits at ~250 durable commits/s:
extra waiters lengthen the WAL queue instead of raising throughput.
[ADR-0015](./0015-optimistic-wal-commits.md) already treats the WAL mutex as the serialize point for
`commit_ts` and first-committer-wins; the fsync does not need to be
1:1 with that assignment.

[ADR-0012](./0012-wal-and-parquet-checkpoints.md)’s rejected alternative bundled **group commit** with
**delayed fsync** (return SUCCESS before durability). Those are
different protocols.

## Decision

Independent commits that are already waiting at the WAL **share one
`sync_all`**. Each remains its **own WAL record** with its own LSN and
`commit_ts`. A client does not observe SUCCESS until the fsync that
covers its LSN has returned. There is no timer and no durability
window: a lonely commit fsyncs immediately.

Write-write evaluation is unchanged ([ADR-0015](./0015-optimistic-wal-commits.md), [ADR-0017](./0017-client-conflict.md)):

1. Assign `commit_ts` and append in waiter order.
2. Conflict-check each attempt against the applied graph **and**
   earlier records in this group that are written but not yet applied.
3. Overlapping write sets are not appended; the later waiter gets
   `Error::Conflict`. Only independent records share the fsync.

`apply_redo` still runs **after** the group fsync, in `commit_ts`
order ([ADR-0014](./0014-single-wal-write-path.md)). Readers do not see a commit that is only in the
page cache.

Suggested approach (implementation detail, not binding): a handoff
queue so waiters park without holding the WAL mutex; one leader
drains the queue, appends the independent records, `sync_all`s once,
applies, and wakes the batch.

## Alternatives considered

- **Keep fsync-per-record ([ADR-0012](./0012-wal-and-parquet-checkpoints.md))** — simplest durability. Rejected
  because concurrent writers cannot exceed one fsync per commit.
- **Delayed / async commit** — return SUCCESS before `sync_all`.
 Rejected; that is the lost-commit window [ADR-0012](./0012-wal-and-parquet-checkpoints.md) refused. Group
  commit is not that protocol.
- **Timed batching** — sleep to grow the group. Rejected; it adds
  latency to single-threaded commits the loadtest still runs.
- **Merge waiters into one WAL record** — fewer bytes, one LSN.
  Rejected; crash recovery, `commit_ts`, and OCC stay per statement
 ([ADR-0012](./0012-wal-and-parquet-checkpoints.md) unit of commit).
- **Explicit multi-statement transactions** — one fsync per
  `BEGIN`/`COMMIT`. Rejected as this ADR; Bolt BEGIN/COMMIT remain
  no-ops. Orthogonal and still useful for payroll-style loops.

## Consequences

- [ADR-0012](./0012-wal-and-parquet-checkpoints.md)’s “every successful write `sync_all`s” holds for the default
  `full` durability mode: every SUCCESS is covered by a barrier, not
 necessarily a private one. [ADR-0077](./0077-embedded-durability-boundary.md) defines the relaxed modes.
- The rejected “group commit / delayed fsync” bullet in [ADR-0012](./0012-wal-and-parquet-checkpoints.md) is
  split: delayed fsync stays rejected; group commit of independent
  records is accepted here.
- Serial commits are unchanged (group size 1). Concurrent disjoint
  `CREATE`s should see `sync_all` count grow slower than commit count.
- Occupancy of the WAL mutex is append + one fsync + apply for the
  group, not one fsync per waiter.
- Follow-on: multi-`CREATE` in one statement (or real transactions)
  still reduces fsyncs for a single connection; this ADR does not
  change auto-commit.

## Later references

[ADR-0028](./0028-user-visible-transactions.md): explicit `BEGIN` / `COMMIT` / `ROLLBACK` are implemented.
Auto-commit is unchanged. One explicit transaction is still one WAL
record and therefore one group-commit waiter. The “BEGIN remain no-ops”
alternative above no longer holds.

[ADR-0077](./0077-embedded-durability-boundary.md) later adds configurable durability. This ADR's SUCCESS-after-barrier
guarantee applies to the default `full` mode. `everysecond` and `snapshot`
acknowledge after apply and use a later WAL barrier or checkpoint,
respectively.

[ADR-0094](./0094-preflight-wal-acceptance.md) (Proposed): deterministic size, ID, serialization and ordered-redo
failures are preflighted across the entire eligible group before its first
append. This refines the failure boundary for independent records without
changing the one-fsync success rule or per-waiter conflict/capacity policy;
post-append I/O outcomes remain uncertain rather than known rejections.
