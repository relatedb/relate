---
type: adr
area: engine
date: 2026-09-01
deciders: MemoryDB maintainers
name: 0037-shallow-bound-shortestpath
status: accepted
tags:
  - pattern-matching
title: Bound `shortestPath` with `max_hops ≤ 3` is set intersection
REFERENCES:
  - "[[adr/0025-shortestpath-bfs]]"
  - "[[adr/0033-compact-live-records-typed-csr]]"
  - "[[adr/0034-bidirectional-shortestpath]]"
---

# 0037. Bound `shortestPath` with `max_hops ≤ 3` is set intersection

## Context

[ADR-0034](./0034-bidirectional-shortestpath.md) bidirectional BFS plus the miss cap (`d_front + d_back + 1 >
max_hops`) brought `payment_path` p50 to ~0.29 ms. The leftover 0.30
tail is mean **1.3 ms** / p95 **6 ms**. The catalog query is

```cypher
MATCH (a:Account {id: $from}), (b:Account {id: $to})
RETURN length(shortestPath((a)-[:TRANSFER*1..3]->(b))) AS hops
```

Generic bidir, after both 1-hop layers miss, expands one side’s 1-hop
set into a 2-hop ball to hunt for a 3-hop meet, and **finishes that
layer** ([ADR-0034](./0034-bidirectional-shortestpath.md): first meet inside a layer may not be shortest).
When both ends have degree tens to hundreds that is O(d²) HashMap
inserts even after a 3-hop hit is already known, and the same O(d²)
edge walk on a miss.

A 3-hop path is exactly: 1-hop from start, 1-hop from end, an edge
between those two sets. Shorter lengths are a direct edge or a
non-empty set intersect. Caching paths stays rejected ([ADR-0033](./0033-compact-live-records-typed-csr.md)).
Do not extend [ADR-0034](./0034-bidirectional-shortestpath.md) (`allShortestPaths`, multi-rel, undirected
`*..8`).

## Decision

When **both** endpoints of a single-type, **directed**
`shortestPath` are bound, `min_hops ≥ 1`, `max_hops ≤ 3`, and the
search is not `allShortestPaths`:

1. **Length 1** — any start→end edge (pattern direction).
2. **Length 2** — intersect 1-hop from start with 1-hop from end
   (reverse direction on the end).
3. **Length 3** — iterate the **smaller** 1-hop set; for each node,
   walk one hop toward the other set; **stop at the first** valid
   meet (after 1 and 2 missed, any 3-hop trail is shortest).

Reconstruct one trail from the three (or fewer) relationship ids.
The no-rel-reuse rule applies to that trail. `length(...)` still
returns the hop count without a `Path` ([ADR-0034](./0034-bidirectional-shortestpath.md)).

Unbound end, `max_hops > 3`, undirected `-`, and
`allShortestPaths` stay on [ADR-0034](./0034-bidirectional-shortestpath.md) / [ADR-0025](./0025-shortestpath-bfs.md).

A miss still walks the last-layer edges (must prove no 3-hop). It
does not build a 2-hop parent map.

## Alternatives considered

- **Keep finishing the last bidir layer** — correct, still O(d²)
  parent inserts after a 3-hop hit. Rejected; that is the mean/p95
  gap.
- **Extend [ADR-0034](./0034-bidirectional-shortestpath.md)** — the generic layer machine would grow a
  special case in place. Rejected; this is a hop-cap-specific
  access method.
- **Cache `shortestPath`** — rejected in [ADR-0033](./0033-compact-live-records-typed-csr.md).
- **Drop `payment_path` or cap hops at 1** — rejected in [ADR-0025](./0025-shortestpath-bfs.md).

## Consequences

- A 3-hop **hit** is first-meet on the smaller 1-hop set, not a
  finished 2-hop layer.
- A 3-hop **miss** is still O(min(d_s, d_e) · d) edge visits.
- Identity `REPORTS_TO*..8` is unchanged.
- Unit tests in `memorydb` stay the language carrier; add a dense
  3-hop hit that must not walk a full 2-hop ball.
