---
type: adr
area: engine
claim: bb44daa4764bab38cef073c905982d1c7d8b1a6d
date: 2026-11-21
deciders: Relate maintainers
name: 0654-profile-expression-memo-counters-and-bytes
status: accepted
tags:
  - executor
  - observability
  - perf
title: PROFILE surfaces expression-memo counters and table bytes
REFERENCES:
  - "[[adr/0245-query-latency-baseline-environment-and-gate]]"
  - "[[adr/0344-query-plan-node-tree-and-per-operator-metrics]]"
  - "[[adr/0345-plan-surface-alignment-cli-bolt-http]]"
  - "[[adr/0348-profile-metric-fields-db-hits-memory-bytes]]"
  - "[[adr/0463-expression-allocation-budget]]"
  - "[[adr/0640-executor-expression-memo-tiers]]"
---

# 0654: PROFILE surfaces expression-memo counters and table bytes

## Context

[ADR-0640](./0640-executor-expression-memo-tiers.md) installs memo tables in
the executor: a node-label memo per guarded state and value-keyed memos for
the string-to-temporal constructors. Each keeps a record of probes, hits,
inserts, refused, bytes and disabled, read today only by unit tests. Nothing
a user sees says whether a memo was installed, paid for itself, hit its byte
ceiling or gave up at its hit-rate review: exactly what a person tuning a
slow query, or judging the 16 MiB shared budget, needs. The `memory_bytes`
and `expression_built_bytes` PROFILE fields
([ADR-0348](./0348-profile-metric-fields-db-hits-memory-bytes.md),
[ADR-0463](./0463-expression-allocation-budget.md)) exclude memo tables, so
a memo's memory is metered but invisible.

PROFILE metrics already travel one typed path: executor counters stamped on
the plan tree, serialized once to the structured plan wire, read by every
surface from it
([ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md),
[ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md)). The plan wire's
convention for optional PROFILE facts is presence-only-when-used: the batch
flag and the zone-skip count are emitted only when true or nonzero, and the
frontier family only when it ran, all together or not at all, validated on
read. The CLI table follows suit: its node-read columns render only when
some operator has a nonzero count.

## Decision

1. **Counters.** Per memo, the existing facts in unchanged meaning:
   `probes`, `hits`, `inserts`, `refused`, `bytes` (the table's allocation
   when read) and `disabled`. Hit rate is derived by the reader. Warm-up
   inputs stay uncounted, as in ADR-0640.
2. **Memo bytes are their own field, `memo.bytes`, never part of
   `memory_bytes`.** Memo tables are cache, not result materialization;
   folding them in would change an accepted metric and make one query's
   figure depend on whether a memo disabled itself. The retained-container
   meter keeps charging them as now.
3. **Attachment.** Guarded-state memos belong to one traversal stage: their
   stats are summed per stage onto that clause's operator, with
   `disabled_count` (how many of the stage's memos gave up) in place of a
   per-memo flag. Constructor memos live in the query overlay and serve any
   expression, so attributing them to an operator would be a guess: they
   attach at the **plan root**, as the same sums plus a `kinds` list with one
   entry per memo type (a closed set of stable names derived from the type,
   such as `node-label` and `datetime`).
4. **Wire: presence only when used.** The plan wire gains one optional key,
   `memo`, defined once in the plan wire module with typed fields and the
   read-side validation the other groups have (all-or-none, non-negative).
   The producer emits it only on a PROFILE node whose memos were probed at
   least once; absent means no memo was used on that node. This follows the
   `batched`, `zone_skips` and `frontier` precedent and replaces any
   always-present, zero-filled shape: no all-zero groups on memo-free
   plans. EXPLAIN plans carry no metrics and no `memo` key. Every surface
   that renders `frontier` carries `memo` in the same change (Rust, C ABI
   plan JSON, Python and other clients exposing the plan, HTTP
   `profiledQueryPlan`, Bolt summary `profile`, agent plan), snake_case,
   numbers as numbers, with the surface specs and client references updated.
5. **CLI table: a new default `Memo` column**, shown like the other
   PROFILE columns but only when the statement used a memo (some operator
   or the root carries a `memo` group), never an always-present empty
   column. The cell reads `hits/probes`, suffixed `*` when any memo of that
   operator is disabled, and is empty where the node has none.
6. **Cost when PROFILE is off: zero added.** The counters are increments
   already on the memo path (they drive the hit-rate review); this adds no
   increment, atomic, thread-local or branch there. Reading, including the
   table walk behind `bytes`, runs only at profile finish behind the
   existing metrics sink, and the stats reader loses its test-only gating.
   The profiled path reads each stage's memos before the stage drops them
   and retains only the summed integers, never the tables; the non-profiled
   path keeps its drop-in-place.
7. **Memory impact of collecting and holding the stats is measured**, not
   assumed: see Acceptance.
8. **Compatibility.** Additive. An older client ignores the unknown key; a
   new client reading an older server sees it absent, which on a PROFILE
   node reads as "no memo used": the accepted cost of the presence-only
   convention. No existing field changes meaning.
9. **Landing.** One pull request: wire type, executor plumbing, every
   surface, the CLI column, specs, client references and examples together,
   so the surface-alignment rule holds at every commit that lands.

## Alternatives considered

- **Fold memo bytes into `memory_bytes`.** Rejected: see decision 2.
- **A statement-level total only.** Rejected: loses which stage's memo gave
  up, the most actionable fact.
- **Per-memo-instance rows.** Rejected: the count scales with the pattern
  and bloats every surface; sums plus `disabled_count` carry the signal.
- **Always-present zero-filled groups, and an opt-in `Memo` column.**
  Rejected: noise on every plan and an extra switch; presence-only matches
  the existing optional facts.
- **Count behind a profile flag.** Rejected: a hot-path branch, no saving.
- **A log, notification or procedure.** Rejected: not machine-comparable
  across surfaces, and memos are gone when the statement ends.
- **Two pull requests.** Rejected: a half-aligned surface set violates the
  result-metadata alignment rule.

## Consequences

- A user sees whether a memo was installed, its hit rate, and whether it
  was refused or gave up, without a debugger.
- `memo` joins the plan wire as an optional group every plan consumer must
  carry; the alignment cost is real but mechanical.
- Memo bytes sit outside `memory_bytes` and constructor-memo attribution at
  the root is approximate, both by design.

## Acceptance

- Unit: a memo driven past warm-up reports exact probes, hits, inserts,
  refused and disabled; `bytes` equals the table's allocation.
- Zero cost: a non-profiled run never calls the stats reader (test-only
  read counter); a memo-heavy query shows no p50 change outside noise.
- **Memory impact (required, measured numbers recorded in the change):**
  peak RSS, allocation bytes and retained stats bytes of a memo-heavy
  statement set, profiled against unprofiled and against the commit
  before, in the same environment. A regression beyond the
  [ADR-0245](./0245-query-latency-baseline-environment-and-gate.md) memory
  band blocks landing.
- Integration (and root): PROFILE of a high-fan-in guarded traversal shows nonzero
  `memo.hits`; a low-repetition query shows `disabled_count > 0`; a tiny
  budget shows `refused > 0` with rows identical to unprofiled; a
  memo-free plan carries no `memo` key and the CLI table no `Memo` column;
  repeated datetime strings show a `datetime` kind at the root.
- Equality: rows, `memory_bytes` and `expression_built_bytes` are identical
  with and without the memo reads.
- Surfaces: plan wire read and validate round trip; HTTP, Bolt, C ABI,
  Python and CLI JSON carry the same values for one fixed query; the table
  renders `hits/probes` and the disabled marker; doc examples run in CI.
