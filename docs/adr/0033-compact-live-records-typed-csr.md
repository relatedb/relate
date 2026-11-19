---
type: adr
area: engine
date: 2026-09-01
deciders: MemoryDB maintainers
name: 0033-compact-live-records-typed-csr
status: accepted
tags:
  - storage
title: Compact live records and typed CSR in RAM
REFERENCES:
  - "[[adr/0005-lpg-parquet-layout]]"
  - "[[adr/0022-id-window-checkpoint-shards]]"
  - "[[adr/0023-incremental-version-gc]]"
  - "[[adr/0031-typed-adjacency]]"
  - "[[adr/0035-ordered-expand-ts]]"
---

# 0033. Compact live records and typed CSR in RAM

## Context

The live graph in `Graph` stores a full public `Node` / `Relationship`
(`Vec<String>` labels, `BTreeMap<String, Value>` properties) on every
MVCC version, and adjacency as `HashMap<String, Vec<RelId>>` per node
([ADR-0031](./0031-typed-adjacency.md)). At fraud scale that is ~5.3 KiB/entity; 8.5× more edges
grows RSS from ~141 MiB to ~1 GiB. Expand still clones a relationship
map to test snapshot visibility.

[ADR-0005](./0005-lpg-parquet-layout.md) already rebuilds adjacency in RAM from Parquet rows. [ADR-0031](./0031-typed-adjacency.md)
deferred packed CSR to a later storage ADR. Wave 3 of the read-path
program is that ADR: in-RAM layout only.

## Decision

Keep Parquet, WAL `RedoOp`, public `Node` / `Relationship`, MVCC
`xmin`/`xmax`, snapshot pins, write overlay, and `apply_redo`
unchanged.

Change only the live store:

1. **Intern** label, relationship-type, and property-key strings. Live
   records hold ids. Materialize public maps at `node_at` / `rel_at`,
   checkpoint encode, and overlay copy-on-write.
2. **Compact node records** — interned labels plus a compact property
   list (interned keys + `Value`), not a `BTreeMap` per version.
3. **Compact relationship payloads** — the common three-key map
   `{id: STRING, amount: NUMBER, ts: INTEGER}` is a fixed struct.
   Any other shape (or a later SET that breaks the shape) uses the same
   compact property list as nodes.
4. **Typed CSR / per-type chunks** — `out_adj` / `in_adj` are
   interned `type_id → Vec<RelId>` chunks, not `HashMap<String, …>`.
   Incremental create appends to the type’s chunk. `rebuild_adj` writes
 the same shape. Lists stay a **superset** ([ADR-0023](./0023-incremental-version-gc.md)): delete does
   not unlink; expand still filters with visibility.

No query/result cache. No Parquet or WAL format change. No
`CREATE INDEX`. Overlay scratch may stay a flat id list ([ADR-0031](./0031-typed-adjacency.md)).

## Alternatives considered

- **Change Parquet / add on-disk CSR** — [ADR-0005](./0005-lpg-parquet-layout.md) already allows
  derived CSR files later. Rejected for this ADR; checkpoint schema
  and load stay row-oriented JSON properties.
- **Cache recent expands or shortestPath results** — hides layout cost
  and needs invalidation on every write. Rejected; this wave is
  density and hop cost, not a cache.
- **Keep public maps as the live record; CSR only** — expand still
  allocates a `BTreeMap` per `rel_at`. Rejected; both sides of the
  record+adj pair are the RSS problem.
- **Lock-free packed global CSR with in-place rewrite** — better
  sequential scan, hostile to concurrent `apply_redo` appends and
  abort rebuild. Rejected; per-node typed chunks preserve DashMap
  isolation.

## Consequences

- `node_at` / `rel_at` allocate the public value types; visibility
  checks on expand must not require a full materialize.
- Intern tables are append-only for the process lifetime (few distinct
  keys/types). Unused intern ids after GC are accepted.
- SET/REMOVE that leaves the three-key shape must promote a compact
  triple to a general property list. Round-trip `Value` kinds (int vs
  float) must be preserved.
- [ADR-0005](./0005-lpg-parquet-layout.md) / [ADR-0022](./0022-id-window-checkpoint-shards.md) checkpoint tests still go through
  `load_checkpoint_*` → intern + compact install → `rebuild_indexes`.
- Measure fraud RSS / KiB/entity and the read-only mix after landing.

## Later references

[ADR-0035](./0035-ordered-expand-ts.md): typed chunks stay a visibility superset but are stored in
integer-`ts` DESC order (nulls first). Create / set-`ts` / rebuild
maintain that order; expand may stop at `ORDER BY t.ts DESC LIMIT`.
