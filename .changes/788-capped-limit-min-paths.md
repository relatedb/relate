### Fixed

- `LIMIT n` without `ORDER BY` over a variable-length or quantified
  pattern now returns `min(n, total)` rows: the frontier-NFA rung's capped
  emission reconstructed one witness path per reached (start, end) pair,
  and when the LIMIT exceeded the pair count it returned that pair-level
  list as the whole answer, dropping every further path. Counts and writes
  behind such a `LIMIT` were wrong (`RETURN p LIMIT 100` over four paths
  returned one row; `WITH a, z LIMIT 100 RETURN count(*)` answered 1). An
  unfilled cap now falls through to the walker's cap rider, and PROFILE
  names the reason `short-cap`.
