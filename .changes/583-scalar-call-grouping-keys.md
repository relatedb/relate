### Fixed

- A grouping-key variable read through a scalar function call beside an
  aggregation now validates and runs — `WITH count(n) AS c MATCH (m) RETURN
  toFloat(c) / count(m) AS coverage, c` executes with `c` as the group key
  instead of failing with `AmbiguousAggregationExpression`. Calls over
  non-grouped variables and aggregates hidden inside list literals keep
  rejecting.
