# Phase-timing example

Run with `cargo run -p relate --example phase_timing`.

Every `QueryResult` carries its statement's engine-reported wall-clock
elapsed (`QueryResult.header.elapsed`, ADR-0221) split always-on across the
pipeline's three stages (ADR-0491): `QueryResult::parse_ms()` — the
front-end parse and lowering; `QueryResult::plan_ms()` — binding, logical
planning and the admission classification; and `QueryResult::execute_ms()`
— the dispatch (or, for `EXPLAIN`, the plan render, that path's
execute-equivalent stage). All four numbers are `f64` milliseconds through
the one Duration→ms conversion behind `elapsed_ms()`, measured once around
`Connection::run_query` — never around row serialization or surface-side
output. The three phases are disjoint sub-windows of the measured window,
so `parse_ms() + plan_ms() + execute_ms() <= elapsed_ms()`: the
inter-stage gaps (the post-EXPLAIN gates, token reservation) stay in the
total alone. There is no opt-in — no PROFILE wrap, no feature flag, no
environment variable — and, like `elapsed`, the split is per statement: a
session or transaction running several statements reports each statement's
own four numbers. The example runs a write, a read, an EXPLAIN and a
PROFILE, and prints each statement's elapsed beside its three phases; it
also shows `relate::timing_stamp_ms(&QueryResult)` — the one
projection bundling the total and its split into the
`relate_model::wire::TimingStampMs` the canonical envelope builder
takes, the same numbers through the same single conversion.

LABS-1120 extends the same vocabulary one level finer, always-on and per
statement like the coarse split: `QueryResult::lex_parse_ms()` /
`rewrite_ms()` / `lower_ms()` partition `parse_ms()` (lexing plus the AST
build; the CIP-231 resolver pass; validation-and-lowering — the lexer and
the parser are fused at the parser's one tokenizing constructor, and the
validation battery runs inside the lowering walk, so those two pairs are
one window each, never two), and `QueryResult::result_generation_ms()`
sits inside `execute_ms()` (the buffered executor result's conversion into
the `QueryResult`; `0.0` on `EXPLAIN`, whose render is itself its result
generation). Beside them, `QueryResult::plan_cache()` reports which
plan-cache stage (ADR-0503) served the statement — `miss` /
`hit-raw-fastpath` / `hit-template` / `replan-generation-mismatch` /
`bypassed` — the fact that explains a hit's near-zero `parse_ms()` and its
zero fine parse sub-windows. The example pins all of these invariants and
observes a miss turn into a raw fast-path hit; its output prints each
statement's fine split and plan-cache outcome beside the coarse three.
