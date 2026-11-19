---
type: adr
area: engine
claim: 2e74ac3908cc689fd1b8d2915bd950f9122e716a
name: 0348-profile-metric-fields-db-hits-memory-bytes
status: accepted
tags:
  - observability
title: PROFILE metric fields — db hits, memory bytes, never-executed and estimate skew
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0218-stats-based-match-orientation]]"
  - "[[adr/0344-query-plan-node-tree-and-per-operator-metrics]]"
---

# ADR-0348: PROFILE metric fields — db hits, memory bytes, never-executed and estimate skew

## Context

[ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md) phase 2 landed the PROFILE per-operator metrics — rows produced
and elapsed time — collected through the opt-in `PlanMetricsSink` in the
dispatch loop and stamped onto the `PlanNode` tree. Its Decision anticipated
the metric fields ("later phases: rows + time first, db-hits counters, then
estimates-everywhere, then memory"); the estimates landed with the tree's
`estimated_rows` ([ADR-0218](./0218-stats-based-match-orientation.md) `PatternStats`). What is still missing is what
makes a PROFILE tree actionable beside a Neo4j plan: the store-access count
(`DB Hits`, the column Neo4j tuning starts from), a memory figure, a
never-executed marker for operators that did no work, and a signal when the
planner's estimate is badly wrong. The executor already carries the graph
walk counters `HOP_EXPANDS` / `NEIGHBOR_WALKS` (thread-locals, incremented
at the walk sites), and the engine already owns one canonical deep-size
policy for values: `Value::logical_bytes` ([ADR-0042](./0042-graph-memory-limit.md)).

## Decision

1. The PROFILE per-operator metric set is extended to
   `{ rows, time_ns, db_hits, memory_bytes }`, collected in the dispatch
   loop through the same sink (`PlanMetricsSink::record_clause` now takes
   one `ClauseMetricsRecord` bundle). The off-path budget stays one branch
   per clause: the store-access snapshot and the row-size walk run only
   when a sink is attached.
2. `db_hits` is the clause's store-access count: the delta of the
   executor's existing graph-walk counters (`HOP_EXPANDS` +
   `NEIGHBOR_WALKS`) across the clause's arm — the hop expansions and
   neighbor walks it performed. It rides the existing counters; no second
   per-clause counting mechanism, no new instrumentation sites. A clause
   that walks nothing (a projection, a filtered-out scan) reports an
   honestly counted zero.
3. `memory_bytes` is defined as **row-materialization bytes**: the summed
   logical size of the rows the operator produced — each binding's key
 counted as an [ADR-0042](./0042-graph-memory-limit.md) string, each value at its
   `Value::logical_bytes` (`rows_logical_bytes`, the one canonical helper
   beside the `Row` type). Stateful operators' non-row state —
   aggregation tables, sort buffers, distinct-tracking sets — is **a
   follow-up**, not covered by this metric.
4. The estimated-vs-actual ratio renders in a text-table column
   (`Est/Actual`) on PROFILE trees that carry estimates, computed only on
   nodes carrying both an estimate and actual rows; an estimate overshoot
   of 10x or greater flags a warning marker (`25.0x !`). An undefined
   ratio (no actual rows) renders the shared unavailable spelling, never
   a fabricated number.
5. An operator with 0 rows in and 0 rows out — never executed — renders
   the Neo4j-style `(never executed)` marker in its metric cells, never
   zeros. It is **derived**, not recorded: the plan tree's bottom-up
   nesting makes each earlier clause the child of the one it feeds, so an
   operator's input rows are its first child's `rows` (`never_executed`,
   the one policy in `wire::plan_table`, shared by the engine renderer and
   the formatter). The tree's root is exempt by construction: the
   projection always executes, and its zeros are the answer, not a marker.
6. The wire form (`args`) carries `DbHits` and `MemoryBytes` beside
   `Rows`/`TimeNs`, emitted together with the same saturation policy
   (`metric_i64`); the text table gains the `Db Hits` and
   `Memory (Bytes)` columns through the existing column-gating vocabulary
   (`METRIC_COLUMNS`).

## Alternatives considered

- Derive `db_hits` per mechanism (a counter per walk engine, summed per
  clause) — rejected: the existing two counters already cover the walk
  engines' graph accesses, and per-mechanism counters add instrumentation
  sites on the hot path for granularity no PROFILE consumer distinguishes.
- Define `memory_bytes` as allocated heap (measured via an allocator hook
  or `size_of` walks over engine internals) — rejected: the engine already
 owns one canonical logical-size policy ([ADR-0042](./0042-graph-memory-limit.md)) and one helper;
  allocator-level measurement is nondeterministic (allocator granularity,
  sharing) and cannot be pinned by tests.
- Record a `never_executed` flag in the sink — rejected: the fact is
  already derivable from the collected metrics and the tree's structure
  (the input clause's rows); recording a second spelling of the same fact
  is parallel state.
- A ratio cell only when the estimate is wrong by 10x (no bare ratio
  elsewhere) — rejected: the honest ratio on every node carrying both
  facts makes the warning comparable at a glance; gating the column to
  only-wrong nodes hides the healthy baseline.

## Consequences

- The PROFILE tree answers the first two Neo4j tuning questions (where
  did the work go, where did the rows go) with deterministic,
  test-assertable numbers; the ratio column points at planner skew
  without a separate tool.
- `db_hits` covers the walk engines' graph accesses; label-probe checks,
  index seeks, and other store reads are not counted yet — a clause that
  scans a label without walking relationships reports `0`, an honest
  count of *counted* accesses. Extending the counted set is a follow-up
  and must ride the same delta policy.
- `memory_bytes` under-reports stateful operators (aggregation, sort);
  reading it as "the operator's whole footprint" would be wrong — the
  docs and this ADR both name the follow-up.
- The sink signature change (`ClauseMetricsRecord`) is engine-internal:
  `ProfileMetrics` is the only implementor, so no surface breaks.
