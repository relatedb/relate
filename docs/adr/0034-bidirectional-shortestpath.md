---
type: adr
area: engine
date: 2026-09-01
deciders: MemoryDB maintainers
name: 0034-bidirectional-shortestpath
status: accepted
tags:
  - pattern-matching
title: Bound `shortestPath` is bidirectional BFS
REFERENCES:
  - "[[adr/0025-shortestpath-bfs]]"
  - "[[adr/0033-compact-live-records-typed-csr]]"
  - "[[adr/0037-shallow-bound-shortestpath]]"
---

# 0034. Bound `shortestPath` is bidirectional BFS

## Context

[ADR-0025](./0025-shortestpath-bfs.md) evaluates `shortestPath` with unidirectional BFS. That is
O(reachable nodes within `max_hops`) from the start. The fraud
`payment_path` query binds both ends:

```cypher
MATCH (a:Account {id: $from}), (b:Account {id: $to})
RETURN length(shortestPath((a)-[:TRANSFER*1..3]->(b))) AS hops
```

After Waves 1–3 and streaming `DISTINCT`+`LIMIT`, that use case is
~37% of 0.30 read-only CPU. p50 is ~1.7 ms; **mean is ~5.7 ms**
because a miss or a 3-hop hit walks the start’s 3-hop out-ball
(degree tens to hundreds). [ADR-0025](./0025-shortestpath-bfs.md) rejected bidirectional BFS until
a benchmark showed it matters. It does.

openCypher still needs one shortest trail or `null`. The hop cap
stays.

## Decision

When **both** endpoints of a single-relationship `shortestPath` are
bound, search with **bidirectional BFS**:

- Forward from the start on the pattern direction; backward from the
  end on the reverse (`->` uses incoming, `-` uses both).
- Expand the smaller frontier, one BFS layer at a time. The shortest
  meet in a finished layer is the answer (do not take the first edge
  meet inside a layer).
- Accept a meet only when `d_front + d_back` is in `[min_hops,
  max_hops]`. Stop when no remaining queued node can beat `max_hops`
  or a found length.
- Reconstruct one trail from the two parent maps. The no-rel-reuse
  rule applies to the spliced trail.
- Unbound end stays on unidirectional BFS ([ADR-0025](./0025-shortestpath-bfs.md)).
- `allShortestPaths` stays unidirectional. Multi-rel patterns stay
  enumerate-then-min.

`length(shortestPath(…))` may return the hop count without building a
`Path` (no node/rel inflate). `length(allShortestPaths(…))` is still
the list size.

Same contract as [ADR-0025](./0025-shortestpath-bfs.md): one shortest path or `null`, default max
32, zero-length when `min_hops == 0` and start already matches end.
Unit tests in `memorydb` stay the language carrier.

## Alternatives considered

- **Keep unidirectional BFS** — correct, still O(d³) on a 3-hop
  miss. Rejected; the 0.30 mix is now this query.
- **Bidirectional `allShortestPaths`** — must enumerate every
  min-length splice. More code, not on the fraud mix. Rejected for
  this ADR.
- **Cache `shortestPath` results** — needs invalidation on every
 write. Rejected in [ADR-0033](./0033-compact-live-records-typed-csr.md); this ADR is less search, not a cache.
- **Drop `payment_path` or cap hops at 1** — already rejected in
 [ADR-0025](./0025-shortestpath-bfs.md).

## Consequences

- A 3-hop hit is ~`2·d²` visits instead of ~`d³` when both ends have
  comparable degree. A high-degree start and a low-degree end expand
  the small side first.
- A miss still respects `max_hops`; it does not walk two full
  3-hop balls.
- Identity `REPORTS_TO*..8` (undirected, both ends bound) uses the
  same path.
- TCK Path features still do not cover `shortestPath`.

## Later references

[ADR-0037](./0037-shallow-bound-shortestpath.md): bound directed `shortestPath` with `max_hops ≤ 3` is
1-hop / 2-hop intersect / 3-hop first-meet, not a finished last
bidir layer. Generic bidir stays for `max_hops > 3` and undirected.
