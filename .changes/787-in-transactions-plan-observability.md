### Added

- `EXPLAIN` and `PROFILE` of a `CALL { … } IN TRANSACTIONS` statement render
  the body's operators as plan children of the `CallSubquery` operator, so
  the plan shows what each batch runs without re-reading the statement; a
  plain `CALL { … }` keeps the opaque node. `PROFILE` also reports the batch
  facts on the operator — batches run, committed, rolled back under the
  `ON ERROR` policy, and the per-batch peaks (input and output row bytes,
  scratch, WAL payload) — in the plan table's `Details` marks
  `[in-tx batches=…]` / `[in-tx-peak …]` and the wire's `InTx*` metric args
  (ADR-0681).
