---
type: adr
area: infra
name: 0294-tck-gate-under-nextest
status: accepted
tags:
  - ci
  - conformance
title: the TCK corpus gate runs under cargo-nextest after all
REFERENCES:
  - "[[adr/0254-cargo-nextest-test-execution]]"
SUPERSEDES:
  - "[[adr/0254-cargo-nextest-test-execution]]"
---

# ADR-0294: the TCK corpus gate runs under cargo-nextest after all

Renumbered: from 0256 (shared-number cleanup)

## Context

[ADR-0254](./0254-cargo-nextest-test-execution.md) moved the unit lanes to `cargo nextest run` but kept the TCK
corpus gate (`cargo test --profile corpus -p cypherlite-tck`) on stock
cargo test, reasoning that nextest's per-test process model "does not map
onto one process that must see the whole corpus."

That reasoning misread the harness shape. The gate is not one `#[test]`
whose accounting spans a whole-binary run: `tck_integration.rs` carries
three self-contained gate tests (`test_tck_clauses`,
`test_tck_expressions`, `test_tck_full_summary`), each of which runs its
own corpus pass through `run_tck_directory` and asserts its own counts —
`test_tck_full_summary` alone asserts the 3892 denominator, the pinned
passing count, the documented-skip set, and allowlist equality over the
full corpus it just ran. Under stock cargo test these tests already run
concurrently (cargo test parallelizes *within* a binary by default), so
"one process sees the whole corpus" was never the mechanism; each gate
test sees the whole corpus in its own big-stack worker thread. Nextest's
per-process scheduling therefore preserves the accounting exactly.

Local pilot on the LABS-310 round-4 branch (2026-10-15, Apple silicon):
`cargo nextest run --cargo-profile corpus -p cypherlite-tck` passes
**55/55 in 3.2 s**, including `test_tck_full_summary` — the same
pass/skip counts as the stock run (3892 pass, 1 documented skip of 3893).

## Decision

The corpus job's TCK step runs `cargo nextest run --cargo-profile corpus
-p cypherlite-tck`. The nextest default profile (`retries = 1`,
[ADR-0254](./0254-cargo-nextest-test-execution.md)) applies to the TCK package without an override: the gate
asserts are deterministic, so a retry only re-fails an already-red lane —
it cannot mask nondeterminism the way the bolt concurrency suite's
real-time asserts could. The other corpus steps (`cypherlite-cip-eval`,
`cypherlite-cookbook`, `cypherlite-query-latency`) stay on stock cargo
test: single-binary suites with no inter-binary serialization to remove.
Doctests keep their stock `cargo test --doc` steps everywhere (nextest
cannot see them), unchanged from [ADR-0254](./0254-cargo-nextest-test-execution.md).

## Alternatives considered

- **Leave the TCK step on stock cargo test.** Rejected: the round-2
  rationale was factually wrong about the harness, and runner uniformity
  matters — one execution tool per lane family removes the "which runner
  semantics apply here" question and gives the TCK nextest's per-test
  timeout/slow-timeout reporting for free.
- **`retries = 0` override for `package(cypherlite-tck)`.** Rejected: the
  gate is deterministic; a retry there re-fails honestly and the summary
  marks FLAKY runs visibly. Stock cargo test parity (zero retries) buys
  nothing for a deterministic suite.
- **Move CIP-eval / cookbook / query-latency to nextest too.** Rejected:
  each is one integration binary whose result is a single gate test; there
  is nothing to schedule and the corpus build step already fans out
  compile-wise.

## Consequences

- The corpus job gains a `taiki-e/install-action` nextest install (~2 s).
- [ADR-0254](./0254-cargo-nextest-test-execution.md)'s "the TCK gate stays on stock cargo test" clause is superseded
  (Later-references note added there); its other scope decisions stand.
- The 3892/3893 gate semantics are unchanged: same harness, same asserts,
  same allowlist — only the process scheduler around the gate tests
  changed. `TCK_ONLY` debugging runs are unaffected (nextest forwards the
  env).
