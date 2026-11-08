# Merge baselines

Frozen measurements from the pre-merge **source mains** (the original
`cyperlite-original` checkout and the archived predecessor tree, both
historical; neither is part of this repository).
`make baseline` writes summaries here. `make gate` re-runs and compares.

Do not commit full loadtest `ops` arrays. Summaries set `ops_omitted: true`.

## Baseline environment (ADR-0070 / ADR-0071)

Fraud, identity, and checkpoint loadtests that are frozen or compared as
a merge gate **must** run in the predefined Docker environment:

- compile unconstrained in `rust:1.88-bookworm`
- run in `debian:bookworm-slim` with two CPUs of CFS bandwidth on a 10 ms
  period (`--cpu-period=10000 --cpu-quota=20000`) and
  `--memory=2g --memory-swap=2g`. Not `--cpus=2`: that is the same quota
  on a 100 ms period, which freezes every thread for the rest of the
  period and injects ~50 ms p99 stalls
- 8 fraud / 4 identity workers, so two CPUs are loaded but not saturated
- on-disk stores on tmpfs (`/tmp/fraud-lt`, `/tmp/fraud-checkpoint`);
  identity stays `:memory:`
- the freeze is a worst-of-`BASELINE_RUNS` (default 5) envelope: lowest
  throughput, highest per-use-case p99, highest logical bytes
- each report is stamped with an `environment` class (recipe +
  `github-actions:ubuntu-24.04` or `local`; image digests, not hostname)
- `make compare-loadtests` sets `LOADTEST_P99_JITTER_NS=5000000`; the
  other ADR-0046 bands are the script defaults

Entry points: `make fraud-loadtest`, `make identity-loadtest`,
`make fraud-checkpoint`, `make baseline-loadtests`. Repo-root
`baselines/*.json` is the only comparison set. Wall-clock ops/s and p99
apply when the `environment` class matches: `errors == 0`, ops/s ≥ 90%
of baseline, p99 ≤ max(115% of baseline, baseline + 5 ms), logical bytes
≤ 110%. CI (`loadtest-baselines` on `ubuntu-24.04`) hard-fails
`INCOMPATIBLE` on a class mismatch. Local `make gate` skips ops/s and
p99 on mismatch but still enforces errors and logical bytes.

`BASELINE_ENV=host` is a debug escape (`cargo run` on the host). Do not
freeze or gate-compare those reports. `make baseline-loadtests` refuses
it. Local envelopes land under `target/` unless
`BASELINE_FREEZE_SOURCE=ci`.

The CI job
runs only the fraud and identity loadtests and uploads fresh
`target/fraud-loadtest.json` and `target/identity-loadtest.json` as the
`loadtest-reports` artifact (`if: always()`); memory-shape and
checkpoint stay local targets (`make memory-shape-loadtest`,
`make fraud-checkpoint`), not CI steps.

### Comparison tables in CI (LABS-305)

After the fraud and identity loadtests, the `loadtest-baselines` CI job
renders the comparison presentation (the job runs no memory-shape or
checkpoint step — those stay local targets):

- `make render-loadtest-comparison` writes
  `target/loadtest-comparison.md` — one `### <pair>` markdown table per
  loadtest: fraud, identity — with the gate's
  `LOADTEST_P99_JITTER_NS` bands, and appends the document to the run's
  **step summary** (`$GITHUB_STEP_SUMMARY`). The step runs `if: always()`
  and never gates: render mode always exits 0, a pair whose loadtest step
  failed renders a `report-missing` note, a pair without a frozen
  baseline renders a `not-compared-no-baseline-frozen` note, and the
  step carries `continue-on-error` so a presentation error cannot fail
  the job.
- The aggregator's pair set is declared once as a registry at the top of
  `scripts/compare-baselines.sh` — file, report kind — which both the
  gate and the render step iterate over every record:
  fraud and identity are the pairs. Memory-shape and checkpoint are not
  registry members (their local targets and the freeze workflow remain;
  re-entry is a future decision), and the comparator still renders their
  kinds when invoked directly. Each rendered pair is dispatched with
  its registry report kind and classified from the comparator's
  machine-readable status token (`--status-out`), never by grepping the
  rendered prose; the document opens with a producer-owned
  `**Status:** ok | warning` line — warning when any pair has failing
  rows, is not comparable, hit an environment-class mismatch, is missing
  its report, or failed to render — that consumers read instead of the
  `**Pairs:**` summary tokens.
- On `pull_request` events, `scripts/update-pr-comment.sh` posts the
  document as exactly ONE self-updating PR comment whose body carries the
  marker `<!-- cypherlite:loadtest-baselines -->` — a rerun PATCHes the
  marker comment in place instead of adding a duplicate. Only a comment
  authored by `github-actions[bot]` (the identity the job's
  `GITHUB_TOKEN` posts as) is ever PATCHed: a third-party comment that
  quotes the marker is left untouched and a fresh bot comment is POSTed
  instead. The comment's status line is read from the document's
  authoritative `**Status:**` line — `ok` renders a note, `warning`
  (or a missing/unrecognized line, which fails loud) renders a warning.
  The step is best-effort (the script exits 0 on every posting failure)
  and the job's token is scoped `contents: read` + `pull-requests:
  write` for it.

The compare gate itself is **not re-enabled**: the `Compare against
baselines` step in `.github/workflows/ci.yml` stays commented out
(re-enabling it is a tracked follow-up). The tables and the PR comment
are presentation only — the `INCOMPATIBLE` hard-fail and the ADR-0046
bands above describe the gate once that step runs again.

### Re-freezing

Committed loadtest JSON is produced by
`.github/workflows/create-loadtest-baselines.yml` (`workflow_dispatch`,
`ubuntu-24.04`, same class as the PR gate). The workflow opens a PR with
the three envelopes; it never pushes to `main`. Do not write repo
`baselines/` from a laptop. Image digest, arch, knob, or
runner changes show up as `INCOMPATIBLE` — re-freeze via that workflow.
A matched-class miss is a performance fail (code or residual GHA noise),
not a reason to add `baselines/ci/` or widen bands.

## Memory controls envelope

`baselines/memory-controls.json` is schema 1. Until a freeze lands,
`frozen` is false and `make compare-memory-controls` reports
`no-baseline` (a warning, exit 0). `make baseline-memory-controls` runs
`BASELINE_RUNS` rounds of `MEMORY_CONTROLS_PROFILE=full` in the ADR-0070
container and folds each numeric metric into a min/max range.
`create-loadtest-baselines` runs that target and opens the pull request;
it does not push to the default branch.

A regression is a behavioural miss, or a lower-is-better metric above
the range by both its band and its floor. An improvement is the same
metric below the range. It warns and the job stays green (ADR-0544).
Confirm an improvement is real, then dispatch
`create-loadtest-baselines` again so the envelope moves to the new
numbers. A regression still fails. The pull-request lane runs one
calibration repeat. The freeze and the nightly cadence run three.
Repeat count is not part of the class match.

## Notes

Knobs for the pre-merge loadtests match the archived predecessor's `Makefile` defaults (the
merging agent's recorded `knobs.json` was devcontainer-specific and was
removed in the post-merge cleanup; the in-tree defaults live in
`testing/cypherlite-datasets`).

CI-only (not required on this Linux agent): getting-started notebook. The Swift/iOS and wasm-pack Chrome `:memory:` smoke checks are no longer CI jobs, and their local tooling (the former `make build-xcframework` / `make test-wasm-browser` targets) is withdrawn along with the WASM and Swift surfaces (ADR-0065).

Lance (`cypherlite-lance`) was skipped on this agent without `protoc` (withdrawn — ADR-0052). Source `make test` minus that package was green.

The predecessor TCK on source main: 3896 passing with 1 allowlisted scenario. The
in-tree TCK floor is the single merged `cypherlite-tck` harness: 3896 of the
3897 pinned, cucumber-expanded scenarios pass with 1 documented skip (see
[`docs/tck/compliance.md`](../docs/tck/compliance.md)). Do not "fix" source
trees as part of the merge.
