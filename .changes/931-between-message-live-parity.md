### Fixed

- The `duration.between`/`duration_between`/`duration.in*` family's rejection of
  non-temporal operands now reports live Neo4j's message byte-for-byte —
  `Invalid call signature for Between: Provided input was [String("abc"), String("def")]`,
  the echoed inputs rendered as Neo4j's `AnyValue.prettyPrint()` prints them
  (`Long(123)`, `Double(1.500000e+00)`, `Boolean('true')`, `List{…}`, `Map{k -> v, …}`,
  bare-id entities, `ByteArray[…]`, `UUID(…)`, temporals/points in display form) —
  instead of the engine's own `Invalid call signature: <spelling> needs temporal values`
  wording. Class (`ProcedureError`), GQLSTATUS (42001) and the
  `Neo.ClientError.Procedure.ProcedureCallFailed` Bolt code are unchanged. Every
  rendering shape and both rounding-carry signs were probed against
  Neo4j 2026.10.1 (REL8-2359).
