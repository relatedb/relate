### Fixed

- A unit `CALL` subquery whose body ends in another `CALL` subquery no
  longer changes the outer row count: the outer rows pass through
  unchanged (an empty inner stream used to drop them, a many-row one
  duplicated them), matching Neo4j; `WHEN` branch bodies ending in a
  nested unit `CALL` keep the driving row the same way, and the
  trailing `FINISH` workaround is no longer needed (#921)
