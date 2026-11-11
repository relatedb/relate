# 0070. Docker baseline loadtest environment (2 vCPU / 2 GB)

- **Status**: Accepted
- **Date**: 2026-09-20
- **Deciders**: CypherLite maintainers

## Context

ADR-0046 froze in-tree loadtest summaries under `baselines/` as a merge
gate (ops/s ≥ 90% of baseline, p99 ≤ max(115% of baseline, baseline +
250,000 ns), logical bytes ≤ 110%, `errors == 0`). ADR-0060 established
that those numbers are host- and condition-specific. ADR-0061 then split
the gate into two host classes (`baselines/` for the local reference host,
`baselines/ci/` for `ubuntu-latest`), and ADR-0062 / ADR-0064 loosened the
CI compare bands because unconstrained GitHub runners could not reproduce
the local-host freeze.

That split made the gate pass, but it did not make a loadtest on a laptop
comparable to CI or to another developer’s machine. The numbers still
tracked whoever had more cores, RAM, and a quieter scheduler.

The in-tree suites that participate in this freeze are `fraud-loadtest`,
`identity-loadtest`, and `fraud-checkpoint` (`make baseline-loadtests`).

## Decision

One baseline class. Fraud, identity, and checkpoint loadtests that are
frozen or compared as a merge gate **must** run in the predefined baseline
environment:

1. **Compile unconstrained** in `rust:1.88-bookworm` (no CPU/memory cap),
   producing GNU/Linux `target/release/{fraud-loadtest,identity-loadtest,fraud-checkpoint}`.
2. **Run constrained** in `debian:bookworm-slim` with two CPUs of CFS
   bandwidth on a 10 ms period (`--cpu-period=10000 --cpu-quota=20000`),
   `--memory=2g`, `--memory-swap=2g` (no extra swap). Prefer bind-mounting
   the repo so reports land in `target/*.json`. When the Docker daemon
   cannot bind-mount the repo (Docker Desktop file-sharing),
   `scripts/baseline-env.sh` streams sources into a named volume and
   copies reports back.
3. **Store on tmpfs.** The run container mounts `--tmpfs /tmp:exec,rw,size=1g`.
   Default on-disk URIs are `/tmp/fraud-lt` and `/tmp/fraud-checkpoint`.
   Identity stays `:memory:`.
4. **Make is the entry point.** `make fraud-loadtest`,
   `make identity-loadtest`, `make fraud-checkpoint`, and
   `make baseline-loadtests` default to this environment
   (`BASELINE_ENV=docker`). `make baseline-loadtests` refuses
   `BASELINE_ENV=host`. `BASELINE_ENV=host` remains a debug escape that
   uses host `cargo run`; those reports must not be frozen or
   gate-compared.
5. **One comparison set.** Repo-root `baselines/` is the only loadtest
   comparison set. Local `make gate` / `make compare-loadtests` and the CI
   `loadtest-baselines` job all compare dockerized reports against it with
   the ADR-0046 bands (script defaults). `baselines/ci/` is withdrawn.
6. **The freeze is a worst-of-N envelope.** `make baseline-loadtests` runs
   each suite `BASELINE_RUNS` times (default 5) and
   `scripts/freeze-baseline-envelope.py` keeps the lowest throughput, the
   highest per-use-case p99, and the highest logical bytes. A single run
   does not pin the tail; the envelope does.
7. **Worker counts are sized for two CPUs.** `WORKERS` 50 -> 8,
   `WORKERS_IDENTITY` 8 -> 4. ADR-0048's counts saturate the container, and
   queueing delay alone then swings per-use-case p99 by 2-4x run to run
   (`recent_transfers` p99 2.35 / 5.43 / 3.63 / 4.09 ms against a 1.24 ms
   envelope). At 8 / 4 the suites still load both CPUs — fraud 6,122-6,647
   ops/s, identity 3,312-3,621 ops/s across six runs, both 1.09x spread —
   and an envelope built from five runs held a sixth, held-out run with no
   throughput breach and no fraud p99 breach. ADR-0048's 40 MiB budget,
   `:memory:` URI, seeds, scale, and ops are unchanged.
8. **Absolute p99 allowance of 5 ms.** The one held-out breach was a
   sub-millisecond read (`detect_sod`, 0.202 ms envelope, 2.962 ms on the
   held-out run): at that scale a single queueing delay dominates the
   measurement. `make compare-loadtests` therefore sets
   `LOADTEST_P99_JITTER_NS=5000000`, the same value ADR-0064 calibrated
   for CI, here with ~1.7x headroom over the measured ~3 ms swing. The
   1.15x ratio still governs every use case above ~1.7 ms, and
   `errors == 0`, the 0.90 throughput floor, and the 1.10 logical-bytes
   ratio are untouched.
9. **Re-freeze.** Throughput under 2 CPUs / 2 GB will not match the old
   unconstrained Mac or CI numbers. This ADR re-freezes
   `baselines/fraud-loadtest.json`, `baselines/identity-loadtest.json`, and
   `baselines/fraud-checkpoint.json` from a dockerized
   `make baseline-loadtests`. Six consecutive verification rounds
   (`make fraud-loadtest && make identity-loadtest && make
   compare-loadtests`) passed against that freeze.

### Why a 10 ms CFS period and not `--cpus=2`

`--cpus=2` is the same bandwidth on Docker's default 100 ms period:
`cpu.max` becomes `200000 100000`. When the workload spends that budget
early, every thread freezes until the next period boundary. A measured
identity run showed `nr_periods 77`, `nr_throttled 75`,
`throttled_usec 12675145` — throttled in 97% of periods, 12.7 s of
enforced sleep in a ~15 s run.

That produced ~45-62 ms stalls that read like an engine pause but were
not. A single-worker run had no stall at all (`provision_user` p99
2.665 ms, max 3.278 ms), and a read-only run still showed ~51 ms maxima
across unrelated use cases at the same instant. The stall also moved
between use cases run to run, which is what made the gate flake:
`add_alias` p99 45.7 ms against a 0.41 ms envelope on one run, 0.10 ms
on the next.

Keeping the quota but shortening the period to 10 ms bounds each freeze
to <= 10 ms while preserving exactly two CPUs of bandwidth, and it stays
independent of which host cores are busy.

`--cpuset-cpus=0-1` was measured too and removes throttling entirely
(`nr_throttled 0`, identity ~5,900 ops/s). It was rejected because it
pins the run to two named cores: with unrelated load on those cores a
verification round collapsed to 780 ops/s against a 4,613 ops/s
envelope. Bandwidth that the scheduler can place on any core is the more
portable cap.

Knobs: `BASELINE_ENV`, `BASELINE_CPUS` (2), `BASELINE_CPU_PERIOD_US`
(10000), `BASELINE_CPU_QUOTA_US` (period x CPUs), `BASELINE_MEMORY` (2g),
`BASELINE_RUNS` (5), `BASELINE_BUILD_IMAGE` (`rust:1.88-bookworm`),
`BASELINE_RUN_IMAGE` (`debian:bookworm-slim`). A host with fewer CPUs
than `BASELINE_CPUS` fails the run rather than producing incomparable
numbers.

This supersedes the two-host-class layout of ADR-0061 and the CI-only
band overrides of ADR-0062 and ADR-0064 for these suites. Residual
differences remain (host kernel, Docker Desktop vs GitHub-hosted Docker,
noisy neighbors). If a dockerized CI run on healthy code still misses
the ADR-0046 bands, calibrate under a new ADR — do not resurrect a
second baseline directory by default.

## Alternatives considered

- **Compile inside the 2 GB run container** — rejected: `rustc` for these
  crates exceeds 2 GB RSS; the cap is a run-time fairness constraint, not
  a compile-time one.
- **Keep two host classes and only document Docker as optional** —
  rejected: optional environments are not comparable; developers would
  keep freezing laptop numbers that CI cannot hit.
- **Pin `--cpus=2` on the host process with `taskset` / cgroups, no
  Docker** — rejected: macOS has no equivalent cgroup, and libc/kernel
  still differ. A Linux container is the portable unit.
- **Keep `--cpus=2` and widen the p99 band to absorb the ~50 ms stalls**
  — rejected: an allowance that large would hide real regressions on
  every sub-millisecond use case, and it calibrates the gate to a
  scheduler artifact instead of removing it.
- **Pin with `--cpuset-cpus=0-1`** — rejected; see the section above. It
  removes throttling but makes the run hostage to whatever else uses
  those two cores.
- **Keep ADR-0048's 50 / 8 workers in the container** — rejected: they
  saturate two CPUs, and the resulting queueing tail moves p99 by 2-4x
  between identical runs.
- **Force `linux/amd64` on every host** — rejected: Apple Silicon would
  then emulate; matching the host architecture with a Debian userland is
  enough for relative compare, and the freeze is re-taken on CI’s amd64
  when the environment definition changes.

## Consequences

- Docker is required for freeze and for CI/local gate reports of the
  three in-tree suites. Agents and humans invoke `make fraud-loadtest`
  (etc.); they do not `cargo run` those binaries for a baseline.
- CI `loadtest-baselines` drops `LOADTEST_MIN_THROUGHPUT_RATIO=0.65`,
  `LOADTEST_MAX_P99_RATIO=4.0`, and `LOADTEST_P99_JITTER_NS=5000000`, and
  compares against root `baselines/`.
- `make gate` still compares last `target/*.json` reports; those reports
  must have been produced by the docker environment.
- Loadtests run by clients implementing embedded Relate are out of scope.

## Later references

- ADR-0071: freeze authority is the `create-loadtest-baselines`
  workflow on `ubuntu-24.04`, not a local Docker envelope. Reports carry
  an `environment` class; CI hard-fails wall-clock compare on mismatch
  (`INCOMPATIBLE`) instead of loosening ADR-0046 bands or restoring
  `baselines/ci/`.
- ADR-0289: `memory-shape-loadtest` joins this environment. It records
  the Bolt server's RSS per graph shape. It is not an in-process fraud
  or identity run. Its run container is 2 GB, the same cap as the other
  suites. Dense-ring RSS at the 32 MB default is about 1.3 GB; a larger
  `LOGICAL_MB` needs a higher `MEMORY_SHAPE_MEMORY`. This target
  is built from `Dockerfile.memory-shape-loadtest` instead of streaming
  the repo into the named volume.
- ADR-0245: the query-latency suite (ADR-0243) joins this recipe with
  its own memory cap: the same images and two-CPU CFS cap, run with
  `--memory=$QUERY_LATENCY_MEMORY` (default `6g`), because the SF0.1
  workload peaks near 3 GB. Its gate compares per-query p50 latency and
  peak RSS against a worst-of-N envelope in
  `baselines/query-latency-sf0.1.json`.
- ADR-0495: every image-built baseline lane, `memory-shape-loadtest`
  included, builds from the one `Dockerfile.baseline`, parameterised by its
  bin list, BuildKit cache-id suffix and entrypoint; the per-lane
  `Dockerfile.memory-shape-loadtest` named above no longer exists.
