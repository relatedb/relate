---
type: adr
area: engine
date: 2026-08-31
deciders: MemoryDB maintainers
name: 0025-shortestpath-bfs
status: accepted
tags:
  - pattern-matching
title: "`shortestPath` is BFS, not full variable-length expand"
REFERENCES:
  - "[[adr/0034-bidirectional-shortestpath]]"
  - "[[adr/0186-match-clause-shortest-path-bfs]]"
  - "[[adr/0189-shared-shortest-path-search]]"
---

# 0025. `shortestPath` is BFS, not full variable-length expand

## Context

`shortestPath` / `allShortestPaths` were evaluated by walking every
trail that matches the inner pattern (`walk_rel` up to `max_hops`), then
keeping the shortest. On the fraud graph a directed `TRANSFER*1..3`
between two accounts is a 3-hop walk of out-degree tens to hundreds:
~67³ paths on a typical pair, tens of millions on a high-degree account.
The fraud load-test `payment_path` use case (886 ops, 50 workers, scale
0.30) ran at **p50 6.8 s / p95 70 s** and cut mix throughput from ~3.3k
ops/s to 59 ops/s.

openCypher only needs **one** shortest path (`shortestPath`) or **all
paths of that minimum length** (`allShortestPaths`). Enumerating longer
trails is not required.

## Decision

Evaluate a single-relationship `shortestPath` / `allShortestPaths`
pattern with **breadth-first search**:

- Expand hop by hop; first time the end node is reached at `h >=
  min_hops` is the shortest length `D`.
- `shortestPath` returns one trail of length `D` (or `null`).
- `allShortestPaths` returns every trail of length `D` (or `null` if
  none). Same-depth arrivals at a node are kept; longer arrivals are
  dropped.
- Do not reuse a relationship on a trail (same as `walk_rel`).
- `min_hops == 0` and a start that already matches the end is a
  zero-length path.
- A cycle `shortestPath((a)-[*]->(a))` is allowed: the start node is
  not treated as a finished visit at depth 0.
- Default max hops stays 32 when the pattern omits an upper bound.

Patterns with **more than one** relationship stay on the existing
enumerate-then-min path (rare; TCK does not require them).

MATCH variable-length expand is unchanged (it must produce every trail).

## Alternatives considered

- **Leave enumerate-all** — correct, unusable on dense money graphs.
  Rejected.
- **Drop `payment_path` from the load-test** — hides the engine bug.
  Rejected as the only fix.
- **Bidirectional BFS** — faster on some graphs, more code, same
  contract. Rejected until a benchmark shows it matters.
- **Cap load-test hops at 1** — a workaround, not a dialect fix.
  Rejected as the engine change.

## Consequences

- Fraud `payment_path` is O(reachable nodes within `max_hops`), not
  O(walks). A 1-hop pair returns after scanning the source’s out-edges.
- `allShortestPaths` on a dense graph can still materialize many
  equal-length paths; that is the language.
- TCK Path features do not cover `shortestPath`; unit tests in
  `memorydb` carry the contract.

## Later references

[ADR-0034](./0034-bidirectional-shortestpath.md): when both endpoints are bound, `shortestPath` (not
`allShortestPaths`) uses bidirectional BFS. Same contract; hop cap
unchanged. Unbound end stays unidirectional.

[ADR-0186](./0186-match-clause-shortest-path-bfs.md): the `MATCH` clause forms (`MATCH p = shortestPath(...)`,
`allShortestPaths(...)`) and the simple GQL shortest selectors also run
on BFS, per (start, end) partition, instead of enumerate-then-select.
That route has no hop cap for an unbounded `[*]`. This ADR's expression
form, including its 32-hop default, is unchanged.

[ADR-0189](./0189-shared-shortest-path-search.md): the expression form runs on the shared shortest-path search
module (`cypherlite-executor/src/shortest/`), which the [ADR-0186](./0186-match-clause-shortest-path-bfs.md) route
also uses. `bfs_shortest`'s inline BFS and `bfs_shortest_all` are
removed. A pattern with `min_hops` > 1 uses exact enumeration instead
of BFS. The 32-hop default and one value per row are unchanged.
