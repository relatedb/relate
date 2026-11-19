---
type: adr
area: engine
claim: cae854af18c14b1a5d0b57fc69cd643e82c3d67d
name: 0399-per-phase-profile-times
status: accepted
tags:
  - observability
title: The per-phase PROFILE times — seek and walk inside the fused operators
REFERENCES:
  - "[[adr/0375-profile-walk-start-and-store-read-kinds]]"
  - "[[adr/0397-reads-cell-legibility]]"
---

# ADR-0399: The per-phase PROFILE times — seek and walk inside the fused operators

## Context

LABS-343 item 1 named the last PROFILE measurement gap against Postgres'
`EXPLAIN ANALYZE`: our table is clause-granular, so one `Expand` row
covers the phase that resolves the pattern's start elements AND the phase
that walks the pattern. "Of this operator's work, how much was the seek
vs the walk?" was unanswerable as a time.

The obvious fix — splitting the compound operators into pipeline stages,
one plan node per phase — was rejected up front: the fused operators are
deliberate (no intermediate row buffer between the candidate resolution
and the traversal; optimizations like the degree-tail work target exactly
the fused form), and splitting them would regress the queries the fusion
made fast.

The facts were already half-measured: the read-kind mix ([ADR-0375](./0375-profile-walk-start-and-store-read-kinds.md))
decomposes `Db Hits` into `NodeReads` (the seek's store work) and
`Hops`/`NeighborWalks` (the walk's) — the phase *counts*. What was missing
was the phase *times*.

## Decision

Stamp the phase boundary **inside** the fused operators — a time
attribution over the same work the counters already count, with no
structural split:

- Two thread-local sums in the executor (`SEEK_NS`/`WALK_NS`), armed only
  while a PROFILE statement's clause arm runs (`PHASE_PROFILING`, set by
  the clause's metric tick, disarmed by its drop): a non-PROFILEd
  statement pays one thread-local read per stamp attempt and never an
  `Instant`.
- The stamps live at the walker boundaries every path shares: the
  candidate **resolution** wrapped in one `phase_seek` helper (the one
  spelling every walker resolves through — expand, pivot, gpm, agg_fold,
  the generic walk), and the relationship-walk entries. The
  per-candidate **admission checks** stay unattributed arm time, like the
  binding and cloning: the seek pairs with the `NodeReads` list
  production, not with every read the arm makes.
  (`expand_rel`, `match_backward`, `gpm_walk_elements`, `walk_rel`, the
  folded aggregation's neighbor traversal). **Inclusive** semantics, like
  the table's `Total (ms)` column: a walk that triggers nested resolutions
  counts both, so per-phase sums can exceed an operator's own wall clock
  share without double-counting the store.
- The phases ride the metric block on the wire (`SeekTimeNs`/
  `WalkTimeNs`, emitted with the kinds), and the `cypherlite.plan/v1`
  document's per-node `actual` gains `seek_ms`/`walk_ms` (additive; the
  kinds stay where they are — the phases add only the time attribution,
  never a second spelling of a count). The document also names `loops`
  (the input clause's rows — the number the table shows one cell up) for
  consumers that do not walk children; `null` at the root (the seed row).

The arm's remainder — binding, residual checks, row cloning — stays
unattributed: `seek + walk` is the store-work share of `time_ns`, not all
of it.

## Alternatives considered

- **Split the compound operators into pipeline stages** (the physical
  breakdown) — rejected: the fusion is the optimization; per-row
  materialization between the phases would regress the degree-tail
  queries (the reason LABS-426 optimized the fused form).
- **Per-phase ROWS, not just times** — rejected: rows is a single
  counter per clause arm (its output); attributing it per phase would
  need per-phase row counters on the hot path for a fact the read kinds
  already carry in spirit.
- **A dedicated phase-plan column family in the table** — rejected: the
 table's kind columns (`Nodes`/`Rels`/`Degree`, [ADR-0397](./0397-reads-cell-legibility.md)) already show
  the counts; two more time columns would push the widest shape past
  readability. The times live in the machine modes, where the phases'
  composition with the kinds is stated.
- **Stamping per row instead of per span** — rejected: the walkers
  interleave per candidate; one span per walker invocation (plus one per
  admitted walk entry) is the granularity that matches the counters'
  per-clause deltas without per-row `Instant` reads.

## Consequences

- Every PROFILE statement's operators carry the phase times; a clause
  that does no pattern work reports zeros (never absences — the stamps
  exist for every profiled clause).
- The inclusive semantics are documented on the wire fields and the
  document: `seek_ms + walk_ms` can exceed the operator's `time_ms`
  share of unattributed work being counted under both phases — the
  subtree-nesting double-count is bounded by the plan's depth.
- The stamps add one `Instant::now` + `elapsed` per walker span under
  PROFILE only; the non-PROFILE path gains one thread-local bool read at
  each span boundary (the same class as the existing counter bumps).
- The phase vocabulary (`seek`/`walk`) is the presentation vocabulary the
  kind columns established: `seek` pairs with `Nodes`, `walk` with
  `Rels`/`Degree`.
