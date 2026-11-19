---
type: adr
area: engine
date: 2026-08-28
deciders: MemoryDB maintainers
name: 0016-write-overlay
status: accepted
tags:
  - write-path
title: Write execute uses a private overlay, not a graph clone
REFERENCES:
  - "[[adr/0013-tuple-mvcc]]"
  - "[[adr/0015-optimistic-wal-commits]]"
  - "[[adr/0018-concurrent-snapshot-reads]]"
SUPERSEDES:
  - "[[adr/0015-optimistic-wal-commits]]"
---

# 0016. Write execute uses a private overlay, not a graph clone

## Context

[ADR-0015](./0015-optimistic-wal-commits.md) made WAL append the write serialize point, but execute still
cloned the committed graph so `CREATE`/`SET` could mutate a private
`HashMap` (MATCH+CREATE in one statement). That copy is O(graph). On the
fraud loadtest (~24k relationships) it raised the uncontended CREATE
floor from ~22ms to ~86ms — more than the fsync the protocol was meant
to overlap. Optimistic concurrency only needs a snapshot timestamp and a
write buffer, not a replica of every tuple.

## Decision

Write execute pins `snapshot_ts` under a **shared read lock** and runs
against the live `Graph` plus a **private `WriteScratch` overlay**:

- Committed reads use [ADR-0013](./0013-tuple-mvcc.md) visibility at `snapshot_ts` on the shared
  graph. The read lock is held only for execute so `apply_redo` cannot
  mutate HashMaps concurrently (they are not concurrent maps).
- Local `CREATE`/`SET`/`DELETE` land in the overlay (nodes, rels, adj).
  MATCH in the same statement sees overlay first, then the snapshot.
- Overlay is dropped on abort. On success the overlay’s redo is
 evaluated at WAL ([ADR-0015](./0015-optimistic-wal-commits.md)) and `apply_redo` publishes it.
- Node/rel ids still come from process-wide atomics.
- The read lock is released **before** WAL `sync_all`.

Reads (`execute_read`) are unchanged: no overlay, no clone.

## Alternatives considered

- **Keep the full-graph clone ([ADR-0015](./0015-optimistic-wal-commits.md) private snapshot)** — simple
  MATCH+CREATE. Rejected; clone dominated write latency on dataset
  graphs.
- **Concurrent HashMaps (DashMap) and no read lock during execute** —
  true MVCC reads while another txn `apply_redo`s. Rejected for this
  change; extra interior mutability. The read lock already freezes the
  snapshot for the statement.
- **Thread-local overlay hidden inside `Graph`** — avoids `ExecGraph`.
  Rejected; implicit TLS is harder to test than an explicit scratch.

## Consequences

- [ADR-0015](./0015-optimistic-wal-commits.md)’s “clone the committed graph” execute step is superseded.
  WAL evaluation, `commit_ts` assignment, and first-committer-wins on
  the write set are unchanged.
- Execute of a write holds the graph read lock for MATCH/CREATE CPU, not
  for fsync. Long `detect_fanout` on another connection still delays
  `apply_redo` (write lock), as today.
- Overlay lookup scans the statement’s created/updated ids in addition
  to base indexes (those sets are small).

## Later references

[ADR-0018](./0018-concurrent-snapshot-reads.md): apply uses concurrent maps and a shared read lock, so a long
MATCH no longer delays `apply_redo`. Overlay execute is unchanged.
