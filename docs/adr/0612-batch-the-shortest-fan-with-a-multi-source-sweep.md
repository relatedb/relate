---
type: adr
area: engine
claim: 5efd49cff276940712830bac133a102878862bd7
name: 0612-batch-the-shortest-fan-with-a-multi-source-sweep
status: proposed
tags:
  - pattern-matching
  - perf
title: Batch the shortest-path fan with a multi-source level sweep
REFERENCES:
  - "[[adr/0186-match-clause-shortest-path-bfs]]"
  - "[[adr/0463-expression-allocation-budget]]"
  - "[[adr/0555-operators-stream-rows-never-accumulate]]"
  - "[[adr/0556-shortest-route-pair-search-for-seekable-endpoints]]"
---

# ADR-0612: Batch the shortest-path fan with a multi-source level sweep

## Context

A shortest-path pattern with both endpoints listed but past the pair
product's bound is answered by a fan: one layered breadth-first search per
origin, each keeping predecessors for every node it reaches ([ADR-0186](./0186-match-clause-shortest-path-bfs.md),
[ADR-0556](./0556-shortest-route-pair-search-for-seekable-endpoints.md)). With a listed far side the fan stops early once every far node
is reached and tests reached nodes against a far bitset, but each origin
still pays a full ball. On a low-diameter graph twenty origins with a
three-hop bound each cover most of the store, so twenty near-identical
sweeps rebuild the same predecessor tables; on the social benchmark's
twenty-by-twenty query that is about 13 seconds, almost all of it ball
growth and the hash tables that record it.

Only the answer's distances and, afterwards, the trails of the pairs that
survive the end-pattern admission are needed; the predecessors of every
other node reached are never read.

## Decision

- A fan with a listed far side, at least two origins and an unfiltered
  selection runs its origins in batches of 64 through one multi-source
  level sweep. Each origin is one bit of a per-node word; one pass
  reports, per level, which origins first reach which far node. The
  sweep keeps no predecessors and answers distance only.
- Each (origin, far node) pair the sweep reports is one partition, handed
  to the same partition body the layered fan uses: the end-pattern
  admission, the common-end-node policy, the pair predicates and the
  selector all still decide every member. Its trails are rebuilt by the
  pair search, which is bounded by the pair's distance. For all-path
  selectors the trail set is the layered fan's; for single-path selectors
  the chosen trail may be a different shortest one, as it already is
  between the pair product and the fan.
- The final level the hop bound allows is not expanded from the frontier:
  each far node reads its neighbours' lanes instead, so that level costs
  the far nodes' degrees, not the frontier's.
- Early stop and the first-layer spellings are per origin: an origin
  retires once every far node is reached or, for a first-layer spelling,
  after the first level that produced a row; the sweep ends when no
  origin is left or the consumer halts.
- The sweep's tables (three words per node id) are charged to the
 workspace for the batch's life ([ADR-0463](./0463-expression-allocation-budget.md)). Rows stream per level
 ([ADR-0555](./0555-operators-stream-rows-never-accumulate.md)); the sweep retains nothing that grows with path or row
  count, and memory is the tables plus at most one entry per far node per
  level.
- The per-origin layered fan is the fallback, taken before any row is
  pushed, and only for these named reasons: a single origin (nothing to
  share), a path predicate in the selection (it answers the shortest
  satisfying trail, which a distance does not decide), an unlisted far
  side, and a node-id span past the tables' cap (the per-origin search's
  memory stays bounded by the ball). It is also the test oracle.

## Alternatives considered

- **Presize and reuse the layered fan's buffers.** The profile attributes
  most allocations to growth of per-origin tables, so this helps, but each
  origin still walks its whole ball; rejected as the larger cost remains.
- **Per-origin bidirectional search to each far node.** Cheap per pair
  but 400 searches over hub neighbourhoods on the benchmark query, and it
  forgoes sharing between origins; rejected.
- **Keep per-level predecessor words and rebuild trails from them.**
  Memory grows with the level count and lane count, and the pair search
  is already the cost-bounded rebuild; rejected for the extra retained
  state.
- **Sweep for single-path selectors only.** The trail rebuild is the pair
  search for both kinds, so the split buys nothing; rejected.

## Consequences

- Many-origin fans over low-diameter graphs cost about one shared pass
  per 64 origins plus a bounded rebuild per surviving pair; the benchmark
  query drops from about 13 seconds to under 3.
- A single-path selector's chosen trail in a fan may differ from the
  layered fan's among equally short trails; every answer is a shortest
  trail and the row set per pair is unchanged.
- A wide node-id range or a path predicate keeps the previous cost
  profile; the batch counter and the batched-off test switch make the
  routing observable in tests.
