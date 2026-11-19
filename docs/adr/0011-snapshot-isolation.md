---
type: adr
area: engine
date: 2026-08-28
deciders: MemoryDB maintainers
name: 0011-snapshot-isolation
status: superseded
status_note: by [ADR-0013](./0013-tuple-mvcc.md)
tags:
  - transactions
title: Snapshot isolation via read snapshots; writers serialize
REFERENCES:
  - "[[adr/0004-memory-first-parquet-object-store]]"
  - "[[adr/0012-wal-and-parquet-checkpoints]]"
  - "[[adr/0013-tuple-mvcc]]"
  - "[[adr/0028-user-visible-transactions]]"
---

# 0011. Snapshot isolation via read snapshots; writers serialize

## Context

v1 serialized every connection behind a mutex. Concurrent reads of a
shared embed were blocked by writers and by each other. Full MVCC with
per-tuple versions is a large engine change.

## Decision

Replace the connection mutex with `RwLock`:

- **Write** queries (CREATE, MERGE, SET, REMOVE, DELETE) take the write
  lock and mutate the live graph.
- **Read** queries take the read lock, **clone** the graph (snapshot),
  and execute on the clone so writers are not blocked for the whole
  query after the clone, and readers do not observe partial writes.

This is snapshot isolation for readers, not tuple-level MVCC.

## Alternatives considered

- **Keep a single mutex** — simple, no snapshot. Rejected; blocks the
  HTTP sidecar and Bolt sessions on large MATCH.
- **Full MVCC from day one** — correct long-term, delays datasets and
  TCK. Rejected for v1; a later ADR may introduce versioned tuples.
- **Optimistic concurrency with rollback** — extra complexity without
  a WAL. Rejected.

## Consequences

- Large graphs make read clones expensive; acceptable while the working
 set is RAM-sized ([ADR-0004](./0004-memory-first-parquet-object-store.md)).
- Bolt BEGIN/COMMIT remain auto-commit no-ops until a transaction ADR.
- Writers still serialize with each other.

## Later references

[ADR-0013](./0013-tuple-mvcc.md) replaces graph `clone` snapshots with tuple **xmin/xmax**
MVCC. Readers pin `snapshot_ts = last_committed` and filter visibility;
they do not copy the whole graph. Writers still serialize. Bolt
BEGIN/COMMIT remain auto-commit no-ops. Optimistic rollback is still
out of scope; durability is [ADR-0012](./0012-wal-and-parquet-checkpoints.md)’s WAL, not per-reader undo. The
“full MVCC rejected for v1” alternative in this file is reversed by
[ADR-0013](./0013-tuple-mvcc.md).

[ADR-0028](./0028-user-visible-transactions.md): Bolt/embedded `BEGIN` / `COMMIT` / `ROLLBACK` are real
connection-scoped transactions. Auto-commit remains the default. The
no-op consequence above no longer holds.
