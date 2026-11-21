### Fixed

- A registered call failing its own signature at runtime — `duration_between('abc','def')`,
  `duration(123, "s 'seconds'")`, `date(123)`, `date(duration('P1D'))`, the truncate
  spellings and a non-string timezone — now reports Neo4j's `ProcedureCallFailed`
  class (`ProcedureError`, GQLSTATUS 42001) instead of a TypeError or SyntaxError,
  matching Neo4j 2026.10.1 (ADR-0639).
