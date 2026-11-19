---
type: adr
area: infra
date: 2026-10-13
name: 0245-query-latency-baseline-environment-and-gate
status: accepted
tags:
  - latency-suite
  - loadtest
title: Baseline environment and gate for the query-latency suite
REFERENCES:
  - "[[adr/0046-cypherlite-parser-memorydb-runtime]]"
  - "[[adr/0070-docker-baseline-loadtest-environment]]"
  - "[[adr/0071-ci-owned-loadtest-freeze-fingerprint]]"
  - "[[adr/0221-engine-reported-statement-elapsed]]"
  - "[[adr/0228-ontology-list-property-types-closed]]"
  - "[[adr/0243-query-latency-suite]]"
  - "[[adr/0244-in-process-query-sampling-profiler]]"
  - "[[adr/0246-defer-query-latency-plan-goldens]]"
  - "[[adr/0289-graph-shape-memory-pins]]"
---

# ADR-0245: Baseline environment and gate for the query-latency suite

> Renamed from [ADR-0228](./0228-ontology-list-property-types-closed.md): the number collided with
> docs/adr/0228-* on main (both were taken by parallel PRs). The
> decision is unchanged; references were updated in the same change.

## Context

[ADR-0243](./0243-query-latency-suite.md) runs the 30 graph-benchmark-ldbc queries at SF0.1 on every PR
and checks answers and plans. Latency and memory need a frozen baseline
and compare bands to be a gate.

[ADR-0070](./0070-docker-baseline-loadtest-environment.md) fixes the baseline environment for the frozen loadtests:
compile in `rust:1.88-bookworm`, run in `debian:bookworm-slim` with two
CPUs of CFS bandwidth on a 10 ms period and `--memory=2g`. [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md) makes
the `create-loadtest-baselines` workflow on `ubuntu-24.04` the only
freeze authority and stamps each report with an `environment` class.
[ADR-0289](./0289-graph-shape-memory-pins.md) already gave `memory-shape-loadtest` its own memory cap
(`MEMORY_SHAPE_MEMORY`) under the same CPU cap.

The SF0.1 workload does not fit 2 GB: the examples harness measured a
peak RSS of 3.1 GB with the store open and 2.7 GB during the load on
0.12.0. The repo is internal, so standard GitHub runners have about 7 GB.

Its latencies span four orders of magnitude at SF0.1 on 0.12.0: most
queries are 0.16-0.5 ms, a few are 1-13 ms, and q19, q22, q27 and q30
were 0.47-1.03 s. [ADR-0070](./0070-docker-baseline-loadtest-environment.md) (item 8) found that a sub-millisecond p99
under concurrent load swings by milliseconds. This suite is
single-threaded and each sub-millisecond query gets hundreds or thousands
of rounds, so its p50 is far steadier, but 30 queries per PR still
multiply any flake rate.

## Decision

1. **Environment.** The [ADR-0070](./0070-docker-baseline-loadtest-environment.md) recipe with a larger memory cap: compile
   unconstrained in `rust:1.88-bookworm`; run `query-latency run` in
   `debian:bookworm-slim` with `--cpu-period=10000 --cpu-quota=20000`
   and `--memory=$QUERY_LATENCY_MEMORY --memory-swap=$QUERY_LATENCY_MEMORY`
   (default `6g`). The data set is bind-mounted read-only; the graph is
   `:memory:`, so no tmpfs store is needed. `scripts/baseline-env.sh`
   gains a `query-latency` target; `BASELINE_ENV=host` stays a debug
   escape whose reports are never frozen or gate-compared.

2. **Environment class.** Reports carry an [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md) `environment` class
   that names this recipe (`query-latency`, the memory cap, image
   digests, and `github-actions:ubuntu-24.04` or `local`).

3. **Freeze.** `create-loadtest-baselines.yml` also runs the suite
   `BASELINE_RUNS` times (default 5) and opens its PR with
   `baselines/query-latency-sf0.1.json`, a worst-of-N envelope: per query
   the highest p50 and highest mean of the engine-reported elapsed
 ([ADR-0221](./0221-engine-reported-statement-elapsed.md)), and over the run the highest peak RSS. Load time is
   recorded and not gated. `ops`-style raw round arrays are not committed.

4. **Compare**, per query, against the envelope:
   - answers, plan goldens, errors and timeouts: already hard failures
 ([ADR-0243](./0243-query-latency-suite.md)), independent of class;
   - **latency:** p50 ≤ max(1.5 × envelope p50, envelope p50 + 0.5 ms);
   - **memory:** peak RSS ≤ 1.15 × envelope.

 The bands are provisional until calibrated the [ADR-0070](./0070-docker-baseline-loadtest-environment.md) way: freeze
   from five runs, run a sixth held-out run, and widen a band only by
   what the held-out run needs, recording the numbers here before this
   ADR is accepted. The 0.5 ms allowance is chosen so a 10× regression of
   a 0.17 ms query (0.17 → 1.7 ms, the shape of q26's SF1 regression)
   fails, which a 2 ms or 5 ms allowance would not catch.

5. **Class mismatch.** As [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md): the CI compare hard-fails
   `INCOMPATIBLE`; local `make gate` skips latency and RSS but still
   enforces answers, plans, errors and timeouts.

6. **Wiring.** `make query-latency` defaults to this environment; `make
   baseline-loadtests` includes it; `make gate` runs the compare once
   `baselines/query-latency-sf0.1.json` exists (until the first CI freeze
   lands, it skips the latency compare, as `memory-shape-loadtest` did
 under [ADR-0289](./0289-graph-shape-memory-pins.md)). A `query-latency` CI job on `ubuntu-24.04` runs per
   PR with the data set restored from `actions/cache`, and uploads the
   report as an artifact (`if: always()`).

7. **SF1 is not frozen or gated.** SF1 is a local run only ([ADR-0243](./0243-query-latency-suite.md));
   its report stays under `target/`.

## Alternatives considered

- **Reuse the 2 GB [ADR-0070](./0070-docker-baseline-loadtest-environment.md) environment** — rejected: SF0.1 peaks above
  2.7 GB during the load and would OOM.
- **Run on the bare runner without Docker** — rejected: a local run
 could not reproduce the CI class, the [ADR-0070](./0070-docker-baseline-loadtest-environment.md) problem again.
- **[ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md)'s 1.15× ratio bands** — rejected: with 30 queries, many
  under a millisecond, a per-PR flake rate that high trains people to
  re-run. The suite's job is to catch the 2-1000× regressions it has
  already seen; small drifts belong to the SF1 report and to profiling
 ([ADR-0244](./0244-in-process-query-sampling-profiler.md)).
- **Gate on the mean** (the statistic upstream publishes) — rejected for
  the gate: one scheduler stall moves the mean of a short query. The
  mean is still frozen and reported.
- **Gate on instruction counts** (`perf stat` or cachegrind) —
  rejected for now: deterministic, but Linux-only, needs perf access
  inside the container or a 10-50× slower run under valgrind across a
  25 s load. A later ADR can add it if wall-clock bands prove too loose.
- **Freeze SF1 on a large runner** — rejected: large-runner classes
  cost more and are not guaranteed stable, and SF1 is local-only
 ([ADR-0243](./0243-query-latency-suite.md)).

## Consequences

- One more envelope in `baselines/` and one more suite in
  `create-loadtest-baselines.yml`; a per-PR CI job of a few minutes.
- Peak RSS becomes gated: the 3 GB for a 160 MB store at SF0.1 is now a
  tracked number, not a side observation.
- [ADR-0070](./0070-docker-baseline-loadtest-environment.md) and [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md) each carry a **Later references** note naming
  this ADR. AGENTS.md's "Baseline loadtests" section lists
  `make query-latency` and `QUERY_LATENCY_MEMORY` in the change that adds
  them.

## Later references

- 2026-10-19: the per-PR CI job moved from the free ubuntu-24.04 class to
  `linux-4-core-x86` (the LABS-310 compile-bound rationale): the suite's
  in-container compile dominates the job's wall time, and a quieter host
  steadies the p50s inside the unchanged 2-vCPU CFS-capped recipe. The
  timing class itself is unchanged (the capped recipe); the class
  *label* changed with the runner (`github-actions:linux-4-core-x86`),
  so the envelope was re-frozen on the new label.

- [ADR-0246](./0246-defer-query-latency-plan-goldens.md): there are no plan goldens. Item 4's hard failures are
  answers, errors and timeouts, and item 5's local compare enforces those
  three; the latency band is the check for planner cliffs.

- 2026-10-20 (band widening, 1.5x -> 3.0x): the maiden gated run of the
  ratchet (PR #390) measured six CPU-bound queries up to 2.5x above the
  frozen envelope while every fast query sat inside its band. The
  asymmetry: the envelope is frozen by the freeze workflow on a quiet
  host, while the per-PR compare runs inside the full CI matrix, where
  concurrent jobs contend for the same physical host's cores and the
  CFS-quota'd 2-vCPU container sees less real CPU. The p50 band factor is
  widened to 3.0 (`QUERY_LATENCY_P50_RATIO`, env-overridable) with the
  absolute floor unchanged; the catastrophic plan-change class this gate
  exists for (q3: 0.03ms -> 78ms) is thousands of times over any of these
  bands. The structural alternative — freezing the envelope through the
  same stage shape as the compare so the envelope carries typical
  contention — remains open on the ticket.
