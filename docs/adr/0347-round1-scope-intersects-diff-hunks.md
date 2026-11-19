---
type: adr
area: infra
claim: 025fa713111ff02044628f6aa1fd08a356ba9952
name: 0347-round1-scope-intersects-diff-hunks
status: accepted
tags:
  - review
title: Round-1 review scope intersects the diff hunks
REFERENCES:
  - "[[adr/0271-stellar-review-deterministic-pipeline]]"
  - "[[adr/0275-stellar-review-scoped-incremental-rounds]]"
  - "[[adr/0346-context-findings-backlog-not-verdict]]"
---

# ADR-0347: Round-1 review scope intersects the diff hunks

## Context

[ADR-0271](./0271-stellar-review-deterministic-pipeline.md)'s scripted pipeline scopes a PR's first round to **every unit of
every changed file** — the file is the unit of change. PR #331 showed the
cost at line scale (a 7-line change → "25/25 units", ten findings on
untouched code); PR #337 showed it at file-set scale (a ~500-line change
across 7 files of one crate → "129/129 units", 15 findings, most on code
the PR never touched). [ADR-0346](./0346-context-findings-backlog-not-verdict.md) (PRs #332/#333) fixed the *verdict* half —
findings whose ranges do not intersect the diff hunks are context: never
inline, never merge blockers — but the *scope* half remained whole-file:
the pass agents still judge, and burn model tokens on, every unit of a
changed file.

## Decision

Round 1 scopes to the units the PR's diff hunks actually touch. A unit is
in scope when its line range intersects a hunk of its file (the same
canonical per-file hunk table (`facts.hunks_by_path`) and
range-intersection predicate (`facts.range_in_hunks`) that [ADR-0346](./0346-context-findings-backlog-not-verdict.md)'s
verdict anchoring uses — one table, one predicate, both consumers), or
the unit is new. Units of a changed file that no hunk touches are **sealed in
round 1**: they are context — carried findings and night-shift backlog
per [ADR-0346](./0346-context-findings-backlog-not-verdict.md), never first-round judgment.

Rounds ≥ 2 keep [ADR-0275](./0275-stellar-review-scoped-incremental-rounds.md)'s incremental contract unchanged (changed-since-
last-round via the ledger's hashes), as does the dependency-dirty
expansion inventory (its files dirty whole, hunks or no hunks) and the
resolution-check machinery (open findings re-checked when their unit,
sibling unit, or file changes).

## Alternatives considered

- **Keep whole-file round-1 scope** ([ADR-0275](./0275-stellar-review-scoped-incremental-rounds.md) as-is) — rejected: it is
 the source of the PR #331/#337 noise and token spend; [ADR-0346](./0346-context-findings-backlog-not-verdict.md) already
  established that off-hunk findings do not gate the PR, so judging them
  in round 1 produces context the verdict then discounts.
- **Shrink scope to changed *lines* only, ignoring unit ranges** —
  rejected: the pass briefs judge units, not lines; a unit whose range a
  hunk clips must be judged whole, or the agent sees a fragment with no
  signature or invariant context.
- **Scope to changed units in round 1 too (hash-diff vs merge base)** —
  rejected: round 1 has no ledger baseline to hash against; the hunk
  intersection is the only diff-derived fact available at first judgment,
  and it is exactly the fact the verdict already trusts.

## Consequences

- Small changes to large files get small first rounds: PR #331's shape
  would scope ~1-2 units instead of 25; PR #337's would scope the ~30-40
  hunk-touched units instead of 129.
- Findings on untouched regions of a changed file can no longer appear as
 round-1 output at all (they were already non-blocking post-[ADR-0346](./0346-context-findings-backlog-not-verdict.md)) —
  they surface only via the night-shift's own passes, which is the drain
  that backlog was designed for.
- A file-level fact (`flag-1k-crossing`, file growth) stays file-scoped:
  its finding needs no unit in scope; the checklist flag exempts it from
 hunk anchoring ([ADR-0346](./0346-context-findings-backlog-not-verdict.md), unchanged).
- Sealing in round 1 means a later push that touches those units still
  unseals them (round ≥ 2 hash rule) — nothing is permanently unreviewed;
  it is reviewed when it changes.
- The scope stats (`units_in_scope`, `units_sealed`) now diverge from
  `units_total` in round 1; consumers must not assume
  `units_in_scope == units_total` for a full round (the one test that did
  was updated to the new contract).
