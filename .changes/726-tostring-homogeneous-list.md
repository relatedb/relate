### Fixed

- `toStringOrNull()` of a homogeneous list literal (`toStringOrNull([1, 2])`)
  now renders `'[1, 2]'` instead of failing during semantic analysis. Neo4j
  renders every list for this function, and the same value read from a
  property, parameter or variable already rendered. `toString()` keeps
  rejecting a homogeneous list literal at compile time — Neo4j's own
  static-type boundary — and a property or parameter list still renders for
  both functions.
