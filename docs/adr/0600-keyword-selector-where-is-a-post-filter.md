# ADR-0600: A clause-level WHERE over the path post-filters keyword selectors

Status: Proposed
Claim: c678952aee79c852adc7aefd365a6dd6a0a5c307

## Context

ADR-0186 routes `MATCH`-clause shortest paths onto a breadth-first search
and records that the search does not apply `MATCH ... WHERE` conjuncts: the
clause `WHERE` filters the selected rows afterwards. A later change made
the route apply every conjunct that reads the path (the named path
variable or the relationship variable) during the search, so a path that
violated the predicate no longer hid a longer path that satisfied it.

That is the behaviour of the function spellings, and only of those. A
differential run against Neo4j 2026.09 over small graphs (a triangle, a
diamond, a chain, a graph with tied routes) and every selector spelling
found 24 diverging cases, all one class: for the keyword selectors
(`SHORTEST 1`, `ANY SHORTEST`, `ALL SHORTEST`, `SHORTEST 1 GROUPS`) a
clause-level `WHERE` that reads the path is applied after selection. On a
triangle, `SHORTEST 1` from a to b selects the direct edge, and
`WHERE length(p) >= 2` then leaves no row; Relate returned the two-hop
path. The remaining spellings already agreed:

- `shortestPath()` and `allShortestPaths()` with a clause `WHERE` over the
  path filter the search, in both systems.
- An inline `WHERE` (a quantified pattern's, a parenthesized path
  pattern's, the end node's) and a relationship property map filter the
  search in both.
- A conjunct on the endpoints only selects whole (start, end) partitions;
  both systems agree.
- `SHORTEST k` for k above 1 and `GROUPS` counts above 1 are not routed and
  already filtered afterwards.

## Decision

For a keyword selector, a clause-level `WHERE` conjunct that reads the
path is a post-filter of the selected paths: the route selects first, and
the conjunct is applied to each emitted row with the other residual
conjuncts. The function spellings, inline `WHERE`s and property maps keep
filtering the search, as in Neo4j. The postfix quantified spelling no
longer needs its own decline to enumeration, because the route now answers
it as the enumeration does.

The rule lives in the one place that splits a route's conjuncts by what
they read, keyed by the spelling the route already carries, so no second
classification of conjuncts exists.

Which of several equally short paths `SHORTEST 1` and `ANY SHORTEST`
select remains unspecified. A post-filter over that arbitrary pick may
therefore keep a row in one system and drop it in the other when ties
exist; the lengths and the all-shortest sets agree.

## Alternatives considered

- **Keep the in-search filter.** It is the friendlier reading, since a
  longer qualifying path is found instead of nothing. Rejected: it
  disagrees with Neo4j and with the benchmark oracle.
- **Make the function spellings post-filter too.** Rejected: Neo4j filters
  their search, and the matrix showed both systems agree today.
- **Decline path-reading keyword queries to enumeration.** Rejected:
  enumeration is exponential in the hop bound and would also skip the
  breadth-first route the post-filter now uses.

## Consequences

- Queries that relied on the in-search reading for keyword selectors now
  return fewer rows, matching Neo4j. The documented workaround for the
  in-search reading is the function spelling or an inline `WHERE`.
- The route no longer reaches the per-node trail walk for keyword
  selectors; it remains for the in-search spellings. No performance claim
  rests on this decision.
- Regression tests pin the triangle and tied-route witnesses for each
  selector against both the route and the enumeration, and the unchanged
  in-search spellings.
- Later references note on ADR-0186 names this ADR.
