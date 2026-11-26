---
type: adr
area: engine
claim: cd26c633288fdd514a5924c74a9c22175e37ef3e
date: 2026-11-25
name: 0687-plan-cache-token-footprint-invalidation
status: accepted
tags:
  - perf
  - planner
title: A cached plan survives the commits it does not touch, judged by token footprint, cardinality drift and plan grade
REFERENCES:
  - "[[adr/0218-stats-based-match-orientation]]"
  - "[[adr/0503-template-keyed-plan-cache]]"
  - "[[adr/0541-resolve-tokens-to-ids-once-per-plan]]"
  - "[[adr/0651-plan-cache-hit-verification-without-front-reparse]]"
---

# ADR-0687: A cached plan survives the commits it does not touch

## Context

[ADR-0503](./0503-template-keyed-plan-cache.md) couples every cache entry to
one store-wide stats generation: `publish_data_state`
(`relate/relate/src/shared.rs`) advances it at the commit tail, the follower's
batch apply and graph replacement, and `admit_from_cache`
(`relate/relate/src/connection/cache.rs`) re-plans from the cached lowered AST
on any mismatch — because plans are built against live cardinalities, which
`GraphPatternStats` (`relate/relate/src/stats.rs`) reads under the publication
read guard ([ADR-0218](./0218-stats-based-match-orientation.md)).

The generation names no token: a commit inserting a `:Purchase` discards a
`:Person` point read's plan, paying the planner work ADR-0503 measured as the
point-read floor once per hot template per commit.

## Decision

1. **A per-token last-changed table.** Plans resolve labels, relationship
   types and property keys to the store's dense, append-only interner ids
   (`relate/relate-executor/src/store/interner.rs`; ids are never reassigned
   and are dense from 0, resolved at plan time by
   [ADR-0541](./0541-resolve-tokens-to-ids-once-per-plan.md)). A table of one
   `u64` slot per id, in the plan-cache module beside the stats generation,
   holds the plan generation at which each token last changed. An entry
   records its **footprint** (the ids it resolved) and its plan generation,
   and is clean while the newest `last_changed` over the footprint is not
   after that generation. The changed-token set comes from the mutation sites
   that hold the entity's version — creates and deletes report its label ids
   and a relationship's type id, a property set the interned key and its
   labels — and `Shared`'s commit tail folds it in.
2. **A value array, not a bitmap.** `PresenceBits`
   (`relate/relate-core/src/presence_bits.rs`: one bit per logical slot in
   `u64` words) carries membership only, and neither side of this check is
   one: the table stores a *generation* per token, and the footprint check
   asks "changed since this plan's stamp", not "changed in this commit". A
   per-plan bitmap needs a per-commit scan of every footprint on the write
   path, or a per-plan cumulative bitmap the commit tail ORs into —
   `token_count/64` words per entry, every commit — where one maximum over the
   3-10 ids the plan already carries costs the same handful of loads on the
   read path. The per-commit set is a handful of mutation-site ids, and
   `PresenceBits` has no clear, intersection or set-bit walk for the fold; no
   second bitmap type is introduced.
3. **Absent names keep the old rule.** A name resolved as absent has no id to
   stamp and a later commit can create it, so an entry carrying any absent
   resolution keeps ADR-0503's check — any commit since its stamp re-plans it.
   Entries whose every resolved name exists ride the footprint.
4. **A full-invalidation epoch for wholesale change.** A second, coarser
   counter advances on schema DDL (index, constraint and graph-type
   create/drop) and on graph replacement (a follower's reload installs a
   different graph instance, whose interner ids a cached footprint cannot be
   compared against). An entry carries the epoch it was planned at; a mismatch
   re-plans whatever the footprint says, and at replacement the token table is
   re-based so a replaced graph's ids never satisfy a check.
5. **Two-times cardinality drift marks a plan dirty.** An entry records the
   numbers it was planned against, per footprint token: label rows,
   relationship-type rows, and the equality and range cardinalities a seek
   consult read. On a footprint-dirty hit admission re-reads them under the
   same publication read guard and takes the larger of planned/current and its
   reciprocal; at **2.0** or above the plan is dirty, below it the entry is
   re-stamped at the current generation and serves. Planned values stay the
   baseline, so drift accumulates: operator choice follows the ordering of
   cardinalities, and 2x is the smallest move at which that ordering plausibly
   flips.
6. **A plan grade gates a dirty plan's re-plan.** Every entry measures the
   store reads one execution performs: the thread-local counters behind the
   PROFILE `Db Hits` total — `StoreReads`
   (`relate/relate-executor/src/metrics.rs`), the sum of hop expansions,
   neighbor walks and node reads — read once at statement end, never through
   the PROFILE sink. No per-input-row normalisation is taken: a statement
   entry drives its plan from exactly one root row, minted by the one
   statement-entry operation (`ExecGraph::begin_statement`), so the
   per-input-row quotient has the constant denominator 1 and the read total
   is the whole measurement. (An earlier draft threaded the row count from
   the entry to the probe through a statement-seeds token and a thread-local
   cell; the count is provably always one, so the channel and its token were
   deleted rather than kept for a generalisation nothing drives.) The
   counters are not always on: the hop and neighbor-walk counters move only
   while `STORE_PROFILING` (`relate/relate-executor/src/exec/profiling.rs`)
   is armed, which a PROFILE clause arm or a test build does and an ordinary
   statement does not, while the node-read counter is unconditional. The
   grade probe (`GradeProbe`,
   `relate/relate-executor/src/exec/plan_grade.rs`) therefore arms those two
   counters around the graded execution and restores the prior flag after it,
   so the reading spans the whole run and no other statement's counters move.
   The reading folds into an exponentially weighted average, smoothing
   **1/8**, against the baseline its first execution recorded; a first
   reading of zero establishes no baseline — a plan that read nothing has
   nothing to degrade from, and a footprint drift past the threshold already
   re-plans it. A dirty plan serves while that average stays below **2.0**
   times its baseline; at or above it the plan re-plans on that hit.
   Counters, not elapsed time; measured work also catches a shape that still
   looks right but degrades.
7. **The delete-token capture seam is the removal site.** A commit's changed
   tokens cannot be reconstructed from the write records afterwards: a
   delete's redo op carries only the entity id, and the dying labels,
   relationship type and property keys live only in the live version the
   removal tombstones. `Graph::delete_node` and `Graph::delete_rel`
   (`relate/relate-executor/src/graph/entity_mutation.rs`) hand the dying
   version's label ids, the relationship's type id and its property-key ids to
   the changed-token set at the count bump they already perform, before the
   tombstone lands. A delete that empties a label is exactly the commit a
   footprint check must see and no later walk can recover; a new removal path
   owes the same report.
8. **The outcome vocabulary is renamed, not extended.**
   `PlanCacheOutcome::ReplanGenerationMismatch` (`replan-generation-mismatch`
   on the wire) becomes `PlanCacheOutcome::Replan` (`replan`): a hit whose
   plan was rebuilt from the cached lowered AST before serving. The old name
   is false — epoch, degraded-grade and background re-plans are not generation
   mismatches — and the outcome owns the timing story (no parse, a fresh
   plan), not the cause. The wire vocabulary has no published users, so the
   rename is taken; `served_from_cache()` still answers true for it. The
   surface docs that spell the vocabulary, updated in the same
   implementation: `docs/api/rust/spec.md` (the variant list and `as_str`),
   `docs/api/rust/examples/phase_timing.md`, `docs/api/bolt/spec.md`,
   `docs/api/cli/spec.md`, `docs/api/http/spec.md` and
   `surfaces/relate-cli/src/skill/bundle/references/agent-safety.md`.
9. **The insert gates and the first two stages are unchanged.** The raw-text
   fast path, the auto-parameterized template, the 16-entry bound, the 64 KiB
   text bound, the dynamic-operand exclusion and the literal-shape guards are
   ADR-0503's and stand; so does
   [ADR-0651](./0651-plan-cache-hit-verification-without-front-reparse.md)'s
   eager hit verification — a re-plan is a clean admission.

## Alternatives considered

- **A per-plan index/constraint dependency set instead of the epoch.**
  Rejected: index dependencies multiply (composite components, type changes)
  and an under-recorded set serves a plan whose seek no longer exists; one
  wholesale epoch cannot under-invalidate.
- **Keep ADR-0503's any-commit invalidation**, or re-plan on every footprint
  touch. Rejected: the first is the coupling this decision removes, and the
  second is the same thing with extra bookkeeping — a label's cardinality
  moves with every insert. The drift threshold keeps a plan hot through small
  moves.

## Consequences

- A commit on tokens outside a plan's footprint leaves every hit in place; a
  footprint token with drift under 2x is re-stamped and serves; at or above it
  the plan is marked dirty and serves until its measured grade reaches 2x its
  baseline. A plan can thus serve data it does read, up to 2x drift and a 2x
  grade: a stale shape with a measured bound.
- Schema DDL and graph replacement re-plan every entry, as today; entries with
  an absent resolved name keep the per-commit check.
- One `u64` per distinct interned token of table, beside the 16 entries; the
  id space is append-only, so it grows only with new labels, types and keys.
- Tests: a commit on unrelated tokens keeps hits; a footprint commit with
  small drift keeps the plan; 2x drift plus a degraded grade re-plans; an
  epoch change re-plans; the raw and template hit paths are unchanged.
