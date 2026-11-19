---
type: adr
area: infra
date: 2026-09-18
deciders: CypherLite / MemoryDB merge
name: 0064-ci-p99-jitter-allowance
status: accepted
tags:
  - ci
  - loadtest
title: CI gate absolute p99 jitter allowance raised to runner reality
REFERENCES:
  - "[[adr/0046-cypherlite-parser-memorydb-runtime]]"
  - "[[adr/0048-identity-loadtest-memory-budget]]"
  - "[[adr/0060-identity-loadtest-baseline-refreeze]]"
  - "[[adr/0061-ci-host-loadtest-baselines]]"
  - "[[adr/0062-ci-loadtest-band-calibration]]"
  - "[[adr/0063-lazy-string-index-postings]]"
  - "[[adr/0070-docker-baseline-loadtest-environment]]"
  - "[[adr/0071-ci-owned-loadtest-freeze-fingerprint]]"
---

# 0064. CI gate absolute p99 jitter allowance raised to runner reality

## Context

[ADR-0062](./0062-ci-loadtest-band-calibration.md) calibrated the CI host class's loadtest bands
(`LOADTEST_MIN_THROUGHPUT_RATIO=0.65`, `LOADTEST_MAX_P99_RATIO=4.0`) but
retained [ADR-0048](./0048-identity-loadtest-memory-budget.md)'s 250,000 ns absolute p99 jitter allowance
(`LOADTEST_P99_JITTER_NS`), which was calibrated on the local reference
host. On PR #13, run 35378653363 ("Loadtest baselines gate",
ubuntu-latest) **and** its re-run both failed on the same check:

- identity use case `clear_expiry` p99: **2,504,394 ns**, then on the
  re-run **2,087,662 ns**, vs the `baselines/ci` seed's **121,508 ns** —
  17–20x on a sub-millisecond-baseline WRITE use case, twice in a row. The
  documented flake recourse (re-run the job, `baselines/README.md`) was
  spent and failed identically.
- Everything else in both runs was green: the fraud leg passed both times,
  throughput was fine (fraud 3,434 vs seeded 3,276 ops/s; identity 2,904
  vs seeded 2,234 ops/s — both *above* seed), `errors == 0` everywhere,
  and `Test (windows-latest)` passed.

The failure is exonerated as host noise, not a code regression:

- `clear_expiry` is
  `MATCH (u:User {id: $uid})-[g:HAS_ROLE]->() WHERE g.until IS NOT NULL REMOVE g.until`
  — an equality seek plus a relationship-property removal. It never
  touches prefix/trigram postings, so the lazy-build mechanism of
 [ADR-0063](./0063-lazy-string-index-postings.md) (node 52, the last change to land before the failures) cannot
  be on its path.
- The remaining hypothesis — a lazy-build stall inside some measured
 window — was tested and killed: local HEAD (post-[ADR-0063](./0063-lazy-string-index-postings.md)) measures
  `clear_expiry` p99 = **131,958 ns** = 1.09x of the CI seed's
  121,508 ns. No regression on a quiet host.

Conclusion: shared-runner scheduler stalls produce multi-millisecond p99
outliers on sub-millisecond use cases. The 250,000 ns absolute allowance
([ADR-0048](./0048-identity-loadtest-memory-budget.md), calibrated on the local host) sits below the CI host class's
noise floor, so a healthy, zero-error, above-seed-throughput run fails the
gate deterministically.

## Decision

Raise the absolute p99 jitter allowance for the **CI host class only**.
The `Compare against CI baselines` step of the `loadtest-baselines` job in
`.github/workflows/ci.yml` gains `LOADTEST_P99_JITTER_NS: "5000000"`
(5 ms) alongside the existing [ADR-0062](./0062-ci-loadtest-band-calibration.md) env, with a comment citing this
ADR. `scripts/compare-loadtest.py` already honors the variable; no script
changes. The effective p99 limit on that job becomes
`max(4.0 * baseline, baseline + 5,000,000 ns)` per use case.

Rationale for 5 ms:

- Worst measured CI stall on healthy code: 2.5 ms (run 35378653363).
  5 ms is 2x headroom over that observation, the same
 measure-worst-then-double pattern [ADR-0062](./0062-ci-loadtest-band-calibration.md) applied to the ratio bands.
- Use cases with millisecond-scale baselines stay governed by the 4.0x
  ratio: the absolute allowance dominates only where baseline p99 <
  ~1.67 ms (`baseline + 5 ms > 4 * baseline`), i.e. exactly the sub-ms
  class where a fixed scheduler stall dwarfs the baseline.
- The gate keeps its regression teeth: `errors == 0` stays absolute, the
  0.65 throughput floor still fails runs that lose >~35% of seeded ops/s,
  and a sub-ms use case must still exceed baseline + 5 ms to pass — a
  genuine 6 ms+ regression on a 0.12 ms baseline still fails.

The local reference-host gate is untouched: `make gate` against root
`baselines/` keeps the strict [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md)/[ADR-0048](./0048-identity-loadtest-memory-budget.md) bands (script defaults,
250,000 ns allowance).

## Alternatives considered

- **Per-use-case warmup in the loadtest harness** — rejected: the failing
  use case never touches the lazy postings (the only first-touch cost
 introduced by [ADR-0063](./0063-lazy-string-index-postings.md)), so warmup has nothing to warm on its path; and
  warmup cannot fix a scheduler stall, which strikes at arbitrary points
  in the measured window regardless of prior execution.
- **Widen the p99 *ratio* further (e.g. to 20x)** — rejected: absorbing a
  20x outlier on a 0.12 ms baseline needs a ratio of 20+, which guts the
  guard for ms-scale use cases (a 45 ms fraud use case would then need
  900 ms to fail). An absolute allowance targets exactly the sub-ms class
  and leaves the ms-scale ratio band intact.
- **Keep re-running on flakes** — rejected: the re-run recourse was spent
  on run 35378653363 and failed identically (2.50 ms then 2.09 ms),
  proving a deterministic host-noise class, not luck. Re-rolling the dice
  burns CI minutes and merge latency against a known noise floor.

## Consequences

- The CI gate's regression teeth now live in `errors == 0` (absolute,
  every host), the 0.65 throughput floor, and the 4.0x p99 ratio (which
  governs every use case with baseline p99 ≳ 1.67 ms). The 5 ms absolute
  allowance only relaxes the sub-ms class, where CI scheduler noise, not
  code, dominates the measurement.
- Both recorded failures pass under the new band: 2,504,394 ns and
  2,087,662 ns are each below 121,508 + 5,000,000 = 5,121,508 ns.
- **Re-calibration path**: if runners get noisier and healthy runs fail
  beyond the 5 ms allowance, gather the failing run's `loadtest-reports`
  artifact numbers against `baselines/ci`, quote them, and adjust the env
  value under a new ADR — the same measure-then-amend pattern as
 [ADR-0060](./0060-identity-loadtest-baseline-refreeze.md) / [ADR-0061](./0061-ci-host-loadtest-baselines.md) / [ADR-0062](./0062-ci-loadtest-band-calibration.md). Do not loosen without measured
  justification.
- This amends the CI-host-class band contract of [ADR-0062](./0062-ci-loadtest-band-calibration.md) (which retained
 [ADR-0048](./0048-identity-loadtest-memory-budget.md)'s 250,000 ns allowance on CI) — see its *Later references*.
 [ADR-0048](./0048-identity-loadtest-memory-budget.md)'s allowance itself is untouched: it stays the script default
  and the local-gate band.

## Later references

- [ADR-0070](./0070-docker-baseline-loadtest-environment.md): CI-only p99 jitter and the two-host-class layout are withdrawn.
  Dockerized fraud/identity/checkpoint runs compare against root
 `baselines/` with the [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md)/[ADR-0048](./0048-identity-loadtest-memory-budget.md) script defaults.
- [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md): `LOADTEST_P99_JITTER_NS=5000000` still applies on a **matched**
  environment class. A class mismatch is `INCOMPATIBLE`, not a reason to
  widen bands.
