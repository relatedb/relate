### Fixed

- `toString()` and `toStringOrNull()` now render list values in display
  form (`RETURN toString([1, 2])` → `'[1, 2]'`) instead of raising a
  compile-time `InvalidArgumentValue` — homogeneous list literals
  included, matching the Cypher 25 spec corpus. A byte array stays the
  one value `toString()` rejects; `EXPLAIN` and never-evaluated `CASE`
  arms containing such calls no longer fail the statement.
