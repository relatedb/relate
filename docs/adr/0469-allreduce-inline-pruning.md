# ADR-0469: Mid-path allReduce inline pruning in quantified path patterns

Status: Proposed

Claim: f995b8a7db80625638623139a7e412bd9e873277

## Context

The QPP expansion (`cypherlite-executor/src/exec/gpm/qpp.rs`) runs a DFS
over `gpm_walk_elements` iterations: within the hop bound the walk fans
out combinatorially, and every MATCH-WHERE conjunct is a **post-filter**,
evaluated once per *complete* path. For the route-planning class of query
— a budgeted route search such as
`MATCH REPEATABLE ELEMENTS p = (a:Geo) (()-[rels:ROAD]-(x:Geo)){1,8}
(b:Geo) WHERE allReduce(acc = 0.0, r IN rels | acc + r.distance_km,
acc < 600)` — that is the worst case: the allReduce can only reject a
route after the walk has already expanded every prefix of it, so the
answer-preserving pruning the query class needs (10–1000x on cyclic
graphs) never happens.

`allReduce`'s list semantics (`eval_all_reduce` in
`cypherlite-executor/src/eval/quantifier.rs`) already carry the needed
per-element structure: the scan **returns false the moment a prefix
predicate is false** (the deciding element ends the scan — a `1 / 0`
after it is never evaluated), while a **null prefix verdict is not
deciding** — the scan continues (`saw_null`), and a later element can
still raise, erroring the row. The per-element policy (update-then-
predicate step, the ADR-0462 wrap fast path with incremental depth
tracking, the int→float accumulator invariant, the finiteness check)
exists as `AllReduceFold`/`AllReduceStep`, carried by the list
evaluator.

Measured on the EV-shaped cyclic test graphs
(`allreduce_inline_tests.rs`' 8-city ring with 250 km chords and charge
self-loops): **54.9x hop reduction** and **~19–71x wall-time speedup**
(wall-time varies by machine; the deterministic hop ratio is the pinned
form — the tests assert >= 5x hops and >= 3x p50 for CI stability).

## Decision

**MATCH-WHERE `allReduce` conjuncts over a QPP group list are advanced
one element per quantification iteration, inside the QPP DFS
(`advance_reduces` + the `BranchReduce` state carried down each branch),
with the per-element policy shared with the list evaluator through
`AllReduceFold`/`AllReduceStep` — one spelling, the update-then-predicate
step exists exactly once.** The post-filter is **not removed**: the
inlined conjunct prunes only rows the post-filter would drop, and the
post-filter stays the correctness backstop (and the error reproducer)
for every declined or poisoned conjunct.

Four fixed properties:

- **Exactness rule — prune on `Failed`, poison on `Null`.** A prefix
  predicate verdict of `Failed` (false) prunes the iteration: no
  extension of the branch can make the conjunct true, since
  `eval_all_reduce` returns false the moment a prefix predicate is
  false. A `Null` verdict is **not** deciding and never prunes: the list
  evaluator scans past a null and a later element can still raise, so
  pruning on null could swallow that error — a null verdict **poisons
  the fold** (`fold: None`) and the branch runs unpruned, the
  post-filter deciding on the completed row. (An earlier draft of this
  rule considered pruning on false *or* null; the final rule is:
  `Failed` prunes, `Null` poisons — the null-prefix-then-raise
  reproducer pinned the correction.)
- **Error-poisoning equivalence.** A raising step — or a raising
  `initial` — poisons the fold instead of propagating: the untouched
  post-filter raises identically on whatever rows survive, so pruning
  never changes *which errors surface*. A branch that yields no rows was
  never evaluated in the post-filter, so a raising dead-end branch
  surfaces no error in either run.
- **Conservative inlinability gate** (anything else stays a
  post-filter, unpruned):
  - the conjunct is a **top-level `AND` conjunct** of the MATCH WHERE —
    a lone failure of it rejects the row, so prefix pruning only
    removes rows the post-filter drops;
  - its list is a **bare variable accumulated by exactly one quantified
    element** of this pattern, and is not that element's subpath
    variable (whose per-iteration value is a constructed path, not an
    iteration row binding);
  - **every free name of the update and the predicate** — minus the
    accumulator and the loop variable, which the fold binds before
    evaluating either, exactly as the post-filter does — is **bound
    before the quantification** (the entering row or earlier elements
    of the pattern);
  - **the `initial` must not reference the accumulator or the loop
    variable at all**, and every name it does reference must be bound
    before the quantification. The `initial` is evaluated on the
    entering walk row — *before* the fold binds those names — so a
    reference to one of them can read a non-local-singleton null
    pre-binding mid-path (ADR-0335) where the post-filter's completed
    row holds the pattern's final binding; the two `initial` values
    then disagree and the mid-path fold tracks a different accumulator
    than the post-filter's (review round 1: the `total = size(b.id)`
    reproducer with `b` the pattern's end node silently dropped a
    surviving route).
- **PROFILE counter change.** QPP/GPM traversals now count their
  iterations into the `hops` store-read counter (`count_hop_expand()`
  in the DFS; previously quantified expansions counted 0 there — the
  classic `expand.rs` walk and `pivot.rs` already counted). PROFILEd
  QPP queries therefore report different `Db Hits` totals than before,
  including queries with no inlinable conjunct; no pinned output relied
  on the old zeros, and the counter is what makes a pruned and an
  unpruned run of the same query comparable in PROFILE.

## Alternatives considered

- **Post-filter only (status quo).** Keep every MATCH-WHERE conjunct a
  post-filter over completed paths. Rejected: it forfeits the
  10–1000x the route-planning query class needs — on cyclic graphs the
  walk fans out combinatorially within the hop bound, and the measured
  54.9x hop reduction (19–71x wall time) is exactly the win the class
  exists to capture.
- **Monotone-predicates-only pruning.** Inline only when the predicate
  is provably monotone in the accumulator (e.g. `acc < budget`), the
  shape classic admissible-search pruning requires. Rejected: it is
  narrower than the sound rule — `allReduce` is false the moment *any*
  prefix predicate is false, regardless of monotonicity, so a
  non-monotone predicate such as `acc % 7 <> 3` (fails some prefixes,
  passes later ones) still prunes exactly (pinned by
  `non_monotone_predicates_prune_exactly`); a monotone-only gate would
  misclassify these sound cases and decline them for no correctness
  gain.
- **Inlining into the classic `expand.rs` walk as well.** Mirror the
  inliner for the classic variable-length expand beside the QPP one.
  Rejected: the classic variable-length form is being unified into the
  QPP engine (LABS-661, in flight); duplicating the inliner there would
  create the second copy of the whole machinery — extraction, gate,
  branch state — that the unification removes, a parallel-state
  violation paid for one transition's lifetime.

## Consequences

- Budgeted route queries over quantified path patterns prune mid-path:
  measured 54.9x hop reduction and ~19–71x wall-time (machine
  variance) on the EV-shaped cyclic test graphs; the deterministic hop
  ratio (>= 5x) and a >= 3x p50 are pinned in
  `allreduce_inline_tests.rs`, and the PROFILE visibility is pinned
  end to end in `cypherlite/tests/profile_metrics.rs`.
- Exactness is pinned differentially: the pruned run, the
  pruning-disabled run of the very same statement (test hook), and the
  WITH-WHERE post-filter spelling must answer identically — on
  acyclic, cyclic, and bidirectional-cyclic graphs — including the
  null-distance, raising-update, null-prefix-then-raise, and
  initial-aliasing cases from both review rounds.
- Declined conjuncts (later-binding references, shared group shapes,
  subpath lists, non-bare lists, non-top-level conjuncts) keep the
  pre-inlining behavior exactly: post-filter evaluation, no pruning,
  no counter semantics beyond the hops counter all QPP traversals now
  report.
- **Known limitation**: a row pruned by one inlined conjunct skips the
  remaining WHERE conjuncts, so errors they would have raised are
  suppressed on that row. This is the same semantics as the engine's
  pre-existing predicate pushdowns and is tracked by the engine-wide
  error-suppression ticket; it is not introduced by, nor specific to,
  this inliner.
- PROFILE output for QPP queries changes (hops previously 0 now
  counted); any future pinned PROFILE output for quantified patterns
  pins the new, meaningful numbers.
- The inliner is engine-internal (`cypherlite-executor`, one module
  family in `exec/gpm/`): no public API, protocol, client surface, or
  dependency moves.

## Later references

ADR-0475 generalizes this decision's exactness discipline (prune on
`Failed`, poison/defer on `Null` and errors, post-filter stays) from
allReduce folds to plain MATCH-WHERE conjuncts evaluated at the walk's
binding points, and adds a selector stand-down **inside this inliner**:
under any path selector `inlined_all_reduces` returns nothing — a
selector chooses among the pattern's full candidate set and mid-path
pruning would change which path it picks (selective-path scenario 8) — so a
selective pattern keeps the pure post-filter behavior this decision's
declined-conjunct rule describes.
