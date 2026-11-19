---
type: adr
area: engine
date: 2026-10-13
deciders: CypherLite maintainers
name: 0218-stats-based-match-orientation
status: proposed
tags:
  - planner
title: Statistics-based orientation of unbound comma-MATCH parts
REFERENCES:
  - "[[adr/0024-match-rewrite-planner]]"
  - "[[adr/0046-cypherlite-parser-memorydb-runtime]]"
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0171-lazy-per-label-snapshot-probe]]"
---

# 0218. Statistics-based orientation of unbound comma-MATCH parts

- **Related**: [ADR-0024](./0024-match-rewrite-planner.md) (MATCH rewrites after bind), [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) (planner
  pipeline), LABS-157 / PR #221 (intra-clause orientation toward bound
 variables), [ADR-0171](./0171-lazy-per-label-snapshot-probe.md) (label probe; a different consumer of label facts)

Note: drafted as 0213, renumbered past the in-flight sibling ADRs
0216/0217 (LABS-276) on rebase; 0216/0217 land with that branch.

## Context

PR #221 (LABS-157) fixed intra-clause orientation: a comma-separated
`MATCH` pattern whose variable is bound by an *earlier* pattern of the
same `MATCH` orients away from that bound node (`order_patterns` +
`orient_path` in `cypherlite-planner/src/rewrite.rs`, with
`add_pattern_vars` running per pattern so later patterns see earlier
bindings).

The residual case is a comma part with **no bound node at all**. Its
orientation is decided by `node_score`, a coarse rank: bound variable
(3) > property-equality map (2) > any label (1) > nothing (0). When both
ends of the part carry a label, the ranks tie and the part keeps its
written order. If the written first node is unselective (a huge label)
and the written last node is selective (a rare label), the executor
expands from the wrong end — and because `match_paths` re-runs each part
once per incoming row, the part's parts-before-it act as a multiplier:
the part behaves like a cartesian product that rescans the huge label
for every outer row.

Repro (in-memory store, release build): 200 000 `:Hub` nodes each with
`-[:S]-> (:Noise)`, only 100 of them with `-[:R]->` one of 10 `:Target`
nodes, plus 50 `:Seed` nodes.

```cypher
MATCH (s:Seed), (h:Hub)-[:R]->(t:Target) RETURN count(*)
```

runs in ~11 s (50 × 200 000 wrong-end expansions), while the
hand-reversed spelling `MATCH (s:Seed), (t:Target)<-[:R]-(h:Hub)` runs
in ~11 ms — a ~1000× gap that orientation alone closes.

## Decision

1. **Plan-time label statistics.** `cypherlite-planner` gains a
   `MatchStats` trait (`label_count(&self, label) -> Option<u64>`) and a
   `plan_with_stats` entry point; `plan` delegates with a `NoStats`
   null object (every count `None`), keeping every existing caller and
   the stats-free behavior unchanged. The
   facade (`cypherlite/src/connection.rs::plan_gate_classify`) already
   plans under the graph read guard, so it passes an adapter over
   `cypherlite-executor`'s `Graph::label_count` — the label-index set
   size, an O(1) approximation (it includes ids not visible at every
   snapshot, which is fine for an estimate). The engine crates stay
   acyclic: the trait lives in the planner, the executor only exposes a
   count, the facade adapts.

2. **Tie-break orientation by cardinality.** In `orient_path`, the
   coarse rank still dominates (a bound node always wins, a
   property-equality end still beats a label-only end). When both ends
   rank equal and statistics are available, the end with the smaller
   minimum label cardinality seeds the part: the path reverses only on a
   strictly smaller estimate, so equal or unknown estimates keep the
   written order.

3. **Order independent parts cheap-end first.** After the stable sort by
   descending seed rank, `order_patterns` floats a part ahead of its
   predecessors while they rank equal and the part's cheapest end (min of
   first/last label counts) is strictly smaller than theirs — a
   constrained adjacent-swap pass, not a re-sort: a part never passes a
   neighbor it shares a variable with, because that neighbor may be what
   binds it (LABS-157). Among equal ranks, a part that will scan 10 nodes
   runs before one that will scan 200 000. The cartesian product itself
   is mandated by the spec and stays; only the enumeration order of
   independent parts changes. Without statistics every estimate is
   `u64::MAX`, no comparison is strictly smaller, and the pass is a
   no-op, so stats-free plans are bit-identical to today.

Row order: comma-`MATCH` carries no order guarantee without `ORDER BY`;
PR #221 already reorders parts and reverses paths. The full openCypher
TCK (3892 scenarios) gates this change.

## Alternatives considered

- **Written order only (status quo).** Rejected: it is the bug — a
  selective last node never re-orients its part, and the per-outer-row
  rescan makes the cost multiplicative (LABS-273).
- **Executor-side reorientation in `match_one_path`.** The executor has
  the graph and could reverse the path physically. Rejected: orientation
  is already a planner concern (PR #221); splitting it across layers
  makes EXPLAIN lie about the executed shape and duplicates the reverse
  logic. The executor does keep all runtime seek decisions.
- **Degree-based costing (`adj_len`, LABS-193).** Per-node approximate
  degrees would refine expansion-cost estimates (label cardinality ×
  average degree). Rejected for now: degrees are per-node, not
  per-(label, rel-type), so using them at plan time requires sampling;
  label cardinality already closes the observed 1000× gap. A later ADR
  can add degree stats to `MatchStats` without changing the plumbing.
- **Full cost-based join reordering across parts** (estimate
  intermediate row counts, dynamic programming over part order).
  Rejected: far larger scope; the constrained cheap-end-first float
  captures the practical win without estimating intermediate rows.
- **Whole-graph census at plan time.** Rejected by [ADR-0171](./0171-lazy-per-label-snapshot-probe.md)'s laziness
  precedent; the label index already maintains the counts incrementally.

## Consequences

- `cypherlite-planner::plan_with_stats` and the `MatchStats` trait are
  new public items of the planner crate; `cypherlite-executor::Graph`
  gains `label_count`. Neither changes existing signatures.
- Plans become data-dependent: the same query can orient differently on
  different stores. This matches every cost-based planner but means
  regression tests must control the data, and EXPLAIN output can differ
  between stores. `plan()` (no stats) stays deterministic.
- Estimates are read once per statement under the plan read guard; a
  concurrent write can make them stale within the statement, which only
  affects performance, never correctness.
- The multi-database routing (`USE`, [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md)) reads statistics from the
  session's home graph even when the statement targets another
  namespace's store; cardinalities are heuristics, so the misestimate is
  accepted until per-namespace stats are needed.
