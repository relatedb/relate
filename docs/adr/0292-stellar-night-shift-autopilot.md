---
type: adr
area: infra
date: 2026-10-14
deciders: CypherLite maintainers
name: 0292-stellar-night-shift-autopilot
status: proposed
tags:
  - review
title: Stellar night-shift autopilot and stale-ledger-proof scoping
REFERENCES:
  - "[[adr/0271-stellar-review-deterministic-pipeline]]"
  - "[[adr/0275-stellar-review-scoped-incremental-rounds]]"
---

# 0292. Stellar night-shift autopilot and stale-ledger-proof scoping

- **Renumbered**: from 0243 (shared-number cleanup)

## Context

[ADR-0271](./0271-stellar-review-deterministic-pipeline.md)/[ADR-0275](./0275-stellar-review-scoped-incremental-rounds.md) made stellar rounds incremental, but two leaks
remain. (1) After a force-push or rebase the ledger's `last_head` is
unreachable or diverged; `git diff last_head head` then either fails or
pulls main's unrelated commits into the round, and unit hashes moved by
the rebase look "changed", so scope balloons (891+ units). (2) Findings
carried open in ledgers only surface when a human opens the next PR on
the same files; there is no durable queue and no background worker.

## Decision

1. **Changed-files focus is a script invariant.** `compute_scope` treats
   a unit as changed only if it differs from the merge base *and* from
   the ledger; a rebase can no longer dirty code the PR did not touch.
   `tools/incremental.py` computes the anchoring diff from the smallest
   covering base: `last_head` if an ancestor, else
   `merge-base(last_head, head)`, else none (empty patch, ledger-hash
   scope) — never the main merge-base — restricted to PR-changed files.
2. **Night-shift planner.** `tools/nightshift.py` clusters carried-open
   findings from ledgers into queue items: one crate per cluster, split
   at 8 files, deduplicated, blockers first, each tagged with a test tier
   (`unit` inside one non-shared crate, `full` otherwise). Clusters that
   touch files under human work are held, not dropped. The planner only
   emits runnable work inside 03:00–07:00 Europe/Berlin (DST-aware) unless
   forced; 25+ open findings sets `escalate`.
3. **`stellar-nightshift.yml`** runs hourly 01:00–06:00 UTC plus manual
   dispatch, gates on the window, plans, syncs the queue to Linear, and
   works one cluster per night: the model step edits files only (no
   token); `nightshift.py check-scope` rejects edits outside the
   cluster's crates; the cluster's fast-tier commands (fmt, clippy,
   `cargo test -p …`, or the workspace for shared/multi-crate clusters)
   must pass; scripts then commit one commit on `nightshift/<key>`,
   push, and open the PR. Existing `nightshift/<key>` branches/PRs are
   skipped (work in flight).
3c. **Worker split.** The model runs in a `fix` job with read-only
   permissions and no GitHub token; it emits a patch artifact after the
   fast-tier checks. A separate `publish` job (write token, fresh
   checkout) re-checks scope, applies the patch with hooks disabled,
   pushes, and opens the PR, deleting the branch if PR creation fails.
   `NIGHTSHIFT_TOKEN` (optional PAT/App token) makes the PR trigger CI;
   GITHUB_TOKEN-created PRs do not. The scope verifier and the cluster
   commands run from a pristine checkout of the tools (git worktree of
   the base ref), never from the model-edited tree. Residual, accepted
   risk: hosted-runner egress is not restricted, and the model step holds
   `FIREWORKS_API_KEY`; the prompt text it consumes is limited to
   human-authored Linear issue bodies (trusted clusters only).
3b. **Trust boundary.** Ledger text rides PR comments and is
   attacker-influenceable, so the write-privileged worker only takes
   `trusted` clusters (hand-filed `review-tasks` issues with a `Files:`
   scope). Ledger clusters are filed to Linear for a human to promote
   (add a `Files:` section and drop the marker). Ledger paths must match a
   strict workspace-path shape before reaching any shell command or edit
   scope, and the anchoring diff uses `--literal-pathspecs`. Residual
   risk: runner egress is not blocked; the model step holds only
   `FIREWORKS_API_KEY` and no GitHub token.
3a. **The durable queue is Linear issues labelled `review-tasks`**
   (`tools/linear_queue.py`, `LINEAR_API_KEY` repo secret). `sync` files
   one issue per cluster, idempotent via a `[stellar-ns:<key>]` title
   marker (a canceled issue is never re-filed); `pull` lists every open
   `review-tasks` issue, including hand-filed ones. A hand-filed issue is
   worked when its description has a **`Files:`** section (heading line,
   then a list of repo paths under `cypherlite/`, `surfaces/` or
   `testing/`, at most 8, no `..`): that list becomes the cluster and the
   edit scope the worker is held to. Issues without one, or with more than
   8 files, are reported as skipped with the reason in the run summary.
4. **Prevention over review.** The top recurring patterns from the
   #263/#278/#288/#295 rounds are codified in AGENTS.md and the stellar
   checklist; `tools/recurrence.py` reports recurrence per checklist id.
5. **Squash before review.** Stellar rounds assume one squashed commit
   per PR; `stellar-prep.sh` emits a CI warning when a PR has more than one commit.

## Alternatives considered

- **Always diff against the main merge-base** — rejected: it is exactly
  the stale-ledger re-scope this ADR closes.
- **Cron pinned to one UTC hour** — rejected: CET/CEST shifts the window
  by an hour; the window check uses `zoneinfo` instead.
- **Autopilot writes fixes in this change** — rejected: unattended
  branch writes need their own safety design (token scope, test-tier
  runner); queue first, worker next.

## Consequences

- A rebased PR re-judges only what it changed; stale ledgers degrade to
  smaller scope, never larger.
- The queue is a deterministic, testable artifact; the worker can be
  added without changing the planner.
- Recurring findings become written rules (AGENTS.md "Code-shape rules",
  checklist "Recurring-pattern checklist"); `tools/recurrence.py` counts
  findings per checklist id so the recurrence drop is measurable.
- `tools/passstats.py` measures model economics: per-pass yield and a
  strong-vs-cheap miss rate (acceptable ≤ 15% on ≥ 20 baseline findings).
  The numbers need real runs; no tier change is made by this ADR.
- Open: re-squashing fix rounds on autopilot branches, and closing the
  Linear issue when its PR merges.
