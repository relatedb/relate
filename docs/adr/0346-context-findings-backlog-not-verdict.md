---
type: adr
area: infra
claim: 0d559584cc2ced79393270b398b689fcefeed560
name: 0346-context-findings-backlog-not-verdict
status: accepted
tags:
  - review
title: Context-unit findings are night-shift backlog, not PR verdict blockers
REFERENCES:
  - "[[adr/0275-stellar-review-scoped-incremental-rounds]]"
---

# ADR-0346: Context-unit findings are night-shift backlog, not PR verdict blockers

## Context

The CI stellar review scopes a PR by its changed files, so a small change
to a large file puts every unit of that file in scope (round 1 of PR #331:
a 7-line test change produced "scope: 25/25 units" and ~10 findings on
pre-existing code — none on the changed lines). Two defects followed from
counting those findings as the PR's own:

- **Verdict inflation.** Findings on context units — code the PR did not
  touch — failed the PR verdict, so pre-existing file debt blocked a
  trivial fix ("10 issue(s) open", verdict fail).
- **Duplicated output.** The poster printed the same findings twice:
  once under "New findings" and once under "Findings on context units",
  doubling the wall of text every consumer (human or agent) has to read.

The design already separates the two kinds: `anchorable(finding, hunks)`
tests whether a finding's lines intersect the PR's diff hunks, and
unanchorable findings ride the summary instead of inline comments
([ADR-0275](./0275-stellar-review-scoped-incremental-rounds.md) anchoring). The verdict, however, ignored that split.

## Decision

- A finding that does not intersect the PR's diff hunks is a **context
  finding**: real, recorded in the ledger, and drained by the stellar
  night-shift's clustered autopilot — but it is not this PR's finding.
- The PR verdict counts only **diff-anchored** findings (new or carried
  from earlier rounds); context findings never block a merge by
  themselves.
- The summary prints every finding **exactly once**: anchored findings
  under "New findings" (with inline comments), context findings once
  under "Findings on context units" — no duplication, so human and
  agent consumers pay for each finding once.
- `degraded` (the review API rejecting inline comments) keeps its
  current re-bucketing: those findings still count toward the verdict
  because they were anchored — only their delivery failed.

## Alternatives considered

- **Keep counting context findings in the verdict** (status quo).
  Rejected: a 7-line change to a debt-carrying file can never merge until
  the whole file is clean — the merge gate stops tracking the PR's own
  quality.
- **Drop context findings entirely.** Rejected: they are real findings on
  merged main; the night-shift queue exists precisely to drain them in
  small per-crate clusters. The ledger stays the source of truth.
- **Scope the review to diff-hunk-intersecting units only** (shrink the
  round itself in facts.py). Deferred: a deeper change to unit scoping;
  the anchoring split achieves the verdict goal without touching scope
  computation, and whole-file context still surfaces debt for the
  backlog.

## Consequences

- A PR's verdict reflects its own diff; context debt surfaces once per
  round as backlog, not as merge blockers.
- PRs on debt-heavy files become mergeable on their own merits; the
  night-shift backlog drains the debt in bounded clusters.
- Summary size roughly halves for context-heavy rounds — the primary
  consumer is an agent, and every duplicated finding costs context
  tokens on every CI round and every night-shift pull.
- The context-section findings still reach the ledger unchanged; no
  finding is lost or double-counted there.
