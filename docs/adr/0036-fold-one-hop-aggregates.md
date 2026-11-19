---
type: adr
area: engine
date: 2026-09-01
deciders: MemoryDB maintainers
name: 0036-fold-one-hop-aggregates
status: accepted
tags:
  - executor
title: Fold one-hop MATCH aggregates while walking adjacency
REFERENCES:
  - "[[adr/0023-incremental-version-gc]]"
  - "[[adr/0033-compact-live-records-typed-csr]]"
---

# 0036. Fold one-hop MATCH aggregates while walking adjacency

## Context

After Waves 1–5 the 0.30 read-only mix is ~72k ops/s.
`outgoing_volume` and `amount_band` are ~25% of CPU. Both need
**every** visible out-edge (Wave 5 cannot stop early):

```cypher
MATCH (a:Account {id: $id})-[t:TRANSFER]->()
RETURN sum(t.amount) AS volume, count(t) AS n

MATCH (a:Account {id: $id})-[t:TRANSFER]->()
RETURN CASE WHEN t.amount >= 1000.0 THEN 'large' ELSE 'small' END AS band,
       count(*) AS n
```

Today MATCH allocates one row per hop, then `aggregate` walks that
vector. Degree ~67 (tail ~300). A running-sum sidecar would be wrong
under MVCC: typed lists are a visibility **superset** ([ADR-0023](./0023-incremental-version-gc.md) /
[ADR-0033](./0033-compact-live-records-typed-csr.md)). A result cache was rejected in [ADR-0033](./0033-compact-live-records-typed-csr.md).

## Decision

When MATCH is a single fixed-length hop and the next `RETURN`/`WITH`
is only streamable aggregates (`count(*)`, `count(x)`, `sum(x)`, no
`DISTINCT` on the agg) plus grouping keys that evaluate from that hop,
**fold while walking** the typed chunk:

- Snapshot-visible hops only (`xmin`/`xmax`, overlay).
- Bind a reused scratch row (start / rel stub / end stub as needed).
- Update per-group running `count` / `sum`. Do not keep one row per edge.
- Empty MATCH with no grouping key still yields the OpenCypher empty
  aggregate row (`sum` → null, `count` → 0). Empty MATCH with a grouping
  key yields no rows.

Do **not** store a per-node running sum or amount-ordered second chunk.
Do **not** cache query results. `detect_fanout` (`count(DISTINCT b)`)
and `collect` / `avg` / `stdev` stay on the row-then-aggregate path.

`OPTIONAL MATCH` and `MATCH WHERE` stay on the full path for this ADR
(`WHERE` can drop a hop after bind; residual already exists, but the
fold gate stays conservative).

## Alternatives considered

- **Per-node `(count, sum)` sidecar** — ignores snapshot visibility
  on a superset list. Rejected.
- **Amount-ordered adj (second order besides `ts`)** — extra RAM, only
  helps a range predicate, not `sum`/`count(*)` of all edges. Rejected
  for this ADR.
- **Cache last aggregate per account** — invalidates on every write.
 Rejected in [ADR-0033](./0033-compact-live-records-typed-csr.md).
- **Keep one row per hop** — correct, still O(degree) allocate+hash.
  Rejected; that is the 25%.

## Consequences

- Typical `outgoing_volume` / `amount_band` cost is one adj walk and
  a few `rel_prop` reads, not N `IndexMap` rows.
- Writes that change `amount` need no extra adj maintenance (no sidecar).
- Unit tests must match the row-then-aggregate answers, including
  empty graphs and two CASE bands.
