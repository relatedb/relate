---
type: adr
area: cypher
claim: dd43a2abc68cda0ff7913d90daaea22f1c9d6609
name: 0674-a-clause-bound-null-matches-nothing
status: proposed
tags:
  - semantics
  - errors
title: A variable an earlier clause bound to null matches nothing in a later pattern
REFERENCES:
  - "[[adr/0335-unbound-variable-fails-not-null]]"
  - "[[adr/0471-gpm-pattern-engine]]"
---

# ADR-0674: A variable an earlier clause bound to null matches nothing in a later pattern

## Context

A `null` in a row has two unrelated meanings, and the pattern engines read
them as one:

- **a clause binding.** An earlier clause can bind a name to `null`: a
  failed `OPTIONAL MATCH`, an explicit `WITH null AS x`, a `null` element
  of an `UNWIND` list, a null-padded `OPTIONAL CALL` row. The name is
  bound; its value simply matches no node, relationship or trail.
- **an engine placeholder.** [ADR-0335](./0335-unbound-variable-fails-not-null.md) §5
  has two sites that legitimately evaluate a name as `null` before its own
  binding exists: a non-local singleton referenced inside a
  quantification and declared later in the same pattern (§5's first site),
  and a deleted entity's tombstone. Those are the engine's own row construction, not a
  clause's binding.

The classic walker has always treated the first kind as a mismatch
(`Binding::Mismatch` in `pattern_walk/binds.rs`). The GPM engine
([ADR-0471](./0471-gpm-pattern-engine.md)) classified a binding by its
*value* alone — `prebound` in `exec/gpm/mod.rs`: `None | Some(Value::Null)`
→ `Prebound::Fresh` — so on a variable-length, quantified or shortest
spelling it rebound a clause-bound `null` to whatever the walk reached.
Rows, `EXISTS` / `COUNT { }` subqueries and the writes behind such a
`MATCH` then covered values Neo4j never matches, and a null endpoint of a
`SHORTEST` / `shortestPath` pattern enumerated every trail before rejecting
each at its end (a K6 graph: 3.9 GB, 24 s).

## Decision

A `null` an **earlier clause** bound is a binding that matches nothing, in
every pattern spelling, on every engine and rung.

1. **The rule is stated once, on the entering row.** One helper
   (`pattern_policies::binds_a_null`) asks whether the entering row
   pre-binds a name this pattern binds to `null`, and its two readers
   both hold a *clause-produced* row: the `MATCH` operator ladder drops
   such a row from the clause's input — so the frontier, chain, kernel
   and per-row rungs all answer the same row — and the walker's own
   per-row entry answers it with no match, which is what an
   `OPTIONAL MATCH` needs (the row survives, null-extended).
2. **Placing it before the walk is part of the decision, not an
   optimisation.** Per-arrival rejection inside a kernel would keep the
   two engines disagreeing, and would still make a null endpoint walk
   every trail. The empty answer is the row's, so it costs nothing and
   cannot depend on which rung serves the clause. Placing it inside the
   per-path fork instead would answer an `OPTIONAL MATCH` empty without
   its null-extended row, which is why the ladder owns it.
3. **ADR-0335 §5's placeholders are not clause bindings, and the rule must
   not be stated where it cannot tell them apart.** A quantification's
   predicate row carries the pattern's own element names as `null`
   placeholders before the iteration binds them; a check in the per-path
   fork would read those as clause bindings and answer the group empty (the
   corpus's quantified-path rows, Neo4j-verified). That is why the rule
   lives on the clause-produced row — the ladder and the walker's entry —
   and the `EXISTS { }` probe, which sees both kinds of row, keeps its
   previous answer as a known residual.
4. **The value alone never decides.** A future site that must rebind a
   `null` binding says so by where it constructs the row, not by the
   value; the classification's doc states this.

## Alternatives considered

- **Keep the GPM's value-only rule and document the divergence.** Rejected:
  it is a wrong answer, several rows reach writes (a `SET` behind the
  pattern), and the corpus evidence is live Neo4j 2026.10 on every
  spelling.
- **A third `Prebound` state derived from the value.** Rejected: the value
  cannot distinguish a clause binding from the engine's placeholder — the
  distinction is *where the null came from*, which the entering-row rule
  has and a value test does not.
- **Reject the null per arrival at each GPM binding site.** Rejected
  above (§2), and it leaves the classic walker and the frontier rungs
  spelling their own verdicts.
- **Fix only the fold / only the shortest route.** Rejected: those are the
  specialised paths the corpus found first; the entering-row rule covers
  every consumer (rows, `DISTINCT`, ordered pages, caps, probes,
  aggregates) because they all route through the one fork.

## Consequences

- `WITH null AS x MATCH (a:A)-[:R*2]->(x) RETURN count(*)` answers 0; the
  `SET` behind the pattern writes nothing; a null `SHORTEST` end answers
  instantly instead of enumerating.
- The two GPM tests that pinned the divergence (`gpm_tests.rs`) now pin the
  agreement; both engines answer the same row for a null pre-binding.
- A one-hop aggregate fold still derives its own walk and does not route
  through the fork: `WITH null AS r MATCH (a:A)-[r]->(b) RETURN count(*)`
  keeps answering 1 until that fold declines the shape (tracked with the
  fold's own finding).
- Tests: `relate/relate/tests/rel8_2029_name_scope.rs` (both engines, every
  spelling, the write and shortest arms); the corpus reproduction is the
  evidence folder of the reported finding.
