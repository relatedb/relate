---
type: adr
area: infra
claim: fa8fc5619d3af83f4d56f9c93b38d428f6b57e9c
name: 0444-stellar-finding-disposition
status: accepted
status_note: 2026-10-29
tags:
  - review
title: Stellar finding disposition — resolved threads deposed, non-critical findings deferred to Linear
REFERENCES:
  - "[[adr/0271-stellar-review-deterministic-pipeline]]"
  - "[[adr/0275-stellar-review-scoped-incremental-rounds]]"
---

# ADR-0444: Stellar finding disposition — resolved threads deposed, non-critical findings deferred to Linear

## Context

The stellar pipeline ([ADR-0271](./0271-stellar-review-deterministic-pipeline.md), [ADR-0275](./0275-stellar-review-scoped-incremental-rounds.md)) gives findings a ledger
lifecycle — open / resolved / waived, with a capped `resolved_tail` so
later rounds do not re-report or contradict accepted remedies — but no
GitHub-side disposition. Each round posts one review; the earlier
rounds' inline threads stay open on the PR forever, cluttering the
conversation and inviting humans to re-litigate findings the machine
already resolved.

Deferral of non-critical findings exists only as a manual AGENTS.md rule
(file a Linear issue tagged `review-tasks`, status `Agent Backlog`, for
the nightly stellar review job). The linkage — which finding became
which ticket — is recorded nowhere machine-readable, so a deferred
finding cannot be traced from the PR or from Linear.

Linear ids (team, project, labels, workflow states) are resolved by name
on every use today (`linear_queue.py` resolves team + label per run;
the filing CLI takes `--team <uuid>`).

## Decision

1. **Addressed findings depose their threads.** When a round's ledger
   marks a finding resolved or waived, the pipeline resolves the
   corresponding GitHub review thread (GraphQL `resolveReviewThread`),
   recording the thread id and finding id in the round's ledger block.
2. **Non-critical findings defer to Linear, with the linkage recorded
   both ways.** Low-severity findings (or a round's explicit defer list)
   become Linear issues in the `cypherlite` project, label
   `review-tasks`, status `Agent Backlog` — the state the work-picking
   queries and agent-backlog sweeps read (AGENTS.md). The PR comment
   names the Linear issue id; the issue body carries the finding id,
   PR number, and head sha.
3. **No id cache.** Linear ids keep being resolved by name on every
   use — the roundtrips are cheap and a cache can drift.

## Alternatives considered

- **Leave resolved threads open (status quo)** — rejected: PR
  conversations grow unbounded across rounds, and open threads invite
  re-litigation of findings the ledger already resolved.
- **Humans resolve threads manually** — rejected: with per-round bot
  reviews this does not scale, and observation shows it does not happen.
- **Read-side markers only** (the digest script's `superseded` flag,
  without touching thread state) — rejected: helps agents reading via
  API but leaves the GitHub UI cluttered for humans.
- **Machine-local id cache for Linear lookups** — rejected: ids are
  stable but a cache drifts silently when a name changes in Linear, and
  a stale label or state id makes tickets invisible — the exact failure
  the AGENTS.md Linear rules exist to prevent. Name resolution per use
  cannot go stale.

## Consequences

- Resolved/waived findings depose their threads deterministically; the
  ledger block gains thread ids, so the next round can verify the
  disposition.
- Deferred work is visible to the night-shift sweeps by construction,
  and every deferral is traceable from both ends (PR comment ↔ Linear
  issue).
- The CI bot gains the `resolveReviewThread` GraphQL write (and the
  existing Linear write path gains finding-id metadata).
- Implementation lands as its own PR, separate from the ADR.
