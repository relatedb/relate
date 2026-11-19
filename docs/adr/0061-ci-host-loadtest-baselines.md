---
type: adr
area: infra
date: 2026-09-18
deciders: CypherLite / MemoryDB merge
name: 0061-ci-host-loadtest-baselines
status: accepted
tags:
  - ci
  - loadtest
title: CI-host loadtest baselines (two host classes)
REFERENCES:
  - "[[adr/0046-cypherlite-parser-memorydb-runtime]]"
  - "[[adr/0048-identity-loadtest-memory-budget]]"
  - "[[adr/0060-identity-loadtest-baseline-refreeze]]"
  - "[[adr/0062-ci-loadtest-band-calibration]]"
  - "[[adr/0070-docker-baseline-loadtest-environment]]"
  - "[[adr/0071-ci-owned-loadtest-freeze-fingerprint]]"
---

# 0061. CI-host loadtest baselines (two host classes)

## Context

[ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) froze loadtest summaries under `baselines/` as a merge gate with
the bands `errors == 0`, ops/s ≥ 90% of baseline, p99 ≤ max(115% of
baseline, baseline + 250,000 ns), and logical bytes ≤ 110%. [ADR-0048](./0048-identity-loadtest-memory-budget.md)
re-froze the identity baseline at corrected knobs (8 workers, `:memory:`,
40 MiB). [ADR-0060](./0060-identity-loadtest-baseline-refreeze.md) re-froze it again after a position-controlled A/B proved
the gate host could not reproduce an overnight-idle baseline in daytime —
establishing that loadtest baselines are host- and condition-specific, and
that moving the gate to a different host class repeats the
measure-then-re-freeze pattern under a new ADR.

The CI `loadtest-baselines` job nonetheless compared fresh `ubuntu-latest`
runs against those local-host-frozen root baselines. On PR #13 (run
35336551067, "Loadtest baselines gate", ubuntu-latest) fraud measured
3,551.14 ops/s — 58% of the Mac-frozen 6,079.03 baseline, far under the
5,471 floor — with 12 p99 misses across use cases, while the zero-error leg
held ("Loadtest smoke" passed). The [ADR-0060](./0060-identity-loadtest-baseline-refreeze.md) re-freeze made `baselines/`
honest for the local dev host in daytime; those numbers were never valid
for CI runners, so the CI gate was deterministically red on healthy code.

## Decision

Two host classes, one band contract.

1. **Local reference host (unchanged).** Repo-root `baselines/` remains the
   `make gate` / `make compare-loadtests` comparison set, frozen from the
 local dev host ([ADR-0060](./0060-identity-loadtest-baseline-refreeze.md)). The local gate does not change.
2. **CI host class.** The `loadtest-baselines` job in
   `.github/workflows/ci.yml` uploads the fresh `target/fraud-loadtest.json`
   and `target/identity-loadtest.json` reports as a `loadtest-reports`
   workflow artifact with `if: always()`, then compares against
   `baselines/ci/` (`make compare-loadtests BASELINES=baselines/ci`). The
 [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) bands are unchanged (ops/s ≥ 90%, p99 ≤ 115% + 250,000 ns,
   logical bytes ≤ 110%); `errors == 0` stays absolute on every host.
3. **Bootstrap rule.** When `baselines/ci/fraud-loadtest.json` is absent,
   the job still runs both loadtests and uploads the artifacts, prints a
   greppable `seeding run — compare skipped` line, and exits 0. Seeding is:
   download the artifacts from a green run, summarize with
   `scripts/summarize-loadtest.py`, commit `baselines/ci/*.json`.

## Alternatives considered

- **Status quo — compare CI runs against the local-host baselines** —
  rejected: deterministically red (fraud at 58% of the throughput floor on
  healthy, zero-error code). A gate that always fails carries no signal and
  trains everyone to ignore red.
- **Looser ratio knobs for CI (e.g. `LOADTEST_MIN_THROUGHPUT_RATIO=0.5`)
  against the Mac baselines without re-freezing** — rejected: an
  uncalibrated band is a gate that cannot fail. The looser ratio would be
  guessed, not measured, and would silently absorb real regressions of up
  to that margin.
- **Drop the CI performance gate and keep smoke-only (`errors == 0`)** —
  rejected: CI is where regressions are cheapest to catch. Smoke-only would
  make throughput and latency drift on the PR path invisible until someone
  happened to run the local gate.

## Consequences

- **Seeding procedure** (once, after this lands): download the
  `loadtest-reports` artifact from the first green run on the merge branch,
  run `scripts/summarize-loadtest.py <report> baselines/ci/<name>.json` for
  fraud and identity, and commit `baselines/ci/*.json`. From then on the CI
 gate compares with the unchanged [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) bands.
- **Re-seed procedure**: when the CI host class changes (runner image or
  CPU class), or the report schema / gate knobs change, regenerate
  `baselines/ci/*.json` from a fresh run's artifacts under a new ADR — the
 same measure-then-re-freeze pattern [ADR-0060](./0060-identity-loadtest-baseline-refreeze.md) established for the local
  host.
- **Flake recourse** on the CI gate is a re-run of the job. If flakes recur
 systematically, adjudicate with [ADR-0060](./0060-identity-loadtest-baseline-refreeze.md)'s interleaved,
  position-controlled A/B protocol and amend under a new ADR.
- `baselines/README.md` and `docs/datasets.md` document the two-directory
  layout and the honored knobs (`BASELINES`, `LOADTEST_*`).
- This amends the comparison set of [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md)'s loadtest gate (Decision item
  4) for the CI host class only — see its *Later references*. It is the
 host-class move [ADR-0060](./0060-identity-loadtest-baseline-refreeze.md) predicted — see its *Later references*.

## Later references

- [ADR-0062](./0062-ci-loadtest-band-calibration.md): the [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) point bands this ADR carried to the CI host class
  (ops/s ≥ 90%, p99 ≤ 115% + 250,000 ns) proved tighter than measured
  `ubuntu-latest` runner variance (0.76x throughput, 3.8x p99 on identical
  code). The `loadtest-baselines` job now sets
  `LOADTEST_MIN_THROUGHPUT_RATIO=0.65` / `LOADTEST_MAX_P99_RATIO=4.0` on the
  compare step. The two-host-class structure and the strict local gate
  decided here are unchanged.
- [ADR-0070](./0070-docker-baseline-loadtest-environment.md): the two-host-class layout is superseded. Fraud, identity, and
  checkpoint loadtests freeze and compare in a Docker baseline environment
  (compile in `rust:1.88-bookworm`, run in `debian:bookworm-slim` with
  2 vCPU / 2 GB). Repo-root `baselines/` is the only comparison set;
  `baselines/ci/` is withdrawn.
- [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md): committed envelopes are produced by CI (`ubuntu-24.04`),
  stamped with an `environment` class. Mismatch is incompatible, not a
  second baseline directory.
