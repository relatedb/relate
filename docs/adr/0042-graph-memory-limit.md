# 0042. Logical cap on the live graph and data-dependent indexes

- **Status**: Accepted
- **Date**: 2026-09-04
- **Deciders**: MemoryDB maintainers
- **Later references**: amended by ADR-0202 (the floor keeps retained
  versions, vector capacity, adjacency containers, and interned names);
  ADR-0529 (a never-updated row is inline, with no `xmax`; a delete's
  tombstone spill does not refuse admission)

## Context

The working set is RAM (ADR-0004, ADR-0033). Operators need a **customer
data** ceiling so a busy writer cannot grow the process without bound.
Process RSS is the wrong meter: it includes WAL, Parquet buffers, query
scratch, and allocator slack (load-test reports already show that).

`CREATE INDEX` names a `(label, property)` pair (ADR-0040). The RAM that
grows with customer data is the always-on label, equality, prefix, and
trigram postings (ADR-0010, ADR-0030). Those must be in the budget.

ADR-0015 serializes commits at WAL append. A check after `apply_redo`
would fsync a record the engine then refuses to represent.

## Decision

Cap a **documented logical size** of live customer graph plus
data-dependent indexes. Default is **unlimited**.

**In budget.** Live (`xmax == 0`) node and relationship versions, typed
adjacency hops for live rels, and the always-on index postings implied
by those live nodes. Future opt-in index structures use the same
counter. MVCC tombstones until GC are **not** charged so `DELETE` /
shrinking `SET` can free quota immediately.

**Out of budget.** WAL, Parquet / object-store buffers, overlay and
query scratch, pin table, procedures, intern key table, allocator
fragmentation.

**Size.** `logical_bytes` on `Value` plus fixed overheads:

| Constant | Bytes | Meaning |
| --- | --- | --- |
| `OVERHEAD_VERSION` | 24 | `xmin` / `xmax` / slot |
| `OVERHEAD_POSTING` | 8 | one node id in an index set |
| `OVERHEAD_MAP_ENTRY` | 24 | DashMap / BTree key slot |
| `OVERHEAD_ADJ_HOP` | 16 | one typed CSR hop |
| `OVERHEAD_STRING` | 16 | `String` header (plus UTF-8 bytes) |

`used` is maintained incrementally on mutate and recomputed after
`rebuild_indexes`. Tests compare the counter to a full live walk.

**Enforce.** In `flush_group`, after write-set conflict and **before**
`wal.append_record`, compute `delta` from the redo (creates/sets minus
deletes/shrinks, including index and adj cost). If a limit `L > 0` is
set, `delta > 0`, and `used + reserved + delta > L`, complete that
waiter with `Error::Capacity { used, limit }` and skip the slot. Reserve
positive deltas inside the batch so two concurrent commits cannot both
pass. The engine does not retry (same client-visible rule as
ADR-0017).

**Reads** never check. **Open and WAL/Parquet replay always apply**,
even when the snapshot is already over the limit. Growing writes after
open then fail.

**Config.** `OpenOptions.memory_limit`: `Some(n)` is `n` bytes;
`Some(0)` is unlimited; `None` uses `CYPHERLITE_MEMORY_LIMIT` or
unlimited. A value that does not parse fails open with the same semantic
error as a bad `--memory-limit`; it is not treated as unlimited. CLI / Bolt
`--memory-limit <SIZE>` and the env var accept an
integer (bytes) or an integer plus `b`/`bytes`/`mb`/`mib`/`gb`/`gib`
(case-insensitive; optional space). `mb`/`gb` are SI (10^6 / 10^9);
`mib`/`gib` are IEC (2^20 / 2^30). C ABI: env at `memorydb_open`;
message `capacity exceeded: used={used} limit={limit}`.

## Alternatives considered

- **Process RSS / cgroup** — unstable and includes WAL and query heap.
  Rejected; the cap is graph + indexes.
- **Check only after `apply_redo`** — durable commit the engine cannot
  keep. Rejected; WAL is the serialize point (ADR-0014 / ADR-0015).
- **Fail `open` when snapshot > limit** — crash recovery of a full
  store would not load. Rejected; replay always applies.
- **Block reads when full** — operators need `MATCH` / `DELETE` to
  recover. Rejected.
- **Charge only named `CREATE INDEX` rows** — those are catalog names;
  the RAM is always-on postings. Rejected.
- **Charge MVCC tombstones** — `DELETE` would not free quota until
  checkpoint GC. Rejected so shrinks recover immediately.

## Consequences

- New `Error::Capacity { used, limit }`.
- `Database::memory_usage()` returns `(used, Option<limit>)`.
- Unlimited default: TCK and existing tests unchanged.

## Later references

ADR-0063: prefix/trigram postings are lazily materialized, so their budget
charge is a deterministic **formula** of the live node (1–3-gram window
positions per `(label, key)` string property) rather than a count of
materialized postings. `used` still cannot move from a read, and the
incremental counter and the live-walk oracle share one accounting core.

ADR-0207: the text term of that formula is now one posting per width-3
window, `char_len - 2`, instead of the 1–3-gram window sum.
