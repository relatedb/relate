---
type: adr
area: engine
date: 2026-09-01
deciders: MemoryDB maintainers
name: 0035-ordered-expand-ts
status: accepted
tags:
  - storage
title: Typed adj is ordered by integer `ts` for `ORDER BY t.ts DESC LIMIT`
REFERENCES:
  - "[[adr/0023-incremental-version-gc]]"
  - "[[adr/0031-typed-adjacency]]"
  - "[[adr/0033-compact-live-records-typed-csr]]"
  - "[[adr/0036-fold-one-hop-aggregates]]"
  - "[[adr/0203-newest-first-expansion-order]]"
---

# 0035. Typed adj is ordered by integer `ts` for `ORDER BY t.ts DESC LIMIT`

## Context

Top-K (Wave 1) avoids sorting every MATCH row. Streaming `DISTINCT`+`LIMIT`
(Wave 4a) stops an unordered expand once enough distinct keys exist. Neither
helps `recent_transfers` / `activity_page`:

```cypher
MATCH (a:Account {id: $id})-[t:TRANSFER]->(b)
RETURN t.id, t.amount, b.id
ORDER BY t.ts DESC LIMIT 20
-- activity_page also SKIP $skip LIMIT 10
```

Those queries still touch every out-edge to learn `t.ts`. After Waves 1–4
they own about half of 0.30 read-only CPU. Triple payloads already store
`ts` as `i64` ([ADR-0033](./0033-compact-live-records-typed-csr.md)). The missing piece is not reading older edges.

The read-path plan reserved this as number 0032. That number was never
written; this file is the next free prefix ([ADR-0035](./0035-ordered-expand-ts.md)).

## Decision

Keep each typed adjacency chunk **ordered for `ORDER BY ts DESC`**: missing
integer `ts` first (OpenCypher NULL ranks after NUMBER in ASC, so first in
DESC), then present `ts` descending, then `RelId`. Maintain that order on
`CREATE`, `SET t.ts` / `REMOVE t.ts`, checkpoint load, and `rebuild_adj`.
Do **not** assume append order is `ts` order.

Lists stay a visibility **superset** ([ADR-0023](./0023-incremental-version-gc.md) / [ADR-0033](./0033-compact-live-records-typed-csr.md)): `DELETE` does
not unlink. Expand still filters `xmin`/`xmax`.

When MATCH is a single typed, directed, fixed-length hop and the next
`RETURN`/`WITH` is `ORDER BY <that rel>.ts DESC` plus `LIMIT` (and optional
`SKIP`), walk the chunk in stored order and stop after `SKIP+LIMIT`
visible hits. Overlay scratch is unordered ([ADR-0031](./0031-typed-adjacency.md)); a write-tx MATCH
does not take this shortcut. Several input rows (global top-K across
accounts) do not either.

No “last 20” result cache (rejected in [ADR-0033](./0033-compact-live-records-typed-csr.md)). `outgoing_volume` /
`amount_band` still scan every edge. `ORDER BY t.ts ASC`, `ORDER BY`
on another key, `DISTINCT`, `OPTIONAL MATCH`, and `MATCH WHERE` stay on
the full expand + heap.

The maintained key is integer `ts` only (`RelPayload::Triple` or `Other`
with `ts: INTEGER`). Other kinds sort as missing.

## Alternatives considered

- **Keep unordered chunks + heap** — correct, still O(degree) on the two
  queries that dominate the mix. Rejected.
- **Cache each account’s last 20 transfers** — invalidates on every
 write; already rejected as cache-like in [ADR-0033](./0033-compact-live-records-typed-csr.md).
- **Assume create-append is time order** — backfill, `SET t.ts`, and
  replay break it. Rejected; maintain order explicitly.
- **Per-node `(ts, RelId)` sidecar instead of reordering the chunk** —
  same access method, extra allocation. Rejected; store the key on the
  hop and keep one vec.
- **Early-stop for `ASC` / `ORDER BY t.amount`** — needs another order
  or a full scan. Not on the fraud mix. Rejected for this ADR.

## Consequences

- Typical fraud degree (~67, tail ~300) becomes O(`LIMIT`) visits for
  those two queries, not O(degree).
- Writes that change `ts` binary-insert into both in and out chunks.
- In-tx MATCH after `CREATE`/`SET` uses the heap path until commit.
- Unit tests must create out of `ts` order and `SET ts`, and must show
  neighbor walks stop at `SKIP+LIMIT`.

## Later references

[ADR-0036](./0036-fold-one-hop-aggregates.md): one-hop `sum` / `count` / grouped `count(*)` fold while
walking the same typed chunks; still no running-sum sidecar.

[ADR-0203](./0203-newest-first-expansion-order.md): the "then `RelId`" tie-break flips to **descending** `RelId`
(newest-first), matching the Neo4j reference's relationship-chain walk
(LABS-255/LABS-258).
