---
type: adr
area: cypher
claim: df471f4458d124e77b9f8e49aa6493b7b90b9fa5
name: 0453-aggregations-in-subclauses
status: proposed
tags:
  - semantics
title: Aggregations in subclauses — ORDER BY/WHERE re-aggregation and DISTINCT grouping
REFERENCES:
  - "[[adr/0188-remove-superseded-tck-scenario-from-pin]]"
  - "[[adr/0363-query-notifications]]"
  - "[[adr/0371-preview-feature-gating]]"
  - "[[adr/0472-repin-order-by-scenarios]]"
---

# ADR-0453: Aggregations in subclauses — ORDER BY/WHERE re-aggregation and DISTINCT grouping

## Context

The engine rejects every Cypher 25 "aggregations in subclauses" shape the
acceptance suite pins (AggregationsInSubclausesAcceptance,
ProjectionSubclauseDeprecationAcceptance, AggregationAcceptance): an ORDER
BY key whose aggregate call is not projected verbatim
(`ORDER BY sum(a+b+c)` beside `count(x) AS cnt`), a WHERE after an
aggregating WITH (`WHERE sum(a+b+c) > 0`), and aggregation under
`RETURN DISTINCT` / `WITH DISTINCT` with a non-aggregating projection. The
executor already folds ORDER BY aggregate calls per group
(`aggregate_ex` parks each key through `agg_eval`), so the rejections are
validation policy, not missing machinery.

The openCypher TCK 2024.3 floor stays the hard gate. Its three relevant
pins are all outside the relaxed surface: aggregation in a MATCH WHERE
(MatchWhere1 [15]), aggregation in ORDER BY of a non-aggregating,
non-DISTINCT projection (ReturnOrderBy2 [14]), and complex non-aggregate
parts mixed with aggregation in ORDER BY (ReturnOrderBy6 [5]).

## Decision

Adopt the Cypher 25 subclause-aggregation semantics, scoped to what the
suite pins:

1. An ORDER BY key of an aggregating projection may contain aggregate
   calls that are not projected; each recomputes over the group. Aggregate
   calls in an ORDER BY key of a DISTINCT (but non-aggregating) projection
   are likewise allowed: the DISTINCT items are the grouping keys. A `*`
   item supplies no such item list, so `DISTINCT *` with an aggregating
   ORDER BY key or WHERE predicate fails closed with `InvalidAggregation`
   rather than evaluating the aggregate per row.
2. The WHERE of an aggregating or DISTINCT WITH may contain aggregate
   calls — HAVING semantics: the predicate is evaluated once per group,
   aggregate calls fold the group, non-aggregate leaves read the projected
   row. MATCH WHERE keeps the InvalidAggregation rejection.
3. Discipline preserved on the non-aggregate parts of both subclauses:
   leaves outside aggregate calls must be grouping keys or projected
   output names (aliases of aggregating items included), and a complex
   (operator) non-aggregate part mixed with aggregation stays
   AmbiguousAggregationExpression — referencing a complex grouping key
   inline remains deprecated; alias it.

The change lives in the two validation layers (front
`validate/aggregation`, planner `validate/clause.rs` + `agg.rs`) and the
executor's projection path (`project_rows` routes DISTINCT-with-aggregating-
subclause through the aggregation path; the aggregating path filters groups
through the aggregating WHERE).

## Alternatives considered

- Gate behind `CYPHERLITE_PREVIEW` like the CIP syntax extensions —
 rejected: [ADR-0371](./0371-preview-feature-gating.md) gates syntax-extending features only; this is a
  validation-policy change on existing syntax, and the TCK floor scenarios
  all keep their verdicts.
- Implement the 01N01 deprecation notifications the Cypher 5 twins expect —
  deferred, not replaced: the notification channel is plan-tree-only
 ([ADR-0363](./0363-query-notifications.md)) and the ambiguous-reference classification is its own
  feature; the Cypher 25 twins (no notification) pass without it.

## Consequences

- 47 acceptance-suite scenarios that previously failed as syntax errors now
  parse (38 pass their full assertions); repo tests pin each newly accepted
  form and each preserved rejection
  (`cypherlite/cypherlite/tests/subclause_aggregation.rs`).
- The planner's ORDER BY aggregation checks stop requiring projected-call
  matching; the alias set they discipline against widens to aggregating
  items' aliases (`ORDER BY s + sum(a.p)` beside `sum(a.p) AS s`).
- **Two pinned TCK 2024.3 scenarios are superseded by this decision** and
  now fail their (Cypher 5) expectations: WithOrderBy4 [13] and [14]
  ("Fail on sorting by a non-projected aggregation on a variable /
  expression") — under the recomputation rule an ORDER BY aggregation over
  an in-scope pre-projection variable is valid, which is exactly what those
  scenarios forbid. They should be removed from the vendored pin the way
 [ADR-0188](./0188-remove-superseded-tck-scenario-from-pin.md) removed its superseded outline (denominator 3892 → 3890); that
  corpus edit was NOT made here — it awaits explicit human approval, since
 the TCK floor may not silently lose scenarios. (Resolved by [ADR-0472](./0472-repin-order-by-scenarios.md): the
  scenarios stay in the pin asserting the Cypher 25 behaviour, so the
  denominator stays 3892 and no scenario is lost.)
- The 01N01 deprecation notifications the acceptance suite's `@fails:cypher-5`
  twins expect (7 scenarios) are NOT implemented — the notification channel
 is plan-tree-only ([ADR-0363](./0363-query-notifications.md)) and the ambiguous-reference classification
  is its own feature; the Cypher 25 twins (no notification) pass without it.
- The acceptance suite's own `@fails:cypher-25` twins (19 scenarios) now
  fail their expected-error assertions — the deliberate consequence of
  adopting the Cypher 25 verdicts those twins pin as failing.

## Later references

- **PR #892 (the HAVING form's filter position).** §2's per-group
  evaluation stands; the filter it feeds moved. The aggregating `WITH`'s
  trailing `WHERE` follows the clause grammar's order — the page
  (`ORDER BY` / `SKIP` / `LIMIT`) is taken from the grouped rows and the
  predicate value, still answered once per group, filters the paged rows
  in the projection's finish tail (the same position the plain trailing
  `WHERE` takes). The closing note's "filters groups through the
  aggregating WHERE" described the pre-#892 placement.

- **[ADR-0472](./0472-repin-order-by-scenarios.md) (re-pin of the four ORDER BY scenarios).** The TCK consequence
  above is resolved: `WithOrderBy4 [13]`/`[14]` stay in the pinned corpus
  asserting this ADR's recomputation behaviour (and `ReturnOrderBy6 [5]`,
  `WithOrderBy4 [20]` likewise assert the Cypher 25 forms), so
  `cargo test -p cypherlite-tck` is green and no scenario leaves the pin.
