---
type: adr
area: engine
name: 0701-hub-join-cost-gate
claim: 6c99a3e6515921da3d34bca0254a3c6385edf6bb
status: proposed
tags:
  - pattern-matching
  - perf
title: The hub join pays only when its one enumeration is no dearer than the nested loop
REFERENCES:
  - "[[adr/0350-bound-node-pivot-for-pattern-expansion]]"
  - "[[adr/0351-cheapest-element-start-for-unbound-patterns]]"
  - "[[adr/0473-contracted-traversal-kernels]]"
---

# ADR-0701: The hub join pays only when its one enumeration is no dearer than the nested loop

## Context

A comma `MATCH` of two patterns that share one hub variable is answered by the
hub join: side A's rows group by hub, side B enumerates once from the base row,
and each hub emits the cross product. Its only gate was a minimum side-A row
count (128). Side B's enumeration ignores how selective side A already is, so
a selective first pattern pays for a full scan of the second:

```cypher
MATCH (p:Person)-[:HAS_INTEREST]->(i:Interest), (p)-[:LIVES_IN]->(c:City)
WHERE tolower(i.interest) = tolower('tennis') AND tolower(p.gender) = 'female'
RETURN c.city, count(p)
```

reads 57,158 nodes and walks 106,304 neighbors (163,462 db hits, 147 ms on a
100,000-person graph) where the same plan as one chain or a `WITH`-separated
pair reads 41 nodes and walks 9,405 (12,547 hits, 11-14 ms). Disabling the join
alone moved this to 22 ms.

## Decision

### Changed

- The hub join engages only when side B's one full enumeration is estimated no
  dearer than the nested loop's per-row expansions from the bound hub:
  `walk(side B from its cheaper unbound end) <= rows(side A) x walk(side B from
  the bound hub)`. Both sides are priced by the ladder's one canonical estimate
  (the per-row start selection's own `GraphStats::estimate`), side B's residual
  conjuncts narrowing its start like any other estimate.
- A tie goes to the join, so the measured fixture where every hub holds one
  side-A row (the original adversarial shape) keeps it.
- The 128-row floor stays: below it the nested loop is cheap either way.

## Consequences

- The graph-benchmark Q5 and Q6 comma forms run as their chain or `WITH` plans
  (7.8 ms and 25.9 ms against 52 and 305 ms with the join, measured on a loaded
  host; 11.9 and 158 ms on a quiet one); answers are unchanged.
- The decision reads estimates, so it inherits their quality: a default-priced
  side can mis-route a pair, and the statistics-backed levels tighten it.
- Q7's comma form is not changed by the gate (its cost is walk volume, tracked
  separately).
