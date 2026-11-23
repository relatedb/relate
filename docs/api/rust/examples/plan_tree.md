# Plan-tree example

Run with `cargo run -p relate --example plan_tree`.

`Connection::explain_plan` (ADR-0344/0345) plans a statement without
executing it and returns the structured `PlanNode` tree — the same plan
an `EXPLAIN` statement stamps on `QueryResult::plan_root()` — for callers that
render it themselves: the CLI's terminal-width table, tooling, and the
later `PROFILE` phases. `ProduceResults` sits outermost with each earlier
clause nested beneath it (`Union`/`UnionAll` above multi-part pipelines,
`Next` above chained stages); admission runs exactly as `EXPLAIN` does,
so a statement that cannot be planned fails with the same typed error and
a planned write leaves the graph untouched.

The plan text is rendered at the boundary, never in the engine
(ADR-0359): the one renderer is `relate_formatter::plan_text_lines`
(the `Query:` echo header, the `Plan:` marker and the boxed operator
table), with `plan_header_lines` and `echo_statement` exposing the header
pieces — the CLI, Bolt and HTTP call these. The engine's contribution is
the value form: `PlanNode::to_value` (the undecorated structured plan)
and `PlanNode::to_neo4j_value(plan_text)`, which heads the caller's
rendered text with the `Cypher`/`Planner`/`Runtime` identity block into
`string-representation` — a surface passes its
`relate_formatter::plan_text_lines(…)` output as `plan_text` (the
formatter crate's own `examples/plan_text.rs` does exactly that); the
engine example cannot call the formatter, so it passes the plain lines a
consumer would hand it.

On a `PROFILE` run the stamped tree (`QueryResult::plan_root()`) carries the
per-operator metrics (`ClauseMetrics { rows, time_ns, db_hits,
memory_bytes, expression_built_bytes, reads, start, phases, batched,
zone_skips, memo }` — `reads` breaks `db_hits` into hops / neighbor walks /
node reads, `start` names the walk's element, direction and reason class
(LABS-369), `phases` the seek/walk time split (LABS-448), `batched`
whether the clause ran the ADR-0468 batch scan/filter/project chain,
`zone_skips` (LABS-645) how many conjunct-batch evaluations that chain's
filter provably replaced with the correct empty survivor set via the
chunk's derived per-column min/max zone, `memo` the expression-memo
counters of ADR-0654 — see the [PROFILE memo-counters example](profile_memo.md)) and
the typed warnings — the
example profiles a
27-row-labeled scan filtered to one row and asserts the scan's metrics
and the `EstimationMiss` warning behind the flagged `Est/Actual` cell
(`27.0x !`) — facts every renderer, the formatter's included, reads from
the tree.

The example checks the tree shape for a MATCH with a lowered WHERE,
verifies a planned write executes nothing, and asserts the
`string-representation` form `to_neo4j_value(plan_text)` composes.
