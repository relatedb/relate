# 0112. Inferred schema via `db.schema.*` procedures

- **Status**: Accepted
- **Date**: 2026-09-26
- **Deciders**: CypherLite maintainers

## Context

LABS-48 originally asked for a richer `.schema` dot-command in the interactive
shell. That request predates the current shell contract: ADR-0058 reduced the
shell to exactly six dot-commands and **withdrew** `.schema` (along with the
other introspection commands), ruling that introspection goes through Cypher
itself. ADR-0058 also states that reinstating any withdrawn command requires a
new ADR referencing it. The workspace was later restructured (ADR-0103), which
moved the relevant crates under `cypherlite/`, `surfaces/`, and `testing/` but
did not change the shell contract.

The engine's procedure registry already advertises the two Neo4j built-in
schema procedures as **non-executable stubs**:
`db.schema.nodeTypeProperties` and `db.schema.relTypeProperties` are registered
in `cypherlite/cypherlite-executor/src/procedure_registry.rs` with
`executable: false` and an empty `returns` list. They are visible to clients
that enumerate procedures but cannot be called.

The decision below re-opens LABS-48 as an engine capability reached through
Cypher, not as a shell dot-command. It implements the two procedures (phase 1),
leaves per-label counts and other aggregates to plain Cypher, and defers any
shell sugar to a separate phase-2 ADR.

## Decision

### Procedures and exact return columns

Implement the two procedures with Neo4j-compatible signatures and return
columns. No count or other CypherLite-specific columns are added.

```
db.schema.nodeTypeProperties() ::
  (nodeType STRING,
   nodeLabels LIST<STRING>,
   propertyName STRING,
   propertyTypes LIST<STRING>,
   mandatory BOOLEAN)

db.schema.relTypeProperties() ::
  (relType STRING,
   propertyName STRING,
   propertyTypes LIST<STRING>,
   mandatory BOOLEAN)
```

Both currently-registered stubs become executable, and their `returns` list is
populated to match. Their READ mode and names are unchanged.

### Semantics

- **Inferred by scanning stored entities.** The property schema is derived by
  scanning the entities actually stored in the database. It is **not** sourced
  from the declared graph type spec; a declared type that no stored entity
  exercises contributes no rows.
- **Type names are `valueType()` names.** Each entry in `propertyTypes` is a
  Cypher type name — the same names the engine's `valueType()` function returns
  (`cypherlite/cypherlite-executor/src/eval/typename.rs`). This follows Neo4j's
  Cypher 25 behaviour; Neo4j's Cypher 5 returned Java type names, which we do
  not adopt.
- **`mandatory`** is `true` when the property key is present on every entity in
  the category, and `false` otherwise.
- **`nodeType`** is the node's full sorted label set rendered as `:A:B` (all
  labels, colon-prefixed, sorted). **`relType`** is the relationship's single
  type rendered as `[:TYPE]`.
- **The row rule for categories with no properties.** A node or relationship
  category that has no properties produces exactly one row for that category
  with `propertyName: null`, `propertyTypes: []` (the empty list), and
  `mandatory: false`. This follows Neo4j's documented convention for these
  procedures: a category is still listed even when it has no properties, rather
  than being omitted. (The convention is chosen to match Neo4j; the exact
  property-less example is not reproduced in the operations manual, so this is
  stated as CypherLite's pinned rule rather than a quoted source.)

### Scope

Phase 1 is exactly the two procedures above. Per-label counts and any other
introspection aggregates remain reachable through plain Cypher (`count(...)`,
`labels(n)`, `type(r)`). Reintroducing a `.schema` shell dot-command, or any
other CLI sugar over these procedures, is deferred to a separate phase-2 ADR
that must reference ADR-0058.

## Alternatives considered

- **Reinstate the `.schema` dot-command (the literal LABS-48 ask).** Rejected:
  `.schema` was withdrawn by ADR-0058, introspection belongs to Cypher, and
  ADR-0058 requires a new ADR referencing it before any withdrawn command
  returns. A shell-only schema surface would also split one capability across
  two contracts (shell and Cypher) and add a public CLI API to test and
  document forever. Shell sugar, if wanted, is a phase-2 decision over the
  Cypher procedures, not a replacement for them.
- **Add count / observations extension columns** (e.g. a per-type node count,
  or an APOC-style `propertyObservations` / `totalObservations`). Rejected:
  counts are trivially expressed in Cypher (`MATCH (n:Label) RETURN count(n)`),
  the Neo4j compatibility target for these procedures is the five- and
  four-column built-in shapes, and extra columns would diverge from Neo4j and
  commit us to a bespoke result contract to maintain. Plain Cypher stays the
  single way to compute aggregates.
- **Source the schema from the declared graph type spec.** Rejected: the
  procedures are documented as the *derived* property schema of the nodes and
  relationships in the graph. A declared spec describes what is permitted, not
  what exists, so it would report types and mandatory properties absent from
  the data and miss inferred shapes the declaration does not cover. Scanning
  stored entities is the compatible and truthful source.

## Later references

- ADR-0153 adds `db.schema.visualization()` as the third `db.schema.*`
  procedure, satisfying this ADR's follow-up-ADR requirement for additional
  schema procedures, and pins its virtual-entity semantics (negative ids,
  virtual-node side-table registration, `{name, indexes, constraints}` node
  shape, zero rows when no stored labels exist).

## Consequences

- `db.schema.nodeTypeProperties` and `db.schema.relTypeProperties` become
  executable; the registry entries' `executable` and `returns` fields change in
  the same change as the implementations.
- Clients that enumerate procedures see populated return columns instead of an
  empty list.
- The result contract is the two Neo4j-compatible shapes above; no count or
  CypherLite-specific columns are added, so counts remain a Cypher concern and
  are not part of this API.
- Schema inference costs a scan of stored entities at call time; there is no
  cached or declared-schema fast path.
- Any `.schema` CLI sugar, or any additional schema procedure or column, needs
  a follow-up ADR; shell sugar additionally needs a new ADR referencing
  ADR-0058.
