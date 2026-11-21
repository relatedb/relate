### Changed

- A `LOAD CSV` seeding a per-row-independent tail (`MATCH … CREATE …`) runs
  its tail batch-major: the records are pulled in bounded batches and the
  remaining clauses run once per batch, so the driving rows never exceed one
  batch instead of the whole record stream. A bulk load of this shape now
  answers under the default per-allocation cap where it refused before (and
  peaked at 13.1 GiB for a ~490 MB store with the cap lifted); the tail's own
  containers stay charged as before, and the statement remains one
  transaction — an oversized commit is the explicit `CALL (a, b) { … } IN
  TRANSACTIONS` form the WAL-payload refusal names (ADR-0641, REL8-1845).
