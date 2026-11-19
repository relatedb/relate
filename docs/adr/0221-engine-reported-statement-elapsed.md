---
type: adr
area: engine
name: 0221-engine-reported-statement-elapsed
status: accepted
tags:
  - result-metadata
title: Engine-reported per-statement elapsed time
REFERENCES:
  - "[[adr/0215-python-result-summary]]"
  - "[[adr/0491-phase-timing-wire-contract]]"
---

# ADR-0221: Engine-reported per-statement elapsed time

## Context

Every surface that reports a statement's wall-clock time measures it at
its own client edge: Python's `ResultSummary.elapsed_ms` wraps the native
`Database.execute` call in `time.perf_counter` ([ADR-0215](./0215-python-result-summary.md), explicitly a
placeholder for LABS-286), the CLI's `.timer` wraps
`exec_cypherlite_query` (including row conversion and output
serialization), and the wire envelope carries no timing at all. Client
edges disagree on what they include — PyO3 marshalling, row capping,
formatter output — so the same statement reports different "elapsed"
numbers per surface, and none of them is the engine's own plan-plus-run
duration.

[ADR-0215](./0215-python-result-summary.md) reserved the semantic switch: when the engine reports elapsed
itself, `elapsed_ms` re-sources to that value inside the same property.

## Decision

The engine stamps every `cypherlite::QueryResult` with `elapsed`
(`std::time::Duration`), measured once in `Connection::run_query` around
the whole statement body — parse, plan, gates and dispatch (execution or
EXPLAIN/PROFILE rendering) — and never around row serialization or
surface-side formatting. Construction sites seed `Duration::ZERO`; the
single `run_query` tail stamps the honest value, so no early-return path
(EXPLAIN included) ships an unstamped result.

Cross-surface rollout in the same change (the result-metadata parity
rule):

- **Wire envelope** (`cypherlite_model::wire::response_envelope`) gains
  an `elapsed_ms: f64` parameter, inserted between `stats` and `error`;
  `query_result_to_wire` feeds it from `QueryResult::elapsed`. The golden
  fixtures pin the new key with the fixed value `0.0`. `error_envelope`
  is unchanged: no statement ran.
- **C ABI** (`cypherlite-ffi` tabular API): `cypherlite_query_result`
  carries `elapsed_ms` stamped in `pack_result`, exposed as
  `double cypherlite_result_elapsed_ms(result)`; the cbindgen header is
  regenerated in the same change.
- **Python**: the native pyo3 `QueryResult` gains `elapsed_ms`, and
  `Connection._raw_execute` feeds `ResultSummary.elapsed_ms` from it
 (the [ADR-0215](./0215-python-result-summary.md) switch). The `perf_counter` client-edge measurement is
  deleted, not kept as a fallback: the native module is co-packaged in
  the same wheel, so the stamp is always present.
- **JavaScript**: `conn.query()` returns `elapsedMs`, read through the
  new FFI export while the result handle is alive.
- **CLI**: `.timer`'s `Time:` line re-sources to the engine value the
  executed statement's `QueryResult` carries; the client-edge measurement
  remains only as a fallback for statements that produce no result.
- **Bolt**: unchanged. Its one per-statement timing, `t_last` on the
  final PULL SUCCESS, is the protocol's *streaming* duration, not the
  statement's execution time; re-sourcing it would misreport Bolt
  semantics. Bolt clients read the envelope/CLI surfaces for engine
  elapsed.

## Alternatives considered

- **Time only the executor dispatch** — rejected: planning is real
  statement work a user experiences; parse+plan+run is the narrowest
  boundary that still excludes surface-side serialization. Sub-millisecond
  statements already round to `0.0` gracefully.
- **Report nanoseconds / integer microseconds** — rejected: every surface
  contract that already exists (Python `elapsed_ms`, CLI `Time: {:.3}ms`,
  Bolt `t_last`) speaks floating or integral milliseconds; `f64`
  milliseconds keeps one unit across the wire envelope, the C ABI and the
 clients, matching the [ADR-0215](./0215-python-result-summary.md) placeholder type.
- **Optional `elapsed` (`Option<Duration>`)** — rejected: the stamp is
  unconditional (one tail in `run_query`), so the field is total;
  `Duration::ZERO` seeds are overwritten before the result escapes.
- **Re-source Bolt `t_last`** — rejected: Bolt's `t_last` is the
  streaming time of the final batch by protocol convention; the engine's
  statement elapsed answers a different question and does not belong in
  that key.

## Consequences

- `cypherlite::QueryResult` gains a public field — additive, but literal
  constructions inside the workspace seed `Duration::ZERO`.
- `wire::response_envelope` gains a parameter; all call sites (binding,
  FFI/CLI golden tests, fixtures) change in the same change.
- The Python `elapsed_ms` docs' "client edge" wording (spec, description,
  examples, website reference, CLI skill bundle, bundled library skill)
  flips to engine-reported in the same change.
- [ADR-0215](./0215-python-result-summary.md)'s predicted semantic refinement lands: same property, new
  source.

## Later references

[ADR-0491](./0491-phase-timing-wire-contract.md) splits the same measured window per phase: `QueryResult` gains
`parse_ms()` / `plan_ms()` / `execute_ms()` beside `elapsed_ms()`, the
wire envelope family gains `parse_ms` / `plan_ms` / `execute_ms` beside
`elapsed_ms`, and the CLI's `Time: {:.3}ms` rendering (the alternatives
section's example) becomes `Time: <total>ms (parse <p> / plan <p> /
execute <e>)` — one-decimal milliseconds each. `elapsed_ms` itself keeps
its meaning, its conversion and its placement; the split is additive
context whose sum never exceeds the total.
