### Changed

- Per-label and per-type count subqueries such as
  `CALL (label) { MATCH (n:$(label)) RETURN count(n) }` now answer from the
  maintained counts instead of scanning the store once per label or type.
