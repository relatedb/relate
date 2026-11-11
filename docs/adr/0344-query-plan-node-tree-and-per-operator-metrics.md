# ADR-0344: Query plan node tree and per-operator metrics

Claim: dd027d24e2a5cb4acd660cd567008ab773573d26
Status: Accepted

## Context

`EXPLAIN` in this engine prints clause names only (`cypherlite/src/explain.rs`)
and `PROFILE` appends `_profile_*` columns to data rows (LABS-163), corrupting
the result shape. Recent RCA work (LABS-315, LABS-317, the LABS-251 sampling
chain) repeatedly fell back to process sampling (`sample`) and scaling
measurements because no per-operator accounting exists. The executor is a
clause-at-a-time dispatcher (`exec/dispatch.rs`) over specialized mechanisms
(node/rel candidates, id seeks, hop expansion, far-node probes, exists probes,
agg folds, pushdown folds, GPM, shortest path, vector SEARCH, bulk writes),
already carrying thread-local counters (`HOP_EXPANDS`, `NEIGHBOR_WALKS`,
`FAR_PROBE`) and ADR-0218 `PatternStats` cardinality estimates that are
computed in the planner but never surfaced.

LABS-163 research (comment 786f7de1) verified the Neo4j contract live
(2025.10.1): a text plan (fixed-width ASCII tree, PROFILE-only actual
columns) plus a structured recursive plan node over Query API v2 and Bolt
SUCCESS metadata — `queryPlan` for EXPLAIN, `profiledQueryPlan` for PROFILE —
which is what client tooling (Browser, drivers) renders.

## Decision

1. A `PlanNode` tree is the engine's plan representation for EXPLAIN and
   PROFILE: `{ id, operator, details, identifiers, children,
   estimated_rows, metrics { rows, db_hits, time_ns, memory_bytes } }`.
   Operator names follow the Neo4j vocabulary where a mechanism has an analog
   (NodeByLabelScan, NodeIndexSeek, Expand, VarLengthExpand,
   EagerAggregation, ProduceResults) and this engine's own names where it
   does not (FarNodeProbe, ExistsProbe, AggFold, GpmMatch, VectorSearchMatch,
   BulkWritePlan).
2. EXPLAIN returns the tree with `estimated_rows` filled from ADR-0218
   stats and no execution; PROFILE returns the same tree with actual
   `metrics` (later phases: rows + time first, db-hits counters, then
   estimates-everywhere, then memory).
3. The tree is **built only on the EXPLAIN/PROFILE path**. The regular
   execution path never constructs `PlanNode`s: phase 1 is pure opt-in
   (no hot-path reads, no allocations). Later PROFILE phases use RAII
   scope guards gated behind a collector `Option` so the non-profiled path
   pays one predictable-null check per operator, nothing more. A loadtest
   comparison accompanies each phase that touches the hot path.
4. Per-op memory bytes, `Indexes Used`/`Ordered by` columns are deferred;
   page-cache stats and planner-implementation metadata are omitted
   entirely (not meaningful for an embedded in-memory engine — emit
   nothing rather than constant zeros).

## Alternatives considered

- Keep clause-name text and `_profile_*` columns — rejected: corrupts the
  result shape, cannot be rendered by client tooling, no per-operator
  attribution, and RCA stays dependent on process sampling.
- Restructure the executor as an operator tree (volcano-style) first —
  rejected for now: the dispatcher's specialized folds exist precisely
  because clause-at-a-time wins on this engine; a `PlanNode` view built
  beside execution gives the observability without the restructure.
- Adopt Neo4j's plan schema field-for-field including page-cache —
  rejected: fields that are always zero for an in-memory engine mislead;
  omit them instead.

## Consequences

- EXPLAIN/PROFILE become first-class debugging surfaces; LABS-315-class RCA
  becomes a PROFILE read-off.
- Deterministic metrics (rows, db-hits, estimates) are test-assertable.
- The `_profile_*` result columns are deprecated (ADR-0345 governs their
  removal path).
- Each PROFILE phase that adds hot-path instrumentation must show a
  loadtest comparison proving the off-path is unaffected.

## Later references

- ADR-0348 defines the metric fields this ADR anticipated (`db_hits`,
  `memory_bytes` as row-materialization bytes) and the never-executed and
  estimate-skew markers.
- ADR-0363 defines the Neo4j notification model and its per-surface transport
  that extends this ADR's typed plan warnings.

## Later references

- ADR-0400 supersedes the shared-module placement this ADR's presentation
  vocabulary rode ("shared by the engine renderer and every surface"):
  the engine's own text renderer was deleted, leaving one consumer, and
  the presentation moved to `cypherlite-formatter`. The metric contract
  itself is unchanged.
- LABS-461 (one warning type) moved this ADR's `PlanWarning` enum from
  the engine (`plan_node/warning.rs`) into `cypherlite-model` beside
  `NotificationKind`, its 1:1 projection target, and into the wire
  accessor's parse: name, payload, notification projection, and message
  text are owned once by the enum instead of three review-coordinated
  layers. `cypherlite::PlanWarning` and the wire shape are unchanged.
- ADR-0555 fixes the operator boundary this ADR's per-operator metrics
  observe: a pull stream of rows or bounded batches rather than a whole
  vector. Row counts describe rows passing that boundary, and retained
  state is charged by the declared breakers (ADR-0463), not by every operator.
