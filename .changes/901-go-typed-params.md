### Fixed

- Go client: `Date`, `LocalTime`, `Time`, `LocalDateTime`, `DateTime`,
  `Duration`, `Point` and `Vector` parameters bind as their Cypher kinds
  through the typed C putters — previously they silently bound as property
  maps, so a value read from a result never round-tripped as a parameter.
  A `DateTime` with a `Zone` binds by that zone (the offset is re-resolved
  at that wall clock); entity values (`Node`, `Relationship`, `Path`)
  refuse as parameters — pass `.Properties` (or a path's elements)
  explicitly.
