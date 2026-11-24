### Fixed

- The HTTP Query API v2 emits Neo4j-shaped values: an entity's `elementId`
  is the ADR-0554 form `elementId()` returns (`4:rl-id:n5`), so a returned
  id can be sent back as a parameter; a point is its EWKT string
  (`SRID=7203;POINT (1.0 2.0)`); a non-finite float spells `Infinity`,
  `-Infinity` or `NaN` instead of `Inf`.
- Typed request parameters on the HTTP Query API keep their type: a
  non-finite `Float` binds a FLOAT (it bound the STRING `"Inf"`, so a
  comparison against it selected nothing) and a `Point` EWKT payload binds
  a POINT (it was rejected with a 400).

### Added

- `relate::params_from_map_with` is the per-entry parameter walk
  `params_from_map` now shares: the `parameter <name>:` error prefix, the
  value-depth check and the collect, with the value decoder supplied by the
  dialect (the HTTP typed fold passes its own).
