---
type: adr
area: engine
claim: 702254a7b49653d31b822a6f64d7a6cb00113c80
name: 0462-value-nesting-depth
status: proposed
tags:
  - security
  - values
title: A fixed value-nesting depth cap of 500
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0170-shared-value-payloads]]"
  - "[[adr/0257-query-input-limits]]"
  - "[[adr/0259-query-execution-stack-provisioning]]"
  - "[[adr/0341-logical-bytes-within-one-walk]]"
  - "[[adr/0395-nested-evaluation-budget]]"
---

# ADR-0462: A fixed value-nesting depth cap of 500

## Context

[ADR-0257](./0257-query-input-limits.md) bounds the **static** shape of a query: 4 MiB of text and 500
levels of expression/primary nesting. The **runtime** values a query builds
are not bounded by that limit. Every dynamically constructed container — a
list literal, a map literal, a list/map comprehension, a map projection, a
list/map concatenation with `+` or `||`, `coll.zip`, the second
(aggregate) evaluator's own Map/List arms — accepts whatever its evaluated
elements are, and several of them can wrap a previous result:

```cypher
RETURN reduce(acc = [], x IN range(1, 100000) | [acc]) AS a
WITH [] AS a WITH [a] AS a …          -- one new level per clause
```

Both queries are tiny and shallow statically; the first builds a
100 000-level list from a few dozen bytes of query text. Nothing rejects
it, because the parser's depth counter never sees a dynamic construction.

The value itself is cheap (one `Arc` node per level), but almost every
consumer of a value walks it **recursively**:

- the renderers `Value::to_cypher_literal`/`to_display_string` and the
  plain/tagged JSON encoders (`value/json.rs`), including the canonical
  `serde::Serialize` tagged form used by the WAL and checkpoint codecs;
- the model bridge `value_to_cypher` (`cypherlite-core/src/convert.rs`)
  and the HTTP Query API v2 encoder
  (`surfaces/cypherlite-http/src/v2_json.rs`);
- the logical-bytes walk (`Value::logical_bytes`/`logical_bytes_within`,
 [ADR-0042](./0042-graph-memory-limit.md)/[ADR-0341](./0341-logical-bytes-within-one-walk.md)), equality and ordering (`eq_non_null`, `compare`,
  `order_by_cmp`), and the property write check's byte-array scan
  (`exec/props.rs`);
- `coll.flatten`, whose recursion follows the **data's** depth;
- the derived `Clone` and `Drop` impls.

A deep enough value therefore overflows a recursive walker's stack. In a
process abort that is fatal, and for an embedded engine reached through the
C ABI it kills the host process: exactly the failure class [ADR-0257](./0257-query-input-limits.md) closed
for *input* shape, left open for *built* values (security finding 5
). [ADR-0395](./0395-nested-evaluation-budget.md) bounds nested **statement evaluation**, an orthogonal
axis; it does not bound a single expression's value.

## Decision

### The cap

One fixed constant, beside `Value`:
`pub const MAX_VALUE_DEPTH: u32 = 500`
(`cypherlite/cypherlite-core/src/value/depth.rs`, together with the one
iterative depth walk and the checked constructors).

500 equals the parser's static nesting limit ([ADR-0257](./0257-query-input-limits.md)), so every
parse-legal literal is accepted unchanged, and it is low enough that 500
levels of the derived recursive drop, clone, and render walks stay deep
inside every surface's stack (the 64 MiB query worker, [ADR-0259](./0259-query-execution-stack-provisioning.md), and a 2 MiB
tokio caller alike). It is a compile-time constant with no env var and no
`OpenOptions` field — the same house rule as [ADR-0257](./0257-query-input-limits.md)'s fixed limits and
[ADR-0395](./0395-nested-evaluation-budget.md)'s fixed pass budget. A knob re-opens the axis untrusted input
drives.

### The checker is iterative, memoized and bounded

`Value::depth()` walks an explicit worklist (precedent: the three-color
iterative DFS in `cypherlite-executor/src/graph_type/inherit.rs`), never
recursion, so an over-deep value is rejected **before** anything recurses on
it. `depth_within(limit)` is its boolean front end (`depth() <= limit`). A
scalar is depth 0; a container's own level is one more than its parent's
(`[]` = 1, `[[]]` = 2); node, relationship and path property values nest
inside their entity. `Value::check_depth() -> Result<()>` is the typed half:
it raises `Error::execution("ValueDepthExceeded: …")` naming the limit.

Values share container payloads through `Arc` ([ADR-0170](./0170-shared-value-payloads.md)), so the value graph
is a **DAG**, not a tree: a fan-out like `reduce(acc = …, x IN range(1, 60)
| [acc, acc])` has 60 distinct payloads but ~2^60 expanded nodes. The walk
therefore **memoizes each container payload by identity** — its `Arc`
allocation address (distinct live allocations never share one, and every
visited payload stays strongly reachable from the walk root while the walk
runs, so a key cannot alias a freed payload) — and memoizes the payload's
relative depth. One `container()` match is the single owner of the container
vocabulary: it yields the memo key and the lazily enumerated children
together, with every scalar variant spelled out so a new `Value` variant is
a compile error there. A shared payload is measured once, so the cost is
linear in distinct payloads, never in the expanded node count. As a
belt-and-braces guard, one walk may take at most
`MAX_DEPTH_WALK_STEPS = 10 000 000` worklist steps; exceeding it answers
`u32::MAX` (over every cap), so a future sharing pattern the memo cannot
collapse is refused rather than hung.

### The checked-constructor seam

`Value::checked_list(Vec<Value>)` and
`Value::checked_map(BTreeMap<String, Value>)` build the container and run
`check_depth`; every dynamic container construction in the evaluator uses
them:

- `eval/mod.rs`: `Expr::Map`, `Expr::List`, `Expr::ListComp`,
  `Expr::MapProjection`, `Expr::MapComp` (via `eval_map_comp`), and the
  pattern-comprehension projection;
- `eval/expr.rs`: the list/map arms of `+` and the list arm of `||`;
- `eval/quantifier.rs`: the `reduce`/`allReduce` loop tracks the canonical
  wrap `[acc]` depth incrementally (it grows by exactly one level per
  iteration) and builds it through `checked_list_with_depth`, so the loop
  is O(1) per iteration and never re-walks the accumulated value; any
  other expression shape goes through the general evaluator and its
  checked constructors. That constructor is an evaluator-internal seam
  (doc-hidden): it trusts the depth the loop tracks, and only those loops
  may call it — every other caller uses `checked_list`;
- `exec/aggregate.rs`: the **second** evaluator's `Map`/`List` arms —
  `agg_eval` has its own Map/List/ListComp/MapComp arms, so an `eval`-only
  seam would miss it — and the aggregate `collect` arm;
- `exec/match_clause.rs`: the horizontal-aggregate `collect`
  (`horizontal_aggregate`), reached when `collect(expr)`'s argument names a
  list variable;
- `exec/subquery_eval.rs`: the `COLLECT { … }` subquery result list;
- `eval/collection.rs`: `coll.zip`'s per-index lists and its result, plus
  the other `coll.*` list constructors.

The canonical bombs die at level 501: `reduce`'s accumulator starts at
`[]` (level 1) and gains one level per `[acc]` iteration; a chain of
`WITH collect(a) AS a` (or `WITH COLLECT { … } AS a`) clauses does the
same, one level per clause. The checked list construction that would cross
the cap throws.

### Defence in depth

Values below the cap are safe, and the checked constructors mean the walks
below can never see an over-cap value. They still carry a depth guard so a
missed seam cannot turn into a stack overflow:

- fallible seams throw the typed error: `Value::check_depth`, JSON
  parameter entry (`params_from_map` — the one fold behind HTTP plain/typed,
  MCP, FFI and the Rust API; Bolt's 128-level PackStream limit and the
  Python client's 128-level limit already reject earlier), and the tagged
  decoder `from_json_tagged` (untrusted persistence input);
- the canonical `serde::Serialize` tagged encoder pre-checks once at its
  public entry and returns a serialization error;
- infallible walks thread a `depth` parameter and **saturate** at the cap
  instead of recursing: `to_cypher_literal`/`to_display_string` render
  `<depth limit exceeded>`, `to_json_plain`, `value_to_cypher` and
  `v2_value_json` produce `null`, `eq_non_null` answers `false`, `compare`
  answers `None`, `order_by_cmp` answers `Equal`, and the logical-bytes
  walk stops counting. These branches are unreachable behind the checked
  constructors; they exist only as the safety valve.

### Drop

No change. The derived recursive `Drop` over at most 500 levels is safe
(measured on the release stack with the [ADR-0259](./0259-query-execution-stack-provisioning.md) worker margin). Lifting
the cap later would require giving the payload a newtype with its own
iterative `Drop` — never `impl Drop for Value`, which breaks the
move-out matches in the evaluator with E0509.

### Error taxonomy

A new row in `error_kinds!` (`cypherlite-error/src/lib.rs`):
`ValueDepthExceeded => ArgumentError`, beside
`NestedEvaluationBudgetExceeded`. It is raised as
`Error::Execution("ValueDepthExceeded: …")`; surfaces need no new arms —
Bolt classifies `Error::Execution` as `SEMANTIC_ERROR`, HTTP's `run_error`
maps it to 400, and the CLI files it `Fatal`.

## Alternatives considered

- **No cap; make every walker iterative.** Rejected: the walkers are many
  (renderers, JSON, bridges, comparisons, logical bytes, drop), several are
  hot paths where an explicit worklist costs allocation and
  readability, and the cap removes the problem at its source instead of
  rewriting every consumer.
- **A recursive checker.** Rejected: it overflows *before* it can throw —
  the exact failure being closed.
- **A byte/allocation budget instead of a depth cap.** Rejected: a
  deep-narrow value (`[[[[…]]]]`) is tiny in bytes and huge in depth; a byte
  budget does not early-exit on it (pinned by a unit test).
- **A per-surface limit (Bolt/HTTP/FFI only).** Rejected: the Rust API, the
  CLI, TCK and embedded hosts all evaluate the same expressions; a core
  constructor seam covers every surface with one check and one error.
- **An env var or `OpenOptions` knob.** Rejected: the house precedent is a
  fixed constant; a knob the host must remember to set re-opens the axis.
- **Raising the cap above the parser's 500.** Rejected: it would let the
  engine build values the parser refuses to express, and deepens every
  recursive walk for no documented use.

## Consequences

- A dynamically built value nested past 500 container levels fails with the
  typed `ValueDepthExceeded` execution error naming the limit; exactly 500
  levels are accepted. Both boundaries are tested through the public query
  path at 500/501 and with the two bombs above, under a 2 MiB caller stack
  and in the release stack lane.
- Legitimate queries keep a 500-level value budget per expression result
  (the same budget the parser already imposed on literals); `reduce`
  accumulators that build a list per iteration must stay within it or
  aggregate differently.
- Every evaluated list/map literal now walks its own subtree once for the
  depth check — O(size) with early exit, O(1) for a scalar element, and
  bounded by the cap.
- The Bolt PackStream encoder still refuses results past its own 128-level
  wire limit with a typed `DepthExceeded`; a 500-deep engine value is
  therefore a typed Bolt refusal, never an abort.
- `docs/limitations.md`, `docs/api/rust/spec.md` and
  `docs/api/bolt/spec.md` gain the value-depth contract in this change.
- [ADR-0257](./0257-query-input-limits.md) (static input limits), [ADR-0259](./0259-query-execution-stack-provisioning.md) (the query worker stack that
 this cap's margin is calibrated against), and [ADR-0395](./0395-nested-evaluation-budget.md) (the orthogonal
  nested-evaluation budget) are the governing precedents.

## Later references

- [ADR-0257](./0257-query-input-limits.md): query input limits (4 MiB, static nesting 500).
- [ADR-0259](./0259-query-execution-stack-provisioning.md): the per-connection 64 MiB query worker.
- [ADR-0395](./0395-nested-evaluation-budget.md): the per-statement nested evaluation budget.
- [ADR-0628](./0628-plan-wire-conversion-uncapped.md): the plan wire conversion does
  not ride this cap — engine-produced plan metadata crosses the model bridge
  uncapped (`plan_value_to_cypher`); the cap governs runtime data values only.
