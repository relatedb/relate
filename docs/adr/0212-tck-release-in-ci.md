---
type: adr
area: infra
name: 0212-tck-release-in-ci
status: proposed
tags:
  - ci
  - conformance
title: Conformance corpora (TCK, CIP-eval, cookbook) run optimized; unit suites stay debug
REFERENCES:
  - "[[adr/0252-corpus-family-single-job]]"
---

# 0212. Conformance corpora (TCK, CIP-eval, cookbook) run optimized; unit suites stay debug

- **Later references**: [ADR-0252](./0252-corpus-family-single-job.md) supersedes the "one pre-compile job"
  mechanism below — the four corpus jobs merged into a single `corpus` job
  because the cross-job rust-cache handoff never restored warm within a
  run. The corpus/debug profile split this ADR decided is unchanged.
- **Note on numbering**: 0207 through 0211 are already taken (0207 =
  packed text trigrams on main; 0208-0211 reserved by in-flight work), so this
  decision takes 0212.

## Context

The CI `TCK (openCypher)` job and the local pre-push gate run the pinned
3892-scenario corpus through a debug-profile engine. Debug is 10-30x slower
than release for this workload, which makes the corpus the longest step in
the `Test (windows-latest)` / `Test (ubuntu-latest)` matrix jobs (14-26 min
observed) — the wall time every PR waits on. The graph-examples differential
suite already runs `--release` for exactly this reason.

Debug profile is also where `debug_assert!` contracts fire (overflow checks,
width bounds, internal invariants). Those belong to the unit suites next to
the code — the executor/storage/CLI suites in the Test matrix — which are
fast (the executor suite is ~30 s in debug) and are the actual home of the
asserts.

Release also exercises optimization-dependent behavior that debug masks
(inlining, autovectorization, UB timing). Running the corpus in release adds
that coverage; debug never sees it.

## Decision

Split test profiles by suite kind, in CI and in the local pre-push gate:

- **Conformance corpora in the `corpus` profile**: a custom Cargo profile
  (`release` at `opt-level = 2`, no debug info — compiles noticeably faster,
  runs within ~1-5% of release) for `cargo test --profile corpus -p
  cypherlite-tck`, `-p cypherlite-cip-eval` (CI job), and `-p
  cypherlite-cookbook` (`make cookbook-native`). The graph-examples
  differential already runs release. Perf baselines (loadtests) keep the
  pinned docker release build, unaffected.
- **One pre-compile job**: a `corpus-build` job compiles the three corpus
  jobs' test binaries once (`cargo test --profile corpus --no-run`) and
  populates a shared rust-cache key; the corpus jobs need it and restore
  warm, so the optimized compile is paid once per PR, not three times.
- **Unit suites stay debug**: the Test matrix steps for executor, storage,
  CLI bundles, and the other crates keep the default profile, preserving the
  `debug_assert!` coverage in full.
- **On a release failure**: re-run in debug locally, scoped to the failing
  feature (`TCK_ONLY=FeatureName`), for assert detail and diagnosis. A
  release failure is always a real behavioral failure; debug is for
  diagnosis, not for the green path.

## Alternatives considered

- **Everything in release** — rejected: it silently stops checking every
  `debug_assert!` contract in CI; the cheap unit suites are where those live.
- **Everything in debug (status quo)** — rejected: the corpus wall time is
  the merge gate's long pole (14-26 min on windows) and debug never
  exercises optimization-dependent paths.
- **A separate debug-TCK nightly** — rejected for now: the unit suites
  already cover the debug-assert surface; revisit only if a corpus scenario
  is found that passes in release and fails only on a debug assert.

## Consequences

- The TCK job and the windows/ubuntu Test matrix jobs get materially faster
  (the corpus is ÷10-30; compile adds ~2-4 min with rust-cache). Before/
  after job times are recorded on the landing PR.
- Release-only behavioral failures (optimization-dependent) now surface in
  CI instead of only in the release-mode differential.
- Timing-sensitive tests may flake differently under release timing; the
  first runs after this change are watched, and any new flake is triaged as
  its own issue rather than reverted wholesale.
- The local pre-push gate recipe changes accordingly: unit suites in debug,
  corpora in release, debug re-run scoped to failures.
