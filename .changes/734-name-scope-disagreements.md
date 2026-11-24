### Fixed

- A variable an earlier clause bound to null (`WITH null AS x`, a failed `OPTIONAL MATCH`) now
  matches nothing in a later pattern. On a variable-length, quantified or shortest pattern the
  engine treated the null as its own unbound placeholder and rebound it to whatever node the walk
  reached, so row counts, `EXISTS` / `COUNT { }` subqueries and the writes behind such a `MATCH`
  covered values Neo4j never matches; a `SHORTEST`/`shortestPath` with a null end now also answers
  instantly instead of enumerating every trail.
- A `CALL { … }` body with neither a scope clause nor a leading importing `WITH` now imports
  nothing, so a name its pattern binds is a new variable even when the outer row binds one of the
  same name (`MATCH (n:A) CALL { MATCH (n) RETURN count(n) AS total } RETURN total` now answers 3,
  not 1). The legacy importing `WITH`, `CALL ()` and `CALL (*)` forms are unchanged.
- Inside a `CALL (k) { … }` body, and inside a `COUNT` / `COLLECT` / `EXISTS` subquery, an
  imported or enclosing name now stays readable after a body `WITH` — a `MATCH … WHERE`, `SET`,
  `CREATE`, `REMOVE` or `DETACH DELETE` there no longer raises `UndefinedVariable` on a name other
  clauses accept.
- After a `DISTINCT` or aggregating `WITH` / `RETURN`, the variables the projection does not
  project are out of scope, so a subquery in the following `WHERE` / `ORDER BY` that re-uses one of
  those names binds a fresh variable. Relate pinned it to the dropped value — silently dropping
  groups and the writes behind them — or rejected the aggregating spelling.
