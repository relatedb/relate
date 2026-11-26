---
type: adr
area: engine
name: 0694-default-selectivity-defaulted-estimate
claim: 501c6100d539cd0c7c4395b9374139c03abc6c29
status: proposed
tags:
  - planner
  - perf
title: Default selectivity table and the Defaulted estimate for unpriced predicates
REFERENCES:
  - "[[adr/0218-stats-based-match-orientation]]"
  - "[[adr/0474-compound-contains-seed-start-cost]]"
  - "[[adr/0486-range-index-starts]]"
  - "[[adr/0493-range-seek-crossover]]"
  - "[[adr/0508-planner-stats-consult-memo]]"
---

# ADR-0694: Default selectivity table and the Defaulted estimate for unpriced predicates

## Context

The endpoint ladder prices an element by its best measured signal: a bound
variable, an index equality or range window, a `CONTAINS` count, then label
rows. A predicate no index serves (`c.city = 'London'`, `p.age > 30` without a
range index, `tolower(p.gender) = tolower($g)`) is invisible to every level, so
the element prices at its whole label count as if the filter removed nothing.
On the graph-benchmark workload (100k persons) this leaves an interest
filter on an 8-to-41-row label priced as that whole label, and a hand-written
comma `MATCH` over the same predicates anchors on the wrong side and runs 14x
slower than the `WITH`-separated form (11.6 ms vs 157 ms).

## Decision

### Changed

- One documented table prices the predicates nothing else can: an equality
  or function-wrapped equality of one property keeps 0.1 of the rows, a
  one-sided range 0.3, both bounds of one property together 0.1 (collapsed,
  not multiplied), `<>` 0.9, `IN` with k items `1-(1-0.1)^k` (25 when the
  list is a parameter), `STARTS WITH` / `ENDS WITH` `0.3 / len` (length 6
  for a parameter). Only a comparison of one of the node's own properties
  (or a function of it) with a constant counts; a join predicate between two
  properties never prices as a filter.
- Several terms on one node combine by exponential backoff over the four most
  selective (`s0 * s1^(1/2) * s2^(1/4) * s3^(1/8)`), between independence
  and full correlation. The result floors at one row for a non-empty label.
- The estimate is a new variant, `Estimate::Defaulted { rows, scan }`: a
  default, never a measurement. Like a sampled `CONTAINS` seed it carries the
  whole-label scan the unseekable filter still pays, and the walk cost is
  floored at it ([ADR-0474](./0474-compound-contains-seed-start-cost.md)).
- A comparison the stats can measure (an index window the crossover gate
  declined, [ADR-0493](./0493-range-seek-crossover.md)) is a measurement,
  not a default: the label level keeps answering for it.
- The measured levels keep their order and still outrank the default; the
  default sits between `CONTAINS` and the plain label level. The table's
  values are the starting point for the statistics-backed levels (distinct
  counts, heavy hitters, min/max) to replace per key, and the `Defaulted`
  tag is what lets later changes keep a default from enabling a risky
  strategy.

## Consequences

- Anchors move toward elements whose filters the table narrows; on the
  30-query SF0.1 suite no query changed answer and no p50 moved beyond noise
  (suite compared against main, same host).
- The table is a heuristic: it can mis-order a pattern whose real
  selectivity differs sharply from the constants. The statistics-backed
  levels are the planned correction, and the executor's per-row start
  choice reads the same ladder, so plan and run time agree.
- EXPLAIN does not yet show the estimate or its provenance; a follow-up
  surfaces both.
