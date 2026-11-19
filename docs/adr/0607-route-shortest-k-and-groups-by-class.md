---
type: adr
area: engine
claim: 99c395b6300146ea0778e4ef33d18ec708ace5b7
name: 0607-route-shortest-k-and-groups-by-class
status: proposed
tags:
  - pattern-matching
title: SHORTEST k and SHORTEST k GROUPS take the shortest-path route by length class
REFERENCES:
  - "[[adr/0111-engine-bounded-execution]]"
  - "[[adr/0186-match-clause-shortest-path-bfs]]"
  - "[[adr/0203-newest-first-expansion-order]]"
  - "[[adr/0463-expression-allocation-budget]]"
  - "[[adr/0555-operators-stream-rows-never-accumulate]]"
  - "[[adr/0567-shortest-route-endpoint-listing-is-capped]]"
  - "[[adr/0600-keyword-selector-where-is-a-post-filter]]"
  - "[[adr/0601-bidirectional-all-shortest-pair-search]]"
---

# ADR-0607: SHORTEST k and SHORTEST k GROUPS take the shortest-path route by length class

## Context

[ADR-0186](./0186-match-clause-shortest-path-bfs.md) routes the `MATCH`-clause shortest selectors onto a breadth-first
search, but only the single-answer and all-shortest forms. `SHORTEST k` with
k above 1 and `SHORTEST k GROUPS` are not routed ([ADR-0600](./0600-keyword-selector-where-is-a-post-filter.md) records that they
post-filter like the other keyword selectors). They fall to enumeration: the
pattern walk collects every trail of every admissible length into one list,
and the selector sorts and cuts it afterwards. That is exponential in the hop
bound and breaks [ADR-0555](./0555-operators-stream-rows-never-accumulate.md), which forbids an unbounded list of intermediate
rows. On the soc-pokec graph, `SHORTEST 3` and `SHORTEST 2 GROUPS` between
one fixed pair of users within six hops do not finish in over four minutes,
where the reference engine answers in tens of milliseconds. The memory hazard
is latent: the walk has not yet reached materialization when the time is up.

Semantics were checked against Neo4j 5.26 on a small graph with parallel
relationships, a cycle, tied routes and undirected hops: the answers are
per (start, end) partition; `SHORTEST k` returns the k shortest trails
(relationship-unique, nodes may repeat, parallel relationships are distinct
trails); `GROUPS` returns every trail of the first k distinct lengths; a
clause `WHERE` over the path removes rows after selection (a `>= 3` filter
over an answer of length 2 leaves nothing). The enumeration the engine runs
today agrees on every one of these, so this is a performance change with no
intended semantic change.

## Decision

1. **Route the pair partition.** `SHORTEST k` (k above 1) and `SHORTEST k
   GROUPS` take the route when both ends are fixed or seeked (the pair
   anchor) or are the one repeated variable (the loop anchor), the minimum
   hop count is at most 1, the path mode is the default or `TRAIL`, and no
   inline predicate filters the search. The decision is taken after the
   anchor is resolved and before any row is pushed. Everything else, fans
   included, stays on enumeration unchanged.
2. **Classes in ascending length.** A partition's trails are produced one
   length class at a time, from the smallest admissible length up to the
   pattern's bound (an unbounded pattern takes the engine's existing cap, the
   live relationship count). `SHORTEST k` stops when k trails are fixed;
   `GROUPS` stops after k non-empty classes. A class is the exact set of
   trails of that length, so lengths and multiplicities equal enumeration.
3. **Pruned class enumeration.** A distance ball grows by whole layers from
   the end against the pattern's direction, and a counting ball grows from
   the start along it. They grow in turn, the one with the smaller frontier
   first, until their radii sum to the class length or the end ball holds
   everything it can reach, so a hub start stays at radius one while the end
   side, whose layers are small, does the rest. A
   depth-first walk from the start extends a trail only through
   relationships not yet used, and abandons a prefix whose remaining length
   is below a lower bound on the distance to the end: exact inside the end
   ball, one past its radius outside it. Where the ball has scanned every
   node a step could still use, the walk reads the step from the ball's
   recorded hops instead of scanning the node, so a hub on the way is not
   read to find the few neighbours near the end. Cost follows the two balls
   rather than every trail from the start. Parallel relationships,
   undirected hops and closed trails (start equal to end) need no special
   case. A ball that empties before the other side appears means no trail
   exists, and the partition ends at once.
4. **Selection keeps the pinned tie-break.** Within a class the boundary
   choice follows the existing order (length, relationship-type sequence,
 node-id sequence, [ADR-0203](./0203-newest-first-expansion-order.md)), so results equal enumeration. Classes that
   fit whole are emitted as they complete. The boundary class feeds a heap
   bounded by the trails still wanted; only that many trails are retained.
   `GROUPS` retains nothing: each class streams into the consumer.
5. **Bounds.** Retained state is the two balls and the heap, both charged to
 the workspace ledger ([ADR-0463](./0463-expression-allocation-budget.md)) as the reach cache is, so an over-large
   ball fails with the existing capacity error rather than a new one. The
 cancellation window ticks per frame ([ADR-0111](./0111-engine-bounded-execution.md)). The seek-listing cap
 ([ADR-0567](./0567-shortest-route-endpoint-listing-is-capped.md)) is unchanged. No new knob and no new error kind.
6. **Order.** Rows within a partition emerge in ascending length and, within
   a length, in tie-break order; across partitions order is unspecified, as
 for any match without an ordering clause ([ADR-0601](./0601-bidirectional-all-shortest-pair-search.md)).
7. **Post-filters** are applied to the selected rows as [ADR-0600](./0600-keyword-selector-where-is-a-post-filter.md) states.

## Alternatives considered

- **Keep enumeration and make the selector a streaming top-k.** Bounds the
  memory but still walks every trail; the time stays exponential. Rejected
  as the fix, and left for the shapes this ADR does not route.
- **Classic k-shortest-path algorithms (Yen, Eppstein).** They return simple
  paths or walks, which differ from relationship-unique trails and from the
  reference engine on cycles and parallel relationships. Rejected: semantic
  change.
- **Read the trails off the shortest-path predecessor DAG alone.** Only the
  shortest class lives there; the next lengths that `SHORTEST k` and `GROUPS`
  need are not. Rejected as incomplete.
- **Stop the boundary class at its first trails.** Cheaper, but the choice
  among ties would no longer match the pinned order. Rejected; unspecified
  ties can be revisited by a later decision.
- **Route fans too.** One enumeration per reached node recreates the
  exponential walk. Deferred to a separate decision.
- **A dedicated ball-size refusal.** A new error and cap behavior for a case
  the ledger already covers. Rejected for now.

## Consequences

- Pair-anchored `SHORTEST k` and `SHORTEST k GROUPS` queries run in time
  bounded by two half-radius balls plus the pruned class walks.
- A class that is itself huge still costs its size in time (not memory for
  `SHORTEST k`); a graph with a dense region reachable but unable to reach
  the end without reusing a relationship can still make a class walk long.
- Fans and inline-filtered k forms stay exponential until a follow-up.
- Tests compare the route with enumeration on small graphs across selectors,
  directions, parallel relationships, ties, caps and post-filters, and the
  pokec pair queries are timed in CI against a single engine revision.
- Later references note on [ADR-0186](./0186-match-clause-shortest-path-bfs.md) names this ADR.
