---
type: adr
area: engine
claim: 64966ca3db10f527117149fea32e35e43752e95c
name: 0351-cheapest-element-start-for-unbound-patterns
status: accepted
tags:
  - pattern-matching
  - planner
title: Cheapest-element start for unbound path patterns
REFERENCES:
  - "[[adr/0218-stats-based-match-orientation]]"
  - "[[adr/0350-bound-node-pivot-for-pattern-expansion]]"
---

# ADR-0351: Cheapest-element start for unbound path patterns

## Context

[ADR-0350](./0350-bound-node-pivot-for-pattern-expansion.md) makes a pattern whose later element is row-bound start there
instead of at element 0. But the most common multi-pattern query has
nothing row-bound at all — the first pattern of `MATCH (c:Comment)-
[:commentHasTag]->(t:Tag)` — and element 0 is chosen by text position,
not cost. When element 0's label set is the largest in the pattern (the
Comment side, 151k nodes) and another element's is tiny (the Tag side,
16k, filtered further by a residual `CONTAINS`), the expansion pays the
big label scan plus a hop per candidate when the reverse order would
enumerate a fraction of the candidates for the same match set. The
planner cannot make this choice: it reorders whole patterns by static
cardinality hints ([ADR-0218](./0218-stats-based-match-orientation.md)'s `order.rs` ladder) but a comma-joined
pattern's *element* order is a single `PathPattern` the executor walks
from index 0. The overlap with that planner ladder is acknowledged and
tracked as its own unification decision (LABS-351) — this ADR records
the executor-side decision that shipped first.

## Decision

When no element of a pattern is row-bound, `cheapest_start` picks the
start element by estimated candidate cardinality: the minimum
[`Graph::label_count`] over the element's static label conjunction
(`np.labels`; an unlabeled element estimates as the whole graph, so any
labeled element beats it), leftmost on ties, and only elements that
strictly beat element 0 displace it. An element whose variable is bound
in the incoming row estimates as exactly one candidate and is therefore
never displaced by a later scan. The chosen element then runs through
the [ADR-0350](./0350-bound-node-pivot-for-pattern-expansion.md) machinery: the backward walker covers the single-hop
prefix, the forward walk the suffix, and the same fallbacks apply — a
shortest-path function, or a variable-length/quantified prefix, keeps
today's index-0 start, as does any pattern whose elements' predicates
reference a peer element's variable (the `pattern_self_contained` gate,
which also guards the cheapest start's own start-element predicates
against prefix references).

## Alternatives considered

- **Always start at element 0** (status quo) — rejected: the match set
  is identical for any start element, and text order has no relation to
  cost; the q19 shape (and the star-join family behind it) pays a full
  large-label scan per row for a match set reachable from a leaf.
- **Reorder the pattern's elements in the planner** — rejected for this
  change: it duplicates the runtime decision into a layer that cannot
  see row bindings, and the executor already owns the walk; unifying
  the two ladders is LABS-351.
- **Selectivity estimates beyond label cardinality** (residual filter
  selectivity, seekability, fan-out) — deferred: label cardinality is
  the one estimate available in the interned domain without new
  statistics machinery, and it is enough to fix the observed pathology;
  richer estimates are the unification work.

## Consequences

- Unbound patterns start at their cheapest labeled element instead of
  their first; results are identical (same conjunctive match set), the
  visit order differs.
- A pathological estimate can pick a worse start than element 0 in
  fan-out-heavy shapes; accepted for now because the estimate compares
  candidate-set sizes, which is the dominant cost in the observed
  family, and the regression net (TCK, differential, cip-eval,
  cookbook, query-latency) gates any plan-visible regression.
- The estimate runs per `match_resolved_path` call (per row for later
  patterns); it is a handful of interner map lookups per element.
- The planner/executor duplication of start-selection policy is now
  explicit and tracked (LABS-351).
