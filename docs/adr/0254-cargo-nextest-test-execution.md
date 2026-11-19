---
type: adr
area: infra
name: 0254-cargo-nextest-test-execution
status: accepted
tags:
  - ci
  - testing
title: cargo-nextest executes the unit-lane test suites; the corpus gate stays on stock cargo test
REFERENCES:
  - "[[adr/0251-test-architecture]]"
  - "[[adr/0252-corpus-family-single-job]]"
  - "[[adr/0253-test-profile-opt-level]]"
  - "[[adr/0255-mold-linker-linux-ci]]"
  - "[[adr/0294-tck-gate-under-nextest]]"
---

# ADR-0254: cargo-nextest executes the unit-lane test suites; the corpus gate stays on stock cargo test

## Context

[ADR-0251](./0251-test-architecture.md) set the ≤ 60 s ambition for continuous suites and [ADR-0253](./0253-test-profile-opt-level.md) cut the
execution side with an opt-level-1 test profile. What remains on the
critical path of the `test` job (the slowest continuous lane at ~10–11 min
on the 4-core pilot runner) is structural to how `cargo test` executes: one
`cargo test -p A -p B …` invocation compiles every test binary and then runs
them **one binary at a time**. The workspace ships ~17 core crates, each
with a lib suite plus several integration binaries, so the run phase is a
serial procession of dozens of process exits, most of which finish in well
under a second. On a 4-core runner the CPU sits mostly idle between
binaries while the slowest suite gates the tail.

`cargo nextest` replaces exactly this execution model: it enumerates every
test binary, lists their tests, and schedules individual tests across a
process pool sized to the machine. It also adds per-test timeouts, retries
for flaky families, and JUnit-style reporting. Compilation is unchanged
(nextest shells out to `cargo test --no-run` semantics and reuses the same
artifacts, so the shared rust-cache contract of [ADR-0251](./0251-test-architecture.md) §5 is unaffected).

Two suites must NOT move to nextest:

- **The TCK corpus gate** (`cargo test --profile corpus -p cypherlite-tck`)
  is a single test binary whose 3893 scenarios run inside one process on a
  big-stack worker thread, and whose pass/skip accounting is a summary gate
 the harness computes over its own run ([ADR-0252](./0252-corpus-family-single-job.md)). Parallelizing *within*
  that binary is the harness's own design decision, not the runner's, and
  nextest's per-test process model does not map onto one process that must
  see the whole corpus. It stays on stock `cargo test`.
- **Doctests** are not visible to nextest at all (nextest runs test
  binaries; doctests are compiled ad hoc by rustdoc). They keep a separate
  `cargo test --doc` step over the same package set so no example loses CI
  coverage (AGENTS.md §4: every public API example runs in CI).

The bolt concurrency suite (`cypherlite-bolt`) is the known timing-sensitive
family: its tests assert on real socket concurrency and deadline behavior,
and a transparent retry could turn a genuine determinism regression into a
green run. Retries are therefore disabled for that package; other packages
get one retry as flake insurance.

This is the execution half of the CI runtime follow-up round;
the linker half is [ADR-0255](./0255-mold-linker-linux-ci.md).

## Decision

1. The `test` job's core-crate step, the `feature-bundles` job, and the
   `cypherlite-bolt` job's `cargo test -p cypherlite-bolt` step run under
   `cargo nextest run`, installed as a prebuilt binary via
   `taiki-e/install-action` (no `cargo install` compile on the merge path).
2. `.config/nextest.toml` carries the default profile: `retries = 1`
   globally, with a `[[profile.default.overrides]]` entry setting
   `retries = 0` for `package(cypherlite-bolt)` so the concurrency suite
   keeps its fail-on-first-flake contract, and a `slow-timeout` that marks
   (not kills) tests over 60 s.
3. The `test` job gains a `cargo test --doc` step over the same package
   list so doctests keep running (nextest cannot see them).
4. The corpus job (`cypherlite-tck`, `cypherlite-cip-eval`,
   `cypherlite-cookbook`, `cypherlite-query-latency`) and every other
   `cargo test` invocation in CI stay on stock cargo test: the TCK summary
   gate counts a single in-process run, and the remaining lanes (stack
   release suite, datasets, aura, python) are single-binary suites where
   nextest buys nothing.
5. Local runs are unchanged: `cargo test` keeps working; nextest is an
   additional tool, documented in `docs/testing/runtime.md`, never the
   only way to run a suite.

## Alternatives considered

- **`cargo test` with `--test-threads` raised.** Already default-parallel
  *within* one binary; the serial cost is *between* binaries, which no
  cargo flag changes.
- **Splitting the test job into per-crate matrix jobs instead.** Rejected
  for now: each shard repays checkout/toolchain/cache-restore overhead,
  and two shards of the `dev` shared-key cache contend on the cache save
  (only one writer wins, so one shard always recompiles what the other
 built). Re-evaluate with the [ADR-0255](./0255-mold-linker-linux-ci.md) timings data if the critical path
  is still compile-tail-bound after this lands; the investigation is
  recorded in `docs/testing/runtime.md`.
- **Nextest everywhere, including the corpus gate.** Rejected: the TCK
  harness is one binary with an internal big-stack worker and a whole-corpus
  summary gate; per-test process scheduling breaks both its accounting and
  its worker model.
- **Retries = 0 globally.** Rejected: the workspace has no other known
  flaky family, but one retry is cheap insurance against runner-scheduler
  noise on shared CI hardware; the one family where a retry could mask a
  real contract (bolt concurrency) is explicitly excluded.

## Consequences

- Test binaries within the covered lanes execute in parallel across all
  cores; the serial per-binary procession is gone. Measured before/after
  numbers live in `docs/testing/runtime.md`.
- Test *output* changes shape (nextest's per-test status instead of the
  libtest harness summary). Failure diagnosis is via `cargo nextest run`
  locally; stock `cargo test` remains available everywhere.
- A failing-then-passing test is visible in the nextest summary as FLAKY
  rather than silently green, except in `cypherlite-bolt` where the first
  failure fails the run.
- CI installs one more prebuilt tool per covered lane (~2–5 s); the
  `taiki-e/install-action` dependency joins the workflow supply chain
 (already used for mold, [ADR-0255](./0255-mold-linker-linux-ci.md)).
- `cargo test --doc` becomes a distinct step; a doctest failure no longer
  fails the same step as unit failures, which makes its signal clearer.

## Later references

- [ADR-0294](./0294-tck-gate-under-nextest.md) round 4) supersedes the "TCK corpus gate stays on
  stock cargo test" clause: the gate tests are self-contained (each
  asserts the denominator over its own full corpus run), so per-process
  scheduling preserves the 3892/3893 accounting. The TCK step runs under
  nextest; every other exclusion here stands.
