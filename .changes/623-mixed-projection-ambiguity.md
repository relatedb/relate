### Fixed

- A mixed aggregating projection whose non-aggregation part is arithmetic
  over a projected grouping key now plans and runs: `MATCH (n) WITH n.x + 1
  AS nx RETURN nx, nx - 1 + count(nx)` no longer fails with
  `AmbiguousAggregationExpression`. The front-end ambiguity check judges
  the variables such an expression reads, not the shape around them, and
  the planner's grouping-key leaf discipline decides the rest — a mixed
  part reading a variable no grouping key covers keeps its rejection.
