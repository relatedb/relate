---
type: adr
area: infra
claim: 093657593b0dcecaa0751e82269c81602088e8a5
name: 0661-spec-harness-regression-gate
status: accepted
tags:
  - ci
  - conformance
  - testing
title: The spec-harness regression gate — a required, id-level ratchet
REFERENCES:
  - "[[adr/0056-opencypher-tck-language-contract]]"
  - "[[adr/0071-ci-owned-loadtest-freeze-fingerprint]]"
  - "[[adr/0482-spec-harness-coverage-lane]]"
  - "[[adr/0520-move-cypher-tests-to-spec-harness]]"
---

# ADR-0661: The spec-harness regression gate — a required, id-level ratchet

## Context

The spec harness (`relatedb/relate-cypher-compat-private`) runs the Cypher
compatibility corpus — ~32k scenarios, the repository's IP — against the
wheel built from the PR's own commit, and [ADR-0482](./0482-spec-harness-coverage-lane.md)'s lane reports a
traffic light on every ready PR and every merge to main. Reporting is all
it does: the lane exits zero in every band ([ADR-0482](./0482-spec-harness-coverage-lane.md) §4, reaffirmed
[ADR-0520](./0520-move-cypher-tests-to-spec-harness.md) §4), the number lives in a PR comment and a job summary, and
nothing on the engine side enforces any check at all — the main branch
ruleset carries only deletion and non-fast-forward rules. A coverage
regression today is a comment nobody must read.

[ADR-0520](./0520-move-cypher-tests-to-spec-harness.md) rejected a ratchet on this corpus for two reasons: syncing
Neo4j's suites moves the number under an engine PR's feet, and freezing a
baseline re-creates the stale-pin problem the move had just retired. The
harness's machine rollup already carries per-scenario failure identity —
feature, scenario, category, the `@fails` marking — and those ids are
corpus-derived IP: they can appear nowhere public.

## Decision

1. **Failing-id ratchet, set semantics.** A ready PR's lane run fails the
   build if and only if a scenario fails — assertion or step crash — that
   the baseline does not already carry as failing. A fix-N-break-N swap
   is red; a pure improvement is green and shrinks the baseline at the
   next merge. Expected-fail, nondeterministic and skipped scenarios stay
   outside the gate exactly as they stay outside coverage ([ADR-0482](./0482-spec-harness-coverage-lane.md) §3).
2. **A machine-written baseline in the harness repository.** The lane's
   existing main-merge run measures the merge commit and writes the
   baseline — engine sha, harness sha, totals, failure categories, the
   failing-id and errored-id sets — through a
   fine-grained read/write token scoped to that one repository, used only
   in push and dispatch runs and never persisted into a git config. No
   hand edits, no freeze workflow, no re-pin ceremony: the baseline
   refreshes by construction on every merge, so the passing number cannot
   go stale the way the retired pin's counts did ([ADR-0520](./0520-move-cypher-tests-to-spec-harness.md) §3). The
   recorded harness sha is what lets the merge watchdog tell an engine
   regression from corpus churn: the delta line carries it as
   `harness_moved`, the triage fact for a main-run regression.
3. **Like-for-like pinning.** A PR run checks out the harness revision
   the baseline records, so the diff the gate reads is engine-only:
   corpus churn can never red an engine PR. A PR co-designed with corpus
   changes opts out with a label and gates against current harness main.
   New corpus reaches the baseline at the next merge.
4. **The IP boundary.** The verdict publishes counts and categories only.
   Scenario ids and corpus paths never reach the engine repository's
   logs, comments, or artifacts — public-repo artifacts are
   world-readable — and the diagnosis of a red gate is a diff inside the
   harness repository. The categories are already public vocabulary:
   today's PR comment carries them.
5. **Required and fail-closed.** The lane's job becomes a required
   status check in the main ruleset — the repository's first
   machine-enforced gate. The lane's path filter moves inside the job,
   because a path-filtered workflow leaves a required check in Expected;
   docs-only diffs skip with an explicit success conclusion, and the
   docs-only detector's file list is fetched fail-closed — an
   unavailable or truncated list fails the step rather than silently
   skipping the gate. One automatic suite re-run absorbs an
   infrastructure flake before the verdict stands. A missing or
   unreadable baseline is red, never a silent pass: bootstrap is the
   first merge run after the change lands, and the baseline write path
   is a workflow of its own triggered by push to main only — GitHub
   runs a push-triggered workflow from the pushed ref's own file, so no
   PR-triggered run of any workflow (and no branch's editable workflow
   copy) ever references the write token.
6. **Merge watchdog.** The baseline writer diffs the previous and new
   baselines; a regression that reaches main — bypass, or the follow
   label — files an issue on the spot, counts and categories only.

## Alternatives considered

- **A counts ratchet** — defect count against a committed number, the
  [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md) envelope precedent in-repo. Rejected: blind to
  net-zero swaps, and the regression worth catching is per-scenario.
- **The id baseline in the engine repository.** Rejected: counts are
  already public in the PR comment, but the id sets are corpus-derived
  IP.
- **Actions cache or repository variables as the baseline store.**
  Rejected: cache eviction disables a required check silently, and
  variables are unversioned and unauditable.
- **Keep reporting-only** ([ADR-0482](./0482-spec-harness-coverage-lane.md) §4 as written). Rejected: a comment
  is not a gate — the premise of this decision.

## Consequences

- Lane cost is unchanged (required is not more runs); the harness
  repository gains a machine-written json refreshed every merge, and one
  new single-repo token secret rides in CI. Harness availability now
  gates merges fail-closed where before it merely reported.
- [ADR-0482](./0482-spec-harness-coverage-lane.md) §4's and [ADR-0520](./0520-move-cypher-tests-to-spec-harness.md) §4's reporting-only
  posture is superseded here; both carry Later-references notes. The
  lane's report — traffic light, bands, comment — stays as it is; the
  gate is a separate verdict beside it.
- Bootstrap order: the harness lists step-crashed scenarios per id, the
  write token lands as a secret, the lane is dispatched once to write the
  first baseline, and the ruleset requires the check after one green
  gated PR.
