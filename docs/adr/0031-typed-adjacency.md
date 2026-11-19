---
type: adr
area: engine
date: 2026-09-01
deciders: MemoryDB maintainers
name: 0031-typed-adjacency
status: accepted
tags:
  - storage
title: Adjacency is partitioned by relationship type
REFERENCES:
  - "[[adr/0010-label-property-indexes]]"
  - "[[adr/0026-eager-residual-match-predicates]]"
  - "[[adr/0033-compact-live-records-typed-csr]]"
  - "[[adr/0035-ordered-expand-ts]]"
---

# 0031. Adjacency is partitioned by relationship type

## Context

Expand walks `out_adj` / `in_adj` as a flat `Vec<RelId>` per node, then
`rel_at` + a type check (`[:TRANSFER]`, `[:E]`, …). On mixed-type graphs
that inspects every incident relationship. [ADR-0010](./0010-label-property-indexes.md) deferred
relationship-type indexes. [ADR-0026](./0026-eager-residual-match-predicates.md) said not to add them yet.

The fraud mix is almost all `TRANSFER`, so the win there is small. Identity
`REPORTS_TO` paths and any later multi-type graph pay the full scan.
Typed CSR (a later storage ADR) needs this partition first.

## Decision

Store in/out adjacency as **per-node, per-type** id lists
(`NodeId → rel_type → Vec<RelId>`). Expand with a non-empty type list
iterates only those partitions. A bare `-[]-` or empty type list still
visits every partition.

`rel_at` still applies for snapshot visibility and endpoints. The type
string compare is skipped when the partition already is that type.

Overlay scratch adjacency may stay a flat list (in-tx, small) and keep
the type check.

No Cypher `CREATE INDEX ON :TYPE`. This is an engine layout detail.

## Alternatives considered

- **Keep flat adj + type-check** — simple, O(degree) `rel_at` on every
  typed expand. Rejected as the long-term access method.
- **Global type → all rel ids** — helps “all TRANSFERs”, not per-node
  expand. Rejected for MATCH walk / BFS.
- **Jump to compact CSR now** — larger storage rewrite, same partition
  decision. Rejected; that is a later ADR.

## Consequences

- `rebuild_adj`, create/load rel, and GC-adjacent rebuild must write the
  typed maps.
- MATCH walk and `shortestPath` BFS pass `RelPattern.types` into
  neighbor iteration.
- Fraud `[:TRANSFER]` expand cost stays ~degree (one type). Mixed-type
  nodes get cheaper typed hops.

## Later references

[ADR-0033](./0033-compact-live-records-typed-csr.md): live adjacency is interned typed chunks (compact CSR in RAM),
still a visibility-filtered superset. Overlay scratch may stay flat.

[ADR-0035](./0035-ordered-expand-ts.md): each typed chunk is ordered by integer `ts` (DESC, nulls first)
so `ORDER BY t.ts DESC LIMIT` can stop after `SKIP+LIMIT` visible hops.
