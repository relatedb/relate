---
type: adr
area: engine
name: 0224-result-column-types
status: accepted
tags:
  - result-metadata
title: Engine-reported per-result-column types
REFERENCES:
  - "[[adr/0220-tabular-abi-single-entry-points]]"
  - "[[adr/0221-engine-reported-statement-elapsed]]"
  - "[[adr/0223-wire-entity-kind-tags]]"
  - "[[adr/0327-engine-statement-scoped-bytes-value]]"
  - "[[adr/0502-typed-c-abi-v2]]"
---

# ADR-0224: Engine-reported per-result-column types

## Context

LABS-290. Today only the Python binding surfaces per-column type metadata,
and it is client-side guesswork: `Cursor.description` (PEP 249) infers each
column's `type_code` via `infer_type_code`
(`surfaces/cypherlite-python/python/cypherlite/types.py`) over the first
row's value, folding everything into four DB-API comparison objects
(`STRING`, `BINARY`, `NUMBER`, `DATETIME`) and returning `None` for anything
else. Nodes, relationships, paths, points, vectors and durations report no
type at all, and an empty result cannot infer anything.

The engine already carries the rest of the result metadata family:
`cypherlite::QueryResult` carries `stats`, `namespace`, `query_type` and
`elapsed` ([ADR-0221](./0221-engine-reported-statement-elapsed.md)), the canonical wire envelope
(`cypherlite_model::wire::response_envelope`) carries `columns`, `rows`,
`stats` and `elapsed_ms` plus the entity `kind` discriminators on entity
values ([ADR-0223](./0223-wire-entity-kind-tags.md)), and the C ABI exposes the metadata as
`cypherlite_result_query_type` / `cypherlite_result_namespace` /
`cypherlite_result_stats` / `cypherlite_result_elapsed_ms`
(`surfaces/cypherlite-ffi/src/tabular.rs`). Column *types* are the one
piece of per-column metadata still invented client-side, which the
result-metadata parity rule in AGENTS.md ("when one surface gains a piece
of result metadata, every surface that can expose it gets it in the same
change") says must be promoted to engine-stamped metadata and rolled out
cross-surface in one change.

## Decision

**1. Canonical type-name set.** The engine stamps each column with one of
18 type-name strings, aligned with the wire value taxonomy in
`cypherlite/cypherlite-model/src/wire.rs` (the `CypherValue` variant table,
and the lowercase `"kind"` values `"node"` / `"relationship"` from
[ADR-0223](./0223-wire-entity-kind-tags.md)) but spelled in the uppercase form clients and specs already use:

`STRING`, `INTEGER`, `FLOAT`, `BOOLEAN`, `NODE`, `RELATIONSHIP`, `PATH`,
`LIST`, `MAP`, `POINT`, `DATETIME`, `DATE`, `TIME`, `DURATION`, `VECTOR`,
`UUID`, `BINARY`, `ANY`.

`UUID` and `BINARY` are reserved names in the taxonomy: `CypherValue` has
no distinct variants for them today (the wire table in `wire.rs` does not
list them), so values of those logical kinds currently encode as `STRING`
and stamp `STRING`; the reserved names exist so a future distinct value
kind does not renumber or rename the set. The engine's core `Value` does
have a `Uuid` variant, but it crosses the `value_to_cypher` bridge as
`Str`, so it likewise stamps `STRING` — the wire taxonomy cannot
distinguish it. `ANY` covers `Null` values, the
empty-result column, and anything a future variant introduces before the
set is extended. Geometry stamps as `POINT`'s sibling spelling the value
table defines — `Geometry` is a distinct `CypherValue` variant but is not
in the per-column set; it reports `POINT` (the same coordinate family) to
keep the client-facing set at the 18 names.

**2. Stamping rule.** Per-column type strings are derived from the FIRST
row at result construction, in the engine's public
`cypherlite::QueryResult` (one `column_types: Vec<String>` beside
`columns`, same length, same order). A column whose first-row value is
`Null`, or a result with no rows, stamps `ANY`. The type name of a column
describes its first row only — Cypher columns are heterogeneous, and no
claim is made about later rows.

**3. Cross-surface rollout (same change, per the parity rule).**

- **Wire envelope**: `response_envelope` gains `columnTypes` beside
  `columns` (an array of the 18-name strings, `["ANY", …]` for an
  empty result); `query_result_to_wire` feeds it from
  `QueryResult::column_types`. `error_envelope` is unchanged.
- **C ABI**: `cypherlite_query_result` carries the per-column type
  strings, exposed as `const char *cypherlite_result_column_type(result,
  col)` — result-owned and valid until `cypherlite_destroy_result`,
  matching the existing accessor family (`cypherlite_result_query_type`,
  `cypherlite_result_namespace`, `cypherlite_result_stats`,
  `cypherlite_result_elapsed_ms` in
  `surfaces/cypherlite-ffi/src/tabular.rs`). Out-of-range `col` returns
  `NULL`, matching `cypherlite_result_column`.
- **Python**: `Cursor.description`'s `type_code` re-sources from the
  engine field (mapped onto the PEP 249 type-comparison objects;
  unmapped names report the engine string itself). The client-side
  `infer_type_code` inference is deleted — it was the placeholder this
  ADR replaces, not a fallback to keep.
- **JavaScript**: `Result.columnTypes` (array of the engine strings),
  read through the new FFI export while the result handle is alive.
- **CLI / Bolt**: surfaced only where those surfaces already present
  column metadata; no new output field is invented for them.

## Alternatives considered

- **Plan-time static typing (derive the column type from the logical
 plan)** — rejected: Cypher is dynamically typed ([ADR-0221](./0221-engine-reported-statement-elapsed.md) makes the same
  observation for timing; the type of `n.prop`, a `CASE`, or a `coalesce`
  cannot be known until values exist), so no plan-time column type exists
  to derive. The first row is the earliest moment an honest type exists,
  and it is honest about being a first-row type.
- **Per-column type on the wire envelope only, no C ABI accessor** —
  rejected: breaks the result-metadata parity rule; the C ABI is the
  contract every language client wraps, so metadata that exists only in
  the JSON envelope forces clients to invent a parallel parse (exactly
 what [ADR-0220](./0220-tabular-abi-single-entry-points.md)'s single entry points were meant to prevent).
- **Client-side inference everywhere (the status quo, generalized)** —
  rejected: it is what exists today — three-plus implementations to keep
  in sync, one per surface, each with its own taxonomy; empty results
  cannot infer anything; and node/relationship/path/point/vector columns
  report no type at all. The engine already materializes the first row at
  result construction, so the stamp costs nothing client-side and one
  implementation instead of N.

## Consequences

- `cypherlite::QueryResult` gains a public field (`column_types`) —
  additive, but literal constructions inside the workspace seed
  `vec!["ANY"; columns.len()]`.
- `wire::response_envelope` gains a key; the golden wire fixtures are
  re-pinned in the same change.
- The C ABI header is hand-edited in the same change — it is
  hand-maintained C (`make gen-ffi-header` is gone) — and
  `docs/api/c/spec.md` documents the new accessor.
- The Python `types.py` docstring and `docs/api/python/` flip from
  "infers the type code from the first row" to "reads the engine-reported
  column type"; the bundled library skill and the CLI skill-bundle
  Python reference update in the same change (the bundled-skill rule).
- TS typegen for the JavaScript `columnTypes` field stays OUT of scope —
  follow-up work, not part of this decision.

## Later references

- [ADR-0327](./0327-engine-statement-scoped-bytes-value.md): the engine's statement-scoped byte array `Value::Bytes` stamps
  the reserved `BINARY` name. The 18-name set is unchanged.
- [ADR-0502](./0502-typed-c-abi-v2.md): the typed C ABI v2 removes `cypherlite_result_column_type` from
  the C ABI — per-value kinds from the native value walker replace the
  first-row stamp for C-ABI clients.
