### Changed

- A UUID operand beside a string no longer concatenates through `+`:
  `'a' + UUID()` and `UUID() + 'a'` now fail with a type mismatch, matching
  Neo4j, where they previously returned the UUID's text glued into a string.
  A UUID is still a property value, a list element and a `toString()`
  argument; `UUID() + []` and `[] + UUID()` still build a list.
