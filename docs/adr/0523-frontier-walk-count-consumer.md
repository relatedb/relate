---
type: adr
area: engine
claim: 72446f317b6b9ed6cd3e44178bd767a48310f31c
name: 0523-frontier-walk-count-consumer
status: proposed
tags:
  - pattern-matching
title: The frontier family's walk-count consumer
REFERENCES:
  - "[[adr/0478-frontier-nfa-qpp-kernel]]"
  - "[[adr/0519-diagnostic-pattern-engine-switch]]"
---

# ADR-0523: The frontier family's walk-count consumer

## Context

[ADR-0478](./0478-frontier-nfa-qpp-kernel.md) Decision 2 names the answer shapes the frontier family serves —
the functions of the (node, state) *pair*, not of the *path* — and sends
every path count back to the per-row DFS: path counts are exponential by
nature, and the pair-deduplicated BFS cannot recover the multiplicity it
collapsed. For the default pool that stays true: under DIFFERENT
RELATIONSHIPS a walk's admissible continuations depend on the
relationships its own prefix already used, so no per-pair summary
determines the count.

The bounded WALK pool (`MATCH REPEATABLE ELEMENTS`, [ADR-0478](./0478-frontier-nfa-qpp-kernel.md) §3) has no
such dependency. Every in-bounds accepting walk is a match, so the walk
count is a sum over levels of walk multiplicities, and the multiplicity
of a (node, state) pair at level `k + 1` is the sum of its predecessors'
level-`k` multiplicities over the rels between them. The count the DFS
produces by emitting one row per walk — exponential in the hop bound on
any branching graph (a twelve-level, branching-four recursion is ~22M
rows, and the statement dies on the expression-memory cap after tens of
seconds) — is computable in `O(levels × reached pairs × degree)`.

The same recursion's distinct-endpoint answers (`count(DISTINCT d)`,
`collect(DISTINCT d)`) were already in Decision 2's vocabulary, but the
cost gate (Decision 1) never engaged them on a branching recursion onto an
unlabeled target: the endpoint estimate of an unlabeled node is the
estimate ladder's rank sentinel, an ordering fact no path count reaches,
and the chained path estimate clamped every hop at the rel type's total —
a bound that holds for a hop leaving distinct seed nodes, not for a hop
leaving paths. Both are estimate defects, corrected in the same change
(the endpoint side bounded by the store's node-id span, the second and
later hops priced over the path multiset); they change no decision.

## Decision

1. **A new consumer, the walk count.** The frontier family serves
   `RETURN count(*)` and `RETURN count(<boundary variable>)` (no
   DISTINCT; a boundary variable is always bound, so it counts every
   row) over one bounded quantified pattern under the WALK pool. The
   answer is computed by a level-synchronous multiplicity pass over the
   compiled NFA: level `k`'s (node, state) multiplicities expand to level
   `k + 1` except at the last level, where each accepting transition
   contributes its pair's multiplicity times the pair's degree over that
   transition — the last leg contracts to a degree sum and is never
   materialized. No walk, row, or path value is built.

2. **The equivalence is guarded; every guard declines to the unchanged
   plan.** The consumer engages only when:
   - the pool is WALK — trail-enforcing modes (the default pool, TRAIL,
     ACYCLIC, SIMPLE) keep the DFS: rel-uniqueness makes the count
     prefix-dependent;
   - the target node is unfiltered — no labels, label expression,
     property map, inline `WHERE`, or MATCH-`WHERE` conjunct on it: the
     degree sum counts every arrival;
   - the compiled NFA is deterministic — no epsilon edges, no per-state
     guards or transition predicates, at most one transition per state,
     no type repeated in a transition's union — so each walk maps to
     exactly one state run and counts once;
   - the statement's suffix is exactly that one count item — no
     grouping, ordering, paging, second item, or intervening clause;
   - the count fits the engine's integer; an overflow declines.

3. **Placement.** The consumer is a `MATCH` fold (the clause and its
   `RETURN` answered together), not a row-rung consumer: the row rung's
   contract is to emit the clause's rows, and a count has none to emit.
   It reuses the matcher's structural proofs and seed scan, skips the
   cost gate (the pass allocates no span tables and its work never
   exceeds the DFS's), records the family's PROFILE counters, and stands
   down with the family (the diagnostic switch's `walk` value,
 [ADR-0519](./0519-diagnostic-pattern-engine-switch.md)).

## Alternatives considered

- **Contract through degree statistics alone.** The per-label degree
  histograms the planner keeps price a hop, but they are not exact for
  the reached node set; an answer is not an estimate. Rejected: the
  count must be exact, and exactness needs the reached pairs' own
  degrees.
- **Serve the default pool on acyclic graphs.** Trails equal walks on a
  DAG, but acyclicity is a store-wide property no plan-time gate can
  prove cheaply, and a cyclic store would answer wrongly. Rejected:
 every gate is a proof ([ADR-0478](./0478-frontier-nfa-qpp-kernel.md) §2).
- **Emit weighted rows from the row rung.** Rows carrying a multiplicity
  would let a downstream aggregate fold them, but no operator in the
  pipeline understands row weights, and teaching every aggregate one is
  a far larger change than one fold. Rejected for this cut.
- **Allow end-node filters by counting only admitted arrivals.** Exact,
  but it gives up the last-leg contraction (every last-level arrival
  must be resolved and admitted). Rejected for this cut; it is a
  natural extension.

## Consequences

- `MATCH REPEATABLE ELEMENTS (s)-[:R*1..12]->(d) RETURN count(d)` on a
  branching graph answers in milliseconds instead of enumerating every
  walk; the default-pool spelling's `count(d)` is unchanged (still a
  path count on the DFS), and `count(DISTINCT d)` answers from the
  endpoint set now that the cost gate prices the recursion.
- The answer-shape vocabulary of [ADR-0478](./0478-frontier-nfa-qpp-kernel.md) Decision 2 gains a path count
  for exactly one pool; the "path count declines" rule stands for every
  other pool and every other aggregate.
- Differential tests (generated multigraphs with parallel rels,
  self-loops, and undirected hops; the diagnostic switch's two legs) pin
  the walk count to the DFS's own row count; one decline test per guard
  pins the fall-through.
- Follow-ups outside this decision: end-filtered walk counts, grouped
  walk counts (`RETURN s, count(*)`), `sum` of a constant, and
  multi-pattern statements.
