### Changed

- Every clause takes a bounded batch stream: `UNWIND`, `MATCH`, `CALL` and
  `LOAD CSV` no longer materialize their output for the consumer, so
  statements that previously refused against the workspace cap now run, and
  `PROFILE` reports a streamed `UNWIND` clause's own row count instead of 0
  (ADR-0660).
