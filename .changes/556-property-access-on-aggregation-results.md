### Changed

- Property access on an aggregation result now plans and evaluates, as
  in Neo4j: `RETURN max(n.p1).p2` and `RETURN collect(n)[0].p1` treat a
  property whose target carries an aggregation as part of the
  aggregating expression instead of a non-grouping reference — the
  aggregate folds first and the property applies to its result (a
  node-typed `collect(n)[0].p1` reads the node's property; a scalar
  result keeps the existing property-on-scalar type error). A genuinely
  non-aggregated leaf beside the property (`max(n.p1).p2 + n.q`) keeps
  its grouping-key rejection.
