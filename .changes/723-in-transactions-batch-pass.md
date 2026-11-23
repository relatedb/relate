### Fixed

- `CALL { … } IN TRANSACTIONS OF n ROWS` no longer refuses bulk loads past
  100 000 rows: an admissible body (e.g. `CREATE`/`MERGE` per row, reads
  included — the body evaluates the batch as a unit, in pipeline order)
  now runs as one engine pass per batch — one nested-evaluation-budget
  tick per batch instead of one per row — so the per-commit WAL payload
  cap's suggested remedy works at any row count and batched bulk loads
  run markedly faster (ADR-0669).

### Changed

- The per-statement nested-evaluation budget is 1 000 000 passes (was
  100 000, ADR-0670): million-row loads through nested `CALL` bodies no
  longer refuse at the 100 001st pass.

