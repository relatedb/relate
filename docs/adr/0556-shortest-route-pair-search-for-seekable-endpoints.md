# ADR-0556: Shortest routes search property-seekable endpoints as pairs

Status: Proposed
Claim: 2a0d5f739d91e848bb4c1304b9322d032c866215

## Context

ADR-0186 routes `MATCH`-clause shortest paths onto a breadth-first search,
and ADR-0189 shares that search with the expression form. The route
anchors each input row by which endpoints are bound. A pattern whose
endpoints are property maps rather than bound variables
(`(a:User {id: 1})-[*..6]-(b:User {id: 2})`) was anchored as "free": a
one-sided layered search from the start to the full hop bound, then a
partition per reached node. On a graph with high-degree hubs that ball
spans most of the graph, while the bidirectional pair search the same
pattern takes when both ends are bound meets in the middle after a small
fraction of the work. The measured gap on a ten-million-node social graph
was two orders of magnitude on exactly this shape.

Separately, every walk in the engine — the traversal kernels (ADR-0473),
the frontier NFA (ADR-0478), the GPM hop walk, the path fold and the
shortest searches — read a relationship's neighbour with a rel walk
followed by a separate endpoint lookup, two version-chain reads per hop.

## Decision

1. **Resolved endpoints.** The route resolves each unbound endpoint's
   property map through the node-candidates funnel, the same seek path
   the pattern walkers use. A row-bound node is a one-element side; a map
   that reads a variable the row does not bind yet, a bare label and an
   unbound variable stay scan territory and are not materialized.
2. **Pair product within a bound.** When both sides are resolved and their
   candidate product stays within a fixed small limit, the route runs the
   pair partition for each combination, exactly as for row-bound ends.
   Past the limit it fans from the shorter side. With only the end side
   resolved, it drives a backward fan only while that list is within the
   limit; otherwise the start candidates fan forward as before.
3. **One hop walk.** The neighbour walk yields each relationship with the
   node it reaches from a single version-chain read; the rel-only walk is
   its projection. A read-only view skips the write overlay's per-hop
   existence check and dedup set. The kernels, the GPM hop walk, the path
   fold and the shortest searches read this one walk. Both-direction dedup
   stays keyed on the relationship id, because a reused id can repeat
   within one adjacency list.
4. **Dense-id hashing.** The pair search's trees and the layered search's
   reach and predecessor tables use the keyless dense-id hasher, under the
   existing rule that engine-allocated ids never need a keyed hash.

Result order, tie-breaking and the shapes the route declines are
unchanged; this decision only changes which search answers a shape.

## Alternatives considered

- **Raise the one-sided fan's efficiency only.** Rejected: the cost is
  structural — one full-depth ball per start — and no constant factor
  closes a gap of this size.
- **Dense id-indexed trees and vector frontiers.** Implemented and
  measured: no gain on the single-path query and a regression on the
  all-paths query, because the search is memory-bound on cache-cold
  neighbourhoods and the arrays traded hashing for allocation churn.
  Rejected.
- **Rewrite property endpoints into bound rows at plan time.** Rejected:
  it duplicates the route's per-row anchoring in the planner and would
  need its own admission rules for the same guards.
- **Degree-aware side selection and shallow intersection for small
  bounds.** Deferred: separate, parity-gated tie-break work.

## Consequences

- Property-endpoint shortest paths on hub-heavy graphs stop paying a
  full-depth ball; row-bound forms are unchanged.
- A bound start with an unbound endpoint whose property map has no index
  now materializes that endpoint's candidate list before searching, a
  scan the old route never ran; bounding that case is follow-up work.
- Every walk-then-endpoint loop in the engine loses one version-chain read
  per followed relationship, so kernel and frontier traversals speed up
  without changing results.
- A stale adjacency entry whose resolved endpoints no longer touch the
  walked node is dropped before the caller sees it, where some callers
  previously counted it before dropping it. This only arises from the
  reused-id replay case already tracked as a storage invariant.
- Which of several parallel relationships the shallow search returns is a
  separate question and is not changed here.
