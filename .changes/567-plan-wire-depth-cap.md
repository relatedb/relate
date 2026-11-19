### Fixed

- EXPLAIN of a long clause chain (roughly 249+ chained clauses) no
  longer panics the plan-wire walk (`plan wire node is a map, got
  Null`): the plan tree is engine-produced metadata, so its crossing to
  the wire form no longer rides the ADR-0462 value-nesting depth cap
  (ADR-0628); the cap still governs runtime data values.
