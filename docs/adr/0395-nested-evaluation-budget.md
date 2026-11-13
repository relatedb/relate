# ADR-0395: A per-statement nested evaluation budget

Status: Proposed
Claim: 7835e5f478866710db7dbcc57e00184b17285948

## Context

The engine evaluates nested statement bodies recursively, per row:

- a `CALL { … }` subquery body runs once per imported row
  (`call_subquery_row`, `cypherlite-executor/src/exec/call/mod.rs`), and a
  `CALL … IN TRANSACTIONS` runs one body pass per row of every batch
  (`exec_call_in_transactions`);
- an import-invariant CALL clause can instead be evaluated once through
  the shared fast path (`exec_call_subquery_shared`,
  `cypherlite-executor/src/exec/call/shared.rs`);
- a statement-level conditional runs its taken branch bodies
  (`exec_conditional`);
- expression-scoped `EXISTS { … }`, `COUNT { … }` and `COLLECT { … }`
  subqueries run a nested query per outer row, through the seeded
  statement engine (`eval_count_query`'s RETURN path, `eval_collect_query`)
  or the clause engine (`eval_exists_query`, `eval_count_query`'s
  omitted-RETURN path; `cypherlite-executor/src/exec/subquery_eval.rs`);
- `FOREACH` runs its clause list per list item per row
  (`cypherlite-executor/src/exec/dispatch.rs`);
- a `Query`'s NEXT stage chains another full statement pass
  (`execute_query_seeded` recursing into `query.next`).

Each level multiplies the passes of the level below, and the constructs
compose freely: a CALL body can contain a CALL, an EXISTS body can contain
EXISTS, a conditional branch can contain any of them. The number of nested
statement-engine passes therefore compounds exponentially with nesting
depth — roughly doubling per +1 level for the canonical doubling bombs.
Nested `CALL` is pinned openCypher (the TCK floor), un-gated and available
on every surface — Rust, Bolt, HTTP, the CLI, the C ABI — so any client of
an embedded engine can submit one; chained `DEFINE` bodies (local
callables, preview-gated, ADR-0382) reach the same curve with per-body state.

Nothing bounds the curve. The measurements behind LABS-416: a bare
nested-CALL doubling bomb costs ~7.7 µs per nested pass in release, a
chained-`DEFINE` bomb ~26–31 µs, and both double per +1 depth; depth 16
on the engine path is already 131,070 passes, and the depth-19 case
(524,288 passes) ran 16.3 s in release on a laptop — with the cost
doubling per further level. The query text of such a bomb is a few hundred
bytes, far under ADR-0257's 4 MiB and depth-500 input limits, which bound
*static* shape, not dynamic multiplication. ADR-0042's stored-graph
capacity engages only for writes at commit. And the compile-side budget of
ADR-0382 — `MAX_CLONED_NODES = 100_000` cloned rewrite nodes per
compilation unit
(`cypherlite-front/src/local_callables/rewrite.rs:101`) — never engages
here: a nested-CALL bomb uses no rewrite at all, and a chained-`DEFINE`
bomb with small bodies stays far under the node budget while its
evaluation count doubles. LABS-416 tracks exactly this residue: the test
that pinned the compile-side twin records "the runtime evaluation cost of
chained definitions is an executor-level budget"
(`cypherlite/cypherlite/tests/local_callables.rs:199`).

## Decision

A per-top-level-statement **nested evaluation budget of 100 000** nested
clause-engine part passes. One counter per top-level statement, incremented
at the nested entry points — never inside `run_clauses` itself, so the
top-level statement driver (`exec/mod.rs:433`) does not tick.

**The counting rule** (the contract for the implementing nodes):

> Per top-level statement, the counter increments once for every nested
> clause-engine part pass — every `execute_query_seeded` invocation ticks
> once **per UNION part of its body** (a wide body costs its part count,
> not one: a wide-UNION body driven by many rows spends rows × parts on
> the budget, so it crosses the limit instead of hiding its width inside
> one invocation tick): one part pass per CALL body row through
> `call_subquery_row` (including every row of every
> `exec_call_in_transactions` batch), per import-invariant CALL clause
> through `exec_call_subquery_shared`, per taken branch body through
> `exec_conditional`, per row through `eval_count_query`'s RETURN path
> and `eval_collect_query`, and once per NEXT-stage recursion — plus
> every direct part pass that is not the top-level statement driver
> (`exec/mod.rs:433`): one per part per row on `eval_exists_query`'s
> clause-engine path and `eval_count_query`'s omitted-RETURN path, and
> one per list item per row in the FOREACH arm; the pattern-probe fast
> paths (`resolved_endpoints_exist`, `probe_multi_pattern_exists`),
> whose eligibility excludes every scope-bearing expression so they can
> never nest, increment nothing.

The per-part granularity is the security-correct one: per-invocation
counting leaves a body's UNION width outside the budget, so a wide body
(each part linear in its own size, the whole body bounded by the
ADR-0257 text cap) driven by `UNWIND`-fan rows multiplies rows × parts
with only the row axis counted; per-part counting spends rows × parts
on the counter, and the multiplicative case crosses the budget at the
100 001st pass. The flat top-level statement is untouched — its own
clause count is bounded by its text, and the budget counts only
*nested* passes.

Concretely, the tick sites are the top of each `execute_query_seeded`
part iteration (`exec/call/mod.rs`) plus the three direct
`run_nested_clauses` call sites — `eval_exists_query`'s clause-engine
path, `eval_count_query`'s omitted-RETURN path (`exec/subquery_eval.rs`)
and the FOREACH arm (`exec/dispatch.rs`). The two probes cannot be used
to dodge the budget:
their eligibility rules exclude every scope-bearing expression, so a
nested-EXISTS chain always takes the clause-engine path and ticks; the
shared import-invariant fast path (`import_invariant_body`,
`exec/call/shared.rs:215`) never applies to a body containing a nested
CALL, so a nesting bomb always stays on the per-row path where the
counter ticks. Both properties were verified by compounding measurements.

**The error.** Exceeding the budget raises a typed error:
`Error::Execution` with the kind prefix, mirroring the twin's style — the
message begins `NestedEvaluationBudgetExceeded:` and names the limit. A
new row in the `error_kinds!` list (`cypherlite-error/src/lib.rs`):
`NestedEvaluationBudgetExceeded => ArgumentError` — the same class the
engine's other budget errors use (`Error::Capacity` maps to
`MemoryLimitExceeded`/`ArgumentError` in
`cypherlite/src/binding.rs:287`). Surfaces need no new arms: Bolt already
classifies `Error::Execution` as `SEMANTIC_ERROR_CODE`
(`surfaces/cypherlite-bolt/src/failures.rs:119`), HTTP's `run_error` maps
it to 400 (`surfaces/cypherlite-http/src/reply.rs`), and the CLI files it
`Fatal` (`surfaces/cypherlite-cli/src/errors.rs`).

**Fixed const, no knob.** The budget is a compile-time constant, the same
style as ADR-0257's fixed 4 MiB/500 and the twin's fixed
`MAX_CLONED_NODES`. No env var, no `OpenOptions` field: a knob re-opens
the axis an untrusted client drives, and the house precedent is a fixed
number. If legitimate work ever needs more, the remedy is a new ADR — at
most a typed `OpenOptions` field — never ambient environment.

**The number.** 100 000 mirrors the compile-side twin so both axes of the
same attack fail at the same scale, and at the measured 7.7–31 µs per
nested pass it bounds a hostile statement's nested work to well under the
depth-19 case — turning an unbounded exponential into a bounded, typed
failure.

**Documentation.** The budget is part of the public query contract for
generators and clients, documented beside the ADR-0257 input limits in
`docs/api/rust/spec.md` and `docs/api/bolt/spec.md` in the same change as
the implementation.

## Alternatives considered

- **No runtime budget; rely on the compile-side `MAX_CLONED_NODES`
  (ADR-0382).** Rejected: the twin bounds *rewrite* work — cloned AST
  nodes per compilation unit. A nested-CALL bomb uses no rewrite at all,
  and a chained-`DEFINE` bomb with small bodies stays far below 100 000
  cloned nodes while its evaluation count doubles per depth. The runtime
  axis is orthogonal and unbounded.
- **Rely on the existing input and capacity limits (ADR-0257, ADR-0042).**
  Rejected: ADR-0257 bounds static query shape (text bytes, parse depth) —
  a doubling bomb is tiny and shallow statically; its cost is dynamic
  multiplication, which no static bound sees. ADR-0042 caps stored-graph
  growth at commit; these bombs can be pure reads that never commit.
- **A wall-clock timeout or the cancel token instead of a pass counter.**
  Rejected: the host's `CancelToken` (ADR-0111) stays available, but a
  timeout is nondeterministic — the same query passes or fails by machine
  load — untestable at exactly the limit, and the embedded C-ABI host
  cannot be assumed to run a watchdog. A deterministic counter is
  reproducible and testable at exactly the limit and the limit + 1, like
  ADR-0257's test obligations.
- **Count something other than passes (rows produced, bytes allocated, or
  one tick per subquery *site*).** Rejected: the attacker's spend is many
  passes of the same small body, and only per-pass counting tracks it. A
  site-counted budget lets one body spend exponential passes per row
  while ticking once — the compounding bombs do exactly that — and
  rows/bytes are proxies that legitimate heavy queries trip
  unpredictably.
- **An env var or `OpenOptions` knob for the ceiling.** Rejected: a knob
  the host must remember to set re-opens the axis; the house precedent
  (ADR-0257, `MAX_CLONED_NODES`) is a fixed constant. The remedy for
  legitimate work that needs more is a new ADR — at most a typed
  `OpenOptions` field — never ambient environment.
- **Per-subquery or per-depth budgets instead of per top-level
  statement.** Rejected: the top-level statement is the unit a client
  submits and a surface fails; per-site budgets compose unclearly (nested
  budgets multiply the effective ceiling) and per-depth budgets do not
  map to work at all.

## Consequences

- A top-level statement whose nested evaluation count exceeds 100 000
  fails with the typed `NestedEvaluationBudgetExceeded` execution error
  naming the limit, instead of running an exponentially growing amount of
  work. The failure is deterministic and testable at exactly the limit
  (accepted) and the limit + 1 (typed error).
- Legitimate work that fans a nested body per row over more than 100 000
  driving rows must split into separate top-level statements — each gets
  its own budget. That is the documented contract for query generators:
  keep the per-statement fan below 100 000.
- The per-pass overhead is one integer increment at the nested entry
  points, outside the shared clause loop; flat queries and the top-level
  driver never tick.
- The compile-side twin (ADR-0382) and this budget now bound both axes of
  the same attack at the same number, 100 000; LABS-416 closes when the
  implementing nodes land.
- Surfaces need no new arms: Bolt `SEMANTIC_ERROR_CODE`, HTTP 400 and the
  CLI's `Fatal` classification carry `Error::Execution` today.
- `docs/api/rust/spec.md` and `docs/api/bolt/spec.md` gain the budget
  contract in the same change as the implementation.
- The counting rule above is the contract the implementing nodes follow:
  tick at the top of each `execute_query_seeded` part iteration plus the
  three `run_nested_clauses` sites (the EXISTS and COUNT-omitted paths in
  `exec/subquery_eval.rs`, the FOREACH arm in `exec/dispatch.rs`), never
  inside `run_clauses` itself.
