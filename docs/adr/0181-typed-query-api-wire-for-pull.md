# 0181. Typed Query API JSON wire format for pull

- **Status**: Proposed
- **Date**: 2026-10-07
- **Deciders**: CypherLite maintainers (Linear LABS-186)

## Context

`cypherlite pull` (ADR-0080, ADR-0158) POSTs shape queries to the Neo4j
Query API v2 with `Accept: application/json`. In that representation a
spatial or temporal property arrives as a plain JSON string — a WGS84
point as `"SRID=4326;POINT (-0.1278 51.5074)"`, a date as
`"2015-03-26"`, a duration as `"P14DT16H12M"` — and the loader stores it
as STRING. The value's type is lost, and no later export can restore it
(LABS-186). The research fixture
`surfaces/cypherlite-load/tests/fixtures/typed_wire.json` (verbatim
response bodies captured from `neo4j:2026.09-enterprise` and
`neo4j:5.26-enterprise`; seed in `typed_wire_seed.cypher`) records the
ground truth for the typed alternative:

- The typed result media type is
  `application/vnd.neo4j.query[.v1.0|.v1.1|.v1.2]`; every value in
  `data.values` is a `{"$type": …, "_value": …}` object, including the
  property values inside nodes and relationships.
- No single-valued typed `Accept` works on both probed servers.
  `application/vnd.neo4j.query.v1.1` and `.v1.2` are HTTP 406 on
  `neo4j:5.26-enterprise`, and the unversioned media type and `.v1.0`
  make `neo4j:2026.09-enterprise` fail (HTTP 500 for a node carrying
  UUID + VECTOR; per-property "Type … is not supported." errors) because
  those versions cannot serialize those types.
- The multi-valued ladder `application/vnd.neo4j.query.v1.2;q=1.0,
  …v1.1;q=0.9, …v1.0;q=0.8, …query;q=0.7` negotiates `.v1.2` on
  2026.09 and `.v1.0` on 5.26 (fixture `negotiation_findings`, 12 rows).
- The capture carries 19 `$type` tags (fixture `type_tags`), and every
  captured temporal/point `_value` parses through the engine today
  (`engine_parse_gaps` is empty).
- `application/json` is deliberately absent from the ladder, so a
  server that cannot serve typed JSON fails loudly (406) instead of
  silently returning untyped JSON.

The engine already owns every constructor the decoder needs
(`parse_iso`, `PointValue`, `uuid_parse`, `VectorCoordKind`), but
`cypherlite-load` has no direct `cypherlite-core` dependency today.
`Base64` (BYTE ARRAY) and `Unsupported` are the two captured tags the
engine cannot represent, and an unknown tag must be handled too.

## Decision

1. **The pull path requests typed JSON with one multi-valued `Accept`
   header, and nothing else.**

   ```text
   application/vnd.neo4j.query.v1.2;q=1.0, application/vnd.neo4j.query.v1.1;q=0.9, application/vnd.neo4j.query.v1.0;q=0.8, application/vnd.neo4j.query;q=0.7
   ```

   `application/json` is not in the list: a source without typed JSON
   answers 406 and the pull fails with `LoadError::Http`, never with a
   silent untyped fallback. The server picks the newest media type it
   supports (observed `.v1.2` on 2026.09, `.v1.0` on 5.26). Request
   bodies stay `Content-Type: application/json`; the statement envelope
   (`statement` / `parameters`) does not change.

2. **Only `load_streaming` requests typed JSON.** `run_query` and
   `run_query_streaming` keep `Accept: application/json` and their
   current signatures: their callers (the graph-examples reference
   client's row comparison, the docker/admin probes) consume plain
   `data.values` JSON. Internally the shared `post_query` prelude takes
   the `Accept` value as an argument; the pull path gets its own
   streaming entry point (`http::run_query_streaming_typed`), which
   `load_streaming` alone calls. `subgraph_from_v2` and the streaming
   extractor decode typed bodies only (item 7).

3. **The typed wire shape is `{"$type": <tag>, "_value": <payload>}` and
   the tag table is fixed.** Property values, cells, and nested values
   all use the same envelope. The engine value each captured tag decodes
   to:

   | `$type` | `_value` payload | Engine value |
   | --- | --- | --- |
   | `Boolean` | JSON `true`/`false` | `Value::Bool` |
   | `Integer` | decimal JSON string (`"42"`) | `Value::Int` (see item 4) |
   | `Float` | JSON string (`"3.14159"`, `"3.2E9"`) | `Value::Float` (see item 4) |
   | `String` | JSON string | `Value::String` |
   | `Date` | ISO date string (`"2015-03-26"`) | `Value::Temporal(parse_iso("date", s))` |
   | `Time` | offset/zoned time string (`"12:50:35.556+01:00"`, `"01:02:03Z"`) | `parse_iso("time", s)` |
   | `LocalTime` | time string (`"12:50:35.556"`) | `parse_iso("localtime", s)` |
   | `LocalDateTime` | `"2015-07-04T19:32:24"` | `parse_iso("localdatetime", s)` |
   | `OffsetDateTime` | `"2015-11-21T21:40:32.142-01:00"` | `parse_iso("datetime", s)`; the engine has one `datetime` kind, so an offset-only datetime is a `DateTime` with `zone: None` |
   | `ZonedDateTime` | `"2015-11-21T21:40:32.142Z[Antarctica/Troll]"`, `"2020-01-01T10:00:00+01:00[Europe/Stockholm]"` | `parse_iso("datetime", s)`; the `[...]` name is kept as the zone |
   | `Duration` | ISO duration string (`"P14DT16H12M"`) | `parse_iso("duration", s)` |
   | `Point` | EWKT string (grammar in item 5) | `Value::Point(PointValue { srid, x, y, z })` |
   | `UUID` | canonical hyphenated string | `Value::Uuid(uuid_parse(s))`; `None` is malformed |
   | `Vector` | `{"coordinatesType": …, "coordinates": [ … ]}` | `Value::Vector { coord, values }` (item 6) |
   | `List` | JSON array of tagged values | `Value::List` — decode each element recursively |
   | `Node` | `{"_element_id", "_labels", "_properties"}` | element position: `SubgraphNode` (labels plus a `BTreeMap<String, Value>` property map) |
   | `Relationship` | `{"_element_id", "_start_node_element_id", "_end_node_element_id", "_type", "_properties"}` | element position: `SubgraphRel` (same property map) |
   | `Path` | flat JSON array of `Node`/`Relationship` typed objects | element position only (not a captured property tag): walked, each node/rel decoded as above; contributes elements, never a property value |
   | `Base64` | base64 string (`"A2+/"`) | **error** (item 7) |
   | `Unsupported` | server reason string | **error** (item 7) |
   | any other tag | any | **error** (item 7) |
   | `Null` (defensive) | JSON `null` | `Value::Null`; cannot occur as a Neo4j property, armed anyway |
   | `Map` (defensive) | JSON object of tagged values | `Value::Map` — recursive; cannot occur as a Neo4j property, armed anyway |

   `Node`, `Relationship`, and `Path` are element tags: they are valid in
   cell (or nested-list-of-elements) position and never as a property
   value; a property carrying one is an unsupported typed value. A
   `Node`/`Relationship` payload missing `_element_id`, or a property
   map that is not an object, is malformed; `_labels` and `_properties`
   absent means empty (the wire always emits them, this is only
   defensive tolerance).

   A missing `_value`, or a `_value` whose JSON shape does not match its
   tag's payload — a non-bool `Boolean`, a non-string
   `String`/temporal/UUID/point, a non-array `List`, a non-object
   `Map`/`Vector` — is malformed.

4. **Numeric payloads are JSON strings.**
   - `Integer._value` is a decimal JSON string (`"42"`), parsed as
     `i64`. A JSON number, a fraction, an exponent, or any other
     non-`i64` string is malformed.
   - `Float._value` is a JSON string. The three special spellings the
     engine's own tagged encoder emits
     (`cypherlite/cypherlite-core/src/value/json.rs`) are recognized
     first: `"NaN"` → `f64::NAN`, `"Infinity"` → `f64::INFINITY`,
     `"-Infinity"` → `f64::NEG_INFINITY`. Every other string goes
     through `str::parse::<f64>()`, which accepts fractional and
     scientific forms such as `"3.14159"` and `"3.2E9"`. A JSON number
     or a string that does not parse is malformed.

5. **Points use the EWKT form, with or without `Z`.** The captured
   grammar, parsed strictly:

   ```text
   SRID=<i32>;POINT (<x> <y>)          → PointValue { srid, x, y, z: None }
   SRID=<i32>;POINT Z (<x> <y> <z>)    → PointValue { srid, x, y, z: Some(z) }
   ```

   `<x>`, `<y>`, `<z>` are decimal f64 tokens (captured: `-0.1278`,
   `51.5074`, `11.0`, `1.0`); `<i32>` is the SRID (captured: 4326, 4979,
   7203, 9157). Any other shape — a missing `SRID=`, a non-integer SRID,
   `POINTZ` without the space, two coordinates after `Z`, three without
   it — is malformed. `z` is `None` for the 2D form and `Some` for the
   3D form.

6. **Vector coordinates map by their declared kind.**
   `coordinatesType` is one of the six Neo4j spellings `FLOAT64`,
   `FLOAT32`, `INT64`, `INT32`, `INT16`, `INT8`; the decoder resolves it
   with `VectorCoordKind::from_alias`, whose alias vocabulary
   accepts exactly those six (`from_canonical_name` does not — it knows
   `FLOAT`/`INTEGER`, not `FLOAT64`/`INT64`). `coordinates` is a JSON
   array of strings; each element is parsed as `i64` for the integer
   kinds (`coord.is_integer()`) into `Value::Int`, or as `f64` for the
   float kinds into `Value::Float`. An unknown `coordinatesType`, a
   non-array `coordinates`, a non-string element, or a value that does
   not parse as the declared kind is malformed.

7. **Every unrepresentable value fails the pull, and the plain-JSON
   element path is removed.** A new `LoadError::TypedWire(TypedWireError)`
   variant wraps the decoder error (`#[from]`, so `?` converts).
   `TypedWireError` is a struct that wraps the decoder's `DecodeError`
   with the owning element id and the property key the decoder never
   sees:

   ```rust
   pub struct TypedWireError {
       pub element: String,
       pub property: String,
       pub error: DecodeError,
   }
   ```

   Its `Display` is `property {property:?} on element {element}: {error}`,
   so each `DecodeError` reads as its own message with the position in
   front:

   | `DecodeError` variant | Display |
   | --- | --- |
   | `Unsupported { tag: String }` | `unsupported typed value {tag:?}` |
   | `Malformed { tag: String, detail: String }` | `malformed {tag} value: {detail}` |
   | `Untagged` | `untagged value where a $type-tagged value was expected` |

   `DecodeError` is re-exported from `lib.rs` because the public
   `TypedWireError::error` field exposes it, so callers can name and
   match the decode reason; `decode_value` stays crate-internal — nothing
   outside `cypherlite-load` consumes the decoder function itself.

   `LoadError::TypedWire` displays as `typed wire decode failed: {0}`.
   Concrete example — a `Base64` property named `blob` on element
   `4:x:0`:

   ```text
   typed wire decode failed: property "blob" on element 4:x:0: unsupported typed value "Base64"
   ```

   The decoder fails on `Base64` (the engine has no byte-array value;
   the captured `:TypedUnmappable` node proves it occurs), on
   `Unsupported` (the server could not serialize the value), on any
   unknown `$type`, on any unparseable `_value` — including every entry
   of the fixture's `engine_parse_gaps`, which is empty today, so no
   captured payload needs a special case and a future gap fails instead
   of degrading to a string — and on any untagged cell or untagged
   property value (a server that ignored the typed `Accept`).

   The property key travels through `List`/`Map` recursion: a failing
   list element reports the owning property key, with the tag and
   `detail` naming the element that failed. In cell position the
   property key is the sentinel `"<cell>"`, and `element` is the plain
   cell's `elementId`/`_element_id` when it has one, else the sentinel
   `"<response>"`.

   The plain-JSON element path is deleted: `is_plain_node`,
   `is_plain_rel`, `node_from_plain`, `rel_from_plain`, `json_plain`,
   and `typed_scalar` are removed from `extract.rs`.
   `SubgraphNode.properties` and `SubgraphRel.properties` become
   `BTreeMap<String, Value>`, and `materialize.rs` builds `Value::Map`
   directly (the `Value::from_json_scalars` call disappears from the
   pull path; the core API itself is unchanged).

8. **`Null` and `Map` are armed defensively.** Neo4j property values are
   scalars or homogeneous lists of scalars, so neither tag can occur as
   a property; the decoder still maps them (`Value::Null`, recursive
   `Value::Map`) so the tag table and the recursion are total. They are
   the only two keys beyond the 19 captured tags.

9. **`cypherlite-load` gains a direct `cypherlite-core` dependency.**
   ADR-0103 R3 permits surfaces to depend on engine crates, and
   `PointValue`, `VectorCoordKind`, and `parse_iso` are not re-exported
   by the `cypherlite` facade; growing the facade's public API for one
   surface's need is the larger change. One workspace and one
   `Cargo.lock` mean no version skew. The decoder uses:

   - `cypherlite_core::temporal::parse_iso(kind, s)` with `kind` one of
     `date`, `time`, `localtime`, `datetime`, `localdatetime`,
     `duration`;
   - `cypherlite_core::PointValue { srid, x, y, z }`;
   - `cypherlite_core::VectorCoordKind::from_alias` (item 6);
   - `cypherlite_core::uuid_parse` (also re-exported as
     `cypherlite::uuid_parse`);
   - `cypherlite_core::Value` (the same type as `cypherlite::Value`).

   `TypedWireError` lives in `error.rs` next to `LoadError` and is
   re-exported from `lib.rs` like `QueryApiError`.

## Alternatives considered

- **Plain JSON with string heuristics.** Keep `Accept:
  application/json` and sniff date/time/point-shaped strings into typed
  values. Rejected: the plain wire carries no type marker, so a genuine
  STRING property `"2015-03-26"` and a DATE are indistinguishable; a
  heuristic silently retypes user data, and it would still miss
  durations, zoned datetimes, vectors, and UUIDs. The typed format tags
  every value; guessing is strictly worse.
- **Keep Base64 as a STRING (or store the decoded bytes as a string).**
  Decode `{"$type": "Base64", "_value": "A2+/"}` into a
  `Value::String`. Rejected: the engine has no byte-array value, and
  both candidates are wrong — the base64 text is not the property (a
  round-trip stores the encoding), and the raw bytes are not UTF-8 (a
  lossy conversion is silent corruption). A BYTE ARRAY that cannot be
  represented must fail the pull, exactly like `Unsupported`.
- **Keep the plain-JSON fallback.** When a cell lacks `$type`, fall
  back to today's `elementId`/`labels`/`properties` decode. Rejected:
  that is the bug — a server (or proxy) that ignored `Accept` would
  silently load untyped STRING values again. The typed `Accept`
  deliberately excludes `application/json`, and the decoder fails on
  untagged cells, so the failure is loud and deterministic instead.
- **Request typed JSON for every caller.** Pass the ladder in
  `run_query` and `run_query_streaming` too, including the
  graph-examples reference client. Rejected: their contract is the
  plain `data.values` JSON (the reference client streams rows and
  compares them against a reference answer; the docker/admin probes
  read plain values), and the typed decoder lands on the pull path only
  in this change; switching them would break those callers or force the
  full decoder into every caller for no consumer.
- **Pin a single typed media type** (for example `.v1.2`, or the
  unversioned type). Rejected: no single value works on both probed
  servers — `.v1.1`/`.v1.2` are 406 on 5.26, and the unversioned/`.v1.0`
  types make 2026.09 fail on nodes carrying UUID + VECTOR. The ladder is
  what lets each server negotiate its newest usable format.
- **Append `application/json` to the ladder as a last resort.**
  Rejected: a server falls back to the untyped representation exactly
  when it cannot serve the typed one, silently reintroducing LABS-186; a
  406 is the correct loud outcome.

## Consequences

- Pulled temporal, point, UUID, and vector properties round-trip as
  their engine types: `MATCH (n) RETURN n.date` yields a DATE, not the
  string `"2015-03-26"`.
- Both probed servers are covered: 2026.09 negotiates `.v1.2` (native
  UUID and VECTOR) and 5.26 negotiates `.v1.0` (those value types do not
  exist there). A server that serves neither typed media type fails the
  pull at the first statement with `LoadError::Http` (406).
- A source value the engine cannot represent (`Base64`, `Unsupported`,
  unknown tag, unparseable payload, untagged cell) aborts the pull with
  `LoadError::TypedWire`, naming the element id and property key.
  Batches already flushed (ADR-0158) remain in the destination; the
  destination is then non-empty, so a retry needs a fresh database
  (ADR-0158's refusal).
- `cypherlite-load` gains `cypherlite-core` in `[dependencies]`;
  `cargo xtask check-boundaries` R3 permits it, and the `cypherlite`
  facade does not grow.
- The extractor is typed-only: `is_plain_node`, `is_plain_rel`,
  `node_from_plain`, `rel_from_plain`, `json_plain`, and `typed_scalar`
  are deleted; `SubgraphNode`/`SubgraphRel` property maps become
  `BTreeMap<String, Value>`. `subgraph_from_v2` decodes typed bodies
  only, so its plain-JSON fixtures — and the loopback streaming
  fixtures — become typed bodies derived from the captured
  `typed_wire.json` responses.
- `run_query` / `run_query_streaming` signatures and behavior are
  unchanged; the graph-examples reference client and docker probes are
  untouched.
- Tests pin the contract: decoder unit tests cover every tag in
  `type_tags` and reproduce the `error_contract` display literally; an
  end-to-end loopback pull of the captured `mappable`/`unmappable`
  bodies verifies the engine-side types and the typed failure.
- No CLI flag changes: `--help` is untouched by this decision.
  `docs/api/cli/spec.md` records the type mapping — the `pull` section's
  tag table and the failure form above.
