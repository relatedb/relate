---
type: adr
area: infra
claim: e6fe1fb5f36c42014715ddbcc50a44b0dfad5b3c
name: 0482-spec-harness-coverage-lane
status: accepted
tags:
  - ci
  - conformance
title: The spec-harness coverage lane
---

# ADR-0482: The spec-harness coverage lane

## Context

The `neo4j-labs/cypherlite-cypher-test-harness` repository (private, same
org) runs Neo4j's own Cypher spec suites — TCK, acceptance, cypher25, the
converted correctness corpus; the graphengine suite is skipped by design —
as behave scenarios against CypherLite **through the Python binding**: the
wheel installed in the run's venv is the engine under test. Against engine
main `49985dbe` (2026-11-02) the suite stands at 13097 passed / 960 failed
/ 178 `@fails`-expected / 7 nondeterministic / 1766 skipped — 93.17%
coverage of the executed scenarios — and work to drive the failures to
zero is ongoing across sessions.

What is missing on the engine side is per-PR visibility: nothing on a
cypherlite PR says what the PR did to spec coverage, and once the suite
reaches green nothing guards it. Two mechanics were verified live while
building the lane (harness session, 2026-11-02): the wheel must be built
by maturin **from `surfaces/cypherlite-python/`, not the workspace root**
(root fails with `TOML parse error … missing field 'package'`), and the
harness venv must never be cached — a stale wheel silently measures the
wrong engine.

The harness's `reports/summary.json` (written by
`scripts/generate_report.py`) is the machine-readable rollup: `totals`
(`passed`, `failed`, `neo4j_expected_fail`, `nondeterministic`, `skipped`)
and `failure_categories`, the per-category defect breakdown.

## Decision

1. **The lane.** `.github/workflows/spec-harness.yml` (its own file, so
   the lane is independently dispatchable, not a Sonic stage in `ci.yml`):
   on every PR to `main` (docs/markdown-only paths ignored) and on
   dispatch, on the `linux-4-core-x86` class, it checks out the engine and
   the harness, builds the wheel from the PR's own commit with maturin
   (release), force-reinstalls it into a fresh, never-cached venv, runs
   `scripts/generate_report.py`, and reports the verdict. Steps are named
   for what they do — a failure must read as a coverage verdict or a build
   failure, never hide behind a composite action's step name.

2. **Coverage.** `passed / (passed + failed)` over the scenarios the run
   executed, reported by `scripts/spec-harness-coverage.py` into the job
   summary with the per-category breakdown.

3. **The not-counted contract.** Three buckets never count against
   coverage: `neo4j_expected_fail` (scenarios Neo4j itself tags `@fails` —
   failing them is the expected state, not a CypherLite defect),
   `nondeterministic` (`rand()` / `randomUUID()` expectations that cannot
   be verified run to run), and `skipped` / `untested`. The first is a
   cross-repo contract: harness PR #16 (`66d007f`) keeps `@fails` failures
   out of `failure_categories`, so that rollup is exactly the
   genuine-defect count and the engine script asserts
   `sum(failure_categories) == totals.failed` — a harness summary that
   breaks the invariant fails the lane loudly instead of passing on
   reinterpreted defaults.

4. **Reporting, not gating.** Coverage never fails the build. The report
   leads with a traffic light in the job summary and in one upserted PR
   comment per PR: 🟢 at zero genuine failures, 🟡 while the failure share
   stays within 10% of the executed scenarios (an exact 10% is yellow),
   🔴 beyond it. The report step exits 0 in every band; the one failure
   mode that still exits non-zero is a summary the report script cannot
   read at all (the §3 contract) — then there is no number to report. No
   baseline, ratchet, or re-freeze machinery: the bands are fixed
   thresholds on the failure share, not positions relative to history.

5. **Cross-repo checkout.** The harness is checked out at its `main` so
   the numbers are always current; the run records the harness sha in the
   report so harness-side drift is one glance to attribute. The checkout
   token is `${{ secrets.HARNESS_READ_TOKEN || github.token }}`. **The
   default token does not work here** — verified in CI on 2026-11-02:
   `github.token` is scoped to the workflow's own repository, so a
   cross-repo checkout of the harness fails with `Repository not found`
   even with the harness repo made *internal* (GitHub's own
   `actions/checkout` documentation groups private and internal together
   for exactly this reason). A read token is therefore required: a
   fine-grained PAT (`Contents: Read` on the harness repo) stored as
   `HARNESS_READ_TOKEN`, or a GitHub App installation token. The workflow
   keeps the `|| github.token` fallback so the day a same-org default-token
   path exists it needs no code change, and warns when the secret is
   absent. Both checkouts run with `persist-credentials: false`: the lane
   never pushes, and the harness PAT (with the comment-writable
   `github.token`) must not sit in `.git/config` while PR-authored code
   runs — the comment step passes `github.token` explicitly instead.

## Alternatives considered

- **A per-category ratchet against a committed
  `baselines/spec-harness.json` with a re-freeze dispatch workflow** (the
  cip-eval / loadtest-envelope precedent). Rejected: the goal is current
  coverage and a below-100% tripwire, and a historical baseline plus
  freeze ceremony is machinery that does not serve it — the anchor is
  100%, so there is nothing to ratchet against. (Considered first and
  dropped on review of the actual goal.)
- **Pinning the harness ref in the baseline.** Rejected with the ratchet:
  pinning buys attribution but costs currency; the sha in the report
   buys the attribution without the pin.
- **A Sonic stage inside `ci.yml`'s job graph.** Rejected: the lane's
  steps (venv, pip, maturin, behave) are foreign to the Sonic stage
  vocabulary, and a standalone file keeps the lane dispatchable on
  demand — the pattern `create-loadtest-baselines.yml` already set.
- **Hard-failing on a missing `HARNESS_READ_TOKEN` secret.** Rejected:
  the token is required (see §5), but a preflight step that aborts adds
  nothing over the checkout's own loud failure, and the fallback
  expression keeps the lane correct the day a default-token path exists.
  The lane warns when the secret is absent and the clone fails with the
  remedy named in the step's comment.

## Consequences

- Every non-docs PR pays ~10–15 min on the 4-core class (release wheel
  build + ~16k scenarios). Acceptable for the one lane that measures the
  product's spec coverage end to end.
- The lane reports 🟡 on today's PRs (960 of 14057 executed — a 6.83%
  failure share) until the ongoing fixes drive the count down, and never
  blocks one: the number is reported, not enforced (§4). The bands are
  the progress signal while the fixes land; after the suite reaches green
  the same light is the regression signal.
- The known-stable failure walls sit inside the count and do not flap:
  the Python binding's microsecond temporal truncation
  (`result-temporal-fidelity`, ~314, LABS-756) drops out of the number
  when that fix lands — the light just goes green sooner; no re-baseline
  step exists or is needed.
- The engine lane depends on the harness repo's summary shape. A harness
  change that alters the rollup semantics breaks this lane loudly (the
  §3 assertion) — the cross-repo contract is revisited in the open, not
  papered over.
- The venv and wheel are rebuilt every run by construction (fresh venv,
  wheel written to a scratch dir, force-reinstall); only cargo/sccache
  artifacts are cached. A stale-engine measurement is impossible.

## Later references

The follow-up PR after merge tightened §1's trigger to the repo's draft
contract: the lane runs when a PR is **ready for review** (the
`ready_for_review` activity type plus a draft guard on the job), not on
every push — drafts carry the light fast gate only, and the lane's
release build + ~16k scenarios is one of the heavy lanes. The maiden PR
itself demonstrated the cost: it ran on every draft push, failed-checkout
runs included, before the fix.

A second follow-up moved §1's runner class to the free tier
(`ubuntu-latest`). Measured against the maiden deployment's own telemetry:
the lane's footprint is ~1–2 GB (its rust-cache is 188 MiB — the build is
one cdylib, no test binaries, so the Bolt lanes' free-tier ENOSPC history
for multi-binary legs does not apply), a warm run is 3–6 min on the same
4 vCPU, and every new PR is a cold first build regardless of class
(GitHub's action caches are PR-ref scoped, and this lane never runs on
main to seed a shared one). The paid 4-core class stays reserved for the
compile-bound lanes it was piloted for; at the observed fleet rate
(seven runs across the org's in-review PRs in ~70 min) keeping a
reporting-only lane off it pays for the flip on its own.

A third follow-up extended §2's denominator: behave's `error` bucket
(step crashes — the step raised rather than asserted) now **counts
against coverage exactly like a failure**. The corpus-scale verification
run surfaced the case concretely: a harness-side rendering crash turned
~2k genuine failures into step *errors*, and under §2-as-written those
scenarios would have left the coverage denominator entirely. A scenario
whose step crashed did not pass, so it cannot be context; the
`@fails`-exclusion contract (§3) still sums categories to `failed` only,
since errored scenarios carry no category. The harness side runs one
behave worker per core (harness PR #24), which the lane inherits
automatically — its suite step already calls the report script with no
arguments.
