### Fixed

- A subquery expression whose body pattern declares a name the enclosing
  projection dropped no longer fails to plan: `WITH DISTINCT p0 WHERE
  COUNT { MATCH (n0) … } >= 0` rejected the body's own `MATCH (n0)` as a
  read of the projected-away outer `n0`. The body's fresh pattern
  declarations are skipped; a name the projection still exposes stays a
  checked correlation, and direct reads of dropped names keep their
  rejection.
