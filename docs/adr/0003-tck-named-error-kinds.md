---
type: adr
area: engine
name: 0003-tck-named-error-kinds
status: accepted
tags:
  - conformance
  - errors
title: Typed engine errors carry openCypher TCK error kinds
REFERENCES:
  - "[[adr/0502-typed-c-abi-v2]]"
  - "[[adr/0511-boolean-sites-typed-invalid-argument-type]]"
---

# Typed engine errors carry openCypher TCK error kinds

To replace the stringly-typed `Result<_, String>` error channel, engine errors
become a typed `EngineError { Syntax, Semantic, Runtime, Storage }` whose
`kind()` accessor returns openCypher TCK error names (`SyntaxError`,
`TypeError`, `ParameterMissing`, `VariableAlreadyBound`, …). We considered
modeling errors on Neo4j's GQLSTATUS codes instead — the cip-eval effort
already targets Cypher 25 semantics, and GQLSTATUS is Neo4j's forward-looking
system — but chose TCK names because: (1) the TCK harness must assert error
*types* to make the compliance claim honest, and it speaks TCK names, not
GQLSTATUS; (2) the engine's existing informal taxonomy is already
TCK-name-shaped string prefixes (`"ParameterMissing: …"`), making the
migration mechanical; and (3) a `gql_status()` accessor can be added to the
same enum later without breaking any consumer, whereas starting from hundreds
of GQLSTATUS codes front-loads taxonomy work no current consumer needs.
`Display` output is preserved verbatim so the CLI and existing tests are
unaffected.

## Consequences

- The TCK harness can check expected error types (previously any error
  passed); Python's PEP 249 exception tree, FFI error codes, structured
  server errors, and Swift error reasons all map from `kind()`.
- cip-eval's GQLSTATUS assertion steps remain satisfied by the documented
  any-error-class limitation until a `gql_status()` accessor lands.
- Error message *text* stays informal; only the kind is contract. Changing a
  message is not a breaking change; changing a kind is.

## Later references

[ADR-0511](./0511-boolean-sites-typed-invalid-argument-type.md) changes the kind of one family this ADR pinned as SyntaxError: the
boolean-requiring sites (`bool_or_null` in the executor; the predicate and
`AND`/`OR`/`XOR` checks in the planner) now raise the typed
`InvalidArgumentType` variant, so they classify as Neo4j's TypeError class
(`22G03` / `Neo.ClientError.Statement.TypeError`) instead of `SyntaxError`
(`42N06`). Everything else here still holds: the vocabulary stays TCK-named,
message text stays informal, and a kind change is the breaking change this
ADR says it is — which is why the convergence got its own ADR. The
encouraged `gql_status()` accessor landed as the ABI v2 classification table
([ADR-0502](./0502-typed-c-abi-v2.md) §2).
