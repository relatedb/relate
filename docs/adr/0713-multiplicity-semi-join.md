---
type: adr
area: engine
claim: c4ffa678bfb782d2aa076b01468a63ce671bc972
date: 2026-11-27
name: 0713-multiplicity-semi-join
status: proposed
tags:
  - executor
  - pattern-matching
  - perf
title: A semi-join that carries multiplicity answers a filter-only pattern without expanding it per row
REFERENCES:
  - "[[adr/0463-expression-allocation-budget]]"
  - "[[adr/0555-operators-stream-rows-never-accumulate]]"
  - "[[adr/0694-default-selectivity-defaulted-estimate]]"
  - "[[adr/0701-hub-join-cost-gate]]"
---

# ADR-0713: A semi-join that carries multiplicity answers a filter-only pattern without expanding it per row

## Context

The social-network suite's seventh query counts persons of an age window,
living in a country's states, who hold one interest:

```cypher
MATCH (p:Person)-[:LIVES_IN]->(:City)-[:CITY_IN]->(s:State)
WHERE p.age >= 23 AND p.age <= 30 AND s.country = 'United States'
WITH p, s
MATCH (p)-[:HAS_INTEREST]->(i:Interest)
WHERE tolower(i.interest) = tolower('photography')
RETURN count(p) AS numPersons, s.state AS state ORDER BY numPersons DESC LIMIT 1
```

On a 100,000-person graph the plan enumerates the first pattern from the
states (16,618 persons survive the age window, 178,040 db hits), then expands
`HAS_INTEREST` from each of them to test the interest (58,197 more db hits)
and keeps 1,030. The interest side is the selective one: one `Interest` node
matches, and 6,101 persons hold it. The comma-separated spelling of the same
query becomes one fused expansion of 236,237 db hits; the hub join
([ADR-0701](./0701-hub-join-cost-gate.md)) declines it, and where it engages it
materializes both sides' full binding rows and emits their cross product per
hub, which is more than this query needs.

In this query the second pattern is a **filter**: its variable `i` is read
nowhere after the join, so the pattern only decides *which* persons survive
and *how many times* each is counted. Cypher counts bag semantics: a person
holding two interests that both fold to the matched value is counted twice,
so a plain existence semi-join (keep a person once) would change answers.

## Decision

1. **A semi-join operator that carries multiplicity.** For a pattern joined
   to the rows so far on one bound node variable, whose remaining variables
   are unused downstream (not returned, not in a later clause, not in a
   residual that needs them), the executor may build, once, a map from the
   joined node's id to the number of matches of the pattern at that node,
   by enumerating the pattern from its selective end. Each incoming row then
   probes the map: absent drops the row, present emits the row once per
   match (multiplicity), so `count(p)` and every other bag-sensitive consumer
   keep their answers.
2. **A cost gate on the same estimates as the hub join.** The operator is
   chosen when `walk(pattern from its selective end) + rows(A) probes` is
   cheaper than `rows(A) x walk(pattern from the bound node)`, both priced by
   the planner's one endpoint estimate. A side priced from the default table
   or a rank sentinel does not qualify (the estimate's basis gates the
   strategy, never a strict preference for measured counts; see
   [ADR-0694](./0694-default-selectivity-defaulted-estimate.md)).
3. **Bounded build.** The map is a declared breaker
   ([ADR-0555](./0555-operators-stream-rows-never-accumulate.md)): it charges
   its memory to the query's pool ([ADR-0463](./0463-expression-allocation-budget.md)), and a build that exceeds its share abandons the operator
   and lets the nested expansion continue from the rows already produced.
4. **Scope.** One shared variable; a single pattern side; no references to the
   side's other variables after the join; relationship uniqueness against the
   rows so far checked at the probe when the pattern could reuse a used
   relationship, otherwise skipped. Everything else keeps today's plan, so no
   statement changes meaning.

## Alternatives considered

- **Plain existence semi-join.** Cheapest, and wrong for bag semantics (a
  double-counted person would be counted once). Rejected on correctness.
- **Better start choice only.** Starting at the one matching interest and
  walking out to persons removes most of the cost for this query shape without
  a new operator. It is the first thing to do and is estimate-driven (a
  function-wrapped equality already prices from the key statistics); but it
  does not help when the filter side's selectivity is unknown to the planner,
  or when the other side must be enumerated anyway for its own variables.
- **Extend the hub join to multi-hop sides.** The hub join groups both sides
  and emits cross products of binding rows; extending it to chains keeps its
  full materialization for a case where one side is only a filter. Rejected as
  the general answer, though its cost gate is reused.
- **Rewrite filter-only patterns to `EXISTS` subqueries.** Changes
  multiplicity, so not meaning-preserving.

## Consequences

- A filter-only pattern joined to a larger row stream costs one enumeration
  plus a probe per row instead of an expansion per row, with answers
  unchanged, including repeated matches.
- The operator reads estimates, so its quality follows theirs; the basis gate
  keeps default-priced sides on the nested plan.
- A new executor operator and a new gate arm; no storage or wire format
  changes. The measured claim to confirm before accepting: the suite's Q7
  comma and `WITH` forms at the plan's stated latency targets, with the
  query-latency suite showing no regression.
- Relationship uniqueness and every residual conjunct keep their current
  semantics by construction: the operator declines when it cannot prove them.
