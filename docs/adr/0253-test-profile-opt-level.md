---
type: adr
area: infra
name: 0253-test-profile-opt-level
status: accepted
tags:
  - ci
  - testing
title: Unit and integration tests compile at `[profile.test] opt-level = 1`
REFERENCES:
  - "[[adr/0212-tck-release-in-ci]]"
  - "[[adr/0251-test-architecture]]"
  - "[[adr/0252-corpus-family-single-job]]"
---

# ADR-0253: Unit and integration tests compile at `[profile.test] opt-level = 1`

## Context

[ADR-0251](./0251-test-architecture.md) set the ≤ 60 s ambition for continuous suites and made compile-vs-
execution balance an explicit design term. The profile-selection measurement
matrix (recorded in `docs/testing/runtime.md`) shows the default test
profile spends its wall time very unevenly: the exec-heavy suites
(`cypherlite` ~200 s exec, `cypherlite-datasets` ~63 s, `cypherlite-storage`
~49 s, `cypherlite-executor` ~40 s, warm local laptop) run unoptimized code,
while their incremental compile is tens of seconds. The same matrix shows an
opt-level-1 test profile cuts those exec times 3-15x (`cypherlite` 200 s →
14 s, executor 40 s → 7 s, datasets 63 s → 27 s, storage 49 s → 37 s) for
single-digit-to-tens-of-seconds of extra compile per suite, and that
opt-level 2 buys nothing further while compiling noticeably slower.

The corpus family (TCK, CIP-eval, cookbook, query latency) already runs in
the optimized `corpus` profile ([ADR-0212](./0212-tck-release-in-ci.md), [ADR-0252](./0252-corpus-family-single-job.md)). The unit/integration
suites were deliberately kept in the dev family because that is where
`debug_assert!` contracts and overflow checks are evaluated (ci.yml `test`
job comment); moving them into the corpus profile would silently drop that
assertion coverage.

One calibration hazard surfaced during measurement: three cancellation
integration tests sized their workloads so the un-cancelled query takes
> 80 ms *in the dev profile*; under any optimized profile they finish too
fast to prove cancellation and fail.

## Decision

- `[profile.test] opt-level = 1` in the workspace `Cargo.toml`. Every
  `cargo test` (dev-family) invocation — local and CI — compiles test code
  and its dependencies at opt-level 1. `debug-assertions` and
  `overflow-checks` keep their dev-inherited defaults (**on**), so the
  assertion coverage that motivated keeping unit suites in the dev family is
  unchanged.
- The corpus family keeps the `corpus` profile ([ADR-0212](./0212-tck-release-in-ci.md), [ADR-0252](./0252-corpus-family-single-job.md));
  `release` is not used for tests (the matrix shows it dominated by
  `corpus`).
- Profile-sensitive timing tests must not assume the dev profile: the three
  cancellation tests in `cypherlite/cypherlite/tests/cancellation.rs`
  escalate their workload until the un-cancelled control provably outlasts
  the cancel latency, instead of asserting a fixed-size workload is slow
  enough. This is the pattern for any future test that calibrates against
  wall-clock engine speed.

## Alternatives considered

- **Move the exec-heavy suites into the `corpus` CI job:** rejected — the
  corpus profile disables `debug_assert!` and overflow checks, and the
  `test` job's unit suites are exactly where those contracts are checked;
  moving them would trade assertion coverage for wall time.
- **`[profile.test] opt-level = 2`:** rejected — the matrix shows exec times
  statistically identical to opt-level 1 (storage and the cancellation
  control run *slower*) for ~25-45% more compile time per suite.
- **Per-package overrides
  (`[profile.test.package.cypherlite-executor] opt-level = 1`, …):**
  rejected — the exec time lives in the engine crates, which every exec-heavy
  suite links, so the overrides converge on optimizing the same set of
  crates the profile-level knob already covers, with a longer, drift-prone
  list.
- **Keep the dev test profile at opt-level 0 (status quo):** rejected — it
  leaves a 5-15x exec win on the table for the four suites that dominate
  local `cargo test` wall time, against single-digit seconds of compile for
  the cheap suites.

## Consequences

- Local full-suite wall time drops substantially (measured: the six-suite
  sweep's total 428 s → 224 s warm; the `cypherlite` suite alone
  225 s → 55 s).
- Cold compiles of the test profile cost more (opt-level 1 codegen);
  locally the extra per-suite compile is 0-20 s warm. CI `test`-family jobs
 share the `dev` rust-cache ([ADR-0251](./0251-test-architecture.md) §5), so the increase is paid on
  Cargo.lock changes, not per PR.
- Test-code behavior is unchanged: same assertions, same checks, plus the
  cancellation tests now pass at any opt-level instead of encoding the dev
  profile's speed.
- Timing-calibrated tests are profile-agnostic by construction going
  forward; a test that asserts a workload is "slow enough" must calibrate
  adaptively (see `cancellation.rs`).
