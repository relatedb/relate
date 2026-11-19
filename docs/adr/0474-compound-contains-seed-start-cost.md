---
type: adr
area: engine
claim: 6dd691a7575b1766d4ff71da8496f332cdb67b10
name: 0474-compound-contains-seed-start-cost
status: proposed
tags:
  - index
  - planner
title: Compound start cost for unindexed CONTAINS seeds
REFERENCES:
  - "[[adr/0218-stats-based-match-orientation]]"
  - "[[adr/0245-query-latency-baseline-environment-and-gate]]"
---

# ADR-0474: Compound start cost for unindexed CONTAINS seeds

## Context

The pattern-order ladder ([ADR-0218](./0218-stats-based-match-orientation.md), `cypherlite-planner/src/order.rs`)
prices a MATCH endpoint by its best measurable signal — a bound variable,
RANGE-equality hits, `CONTAINS` candidates, label rows, then rank
sentinels — and the start's walk cost is that estimate scaled by the
first hop's degree (LABS-351's shared ladder, LABS-426's mean-to-tail
flip). Both the planner's pattern orientation and the executor's per-row
start selection consume the same numbers.

LABS-643 made `CONTAINS` measurable without a TEXT index: a bounded
sample scan of the label's index bucket, floored at 1% of the bucket,
reported as `ContainsCount::SampleScan(rows)` — the seed's **output**
estimate only. GBL SF0.1 q8 exposed what that cannot see:

```
MATCH (p:Person)<-[:hasModerator]-(f:Forum)
WHERE date(p.birthday) > date("1990-01-01")
  AND f.title CONTAINS "Emilio Fernandez"
RETURN DISTINCT p.ID
```

The unindexed `f.title CONTAINS` sampled ~137 rows (the 1% floor of the
13,750-row Forum label) and won the anchor over Person's 1,528 label
rows — so the walk scanned all 13,750 Forums (PROFILE: NodeReads 13,750)
at ~1.9 ms host p50, while the Person-anchored walk pays 1,528 candidate
reads plus 138 hops at ~0.26 ms. An unseekable filter pays the
**whole-label scan** before any match; the output estimate prices only
the survivors. Two naive patches were measured and both fail one side:

- Pricing the seed at its sampled output only (LABS-643 as shipped)
  keeps q8 Forum-anchored at ~1.9 ms.
- Pricing the seed at its label-bucket rows (the scan) fixes q8
  (~0.26 ms) but regresses q19 60x (2.77 → 164.9 ms): the genuinely
  selective Tag seed — ~35 survivors on the 16k Tag label — loses its
  anchor to Place's 1,460 whole-label rows, and the Place-anchored walk
  rides ~342k neighbor hops.

## Decision

The sampled `CONTAINS` provenance carries both costs, and the ladder
compounds them at the walk level:

- `ContainsCount::SampleScan` becomes `{ rows, scan }`: the sampled
  output estimate and the label bucket's whole row count — the scan the
  unindexed filter pays — both facts the sampler already holds. The type
  moves to `cypherlite-planner` (the `PatternStats` contract's
  vocabulary, the one place both ladder consumers meet it);
  `cypherlite-executor` re-exports it so its construction sites and
  tests keep one canonical path. The TEXT-indexed arm is unchanged: an
  index seek pays no label scan.
- A sampled seed's **walk cost** is `max(scan, output × degree)`: the
  scan is a one-time floor — paid once, before any hop, never multiplied
  by the degree — while the output scales through the hops
  (`direction_cost` and `GraphStats::estimate` run the same arithmetic,
  so plan-time orientation and exec-time start selection agree).
- The orientation/start tie-break seed is `max(output, scan)`: when both
  directions land on the same relationship count (q8: Person's
  degree-scaled walk and Forum's scan floor both reach the 13,750
  `hasModerator` rels), the cheaper scan wins — and an unindexed
  `CONTAINS` seed's scan is its whole label.

Measured (GBL SF0.1, host, upstream RANGE-only index profile): q8 flips
to the Person anchor (PROFILE: start #0 Person, 1,528 nodes, answer
unchanged) — the estimate alone takes q8 from 1.92 ms to ~0.26 ms p50,
and the scan gate's settled-conjunct skip (the same node's per-row
machinery: the residual pass no longer re-evaluates a conjunct the gate
already settled under the candidate's own read) takes the walk to
~0.23 ms. q19 keeps its Tag anchor at ~2.8 ms (the 16,059-row Tag scan
floor still beats Place's ~342k degree-scaled walk); q2/q16's
RANGE-equality seeds keep their starts (the compound only raises the
unindexed CONTAINS side); the full 30-query suite stays within its
[ADR-0245](./0245-query-latency-baseline-environment-and-gate.md) bands.

## Alternatives considered

- **Selectivity-only** (LABS-643 as shipped, the sampled output as the
  seed). Rejected: q8 stays Forum-anchored at ~1.9 ms (measured) — the
  output estimate cannot see the whole-label scan an unindexed filter
  pays, so a 137-survivor seed on a 13,750-row label steals the anchor
  from a 1,528-row scan.
- **Scan-only** (price the seed at its label-bucket rows). Rejected: q19
  regresses 60x, 2.77 → 164.9 ms (measured) — the scan rides the degree
  as a seed value, so a genuinely selective seed on a large label loses
  to a small label whose degree-scaled walk is far more work. The scan
  must floor the walk, not ride it.
- **TEXT-index-only measurement** (the pre-LABS-643 stance: unmeasurable
  without a TEXT index, fallback to whole-label rows). Rejected: the
  fallback IS scan-only at the seed level, with the same measured q19
  regression, and it blinds every RANGE-only store to the selectivity
  its queries actually have.

## Consequences

- A sampled `CONTAINS` seed can now lose an anchor it used to win
  whenever its label scan exceeds the rival's degree-scaled whole walk
  (q8's intended flip), and keep one whenever its scan floor stays under
  that walk (q19). Both decisions flow through the one ladder, so the
  planner's orientation and the executor's start selection cannot
  disagree on them.
- `ContainsCount::SampleScan` is a struct variant on an engine-internal
  surface (`cypherlite-executor`'s re-export path is unchanged); no
  public client surface sees it.
- The `PatternStats::contains_rows` contract returns the typed count,
  so every implementation states its provenance — a stub cannot answer
  a bare number the ladder would misprice.
- The pinned tests carry each side of the decision at three scales:
  `order.rs` stub tests (the flip, the keep, and the TEXT-indexed
  no-floor), `graph_stats_tests.rs` toy graphs (the q8 tie-break and the
  q19 keep on live sampled counts), and the GBL suite itself (the
 acceptance bands, [ADR-0245](./0245-query-latency-baseline-environment-and-gate.md)).
