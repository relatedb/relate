---
type: adr
area: engine
claim: 95c6d2f03fa9be5106c7b4c34a604d98ed85a487
name: 0396-profile-cumulative-time-column
status: accepted
tags:
  - observability
title: PROFILE cumulative time — the `Total (ms)` column and the `total_time_ms` document field
REFERENCES:
  - "[[adr/0344-query-plan-node-tree-and-per-operator-metrics]]"
  - "[[adr/0357-plan-summary-placement-and-neo4j-plan-args]]"
  - "[[adr/0365-http-query-api-v2-plan-node-shape]]"
  - "[[adr/0375-profile-walk-start-and-store-read-kinds]]"
  - "[[adr/0381-agent-native-plan-schema]]"
---

# ADR-0396: PROFILE cumulative time — the `Total (ms)` column and the `total_time_ms` document field

## Context

The PROFILE operator table reports each operator's **self** time (`Time
(ms)`, [ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md) phase 2). Every reference engine we researched also
reports a **cumulative** form — Postgres' `EXPLAIN ANALYZE` prints a
node's subtree actual-time, DuckDB a self-time percentage — because
self time alone cannot answer "what does this sub-pipeline cost?": the
root's own cell is a fraction of the query's wall time, and ranking
operators by subtree cost is how a reader finds the expensive *stage*
rather than the expensive *operator*. LABS-343's review of the plan
presentation named this the largest measurement gap left after
[ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md)/[ADR-0375](./0375-profile-walk-start-and-store-read-kinds.md).

The plan tree nests bottom-up (each earlier clause sits under the one
it feeds), so a node's subtree is exactly the sub-pipeline it heads:
its own elapsed time plus every descendant's is the cumulative form.

The `cypherlite.plan/v1` agent document ([ADR-0381](./0381-agent-native-plan-schema.md)) carries the same
per-node `actual` object without a cumulative field, so an agent cannot
rank sub-pipelines either.

## Decision

Add the cumulative form as a **derived presentation fact**, not a
producer-stamped wire field:

1. One fold in the model's plan vocabulary,
   `wire::subtree_time_ns` — a node's own `time_ns` plus every
   descendant's, saturated through the shared metric width policy — the
   single spelling of the fact.
2. The plan table's metric group gains a `Total (ms)` column, right
   after `Time (ms)`; the shared cell assembly renders it on every
   metrics-carrying node (a leaf's total equals its own time, the
   root's covers the whole pipeline). A never-executed operator keeps
   the shared marker in the cell.
3. The agent document's per-node `actual` gains `total_time_ms` (a
   float, milliseconds), computed in the document's own single walk —
   the child recursion reports its subtree total, so no second pass
   over the built value.

The producer (`PlanNode::to_value`) is untouched: like the
`Est/Actual` ratio and the `Executed` column, the total is derived at
the consumer from the wire tree, so no engine change and no wire-key
addition rides this ADR.

## Alternatives considered

- **Stamp `TotalTimeNs` in the producer's args** — rejected: it would
  add a wire key every surface's Neo4j-shaped projection must then
  consciously omit, and it duplicates a fact two consumers can derive
  from the tree they already walk; the ratio and the run-state column
  set the precedent that derived presentation facts stay derived.
- **A self-time percentage column (the DuckDB form)** — rejected: it
  needs a whole-plan total as the denominator, which is only known at
  the root, and a percentage of a sub-millisecond plan misleads; the
  absolute total composes with the existing `Time (ms)` without it.
- **No column; agents sum the tree themselves** — rejected: every
  consumer would re-implement the fold (a second policy, the shape the
  code rules forbid), and the human-facing table — the primary PROFILE
  surface — would keep the gap.

## Consequences

- Every PROFILE table gains one column; pinned outputs and the CLI
  spec's sample table carry it. The column rides the metric group, so
  an EXPLAIN tree gains nothing.
- The `cypherlite.plan/v1` document gains an additive field
 (`total_time_ms`) — schema-stable per [ADR-0381](./0381-agent-native-plan-schema.md)'s add-only rule; no
  version bump.
- The Bolt/HTTP Neo4j-shaped projections are unchanged (Neo4j has no
  such column; those surfaces mirror Neo4j's shape by design,
 [ADR-0357](./0357-plan-summary-placement-and-neo4j-plan-args.md)/[ADR-0365](./0365-http-query-api-v2-plan-node-shape.md)).
- The table layout computes the fold per node (a pre-order walk re-walks
  its subtree): O(n²) on a tree of n operators, each step a typed
  accessor validate — negligible at real plan sizes (tens of nodes).
