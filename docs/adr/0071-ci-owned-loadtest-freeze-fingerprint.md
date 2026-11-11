# 0071. CI-owned loadtest freeze with environment fingerprint

- **Status**: Accepted
- **Date**: 2026-09-21
- **Deciders**: CypherLite maintainers

## Context

ADR-0070 made fraud, identity, and checkpoint merge-gate loadtests run in
a Docker recipe (compile in `rust:1.88-bookworm`, run in
`debian:bookworm-slim` with two CPUs of CFS bandwidth on a 10 ms period
and 2 GB). Local `make gate` and the CI `loadtest-baselines` job then
compared those reports against the same repo-root `baselines/` with the
ADR-0046 bands.

That equalized the **recipe**, not **delivered wall-clock performance**.
A freeze taken on a quiet nested VM (Codespace, Docker Desktop/linuxkit)
is not the same class as GitHub-hosted `ubuntu-24.04`. CFS quota does
not remove steal time, ISA differences, or image drift. Comparing those
runs as if they were one host produced a stream of `loadtest-baselines`
failures on healthy code.

ADR-0070's escape hatch was: if a dockerized CI run on healthy code still
misses the ADR-0046 bands, calibrate under a new ADR — and do not
resurrect `baselines/ci/`. This ADR spends that hatch on **freeze
authority and an environment class**, not on looser ratios.

## Decision

1. **CI owns the freeze.** Committed `baselines/fraud-loadtest.json`,
   `baselines/identity-loadtest.json`, and
   `baselines/fraud-checkpoint.json` are produced by the
   `create-loadtest-baselines` workflow (`workflow_dispatch` on
   `ubuntu-24.04`, `make baseline-loadtests` with
   `BASELINE_FREEZE_SOURCE=ci`). The workflow opens a PR with those
   three files and never pushes to `main`. Local
   `make baseline-loadtests` still builds a worst-of-N envelope under
   `target/` for debug; it does not write repo-root `baselines/` unless
   `BASELINE_FREEZE_SOURCE=ci`.

2. **One environment class, recorded on the report.** Each dockerized
   report is stamped with a sidecar `environment` object (report schema
   stays 6). The class is the recipe, not the VM:

   - `class`: `cypherlite-loadtest-baseline/v1`
   - `os` / `arch` (`linux` / `uname -m` in the run image)
   - `build_image` / `run_image` as `name@sha256:...`
   - `cpus`, `cpu_period_us`, `cpu_quota_us`, `memory`
   - `ops`, `warmup`, `workers` from the report `config` (checkpoint may
     omit ops/warmup/workers)
   - `runner`: `github-actions:ubuntu-24.04` on the freeze and gate
     jobs, `local` otherwise

   Hostname and CPU model are not part of the class. The freeze envelope
   requires every input run to share the same `environment`.

3. **Compare is regression vs incompatible.**
   `scripts/compare-loadtest.py` always checks `errors == 0`, logical
   bytes, schema, and workload kind. Wall-clock ops/s and p99 use the
   ADR-0046 bands (and `LOADTEST_P99_JITTER_NS=5000000` from
   `make compare-loadtests`) **only when** baseline and current
   `environment` objects match. Missing or differing class fields are
   **incompatible**:
   - In GitHub Actions (`GITHUB_ACTIONS=true`): hard fail
     (`compare-loadtest: INCOMPATIBLE: ...`). Do not also emit p99
     misses for an incomparable pair.
   - Locally: print the same line, skip ops/s and p99, still fail on
     errors or logical bytes so `make gate` remains a correctness
     ratchet.

4. **The PR job and the freeze workflow share a runner class.** Both use
   `ubuntu-24.04` (not `ubuntu-latest`) and
   `BASELINE_RUNNER=github-actions:ubuntu-24.04`. Wall-clock compare is
   valid only against a freeze from `create-loadtest-baselines` on that
   class.

5. **No second baseline directory. No auto-widened bands.** Flake
   recourse is not “re-run until lucky” and not `baselines/ci/`.
   Incompatible means the class changed (image digest, arch, knobs,
   runner) — re-freeze via the workflow. A matched-class miss is a
   performance fail (code or residual GHA noise on that class).

## Alternatives considered

- **Calibrate CI to 0.65× throughput / 4× p99 (ADR-0062 / 0064)** —
  rejected as the strategy: that made shared-runner noise in-band
  instead of naming the freeze host. Catastrophe detection on a noisy
  SKU is a different job; this gate keeps ADR-0046 bands on a matched
  class.
- **Resurrect `baselines/ci/`** — rejected in ADR-0070; a second
  directory duplicates freeze authority instead of recording it.
- **Fingerprint CPU model / hostname** — rejected: GitHub rotates
  hardware under one label; the class would mismatch every week.
- **Treat local Docker as freeze-equivalent when the recipe matches** —
  rejected: nested VMs still do not deliver GHA wall-clock. Local
  `runner` is `local`; wall-clock compare is skipped there.
- **Auto-commit freeze artifacts to `main`** — rejected: a re-freeze is
  an explicit reviewable PR, not a push to the default branch. The
  workflow may open that PR.

## Consequences

- The first comparator landing will INCOMPATIBLE-fail CI until the
  `create-loadtest-baselines` PR that adds `environment` is merged.
- Image tag moves (`debian:bookworm-slim` digest change) fail CI as
  incompatible until the next freeze, which is the intended signal.
- Matched-class GHA noise is unchanged by this ADR (dedicated runners,
  longer `OPS`, median-of-3 remain later knobs).
- Loadtests run by clients implementing embedded Relate are out of scope.

## Later references

- ADR-0245: `create-loadtest-baselines` also freezes
  `baselines/query-latency-sf0.1.json`, and query-latency reports carry
  an `environment` class naming their recipe and memory cap. The
  compare follows item 3 of this ADR: `INCOMPATIBLE` hard-fails in CI,
  and locally latency and RSS are skipped while answers, plans, errors
  and timeouts are still enforced.
- ADR-0512 supersedes this ADR's §4 spelling ("both use `ubuntu-24.04`"):
  the shared class is now the repo Actions variable `LOADTEST_RUNNER`
  (`linux-4-core-x86`), read by every stamping lane's `runs-on` and
  `BASELINE_RUNNER` and by the freeze's default — the invariant §4
  wanted (PR job and freeze share a class) holds structurally instead of
  through the same label hand-copied into six sites, which had drifted
  until every fraud/identity comparison went INCOMPATIBLE.
