### Added

- `EXPLAIN` warns with `01N60` when the statement references a parameter
  the caller did not supply, one notification naming every undefined
  parameter — `EXPLAIN RETURN $foo` reports `The undefined parameter(s)
  $foo prevent plan reuse.` A fully supplied statement warns nothing.
  `PROFILE` and the plain path are unchanged: both refuse the statement
  with the ADR-0308 error instead, and `EXPLAIN` keeps planning without
  parameter values, as in the reference server.
