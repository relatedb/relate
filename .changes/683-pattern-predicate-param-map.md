### Fixed

- A whole-map parameter inside a pattern an expression carries —
  `WHERE (a:L1 $props)-[:R]->(b)`, its `exists(…)` spelling, a pattern
  comprehension, a `shortestPath()` argument — now rejects with the
  parameter-use error the MATCH/MERGE clause spellings already had,
  instead of executing and filtering by the parameter's map. CREATE
  keeps the shorthand, and a literal map with a parameter value
  (`{t: $v}`) keeps executing.
