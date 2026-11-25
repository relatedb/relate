### Fixed

- `CALL { … } IN [CONCURRENT] TRANSACTIONS OF $batchSize ROWS` rejects a
  FLOAT batch size parameter (`NumberOutOfRange`) instead of silently
  truncating a whole FLOAT like `1.0` to its integer, and `IN $c
  CONCURRENT TRANSACTIONS` rejects a FLOAT concurrency with the integer
  TypeError instead of passing it as its truncated value — both matching
  live Neo4j (22N03)
