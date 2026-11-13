# 0287. Catalog DDL stages seed NEXT chains with a unit row

- **Status**: Accepted
- **Renumbered**: from 0176 (shared-number cleanup)
- **Date**: 2026-10-07
- **Deciders**: Oskar Hane (agreed in the end-of-feature review conversation), CypherLite maintainers

## Context

Sequential statement composition (`stmt1 NEXT stmt2`) executes the
stages in order and seeds each stage with the previous stage's result rows.
A stage whose result is empty would starve the next stage entirely, so the
executor compensates for stages that produce no rows *by nature*: data
writes (`CREATE`, `MERGE`, `DELETE`, `SET`, `REMOVE`, `FOREACH`,
`CALL { … }`) and `FINISH` count as "unit output" in
`stage_has_unit_output`
(`cypherlite/cypherlite-executor/src/exec/mod.rs`), and an empty result
from such a stage seeds the next stage with one unit (empty) row — which
is why `CREATE (n) NEXT MATCH (m) RETURN m` works.

The schema-commands-zero-records work on this branch (LABS-184) made
schema/administration DDL return zero rows and zero columns — the "omitted
result" shape of a statement without `RETURN` (Neo4j parity; the
omitted-result scenarios pin it). Before that change a catalog DDL stage finished with
one zero-column bug row, which happened to seed the next `NEXT` stage; the
fix removed that row but did not extend `stage_has_unit_output`, so a
non-final catalog-DDL stage now yields zero seed rows and starves the next
stage. Verified on the branch:

```cypher
CREATE (n:N {p: 1});
CREATE CONSTRAINT c FOR (x:N) REQUIRE x.p IS UNIQUE NEXT MATCH (n) RETURN n;
// => [] (starved); pre-change it returned the node.
```

The IR-native index DDL path (`CREATE INDEX i FOR (x:N) ON (x.p) NEXT …`,
lowered to `Clause::CreateIndex` rather than `Clause::Catalog`) starved
even before the zero-records change, so the two DDL paths ended up
consistently broken. The end-of-feature audit flagged the asymmetry: data
writes seed the next stage; catalog writes must mirror them.

## Decision

A schema/administration DDL stage seeds the next `NEXT` stage exactly like
a data write does. `stage_has_unit_output` returns true for a part ending
in:

- the IR-native DDL variants `Clause::CreateIndex`, `Clause::DropIndex`,
  `Clause::CreateConstraint`, `Clause::DropConstraint` — outright; and
- `Clause::Catalog(command)` exactly when the canonical row-producing
  predicate `cypherlite_front::show_columns::catalog_produces_rows(command)`
  says the command is NOT row-producing (schema/admin DDL:
  index/constraint/database/graph-type commands).

Row-producing catalog commands (the SHOW family and `TERMINATE
TRANSACTIONS`) keep their current row semantics: they seed the next stage
from their projected rows and never gain a unit row, so a zero-row SHOW
stage still starves the chain — unchanged. The unit seed still fires only
when the stage's own result is empty (`execute_plan_seeded` /
`execute_plan_read_seeded`), so a stage is never seeded twice.

The classification is not re-enumerated in the executor: the
`catalog_produces_rows` predicate in `cypherlite-front` (the single home
the zero-records change established, consumed by the executor's dispatch
and its `CALL { … }` merge check) is the one source of truth, exhaustive
over every parser clause, so a new catalog command is a compile error
there until classified.

## Alternatives considered

- **Pin the starvation with a test.** Rejected: it would enshrine an
  arbitrary asymmetry — `CREATE (n) NEXT …` seeds its chain while
  `CREATE CONSTRAINT … NEXT …` silently returns nothing — and it would
  keep the two DDL lowering paths (catalog vs IR-native) consistently
  broken instead of consistently working. The pre-zero-records behavior
  shows the seeded chain is the behavior users had.
- **Reject DDL in `NEXT` chains outright, as Neo4j reportedly does.**
  Rejected: the Neo4j claim comes from the end-of-feature audit and was
  NOT verified against a live Neo4j, so rejecting on its authority alone
  would risk diverging from the reference in the opposite direction. It
  would also need a new validation path and error text, and it would
  break `NEXT` chains that worked before the zero-records change (the
  catalog-constraint path seeded via the old zero-column row). Seeding is
  strictly more useful and matches how data writes already behave; if a
  verified Neo4j rejection surfaces later, a new ADR can revisit.

## Consequences

- `stage_has_unit_output` in
  `cypherlite/cypherlite-executor/src/exec/mod.rs` gains the four
  IR-native DDL variants and the `Clause::Catalog` case gated on
  `!catalog_produces_rows(command)`; its doc comment states the rule.
- Both DDL lowering paths behave identically in `NEXT` chains:
  `CREATE CONSTRAINT … NEXT MATCH (n) RETURN n`,
  `CREATE TEXT INDEX … NEXT …` and `CREATE INDEX … NEXT …` (IR-native)
  all return the graph's nodes after a seeding `CREATE (n:N {p: 1})`;
  pinned by `ddl_stage_seeds_next_chain` in
  `cypherlite/cypherlite/tests/ddl_result_shape.rs`.
- SHOW-family stages are untouched: `show_stage_seeds_next_chain_from_its_rows`
  pins that a row-producing catalog stage seeds the next stage from its
  projected rows exactly once (no unit row on top).
- `docs/api/rust/spec.md`'s `QueryResult` section states that a
  schema/admin DDL stage in a `NEXT` chain seeds the next stage like a
  data write.
- No TCK or CIP-eval impact: no pinned scenario chains DDL with `NEXT`;
  the allowlist and baselines are unchanged.
