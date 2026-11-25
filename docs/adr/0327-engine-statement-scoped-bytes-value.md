---
type: adr
area: engine
claim: 43c9b20a07541fc7f5a090c2d09a8b9d747386c5
name: 0327-engine-statement-scoped-bytes-value
status: proposed
tags:
  - values
title: The engine gains a statement-scoped byte-array value
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0170-shared-value-payloads]]"
  - "[[adr/0219-python-parameter-conversion]]"
  - "[[adr/0224-result-column-types]]"
  - "[[adr/0328-bolt-bytes-parameters-round-trip]]"
  - "[[adr/0688-tostring-renders-every-list]]"
---

# ADR-0327: The engine gains a statement-scoped byte-array value

## Context

The engine's runtime `Value` (cypherlite-core) has no byte-array variant,
and openCypher defines no byte-array literal. [ADR-0219](./0219-python-parameter-conversion.md) therefore sends a
Python `bytes` parameter as a `LIST<INTEGER>` of byte values, and
[ADR-0224](./0224-result-column-types.md) reserves the column type name `BINARY` "so a future distinct
value kind does not renumber or rename the set".

Neo4j drivers send byte arrays as PackStream Bytes, and Neo4j returns them
unchanged finding F-64, neo4rs D-25). [ADR-0328](./0328-bolt-bytes-parameters-round-trip.md) makes the Bolt
surface do the same, which needs an engine value that can carry the bytes
from a parameter to a result. Neo4j can also store byte arrays as
properties. The user decided on 2026-10-16 that CypherLite takes the
statement-scoped half now: persistence needs WAL, snapshot and checkpoint
codec work, which is the Linear follow-up **"Engine: persist bytes
(byte-array) values as node and relationship properties (WAL, snapshot and
Parquet/JSON checkpoint codecs)"**.

## Decision

1. **The value.** `cypherlite_core::Value` gains `Bytes(Arc<[u8]>)`, a
 shared payload like `String` ([ADR-0170](./0170-shared-value-payloads.md)). No Cypher expression creates
   one. It enters only as a query parameter (the Bolt surface, or a Rust
   caller passing `Value::Bytes`) and flows through expressions, lists and
   maps into results.
2. **Semantics.**
   - Equality, hashing and DISTINCT/grouping keys are byte-wise. Bytes
     never equal a list of the same integers.
   - `<`/`>` compare byte-wise (element by element, then length, like a
     list). ORDER BY places BYTES directly after LIST: Neo4j orders a byte
     array as a sequence value.
   - `valueType()` reports `LIST<INTEGER NOT NULL> NOT NULL`
     (`LIST<NOTHING> NOT NULL` when empty), and `IS :: LIST<INTEGER>`
     holds, as for a Neo4j byte array. `IS :: PROPERTY VALUE` is false.
   - `toString()` rejects it like a homogeneous list
     (`InvalidArgumentValue: toString()`).
 - `logical_bytes` ([ADR-0042](./0042-graph-memory-limit.md)) counts a 16-byte header plus the length,
     the String formula.
 - The column type stamp ([ADR-0224](./0224-result-column-types.md)) is the reserved `BINARY`.
3. **Statement-scoped.** Storing a byte array (or a list holding one) as a
   node or relationship property fails before any write with the typed
   `Error::TypeError` (Display `type error: TypeError: byte array values
   cannot be stored as node or relationship properties; they live only
   inside a statement (parameter to result)`). CREATE, MERGE, `SET n.p`,
   `SET n = map` and `SET n += map` share the one check
   (`check_property_value`). The tagged WAL/checkpoint encoder and the
   JSON snapshot bridge reject a byte array explicitly as a second line of
   defence. No codec format changes.
4. **Other surfaces.** `CypherValue` (cypherlite-model) gets no new
   variant. `value_to_cypher` crosses a byte array as the `LIST<INTEGER>`
 of its byte values 0-255, the shape [ADR-0219](./0219-python-parameter-conversion.md) already gives Python
   `bytes`, so the FFI clients (JS, Java, Go, C), Python, WASM, HTTP, MCP
   and the CLI render it as that list. `to_cypher_literal`,
   `to_display_string` and `to_json_plain` use the same list. None of
   those surfaces can supply a byte array today (their parameters are
 JSON or [ADR-0219](./0219-python-parameter-conversion.md)'s list), so only a Rust embedder or Bolt reaches this
 path. Bolt sends real PackStream Bytes ([ADR-0328](./0328-bolt-bytes-parameters-round-trip.md)).

## Alternatives considered

- **Add `CypherValue::Bytes` too.** Every surface could carry real bytes.
  Rejected for now: it changes the wire JSON envelope, Arrow, Python and
  the persistence codecs, each a public format decision of its own, for
  values those surfaces cannot receive. The persistence follow-up is the
  place to decide it.
- **Persist byte arrays now.** Full Neo4j parity. Rejected by the user's
  2026-10-16 decision: the codec work is the Linear follow-up named above.
- **Make `value_to_cypher` fail on bytes.** No lossy crossing. Rejected:
  it makes the public, infallible bridge fallible for every caller,
  storage included, to guard a value no non-Bolt surface can produce. The
 list crossing is the representation [ADR-0219](./0219-python-parameter-conversion.md) already documents.
- **Reuse `Value::List` of integers for bytes.** No new variant. Rejected:
  the Bolt surface could not tell bytes from a list on the way out, so a
  parameter would not round trip.
- **A `BYTES` type name for `valueType()`.** Rejected: Neo4j's Cypher type
  system has no byte-array type and reports a byte array as a list of
  integers.

## Consequences

- `cypherlite::Value` is a public Rust enum: a new variant breaks
  exhaustive matches in embedders. `docs/api/rust/spec.md` documents it.
- Every exhaustive match on `Value` in the workspace has an explicit
  `Bytes` arm.
- A byte-array property fails with `Error::TypeError`, which the Bolt
  failure mapping sends as `Neo.ClientError.Statement.TypeError`.
- The surfaces' docs say a byte array renders as a list of byte values.
  When the persistence follow-up lands, it replaces the statement-scoped
  rule and decides the `CypherValue` question.

## Later references

- [ADR-0688](./0688-tostring-renders-every-list.md): the §2 analogy "`toString()` rejects it like a
  homogeneous list" lost its referent — every list renders now, and a byte
  array is the one value `toString()` rejects. The bytes rejection itself
  stands.
