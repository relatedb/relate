---
type: adr
area: cypher
claim: cd72c1533d7267a644259d6412965866299e46c0
date: 2026-11-25
deciders: MemoryDB maintainers
name: 0685-pattern-comprehension-is-a-collect-subquery
status: proposed
tags:
  - executor
  - pattern-matching
  - planner
  - semantics
title: A pattern comprehension lowers to the collect subquery that is its meaning
REFERENCES:
  - "[[adr/0463-expression-allocation-budget]]"
  - "[[adr/0555-operators-stream-rows-never-accumulate]]"
---

# 0685: A pattern comprehension lowers to the collect subquery that is its meaning

## Context

`[(n)-[:R]->(m) WHERE p | f(m)]` has had its own evaluation machinery since
the first engine: a dedicated IR variant (`Expr::PatternComp`), a dedicated
executor entry (`eval_pattern_comp`), and a dedicated pattern walk
(`collect_pattern_rows`) that materializes every match into a `Vec<Row>`
before the WHERE filters and the projection runs. The construct is
semantically the expression-position subquery
`collect { MATCH (n)-[:R]->(m) WHERE p RETURN f(m) }` — Cypher 25 gives both
forms to the user, and the engine already owns the second: the seeded
nested statement engine, whose consumers stream
per [ADR-0555](./0555-operators-stream-rows-never-accumulate.md) and whose
memory is bounded by the workspace admission rather than by an unbounded
match-row buffer.

The divergence is not only a second code path. The materializing walk made
a pattern comprehension's peak memory follow the pre-filter match count —
the one form [ADR-0555](./0555-operators-stream-rows-never-accumulate.md)
forbids — and it kept every planner improvement to nested MATCH bodies
(index selection, seek planning, pattern-engine routing) out of the
comprehension, which always ran the eval-layer walk.

## Decision

The front lowers a pattern comprehension to the collect subquery that is
its meaning. In `relate-front`, the `PatternComprehension` AST arm
synthesizes the equivalent single-part body — `MATCH <path> [WHERE …]`
followed by `RETURN <map_expr> AS <element>` — and emits
`Expr::CollectSubquery` with that body. The parser AST and the whole
validation battery are unchanged: the rewrite happens after validation, at
lowering, so every comprehension-specific rule (WHERE placement, inner
scope binding, single-expression shape) still reads and rejects the
original form.

With the front no longer emitting `Expr::PatternComp`, the variant is
retired from the IR, the planner's `PatternComp` arms go, and the
executor's `eval_pattern_comp` and `collect_pattern_rows` are deleted. The
IR's query walk already descends subquery bodies (`subquery_body`), so
scope analysis, the dynamic-operand check, and plan construction cover the
synthesized body unchanged. The one thing the body cannot inherit is the
comprehension's own shadowing: the body's path variable is carried as
`path_shadow` and marked shadowable in the planner's nested scope, since a
nested MATCH would otherwise reject a name the enclosing scope binds. Pattern predicates (`EXISTS`-shaped
`Expr::PatternPred`) and `shortestPath` keep their dedicated walks — they
are existence and path answers, not list constructions.

Semantics carried by the rewrite, stated once:

- Outer variables import and join: a pattern element named by an outer
  variable is bound to that value (both forms match on the imported node);
  variables new to the pattern bind inside.
- No match yields the empty list; each match yields one element.
- The projection is returned unaliased: the collector reads the body's
  single column and discards its name, so no synthetic element name (and
  no name in the executor's reserved `__` namespace) is needed.
- The optional path variable (`[p = (n)-->() | …]`) is an *assignment*,
  not a join: it binds the whole path per match and **shadows** any outer
  name of the same spelling for the body's whole scope (live-verified:
  `MATCH p = (n:X)-->(b) RETURN [x IN nodes(p) | size([p=(x)-->(:Y) | p])]`
  answers the inner pattern's own matches). The rewrite carries that
  shadow explicitly: the synthesized body records the path variable's name
  (`Expr::CollectSubquery::path_shadow`), the planner's nested scope marks
  it shadowable whatever the outer binding's kind (`Scope::comprehension_body`,
  the one entity name that scope may rebind), and the nested seed neither
  imports nor carries it — so every reference, a nested subquery's
  included, resolves to the body's own path. A user-written
  `collect { … }` keeps the ordinary rejection for the same spelling.
- The WHERE sees the same bindings the projection sees.

One reconciliation shape is deliberately not carried: a comprehension
whose pattern rebinds an outer *single-relationship* variable in a
var-length slot (`OPTIONAL MATCH (x)-[r:R]->(y) … size([(x)-[r*1..2]->(z)
| z])`). The old eval walk reconciled the pre-bound value (a null
pre-binding answered zero matches); a MATCH var-length slot may only
rebind an outer *list* binding — the rule every user-written
`collect { MATCH … }` already follows — so the comprehension now answers
that plan-time `GroupVariableReuse` error instead, by full subquery
parity. The list-bound and single-hop spellings keep their reconciliation
answers (an imported trail pins the walk; a null single-rel pre-binding
still matches nothing).

## Alternatives considered

- **Stream the existing walk** — keep `Expr::PatternComp` and rebuild
  `eval_pattern_comp` over `fold_pattern_rows` (the ADR-0555 streaming
  spelling), charging each candidate slot. Rejected: it preserves two
  engines for one meaning, and the comprehension still plans no better
  than the eval walk — the nested body's planning never applies.
- **Rewrite in the parser** — parse the comprehension directly into a
  `CollectSubquery`. Rejected: the AST distinction carries the validation
  rules and error classes users and the conformance corpus see; erasing it
  at parse time would re-classify comprehension misuse as subquery misuse.
- **Rewrite in the executor** — evaluate `PatternComp` by synthesizing a
  subquery at eval time. Rejected: synthesizing IR at runtime moves a
  static lowering fact into the hot path and leaves the dead variant in
  the IR.

## Consequences

- One engine evaluates both spellings: the comprehension inherits the
  nested body's planning (indexes, seeks, pattern-engine routing) and the
  workspace admission's memory bound; the unbounded match-row buffer is
  gone.
- The element list a comprehension produces is the nested engine's final
  page — the same materialization `collect { … }` pays, and no more; a
  consumer that folds (an aggregating projection) still folds the
  elements, so a comprehension's answer no longer costs its pre-filter
  match count in memory.
- `EXPLAIN`/`PROFILE` render a comprehension as its subquery plan — more
  detail where there was an opaque expression node.
- Per-outer-row cost is the nested engine's seeding cost rather than the
  eval walk's setup; the query-latency suite's bands are the guard on the
  trade.
- The conformance corpus's comprehension scenarios must keep passing
  unchanged; any that pin the old engine's error classes move with an
  explicit corpus note, never silently.
