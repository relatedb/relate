---
type: adr
area: cypher
claim: 5749b367c08d08823d4e88b8e94ca11c05c7b336
date: 2026-11-24
deciders: Relate maintainers
name: 0680-search-match-filter-semantics
status: proposed
tags:
  - dialect
  - search
title: |-
  Fulltext search procedures: the await refresh call and the
  eventually-consistent rejection
REFERENCES:
  - "[[adr/0497-catalogue-corpus-alignment]]"
  - "[[adr/0678-search-complex-patterns-and-entity-constrained-window]]"
  - "[[adr/0463-expression-allocation-budget]]"
  - "[[adr/0657-executor-memo-budget-open-setting]]"
---

# ADR-0680: Fulltext search procedures: the await refresh call and the
eventually-consistent rejection

## Context

The 2026-11-24 re-sync of Neo4j's Cypher-25 acceptance corpus brought
the fulltext SEARCH expectations onto the engine. ADR-0678 (merged as
PR #784) landed the window semantics, the inner `WHERE` on full-text
sources and the entity match filter — that work is the window's record.
Two expectations remained unimplemented when that landed, both verified
against a live Neo4j 2026.10.2 enterprise container on decision day
(live still answers the old way in each case — the corpus pins the next
release, and the corpus is the conformance target):

1. **The fulltext acceptance background now calls
   `db.index.fulltext.awaitEventuallyConsistentIndexRefresh()`** — the
   procedure ADR-0497 registered as catalog-only ("no corpus scenario
   CALLs it"). The call must succeed; on the engine it failed
   `ProcedureNotFound`.
2. **A SEARCH over an eventually-consistent fulltext index now raises a
   typed rejection.** The engine accepted it and scanned; live 2026.10.2
   answers such a call with `IndexDoesNotExist` (the index exists and the
   `db.index.fulltext.query*` procedures reach it), so the corpus's
   explicit "eventually consistent index not supported with `SEARCH`"
   rejection is ahead of live.

The same sync also exposed a memory-shape gap in the fulltext corpus
memo the SEARCH clause and the procedures share: it kept exactly one
live corpus, so a `COMBINE` alternating two sources rebuilt each corpus
on every input row, while the procedures' per-row `indexName`
evaluation is runtime data — an `UNWIND` over index names must never
turn into unbounded retained memory.

## Decision

1. **`db.index.fulltext.awaitEventuallyConsistentIndexRefresh` becomes an
   executable void no-op.** The engine applies committed index updates
   synchronously at the apply point, so the wait is trivially complete —
   the same contract `db.awaitIndexes` already carries. The signature
   keeps its catalog metadata; only `executable` flips. ADR-0497's row
   gains a Later-references note.

2. **A SEARCH over an eventually-consistent fulltext index fails with
   the typed `EventuallyConsistentIndexNotSupported` rejection.** The
   rejection sits at the SEARCH arm's own resolution entry
   ([`fulltext::resolve_for_search`]): one catalog read, the policy
   raised inline. The `db.index.fulltext.query*` procedures remain the
   supported surface for such an index, exactly as live serves them —
   their resolution entry ([`fulltext::resolve_analyzers`]) is
   untouched.

3. **The corpus memo is bounded twice over.** One corpus per distinct
   `(index, analyzer)` up to a constant slot cap (16) — a COMBINE
   alternating sources hits every slot instead of thrashing one — under
   the kernel memos' own byte ceiling (`MemoPolicy::moderate`). Every
   charge is the kernel memo's own three-part protocol
   (`Memo::remember`): through the statement's retained-container meter
   (ADR-0463), into the query's shared live-bytes counter (ADR-0657) —
   so the joint per-statement bound sees the corpora, an evicted slot
   releases its share at once, and the memo's drop returns the rest —
   and into PROFILE's memo group, the retained bytes folded at the
   memo's drop (`MemoState::drop`'s own fold, ADR-0654). A refusal on
   either meter is not an error to the query: the memo freezes (the
   entry stays, the overshoot is at most one corpus, the overshooting
   walk never runs again — a frozen memo's misses cost a build and
   nothing more). Eviction is least-recently-used over the shared
   `RecencySlots` table (also the regex pattern cache's policy,
   extracted so one spelling serves both), and every counter moves by
   the same net delta — the new corpus against the evicted refund — so
   each running total always equals the retained set's sum.

## Alternatives considered

- **Carry the eventually-consistent fact on the resolved scan** every
  caller holds. Rejected: it is SEARCH-only surface policy; the
  procedures admit such an index, so the flag would ride one consumer's
  decision on every resolved instance.
- **Read the stored option in the SEARCH orchestration after the shared
  resolution.** Rejected: a second catalog read per input row. The
  policy belongs at its own resolution entry, which performs the one
  read.
- **A single-slot memo with an LRU refresh.** Rejected: still rebuilds
  every alternating source per row (the COMBINE thrash), and bounds
  nothing in bytes — the procedures' per-row index names are runtime
  data, and a byte bound needs the charge, not just a slot count.
- **Gate the new semantics behind a preview row.** Rejected: the corpus
  pins both in untagged scenarios; these are the surface's semantics,
  not an opt-in.

## Consequences

- `docs/reference/cypher-syntax.md`'s SEARCH rules gain the
  eventually-consistent rejection line; the procedure is
  `db.index.fulltext.awaitEventuallyConsistentIndexRefresh`'s
  executable flip, pinned beside the `db.index.fulltext.*` family.
- The memo's byte bound is a slot-count cap plus a charged ceiling —
  the two bounds together, whichever the next insert would cross first
  stops the growth; the charging follows the kernel memos' own meter
  contract so a fulltext statement cannot pin unbounded uncharged
  memory.
- The acceptance harness (a separate repository) rides this change: the
  scope entries it carried for the retired complex-pattern preview gate
  are already retired by ADR-0678's merge (its PR #84), and its
  fulltext acceptance file now passes ungated against this engine.
- Live 2026.10.2 evidence, recorded here, becomes stale the moment a
  Neo4j release ships the corpus behavior: the next re-verification
  round should re-probe both expectations against it.
