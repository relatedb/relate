### Fixed

- `LOAD CSV … CALL (row) { … } IN TRANSACTIONS` now runs: `LOAD CSV` only
  reads a file, but the write-before-batched-subquery check counted it as a
  write and refused the bulk-load idiom with
  `WriteBeforeCallInTransactions`. A real write before the batched call is
  still refused.
