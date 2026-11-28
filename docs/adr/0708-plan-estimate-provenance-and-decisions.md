---
type: adr
area: engine
name: 0708-plan-estimate-provenance-and-decisions
claim: f0c7cb99778dc9b99084edc69f67b1f414458c37
status: proposed
tags:
  - observability
  - planner
title: EXPLAIN and PROFILE show the planner's estimate, its provenance, and its decisions
REFERENCES:
  - "[[adr/0218-stats-based-match-orientation]]"
  - "[[adr/0344-query-plan-node-tree-and-per-operator-metrics]]"
  - "[[adr/0538-traversal-rung-observability]]"
  - "[[adr/0582-frontier-boundary-pair-and-decline-reason]]"
  - "[[adr/0694-default-selectivity-defaulted-estimate]]"
  - "[[adr/0701-hub-join-cost-gate]]"
---

# ADR-0708: EXPLAIN and PROFILE show the planner's estimate, its provenance, and its decisions

## Context

The planner prices every pattern element on one ladder (`endpoint_estimate`):
a bound variable, an index equality or range window, a sampled `CONTAINS`
count, a default-selectivity estimate ([ADR-0694](./0694-default-selectivity-defaulted-estimate.md)),
label rows, then unmeasured rank sentinels. Its choices (the start element,
the direction, the hub join, the seek against the scan, the kernel rung) all
read that one ladder.

EXPLAIN does not. `EstimatedRows` on a `MATCH` operator is re-derived in the
plan-node builder from a narrower rule (an index seek, a lone label, a single
unfiltered relationship type) and is absent for everything else. A pattern
the planner prices by a default shows no number; one it prices from a
measurement shows the same number as one it guessed. When a plan is slow the
reader cannot tell whether the planner mis-priced an element, priced it from a
default, or priced it right and chose a strategy that declined at run time.
[ADR-0538](./0538-traversal-rung-observability.md) and
[ADR-0582](./0582-frontier-boundary-pair-and-decline-reason.md) name the rung
that ran and some run-time declines, not the choice that preceded it.

## Decision

### Changed

1. **One source for the estimate.** `EstimatedRows` on a `MATCH` operator, and
   on each fused hop child, is the value the planner's ladder produced for that
   element, read from the planner's own result. The plan-node builder keeps no
   second derivation. A test pins that EXPLAIN's estimate equals the planner's
   pick for the same statement and statistics.
2. **Provenance per estimate.** Each estimate carries `EstimateProvenance`,
   one kebab-case name per planner `Estimate` variant, derived one-to-one from
   it (a new variant is a compile error in the one mapping, not a silent gap):
   `measured` (a bound variable, an index equality or window, label rows),
   `sampled` (a sampled `CONTAINS` seed), `defaulted` (the default-selectivity
   table, [ADR-0694](./0694-default-selectivity-defaulted-estimate.md)), and
   `statistic` once the key-statistics rung lands. An element the ladder could
   only rank (no count) omits `EstimatedRows` and `EstimateProvenance`. This is
   the ladder level, distinct from the key statistics' own trust label.
3. **Start candidates.** A `MATCH` operator with more than one possible start
   carries `StartCandidates`: a list ordered as the planner ranked them, each
   with the element index, `EstimatedRows`, `EstimateProvenance`, and a `Chosen`
   flag set on exactly one entry.
4. **Decisions.** A `MATCH` operator carries `Decisions`, a list of typed
   records, one per gated choice the planner or the run-time gate made for the
   clause. Each record has a kebab-case `Kind` (`hub-join`, `seek-or-scan`,
   `range-crossover`, `frontier`, ...), an `Outcome` (`engaged` or `declined`),
   the costs the gate compared when it compares costs, and, for a decline, a
   typed `Reason`. The vocabulary of `Kind` and `Reason` is one table, shared
   with [ADR-0582](./0582-frontier-boundary-pair-and-decline-reason.md)'s
   decline names, which this extends to every traversal rung and fold: the first
   decline per clause and rung wins.
5. **PROFILE pairs estimate with actual.** A profiled `MATCH` adds `QError`,
   `max(est/actual, actual/est)` over the operator's `EstimatedRows` and
   `Rows` (both floored at one), only when both exist.
6. **Presence-only.** Every key is emitted only when the fact exists, like the
   existing metric facts, and every surface that carries the structured plan
   (the Bolt summary, the HTTP plan, the MCP tool, the C ABI and each client,
   the text plan's `Details`) carries the same keys with the same spelling.
   Result metadata stays aligned across surfaces.

### Delivery

The change lands in slices that each stay inside this vocabulary: the estimate
and provenance first (1, 2), then candidates, decisions and `QError` (3, 4, 5),
then the typed decline reasons per rung (4's `Reason`).

## Consequences

- A slow `MATCH` is diagnosable from the plan: the estimate says what the
  planner believed, the provenance says how much to trust it, the candidates
  and decisions say what else it considered and why a strategy did not run.
- The estimate no longer disagrees with the planner by construction, so a
  planner change moves EXPLAIN with it.
- Plans for patterns the old rule could not price now show a number, so plan
  snapshots in tests and docs change.
- The wire gains optional keys; consumers that ignore unknown keys are
  unaffected. The surfaces' client references and the Python library skill
  document them in the same change.

## Alternatives considered

- **Extend [ADR-0538](./0538-traversal-rung-observability.md) or
  [ADR-0582](./0582-frontier-boundary-pair-and-decline-reason.md) in place.**
  Rejected: both are about what ran, at run time; the estimate, the candidates
  and the plan-time gates are one new family with their own keys and a shared
  vocabulary, and are easier to find and amend as one record.
- **Only show `EstimatedRows` for more shapes.** Rejected: a number without
  its provenance reads as a measurement when it may be a default.
- **A free-text `Why` string per decision.** Rejected: strings cannot be
  asserted in tests or matched by agents; typed names can.
