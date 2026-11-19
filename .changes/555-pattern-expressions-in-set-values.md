### Changed

- `SET n.p1 = EXISTS((n)-[:R]->(:L2))` now plans, executes and stores the
  predicate's boolean per row, as in Neo4j: a SET value accepts the same
  pattern-expression positions a projection does — the `exists(<pattern>)`
  argument, a comprehension's WHERE filter, and predicate positions
  (`NOT`, boolean operands, a `CASE WHEN` condition). A bare pattern as
  the value (`SET n.p1 = (n)--()`) keeps its rejection with the SET
  spelling.
