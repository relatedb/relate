---
type: adr
area: engine
name: 0717-bounded-probe-prices-an-unseekable-start
claim: e75d682c7fae1ee0632caf5ba2e224cf5684df18
status: proposed
tags:
  - pattern-matching
  - planner
  - perf
title: A small label's unseekable start filter is priced by a bounded probe, so the strategy gates see a measured count
REFERENCES:
  - "[[adr/0350-bound-node-pivot-for-pattern-expansion]]"
  - "[[adr/0351-cheapest-element-start-for-unbound-patterns]]"
  - "[[adr/0694-default-selectivity-defaulted-estimate]]"
  - "[[adr/0701-hub-join-cost-gate]]"
  - "[[adr/0713-multiplicity-semi-join]]"
---

# 0717: A small label's unseekable start filter is priced by a bounded probe, so the strategy gates see a measured count

## Context

[ADR-0713](./0713-multiplicity-semi-join.md) proposes a multiplicity-carrying
semi-join for a filter-only pattern, chosen by a cost gate that refuses a side
priced from the default table or a rank sentinel
([ADR-0694](./0694-default-selectivity-defaulted-estimate.md)). The hub join's
gate ([ADR-0701](./0701-hub-join-cost-gate.md)) reads the same estimates.

The social-network suite's Q7 is the case both target, and its filter side is
exactly a default-priced one:

```cypher
... (p)-[:HAS_INTEREST]->(i:Interest)
WHERE tolower(i.interest) = tolower('photography')
```

A function-wrapped equality cannot be seeked, so the start estimate for
`Interest` falls to the default selectivity: about five of the 41 nodes,
times the start degree, 30,495 hops. The true start set is one node and about
6,100 hops, a factor of five off, and the guess is what keeps the gate on the
nested plan: measured on 100,000 persons, forcing the existing hub join past
its gate answers the comma form in about 35 ms instead of 96 ms with the same
answer, while the gate prices the join at 30,495 against the nested loop's
16,618 and declines. Under ADR-0713's rule the same default price would
disqualify the semi-join, so the better operator would never be chosen for
the query it was proposed for.

## Decision

1. **A bounded probe turns the guess into a count.** When a start element's
   rows come from the default table or a rank sentinel, its label has at most
   1,024 nodes, and its conjuncts are evaluable on the start node alone, the
   estimate evaluates those conjuncts over the label's nodes (a bounded scan,
   no expansion) and uses the exact passing count. The result carries a
   measured basis. A larger label, or a conjunct that needs another
   variable, keeps the default price.
2. **Gates need nothing else.** The probe changes the number and the basis a
   gate already reads; ADR-0713's operator, the hub join's cost gate, and
   the per-row start choice all consume it unchanged. Basis still gates and
   breaks ties only, never overrides a strictly cheaper price.
3. **Bounded and cached per statement.** The scan is capped by the label
   size above, runs at most once per (start element, statement), and charges
   no retained memory; a cancelled statement stops it like any scan.

## Alternatives considered

- **Raise or retune the default selectivity.** One constant cannot be right
  for every predicate; the gain here is specific to a small label. Rejected.
- **Sample the label.** A sample of 1,024 nodes is a scan for the labels this
  covers, so the exact count costs nothing more. Rejected.
- **Widen the hub join to a multi-hop first pattern** (the first draft of
  this ADR). It answers the comma form too, but ADR-0713 already proposes the
  operator that answers both forms without materializing both sides, and
  rejects widening the hub join as the general answer. Dropped here; the
  prototype timing above is evidence for the probe only.

## Consequences

- Q7's filter side prices at one start node and about 6,100 hops, so
  ADR-0713's gate can admit it and the hub join's gate stays consistent with
  it.
- Planning (or the gate) pays up to 1,024 predicate evaluations once per
  affected element, only for a guess-priced start on a small label.
- Every strategy that reads start estimates sees the better number, so the
  30-query suite and the ordering tests confirm no plan changes elsewhere
  before this is accepted.
- This is a proposal until ruled on; ADR-0713 owns the operator, and the
  split with it is the one stated above.
