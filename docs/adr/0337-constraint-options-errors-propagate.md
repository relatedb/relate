---
type: adr
area: engine
claim: e9fa7c0d09c8d2ea73586864e1b74293836f8c31
name: 0337-constraint-options-errors-propagate
status: proposed
tags:
  - catalog
title: CREATE CONSTRAINT OPTIONS lowering and eval errors propagate
REFERENCES:
  - "[[adr/0307-bolt-failure-carries-neo4j-status-codes]]"
  - "[[adr/0308-absent-parameter-fails-before-execution]]"
---

# ADR-0337: CREATE CONSTRAINT OPTIONS lowering and eval errors propagate

## Context

`catalog_create_constraint` validates a `CREATE CONSTRAINT … OPTIONS {…}`
map by lowering each value expression and evaluating it at run time
(`cypherlite-executor/src/exec/catalog_schema.rs`):

```rust
let value = cypherlite_front::lower_expr(value)
    .ok()
    .and_then(|expr| eval(graph, &Row::new(), &expr, params, cypher_version).ok());
if key != "indexConfig" || !matches!(value, Some(Value::Map(ref map)) if map.is_empty()) {
    return Err(Error::semantic("InvalidOptionsMap"));
}
```

The `.ok()`/`.ok()` chain is a silent fallback: a missing parameter in
`OPTIONS {indexConfig: $p}` is swallowed and re-reported as the generic
`InvalidOptionsMap`, even though [ADR-0308](./0308-absent-parameter-fails-before-execution.md) split the sibling
`CREATE INDEX … OPTIONS $p` path out to the typed
`ParameterMissing`. A lowering failure or an eval failure (e.g. an
arithmetic error) is likewise eaten and re-labelled as a shape error.

## Decision

`catalog_create_constraint` propagates the lowering and eval errors with
`?` and validates only the resulting shape: a key other than
`indexConfig`, a non-map value, or a non-empty map is the existing
`InvalidOptionsMap` semantic error; an absent parameter surfaces the typed
`ParameterMissing` ([ADR-0308](./0308-absent-parameter-fails-before-execution.md)), and an eval failure surfaces its own error.
The constraint is not created on any failure.

## Alternatives considered

- **Keep the `.ok().ok()` chain and document `InvalidOptionsMap`.**
  Rejected: it is the silent-fallback shape AGENTS.md bans, and it makes
  one failure report a different class depending on which catalog command
  spells it.
- **Map every eval error to `ParameterMissing`.** Rejected: only an
  absent parameter is `ParameterMissing`; an arithmetic or type failure
  must keep its own class.
- **Skip validation for a supplied parameter value.** Rejected: the shape
  check (only an empty `indexConfig` map is supported) is what the
  command promises; skipping it would silently accept options the engine
  ignores.

## Consequences

- `CREATE CONSTRAINT … OPTIONS {indexConfig: $p}` with `p` absent now
 fails with `ParameterMissing` ([ADR-0307](./0307-bolt-failure-carries-neo4j-status-codes.md)'s wire class), matching the
  `CREATE INDEX` path; the shape error is unchanged for supplied values.
- Tests: `catalog_schema.rs` unit tests pin both outcomes;
  `cypherlite/tests/parameter_missing.rs` adds the constraint-OPTIONS
  statement to its run-time catalog-parameter list.
