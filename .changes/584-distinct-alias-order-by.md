### Fixed

- `ORDER BY` now reads the base variable of a bare projection item under
  `RETURN DISTINCT` — `RETURN DISTINCT m AS node ORDER BY m.p1 DESC` plans
  and runs (dedup on the projected value, ordering through the base
  variable) instead of failing with `UndefinedVariable`. Variables only
  derived from (`m{.p1} AS node`) or absent from the projection stay
  ORDER-BY-inaccessible under `DISTINCT`, matching Neo4j; and the
  unaliased spelling over a path/scalar/list-bound variable now rejects
  uniformly instead of passing by insert-order accident.
