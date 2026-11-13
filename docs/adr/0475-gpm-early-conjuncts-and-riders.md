# ADR-0475: Early conjunct evaluation and riders in the GPM walk

Status: Proposed

Claim: 9279b680b2ca11ab1bc47174bb472d577202c5e7

## Context

ADR-0471 made the GPM quantified traversal the one engine for every
var-length spelling; its Consequences record the cost: "the exists probe
loses its early-exit for var-length shapes — a follow-up candidate."
Two further costs of the post-filter-only discipline remained beside it:

- Every plain MATCH-WHERE conjunct (the non-fold majority — `m.keep =
  true`, `t.q > 1`) is evaluated once per *complete* path, after the
  walk has already enumerated the whole failing prefix behind it. The
  allReduce inliner (ADR-0469) prunes mid-path for its fold conjuncts
  only; a failing plain conjunct on a mid-pattern node still pays the
  full tail — the `early_tests.rs` mid-gate fixture walks the failing
  mid node's four-path tail in full only for the post-filter to drop
  every row of it.
- `LIMIT` truncates only after the match has enumerated everything: the
  projection's own truncation keeps the walk-order prefix, but the walk
  behind it expanded the whole graph first.

And one correctness trap surfaced when early evaluation was first tried
under a path selector: the selective path patterns suite, scenario 8
(`testdata/cip-eval/gpm-graph-patterns/05-selective-path-patterns.feature`)
— a MATCH-WHERE predicate on a selective pattern is a **post-filter
over the selector's chosen paths**: `MATCH p = ALL SHORTEST (…) WHERE
q1.blocked = true` returns *empty*, because the blocked route is still
the selected shortest path and the predicate then narrows it to nothing.
Pruning the walk mid-path would remove candidates *before* selection and
change which path the selector picks — the incident that forced the
selector stand-down in this decision.

## Decision

**The GPM walk evaluates qualifying MATCH-WHERE conjuncts at the walk's
binding points and aborts the failing branch; a MATCH cap or an EXISTS
probe can ride the walk as a consumer that halts the enumeration. The
post-filter stays in place in every form.** Four mechanisms, one
discipline — ADR-0469's exactness rule generalized from allReduce folds
to plain conjuncts:

1. **Early conjunct evaluation** (`exec/gpm/early.rs`). A top-level
   `AND` conjunct of the MATCH WHERE qualifies only when every scope
   name it references is either bound in the entering row or binds
   exactly once inside the pattern — a top-level node element's variable
   or a plain (non-var-length, non-quantified) relationship's — and
   never a quantified group's name (rebound per iteration; the
   post-filter reads the last binding) or the pattern's path variable
   (bound after the whole pattern). A qualified conjunct is evaluated
   the moment its last name binds **non-null** (the binding event: a
   node accept, a rel accept, a target accept), pre-hop at the anchor
   side; a false or null verdict drops the state there — the **branch
   abort**: the prefix behind it enumerates no further hops. Null
   bindings never decide: a null binding of a pattern variable may be a
   non-local singleton pre-binding (ADR-0335) or a deleted
   entity's tombstone, and either value differs from the post-filter's —
   such a conjunct stays a post-filter.
2. **Errors defer, never prune — walk-wide.** A conjunct that raises is
   deferred for the rest of the walk: skipped at every later binding
   point, never re-evaluated early, never pruning. The untouched
   post-filter reproduces the error on whatever rows survive, so which
   errors surface never changes (ADR-0469's poison rule, carried from
   allReduce folds to plain conjuncts).
3. **Selector stand-down.** Under any path selector (`ANY`, `SHORTEST
   k`, `ALL SHORTEST`, …) no conjunct evaluates early and the ADR-0469
   allReduce inliner stands down too: a selector chooses among the
   pattern's FULL candidate set and a MATCH-WHERE predicate narrows the
   already-selected path (selective-path scenario 8) — mid-path pruning would
   change which path the selector picks. Every conjunct stays a
   post-filter.
4. **Riders** (`exec/gpm/emit.rs`). With no selector choosing across
   candidates, a consumer rides the walk itself: every complete state
   flows through exactly the per-candidate pipeline the materializing
   path runs afterwards (path mode, path variable, path predicate,
   deferred element predicates, reduce checks, cap count, projection),
   and the walk halts the moment the rider is answered:
   - the **EXISTS probe rider** — GPM patterns (every var-length
     spelling since ADR-0471) probe through the GPM walk and the first
     accepted row halts the enumeration, **restoring the early exit
     ADR-0471's Consequences record losing**. The probe is a full
     pattern walk (the clause-engine pass it replaces), so it ticks the
     nested-evaluation budget once per evaluation, keeping the security
     bound on compounding EXISTS chains. A selector pattern keeps the
     materializing walk and answers from the chosen rows — no early
     exit, the same verdict.
   - the **walk-halting LIMIT cap** — an un-ordered
     `RETURN/WITH … [SKIP s] LIMIT n` halts the walk at `s + n` emitted
     rows (`plain_limit_cap`, the third cap shape beside the distinct
     and ordered ones, all sharing one suffix parse). LIMIT without
     ORDER BY keeps an unspecified prefix — the engine's deterministic
     enumeration order, exactly the prefix the projection's own
     truncation keeps — so halting the walk at `SKIP+LIMIT` reproduces
     the same rows without enumerating the rest. Declines: DISTINCT
     (the distinct cap's shape), any `ORDER BY` (the ordered cap's
     shape — a sort's top rows are not the walk's prefix), an explicit
     `GROUP BY`, a following `WHERE`, `*`, and aggregate items (grouped
     output is not one row per match).

Each conjunct is evaluated exactly once per binding: within one
rel+target accept the rel variable binds first, so the rel event runs
before the target accept and a conjunct naming both variables becomes
ready only at the target's own event.

## Alternatives considered

- **Post-filter only (status quo).** Keep every plain conjunct a
  post-filter over completed paths. Rejected: it forfeits the branch
  abort — the mid-gate fixture's failing branch pays its whole tail —
  and leaves ADR-0471's recorded EXISTS early-exit loss and the
  full-enumeration-before-LIMIT cost in place.
- **Prune under selectors as well** (apply the early evaluation
  uniformly, selector or not). Rejected: the selective-path scenario pins the
  opposite semantics — the predicate narrows the already-selected path
  and can return nothing where pruning would hand the selector a
  different path; the answers differ, not just the cost.
- **A second probe engine beside the GPM walk for the EXISTS early
  exit.** Rejected: ADR-0471's own lesson — two engines for one
  language is the spelling-dependent trap the unification removed; the
  probe rides the one walk instead.
- **Evaluate every ready conjunct at every binding point** (no
  binding-event filter). Rejected: a conjunct whose bind-once inputs
  cannot have changed would be re-evaluated per hop — the per-hop cost
  the early evaluation exists to remove, put back.
- **Memoize evaluated conjuncts per row state** (a table keyed by row,
  to deduplicate the rel+target accept point). Rejected: rows are
  cloned per branch of the DFS, so the memo costs more than the
  duplicate evaluations it removes; the binding-order rule (rel event
  before the target accept) gives exactly-once for free.

## Consequences

- A failing plain conjunct aborts its branch before the tail enumerates:
  the PROFILE hop counter shows the early run expanding strictly fewer
  hops than both the post-filter spelling and the evaluation-disabled
  run of the same statement, and the evaluation counter shows strictly
  fewer evaluations than the once-per-complete-path post-filter
  (`early_tests.rs`). A bounded un-ordered LIMIT and an EXISTS over a
  GPM pattern stop the walk at the cap / first hit, answering exactly
  the truncated full enumeration / the engine's verdict
  (`emit_tests.rs`).
- Exactness is pinned differentially: the early run, the
  evaluation-disabled run of the very same statement (test hook), and
  the WITH-WHERE post-filter spelling must answer identically —
  including the rebinding-name decline, the null pre-binding,
  and the raising conjunct (deferred; errors surface exactly where the
  post-filter raises them, and a raising dead-end prefix surfaces
  nothing).
- ADR-0471's recorded EXISTS early-exit loss is restored (Later
  references note there); ADR-0469's exactness discipline now covers
  plain conjuncts and its inliner stands down under selectors (Later
  references note there).
- The error-suppression limitation is unchanged from ADR-0469's: a row
  pruned by one early conjunct skips the remaining conjuncts' possible
  errors — the engine's pre-existing pushdown semantics, tracked by the
  engine-wide error-suppression ticket.
- Engine-internal (`cypherlite-executor`, the `exec/gpm/` module
  family): no public API, protocol, client surface, or dependency
  moves.
