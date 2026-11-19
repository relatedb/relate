---
type: adr
area: infra
date: 2026-09-18
deciders: CypherLite / MemoryDB merge
name: 0062-ci-loadtest-band-calibration
status: accepted
tags:
  - ci
  - loadtest
title: CI loadtest band calibration to runner variance
REFERENCES:
  - "[[adr/0046-cypherlite-parser-memorydb-runtime]]"
  - "[[adr/0060-identity-loadtest-baseline-refreeze]]"
  - "[[adr/0061-ci-host-loadtest-baselines]]"
  - "[[adr/0064-ci-p99-jitter-allowance]]"
  - "[[adr/0070-docker-baseline-loadtest-environment]]"
  - "[[adr/0071-ci-owned-loadtest-freeze-fingerprint]]"
---

# 0062. CI loadtest band calibration to runner variance

## Context

[ADR-0061](./0061-ci-host-loadtest-baselines.md) split the loadtest gate into two host classes: repo-root
`baselines/` for the local reference host (`make gate`, strict [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md)
bands) and `baselines/ci/` for `ubuntu-latest` GitHub runners, seeded from
runner artifacts. The CI seeding worked mechanically — the
`loadtest-baselines` job compared fresh runs against `baselines/ci` as
designed — but the [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) point bands (ops/s ≥ 90% of baseline, p99 ≤
max(115% of baseline, baseline + 250,000 ns)) were carried over
uncalibrated, and they are wrong for that host class.

Measured on PR #13, run 35344807763 ("Loadtest baselines gate",
ubuntu-latest), on code identical to the run that produced the seed:

- fraud throughput: 2,501.25 ops/s vs the seeded 3,276.54 — a 0.76x ratio,
  under the 0.90 floor of 2,948.9. Two consecutive CI runs of the same code
  measured 3,276 then 2,501 ops/s: a 24% run-to-run throughput swing.
- p99 latency: worst observed ratio 3.8x baseline (fraud `p2p_transfer`
  172 ms vs 45 ms seeded); 7 fraud use cases plus 1 identity use case
  missed the p99 band.
- `errors == 0` held on every use case in both runs.

So GitHub-hosted runner variance exceeds the [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) point bands by a wide
margin: a healthy, zero-error run of identical code fails the CI gate.
[ADR-0060](./0060-identity-loadtest-baseline-refreeze.md) established that loadtest numbers are host- and condition-specific;
this ADR is the same lesson one level down — on a noisy shared host class,
the *bands* are host-class-specific too, not just the frozen numbers.

## Decision

Keep the two-host-class structure of [ADR-0061](./0061-ci-host-loadtest-baselines.md). Calibrate the compare bands
for the CI host class only, via env vars on the `Compare against CI
baselines` step of the `loadtest-baselines` job in
`.github/workflows/ci.yml` (`scripts/compare-loadtest.py` already honors
them; no script changes):

- `LOADTEST_MIN_THROUGHPUT_RATIO=0.65` — worst observed healthy ratio was
  0.76x (2,501.25 / 3,276.54); the floor sits below that with headroom, and
  still fails any run that loses more than ~35% of seeded throughput.
- `LOADTEST_MAX_P99_RATIO=4.0` — worst observed healthy ratio was 3.8x; the
  250,000 ns jitter allowance (`LOADTEST_P99_JITTER_NS`) is retained.
- Unchanged: `LOADTEST_MAX_LOGICAL_BYTES_RATIO=1.10`,
  `LOADTEST_MIN_OPS_FOR_P99=8`, and `errors == 0`, which stays absolute on
  every host.

The strict [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) bands remain the script defaults and stay in force for
the local reference-host gate (`make gate`, root `baselines/`): CI catches
catastrophic regressions on a noisy shared host, local catches drift on a
controlled host.

## Alternatives considered

- **Re-seed `baselines/ci` until a green compare** — rejected: with a 24%
  run-to-run throughput swing and up to 3.8x p99 swings on identical code,
  any point seed is stale next run; re-seeding is a coin flip, not a fix.
- **Mark the gate `continue-on-error` (informational only)** — rejected:
  gates that don't fail get ignored; an always-green informational check
  carries no signal and trains everyone to ignore it.
- **Drop the performance compare in CI, keep smoke (`errors == 0`)** —
  rejected: a calibrated band still catches 2–5x blowups; smoke catches
  none of them. Losing the PR-path perf signal entirely was already
 rejected in [ADR-0061](./0061-ci-host-loadtest-baselines.md) and the measurement here does not change that.

## Consequences

- The CI gate now passes healthy runs with up to ~35% throughput loss or
  ~4x p99 inflation against the CI seed, and still fails catastrophic
  regressions (e.g. 50% throughput, 5x p99) and any non-zero error count.
- **Re-calibration procedure**: if runner variance grows and healthy runs
  flake at these bands, gather the failing run's `loadtest-reports`
  artifact numbers against `baselines/ci`, quote them, and adjust the two
  env values under a new ADR — the same measure-then-amend pattern as
 [ADR-0060](./0060-identity-loadtest-baseline-refreeze.md)/[ADR-0061](./0061-ci-host-loadtest-baselines.md). Do not loosen bands without measured justification.
- The local reference-host gate is unchanged: `make gate` against root
 `baselines/` keeps the strict [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) bands (script defaults), so drift
  that CI can no longer see is still caught locally on the controlled host.
- This amends the CI-host-class band contract of [ADR-0061](./0061-ci-host-loadtest-baselines.md) (which carried
 the [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) bands to CI uncalibrated) — see its *Later references*. The
 [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) bands themselves are untouched.

## Later references

- [ADR-0064](./0064-ci-p99-jitter-allowance.md): the 250,000 ns absolute p99 jitter allowance this ADR retained
  on the CI host class proved to sit below the `ubuntu-latest` noise floor
  (identity `clear_expiry` p99 of 2,504,394 ns and 2,087,662 ns vs a
  121,508 ns seed on healthy code, PR #13 run 35378653363 and its re-run).
  The `loadtest-baselines` compare step now also sets
  `LOADTEST_P99_JITTER_NS=5000000`. The 0.65 throughput floor and 4.0x p99
  ratio calibrated here are unchanged.
- [ADR-0070](./0070-docker-baseline-loadtest-environment.md): CI-only band overrides are withdrawn. The `loadtest-baselines`
  job compares dockerized reports against root `baselines/` using the
 [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) script defaults.
- [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md): freeze authority and an `environment` fingerprint replace
  CI-only ratio widening as the response to host mismatch.
