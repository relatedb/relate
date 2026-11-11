# ADR-0565: Argument-value and procedure rejections carry the class live Neo4j reports

Status: Proposed
Claim: 76d88fb41d8c4d8873e53cbc903eaaff80eb74de

## Context

The spec harness enforces one slot of an expected error: the openCypher
class the language bindings expose (`tck_class`, ADR-0482). A corpus
triage found five clusters where the engine's class disagreed with both
the corpus and live Neo4j 2026.10.1 (probed the same day):

- The vector argument family — `vector.similarity.cosine`/`euclidean`
  dimension mismatches, empty lists, zero vectors, null and NaN
  coordinates; `vector_distance` dimension mismatches; the `vector()`
  dimension bound — reported the type class; live reports the
  `ArgumentError` class (22N38).
- `round()` precision and rounding-mode checks, `replace()`'s limit, the
  `trim()` single-character rule and a zero `CALL … IN TRANSACTIONS …
  ROWS` batch size reported the semantic or type class; live reports
  `ArgumentError` (22000/22003/22N38).
- `NODETACH`/plain `DELETE` on a node that still has relationships
  reported `SemanticError`; live reports `ConstraintVerificationFailed`
  (G1001).
- `toInteger()`'s out-of-range numeric string reported `ArgumentError`;
  live raises its `CypherTypeException` under 22003, the `TypeError`
  class. ADR-0175's 22N03 note generalized the integer-argument range
  family onto this site; live splits them.
- A call to an unregistered procedure reported `SyntaxError`; live
  reports `ProcedureError` (42001).

ADR-0559 introduced the runtime argument-value variant for a first set
of sites. This ADR extends that migration and records the three
non-argument rows.

## Decision

Existing class vocabulary only; no new error variant, no new class.

- The run-time argument rejections above construct
  `Error::argument_value_kind` with `NumberOutOfRange` (value bounds) or
  `InvalidArgumentValue` (invalid vector, unknown rounding mode). A null
  list coordinate is an invalid vector, while a wrong-typed argument
  stays `Error::ty` — live splits those two at run time.
- The delete-connected error, shared by every node-delete path, leads its
  payload with the `ConstraintVerificationFailed` class token, which the
  one nested-class scan of still-stringly payloads resolves.
- `toInteger()`'s out-of-range string constructs
  `Error::invalid_argument_value`, the existing type-class constructor
  for a value outside an argument's domain.
- The typed procedure-not-found variant's binding metadata reports the
  fine `ProcedureNotFound` kind and the `ProcedureError` class. Its Bolt
  code and GQLSTATUS stay as they are (the Neo4j procedure code under
  42001).

## Alternatives considered

- **Keep the coarse classes.** Rejected: both the corpus and live
  disagree, and the harness enforces exactly this slot.
- **A new typed variant for the delete-connected class.** Rejected: the
  nested-class scan already classifies the payload, the bindings and the
  GQLSTATUS derivation both read it, and a variant gains no consumer.
- **Keep `toInteger()` in the argument class.** Rejected: live raises
  the type exception at 22003; ADR-0175's note is the over-generalization,
  not the class.
- **Change the procedure-not-found Bolt code to match the class.**
  Rejected: live sends the procedure code under 42001, and the code is
  what clients branch on.

## Consequences

- The Python binding's `kind`/`tck_class`, the C ABI's error metadata and
  the classes any surface derives from the binding metadata move for
  these sites. The Bolt code and GQLSTATUS move with them, both being
  derived from the class: `toInteger()` moves to `Statement.TypeError`,
  the vector, `trim()` and batch-size sites move to
  `Statement.ArgumentError`, and `round()`/`replace()` leave
  `Statement.SemanticError`. Recorded stages are unchanged.
- Rendered messages move with the class: the argument sites gain the
  `semantic error: <Kind>: ` prefix, the delete-connected error gains the
  class token, and `toInteger()` renders the type-class prefix.
- ADR-0175 gains a Later references note for its 22N03 rows.
