---
type: adr
area: surface
date: 2026-09-23
deciders: CypherLite maintainers
name: 0088-explicit-arrow-value-provenance
status: proposed
tags:
  - parquet
  - values
title: Explicit provenance and versioned composite values in Arrow batches
REFERENCES:
  - "[[adr/0087-fallible-persistence-decoding]]"
---

# 0088. Explicit provenance and versioned composite values in Arrow batches

## Context

`cypherlite-arrow` currently writes node/relationship structs without a
graph-type marker; `from_arrow` guesses graph variants from field names and
defaults a missing or wrongly typed ID to zero. Its `json`/`tagged` Utf8
fallback uses untagged `cypherlite-model::wire` JSON and guesses nested nodes,
relationships, and paths from their shape. An ordinary map/list can therefore
become a graph entity. Any non-`tagged` `cypherlite.encoding` metadata currently
triggers JSON parsing, even a misspelling. Malformed JSON, properties, WKB, or
unsupported child types may become strings, empty maps, or nulls. The public
`record_batch_to_rows` API cannot report these errors. The WKB array helper
panics on a non-Binary array; untrusted counts can reserve memory before length
validation, and big-endian/nested headers are not interpreted correctly.

Arrow is a typed interchange format, not a license to infer Cypher graph
identity from a coincidentally similar user struct. The persistence encoding
of [ADR-0087](./0087-fallible-persistence-decoding.md) is distinct from the untagged JSON wire format used by old Arrow
composite columns.

The `typed-json-v1` Arrow writer currently repeats the model's Point,
Geometry, and Vector persistence payload builders even though the reader
already delegates their decoding to [ADR-0087](./0087-fallible-persistence-decoding.md). Its Float32 predicate uses
bitwise round-trip equality while model decoding uses numerical equality,
which disagrees on signed zero. Arrow timestamp timezone metadata also treats
an invalid numeric offset such as `+00:60` as a named timezone when its
fixed-offset parser declines it.

Full-column inference currently distinguishes public `ColType::Json` for
homogeneous composites or out-of-native-range temporal/duration values from
`ColType::Mixed` for incompatible kinds. Both already produce the same
`typed-json-v1` Utf8 field and exact encoding metadata, so the distinction
duplicates merge state without changing the representation.

The native Arrow `Time64(Nanosecond)` field carries only a time of day: the
current writer discards the offset in `CypherValue::Time(_, Some(offset))`.
For native `DateTime` timestamps, `format_offset` emits only `[+-]HH:MM`,
truncating the seconds of a constructible non-minute `FixedOffset` before
writing timezone metadata. The fallible [ADR-0087](./0087-fallible-persistence-decoding.md) model writer can protect
typed JSON payloads, but neither native path invokes it today.

Even with minute-aligned offsets, the column-wide timezone is currently taken
from the first DateTime: a later `-05:00` row after a `+02:30` row reopens with
the first row's offset and the wrong local time. A named-zone hint is also
lossy on the native path: the reader retains the UTC instant and name, but
reconstructs the DateTime with a UTC fixed offset, even for a single row.

The native spatial writer encodes `PointValue` as 2D WKB and silently drops
`z` on a 3D Point. Its spatial preflight checks SRIDs and nesting depth, but
not finite coordinates; the WKB decoder likewise accepts externally supplied
NaN or Infinity as valid Point/Geometry coordinates, including nested members.
Full-column `Int`+`Float` promotion also casts every integer with `i as f64`:
`9_007_199_254_740_993` changes value, and boundary integers such as
`i64::MAX` cannot be represented exactly in `f64`.

## Decision

Use **field** metadata as provenance, matched by exact key and value:

| Arrow field | Metadata | Meaning |
| --- | --- | --- |
| Node `Struct` | `cypherlite.value_type=node-v1` | Cypher node |
| Relationship `Struct` | `cypherlite.value_type=relationship-v1` | Cypher relationship |
| Vector `FixedSizeList<primitive>` | `cypherlite.value_type=vector-v1` | Cypher vector |
| Composite/mixed `Utf8` | `cypherlite.encoding=typed-json-v1` | Version 1 recursively typed JSON below |
| Spatial `Binary` | `ARROW:extension:name=geoarrow.wkb` and `cypherlite.srid=<signed decimal i32>` | WKB with explicit SRID |

Use one public `ColType::TypedJson` fallback in full-column inference in place
of `ColType::Json` and `ColType::Mixed`. A `List`, `Map`, or `Path` classifies as
`TypedJson` even when the column is homogeneous. A `DateTime` or
`LocalDateTime` outside Arrow's nanosecond timestamp range, or a `Duration`
whose month/day count or nanosecond conversion cannot fit its native Arrow
interval, also classifies as `TypedJson` so its complete value is retained.
Incompatible kinds (including differently shaped vectors) merge to
`TypedJson`. `Null` remains the merge identity; matching native kinds retain
their native Arrow types. `Int` plus `Float` promotes to native `Float64` only
when **every** integer in the full column is exactly representable as `f64`;
otherwise the whole column uses `TypedJson`. Determine exactness using
`((i as f64) as i128) == (i as i128)`, not by casting the `f64` back to
`i64`: Rust's saturating float-to-`i64` conversion can make
`i64::MAX` appear exact even though it rounds to `2^63`. For example,
`9_007_199_254_740_993` and `i64::MAX` require typed JSON in an `Int`+`Float`
column, while exactly representable integers (including large boundary
values such as `i64::MIN`) may still use native `Float64`. An all-`Int`
column retains native `Int64`; the fallback's per-cell `INTEGER` and `FLOAT`
envelopes preserve both values. Once inferred, `TypedJson` absorbs every
subsequent kind, including native kinds and `Null`; it cannot merge back to a
native type. The fallback keeps the same Utf8
`cypherlite.encoding=typed-json-v1` field, typed envelopes, decoding rules,
and writer errors as before.

Classify a 3D `CypherValue::Point` (`PointValue.z.is_some()`) as `TypedJson`.
Its absorbing merge routes the **entire** column through the existing
`typed-json-v1` Utf8 representation, including a mix of 3D Points, 2D Points,
Geometry, and null/absent rows. Preserve each Point's `x`, `y`, `z`, and SRID
in its `POINT` payload and each other spatial value's coordinates and SRID;
infer over the full result before both single-batch and chunked writes so a
3D Point in a later chunk changes every chunk's schema. Columns containing
only 2D Point/Geometry values retain the existing Binary GeoArrow 2D WKB
representation and single field SRID. No new 3D WKB format or metadata is
introduced. The field-SRID restrictions below apply to native WKB, while
typed JSON keeps per-cell SRIDs. A column planned as `TypedJson` (whether
because of a 3D Point or another composite/mixed value) does not undergo the
WKB-only field-SRID, nested-member SRID, or WKB header-depth preflight. Its
spatial cells are instead validated by the fallible model payload writer,
including finite `x`/`y`/`z` and nested Geometry coordinates.

Classify `CypherValue::Time(_, Some(_))` as `ColType::TypedJson` rather than
native `Time`, even when all non-null rows are offset-bearing Times. The
absorbing merge makes a column mixing offset-bearing and unzoned Times (or
other kinds) `TypedJson` for every row, preserving the optional offset in
each `TIME` typed envelope through [ADR-0087](./0087-fallible-persistence-decoding.md)'s tagged Time payload. Reject a
non-minute offset through the fallible model payload writer, with Arrow
column/row and nested value path context; minute-aligned offsets round-trip.
`CypherValue::Time(_, None)` still uses native `Time64(Nanosecond)` when the
column is otherwise homogeneous and native-representable. This is an
additional losslessness classification for zoned Time, not a change to the
`typed-json-v1` envelope or metadata.

For `DateTime`, full-column inference (`infer_column_type`) returns native
`ColType::DateTime` **only** when every non-null, present DateTime in the full
result has the same minute-aligned `FixedOffset`, has no named-zone hint, and
is within Arrow's native nanosecond timestamp range. Null/absent rows do not
affect this comparison. A column with different fixed offsets (even though
each is minute-aligned), any `Some(timezone_name)`, or an out-of-native-range
DateTime instead returns `ColType::TypedJson`; a non-minute-aligned DateTime
also routes there for a model-writer error. Other incompatible kinds already
merge to that fallback. `plan_columns` must use this same full-result decision
before building either a single batch or any chunks: only a native DateTime
plan writes one `[+-]HH:MM` timestamp timezone from the common offset.
Fallback cells use the existing `cypherlite.encoding=typed-json-v1` Utf8
field and per-cell `DATE_TIME` payload from [ADR-0087](./0087-fallible-persistence-decoding.md), preserving each row's
fixed offset, local time, UTC instant, and optional timezone name without
adding a new metadata key. This also keeps one schema across chunk boundaries.
Any non-minute-aligned DateTime offset must still fail through [ADR-0087](./0087-fallible-persistence-decoding.md)'s
fallible model payload writer, including when another row or a named-zone
hint already caused a typed-JSON fallback; it is never accepted just because
the column falls back.

These are the complete recognized values of the two `cypherlite.*` type/
encoding keys in new batches; `typed-json-v1`, `node-v1`,
`relationship-v1`, and `vector-v1` are format versions, not prefix matches. A
field with an unknown `cypherlite.encoding` or `cypherlite.value_type`,
conflicting markers, or a marker on the wrong Arrow physical type fails
decoding. A bare Utf8 field is a string (even if its contents look like JSON);
a bare Struct is decoded field-by-field as a **Map**, never as a
Node/Relationship. Bare user lists
remain lists (including unmarked FixedSizeList columns) and bare user structs
remain maps. A Binary field without the
exact WKB extension is unsupported by the CypherValue decoder and errors.

For columns planned as native `ColType::Spatial` Binary/WKB (2D-only), WKB has
no per-cell SRID: `cypherlite.srid` is one field-level value applied to every
non-null Point or Geometry in that column. Both `result_to_record_batch` and
`result_to_record_batches` preflight **the full result** for these native
columns before building any batch or chunk. Compare all non-null spatial
values' SRIDs (including mixed 2D Point/Geometry values); if any differ,
return `ArrowError` identifying the column and conflicting SRIDs rather than
writing them with the first value's SRID. Different SRIDs in separate chunks
fail the whole call rather than yielding batches with a shared, misleading
schema. Null/absent cells do not participate in the comparison. Uniform-SRID
native spatial columns retain the Binary WKB/GeoArrow field and common SRID.
For those **same native WKB columns**, before recursively comparing collection
SRIDs or encoding WKB, preflight every spatial value's WKB nesting depth
across the full result. Count the top-level geometry header as depth 1 and
each nested header as depth + 1, exactly as the WKB reader does (including
nested Multi* member headers). Check the bound before descending: depth 64 is
permitted; depth 65 returns `ArrowError` with column, row, and nested member
index path where applicable, before any batch is emitted, even when the value
is in a later chunk. This prevents unbounded recursion on native in-memory
collections and ensures writer WKB output remains readable by the 64-level
WKB reader. Recursively inspect every native `GeometryCollection` member,
including descendants of nested collections, during this full-result WKB
preflight. Every descendant's SRID must equal its immediate parent's SRID
**and** the single field SRID, even for a collection in a later chunk. A
mismatch returns `ArrowError` with column, row, and nested member index path
(for example, `[1][0]`), identifying the conflicting SRIDs, before either
writer emits any batch. Standard WKB does not carry child SRIDs, so only
uniform-SRID nested collections can round-trip through native WKB without
changing their child SRIDs. Typed-JSON columns carry per-value SRIDs and do
not apply these WKB-only field/member SRID or depth rules.

Keep the 64-level WKB geometry-header limit in one Arrow-owned
`geo_encoding::MAX_GEOMETRY_DEPTH: usize = 64`, made `pub(crate)` so
`record_batch`'s native-WKB full-result writer preflight imports it rather
than declaring a separate `MAX_WKB_GEOMETRY_DEPTH`. The WKB reader and writer
count the top-level header as depth 1 and nested headers as depth + 1; both
allow level 64 and reject level 65. The constant is intra-crate, not a new
public API or a different WKB wire format.

In this native-WKB preflight, require finite `x` and `y` for every 2D Point
and every Geometry coordinate: LineString vertices, Polygon ring vertices,
MultiPoint points, MultiLineString vertices, MultiPolygon ring vertices, and
GeometryCollection descendants at any nesting depth. NaN or Infinity returns
`ArrowError` identifying column, row, coordinate/member index path and axis,
before constructing a batch, including when the bad value is in a later
chunk. Typed-JSON spatial values retain the separate fallible model payload
validation described below (including `z`); neither path substitutes JSON
null or writes non-finite WKB coordinates.

`typed-json-v1` stores each non-null cell as UTF-8 JSON of an exact
`{"type":T,"value":V}` object. The tag is an uppercase Cypher variant name:
`NULL`, `BOOLEAN`, `INTEGER`, `FLOAT`, `STRING`, `LIST`, `MAP`, `NODE`,
`RELATIONSHIP`, `PATH`, `DATE`, `TIME`, `DATE_TIME`, `LOCAL_DATE_TIME`,
`DURATION`, `POINT`, `GEOMETRY`, or `VECTOR`. Require both keys, no extras, and
the stated payload type; an Arrow null cell represents `CypherValue::Null`
(a non-null `NULL` envelope also decodes to Null if its payload is JSON null).
`BOOLEAN`/`INTEGER`/`FLOAT`/`STRING`/`NULL` carry a JSON bool/i64/finite
number/string/null, respectively. `LIST` carries an array of **typed
envelopes**; `MAP` carries an object mapping arbitrary keys to typed
envelopes. Consequently a map whose keys imitate an envelope or graph entity
remains a map. `NODE` carries
`{"id":i64,"labels":[string,...],"properties":{key:typed-envelope,...}}`;
`RELATIONSHIP` carries
`{"id":i64,"type":string,"start_node":i64,"end_node":i64,"properties":{key:typed-envelope,...}}`.
`PATH` carries `{"nodes":[NODE-envelope,...],"relationships":[RELATIONSHIP-envelope,...]}`
with `nodes.len() == relationships.len() + 1`. Require all fields with exact
types; never synthesize IDs or filter invalid members. Temporal, duration,
point, geometry, and vector envelopes use their **validated [ADR-0087](./0087-fallible-persistence-decoding.md)
persistence JSON object** as `value` (including its `__type`), and the decoded
variant must match `T`; `GEOMETRY` includes every supported geometry subtype.
All nested entries use the same envelope contract. This composite payload is
an Arrow-only encoding and does not change `cypherlite-model::wire` output.
When writing these envelopes, recursively reject non-finite Point x/y/z and
Geometry coordinates, including members of nested GeometryCollections, with
`ArrowError` before JSON serialization can substitute `null` for a coordinate.
The same no-loss rule applies to Float32 Vector coordinates in typed JSON:
reject a finite `f64` value unless conversion to `f32` and back to `f64`
preserves it exactly (including the sign of zero); reject non-finite values.
For `POINT`, `GEOMETRY`, and `VECTOR`, obtain the entire tagged `value` object
from [ADR-0087](./0087-fallible-persistence-decoding.md)'s fallible `cypher_value_to_json` instead of rebuilding its
payload shape in Arrow. Propagate its path-qualified `EncodeError` as an
`ArrowError` with column/row context, including nested graph properties and
composite cells; keep the Arrow-owned `{"type":T,"value":V}` envelope,
NODE/RELATIONSHIP/PATH validation and typed-member recursion. Do not change
valid `typed-json-v1` bytes, graph provenance markers, or legacy-batch policy.
Use [ADR-0087](./0087-fallible-persistence-decoding.md)'s model-owned `float32_exact(f64) -> bool` for both direct Arrow
FixedSizeList vector construction and typed-JSON vector encoding. The shared
predicate requires finite, bit-identical `f64 -> f32 -> f64` conversion;
Arrow-specific i32 dimension and child-element budget checks still apply.

The marked node/relationship Struct fields retain their existing child layout.
For each non-null row require a non-null `Int64` `id` (and `start_node` and
`end_node` on a relationship), a non-null `Utf8` relationship `type`, a
`List<Utf8>` of non-null labels, and a non-null `Utf8` `properties` object.
That object maps each key to a `typed-json-v1` envelope (not the old untagged
wire value); require an object, validate every member, and reject missing,
null, or wrong-type children. Parent null rows decode to Null regardless of
placeholder child values. An unmarked Struct decodes its supported
children recursively as a Map; unsupported or malformed children error.
Marked vector columns must be FixedSizeList with a supported primitive child
(`Int8/16/32/64` or `Float32/64`) and consistent list size; malformed child
types or unexpected null coordinates error. Unmarked FixedSizeList columns
decode as ordinary Lists, with no inferred Vector variant.

Legacy policy: old `cypherlite.encoding=json` and `tagged` Utf8 columns are
**rejected with a legacy-format error**; their untagged nested values cannot
reliably distinguish user maps/lists from graph entities. Misspelled or future
encoding values are unsupported errors, never a JSON-decoding fallback. Old
unmarked node/relationship Structs and user Structs decode as ordinary Maps,
with no shape-based promotion; unmarked legacy vector columns decode as Lists.
Callers needing graph/vector identity must regenerate those batches with the
new writer. Plain Utf8 columns (including old
`result_to_string_batch`) decode verbatim. This may be an observable migration
for in-memory or externally retained legacy Arrow batches; no automatic
guesswork recovers missing provenance.

Make `record_batch_to_rows(&RecordBatch) -> Result<(Vec<String>,
Vec<IndexMap<String, CypherValue>>), arrow::error::ArrowError>`; fail the whole
batch with field/row context for invalid metadata, structured values, WKB,
unsupported physical types, or malformed non-null cells. `result_to_record_batch`
and `result_to_record_batches` already return `ArrowError`: reject malformed
public Vector values (dimension/length mismatch, dimension not representable
as `i32`, wrong coordinate variant, out-of-range integer or inexact Float32
narrowing). A finite Float32 coordinate must round-trip exactly through
`f64 -> f32 -> f64` (including signed zero); non-finite coordinates error.
Before constructing **any** FixedSizeList vector child array, require a
checked `dimension * full_result_row_count <= 16,777,216` child elements
**per vector column**, counting masked placeholders for null/absent rows too.
Arithmetic overflow or a count above 16,777,216 returns `ArrowError` with
column, dimension, row count, and bound, before appending child elements.
`result_to_record_batch` checks its entire input, and
`result_to_record_batches` checks the entire result before splitting into
chunks or emitting a batch; a small chunk size cannot bypass the budget.
The boundary is inclusive (exactly 16,777,216 elements is allowed). Reject
rather than silently switching to a different physical encoding. Both writers
propagate checked `FixedSizeListArray` construction errors. No coordinate
zeros or truncation are substitutes for bad input; null rows may use masked
child placeholders. Preserve valid six-kind vectors, nulls, and chunked schemas.

For timestamp timezone metadata, accept `Z`, `UTC` (case-insensitive), and
valid fixed offsets of the exact ASCII form `[+-]HH:MM` with minutes in
`00..59`, hours within `chrono::FixedOffset`'s representable range, and
checked seconds arithmetic. Delegate signed offset validation to [ADR-0087](./0087-fallible-persistence-decoding.md)'s
public `cypherlite_model::serde_value::parse_numeric_offset`; keep `Z`/`UTC`
and named-zone dispatch in Arrow without a second numeric parser. A
timezone beginning with `+` or `-` is a numeric-offset candidate: reject
malformed syntax or an out-of-range offset (including `+00:60` and `-12:99`)
with `ArrowError`; never reinterpret it as an IANA zone name. Keep the prior
reader behavior for externally supplied native timestamp metadata with
nonnumeric named zones: preserve the UTC instant and zone name for display
without attempting timezone-database resolution. New DateTime writes with a
named-zone hint use per-cell typed JSON instead. Invalid metadata errors even
when no non-null timestamp row is present.

Before formatting **native DateTime** fixed offsets into timestamp timezone
metadata, reject any `DateTime<FixedOffset>` whose
`offset().local_minus_utc() % 60 != 0` with `ArrowError`; native planning must
already have ruled this out across the full result. Never choose a native
timezone string from just the first DateTime when later rows differ. A
DateTime routed to `TypedJson` uses [ADR-0087](./0087-fallible-persistence-decoding.md)'s fallible model payload writer
for the same offset rule, including a named-zone or heterogeneous column with
an invalid offset. Single and chunked batch writers fail the whole call
instead of publishing minute-truncated data. Only homogeneous, minute-aligned,
unnamed fixed-offset DateTimes retain the native timestamp representation.

Make `wkb_to_cypher_value(&[u8], i32) -> Result<CypherValue, ArrowError>` and
`wkb_array_to_geometries(&dyn Array, i32) -> Result<Vec<CypherValue>, ArrowError>`.
The latter accepts only `BinaryArray`, preserves genuinely null cells, and
returns an error for a malformed non-null cell or a wrong array type. Read WKB
byte order (`0` big, `1` little) per top-level **and nested** header; check
geometry types before decoding nested members, checked count/offset math and
remaining bytes before allocating, no reserve from unchecked counts, finite
nesting (at most 64 geometry levels), and no trailing unread bytes. Support
Point, LineString, Polygon, MultiPoint, MultiLineString, MultiPolygon, and
GeometryCollection, including big-endian coordinates. Reject bad markers,
types, counts, truncation, or nested mismatches rather than returning Null.
Reject non-finite `x` or `y` at the coordinate-read boundary for **all** of
these WKB types, recursively through Multi* and GeometryCollection members,
regardless of top-level or nested byte order. Both the raw
`wkb_to_cypher_value` decoder and `wkb_array_to_geometries` must return a
contextual `ArrowError` (coordinate/member path, and array row when present)
instead of constructing a Point/Geometry with NaN or Infinity. A WKB Point
EMPTY represented by NaN sentinels may therefore be rejected; no distinct
Cypher Point EMPTY representation is defined by this encoding. Finite WKB
and genuinely null BinaryArray cells retain their existing behavior.

An explicitly WKB-marked Binary field requires a valid `cypherlite.srid`
integer (no default zero); invalid metadata errors before row decoding.

## Alternatives considered

- **Continue recognizing graph values by Struct/JSON shape.** Rejected: a
  user map/list with the same keys can be promoted to a Node/Path, and a
  missing struct ID can alias real node zero.
- **Accept any `cypherlite.encoding` value as JSON, or parse unmarked Utf8.**
  Rejected: metadata typos and ordinary strings become unintended values.
- **Keep legacy `json`/`tagged` composite decoding as a best-effort fallback.**
  Rejected: untagged nested wire JSON cannot reliably recover graph identity
  or distinguish a lookalike user map/list; an error makes migration explicit.
- **Replace typed Arrow arrays with strings everywhere.** Rejected: clients
  lose the native Arrow scalar/vector/spatial types and schema information.
- **Keep writing 3D Points as 2D WKB by dropping `z`.** Rejected: decoding
  cannot recover the third coordinate, while the existing per-cell `POINT`
  typed-JSON payload retains it and its SRID without changing 2D GeoArrow.
- **Always promote mixed Int/Float columns with `i as f64`.** Rejected:
  integers such as `2^53 + 1` and `i64::MAX` change value; the existing
  typed-JSON envelopes preserve both integers and floats when native
  promotion is inexact.
- **Pass NaN/Infinity through native WKB writes or external WKB reads.**
  Rejected: invalid spatial coordinates would be accepted as valid
  Point/Geometry values, including hidden non-finite descendants; both
  boundaries must fail with location context.
- **Route otherwise-native 2D mixed-SRID spatial columns through lossless
  typed-JSON Utf8.** Rejected: dynamically changing a 2D WKB column to Utf8
  loses its GeoArrow Binary/WKB schema and interoperability; an explicit
  error lets the caller normalize SRIDs or choose an interchange format
  intentionally. A 3D Point already requires typed JSON regardless of SRID.
- **Carry each GeometryCollection child's SRID in EWKB or a custom WKB
  extension.** Rejected: per-child SRIDs are outside the chosen standard
  GeoArrow WKB encoding and would break interoperability with ordinary WKB
  readers.
- **Silently relabel nested GeometryCollection members with the field SRID.**
  Rejected: decoding would change the child's spatial reference system and
  make a lossy round-trip appear valid.
- **Use null/default values or unchecked allocation on invalid Arrow/WKB.**
  Rejected: this hides corruption, invents identities/coordinates, and permits
  count-field resource exhaustion.
- **Keep Arrow-owned Point/Geometry/Vector payload builders and a second
  Float32 exactness test.** Rejected: identical-looking code can drift from
  the model's persistence encoding and disagree on signed zero.
- **Retain distinct public `ColType::Json` and `ColType::Mixed` fallbacks.**
  Rejected: they have identical `typed-json-v1` representation and metadata;
  separate variants duplicate merge state without conveying a wire-level
  difference.
- **Treat any unparseable numeric timezone offset as a named zone.** Rejected:
  malformed offset metadata would silently acquire a different meaning, while
  valid named zones have no leading offset sign.
- **Retain an Arrow-private numeric parser alongside [ADR-0087](./0087-fallible-persistence-decoding.md)'s model
  parser.** Rejected: independent sign, ASCII, width, minute, and range checks
  can drift; Arrow only needs to dispatch nonnumeric timezone forms and map a
  failed signed parse to `ArrowError`.
- **Keep separate reader and writer constants for WKB geometry depth.**
  Rejected: two copies of 64 within the Arrow crate can diverge and permit
  output that the WKB reader refuses; one crate-visible reader constant
  keeps the full-result writer preflight aligned with decoding.
- **Keep truncating DateTime offset seconds in native timestamp metadata or
  discarding zoned Time offsets in native Time64.** Rejected: both can change a
  valid input value on an Arrow round-trip, even when the metadata parses.
- **Store zoned Time in native Time64 with a timezone field-metadata hint.**
  Rejected: metadata is column-wide, while Times in one column may carry
  different offsets or mix zoned and unzoned values; it cannot losslessly
  identify each row's optional offset. The existing per-cell typed-json-v1
  envelope already does so without a new metadata convention.
- **Use a timezone name to bypass native DateTime fixed-offset validation.**
  Rejected: a display hint cannot restore the input's non-minute fixed offset;
  an error is required even if the UTC instant and name are otherwise retained.
- **Reject an entire DateTime column when valid fixed offsets differ or any
  value carries a named-zone hint.** Rejected: each value already has a
  lossless per-cell `DATE_TIME` typed-JSON representation; refusing the column
  is unnecessary when a column-wide native timezone cannot represent it.
- **Keep the first DateTime's timezone as native field metadata (`first_tz`).**
  Rejected: later rows with different offsets decode with the first offset and
  wrong local time; named-zone metadata also changes the decoded fixed offset
  to UTC, even when there is only one row.

## Consequences

- New batches carry graph provenance and a versioned, recursive composite
  format; graph-shaped user data is not promoted. Old unmarked structs remain
  inspectable as maps, while ambiguous old `json`/`tagged` columns explicitly
  require regeneration. Golden schemas and roundtrip tests must pin markers
  and legacy behavior, including mixed/nested maps and paths.
- Public Rust callers pattern-matching `ColType::Json` or `ColType::Mixed`
  must migrate both arms to `ColType::TypedJson`. Full-column inference
  regression tests must cover homogeneous List/Map/Path, incompatible mixes,
  out-of-range temporal/duration values, absorption after subsequent rows,
  `Null` identity, and `Int`+`Float` promotion; writer/reader roundtrips must
  confirm unchanged typed-json-v1 Utf8 metadata, value preservation, and
  error behavior.
- For **native Binary/WKB spatial columns only**, single and chunked batch
  writers preflight the full result and error on mixed SRIDs before emitting
  a misleading `cypherlite.srid`; tests must cover differing SRIDs within one
  batch and across chunk boundaries, mixed 2D Point/Geometry rows, null rows,
  and same-SRID roundtrips with a shared schema. This WKB preflight also
  rejects a GeometryCollection child whose SRID differs from its parent/field
  at any nesting depth, with column/row/nested-index context before any batch
  is emitted. Single and chunked writer regressions must cover a parent SRID
  4326 with a child SRID 7203 (including a later chunk), and uniform nested
  collections whose coordinates and every child SRID survive the WKB
  round-trip. Native WKB writer depth preflight must reject level 65 with
  contextual `ArrowError` (also in a later chunk) before SRID traversal or
  WKB encoding; tests must exercise the exact boundary by round-tripping a
  uniform-SRID level-64 collection and rejecting level 65 in single and
  chunked writes without emitting partial batches. Typed-JSON columns do not
  apply these WKB-only checks: per-cell SRIDs survive instead, and malformed
  spatial payloads still fail through the model writer.
- Arrow/WKB public decoder signatures become fallible and callers must handle
  `ArrowError`. Tests must cover nulls, bad child/metadata types, malformed
  graph IDs/properties, vector dimensions/narrowing, count bombs, nested
  headers, a big-endian point, and geometry collections.
- Both Arrow batch writers preflight the full-result, per-vector-column
  16,777,216-element ceiling with checked multiplication, including null-row
  placeholders; tests cover the exact limit, above-limit/overflow and
  cross-chunk cases (for example dimension 10,000 with 100,000 null rows).
  Tests also cover inexact Float32 values and non-finite Point/Geometry
  coordinates nested in typed JSON: all return errors instead of changed data.
- Rust API specs, descriptions, and runnable examples for the new errors and
 migration ship with implementation. [ADR-0087](./0087-fallible-persistence-decoding.md) governs persisted JSON, not
  Arrow provenance or the canonical CLI/C-ABI wire format.
- Arrow typed JSON inherits the model's fallible payload shapes and finite,
  bit-preserving Float32 rule; invalid nested values fail the whole writer
  call as `ArrowError` instead of becoming JSON `null`. Tests must compare
  persisted and Arrow payloads for Point/Geometry/Vector and cover signed
  zero, inexact and non-finite coordinates in direct and composite paths.
- Timestamp metadata tests must reject malformed signed offsets, including
  `+00:60` and `-12:99`, while roundtripping `+02:30` and preserving valid
  `Z`/`UTC` and named-zone behavior, including on empty/all-null batches.
  Tests must cover multibyte and oversized signed candidates via the shared
  model parser and Arrow's error mapping. Rust API specs, description, and a
  runnable example document the public parser and the timestamp error
  boundary.
- A single intra-crate WKB depth bound now governs reader headers and native
  writer full-result preflight. Reader and writer tests must accept a
  top-level-at-1, level-64 collection and reject level 65, including in later
  chunks without partial batch output. Rust API docs describe the shared
  64-level WKB limit; no extra public constant or new wire marker is needed.
- Arrow callers writing a native DateTime with a `+1` or `-1` second fixed
  offset now receive `ArrowError` (including in later rows/chunks) rather than
  truncated metadata. The typed-JSON fallback must also propagate the model
  error for subminute offsets, including in a named-zone or heterogeneous
  column and in later chunks.
- Public `infer_column_type` and the writer's `plan_columns` now select
  `ColType::TypedJson` for heterogeneous fixed-offset DateTimes or any
  named-zone hint, rather than `ColType::DateTime`; consumers expecting a native
  Timestamp for those columns must handle the `typed-json-v1` Utf8 schema.
  Full-result inference before chunking ensures one schema and no first-row
  timezone aliasing.
  Tests must cover native same-offset `+02:30` rows (with nulls) and exact
  fixed-offset/instant round-trip; mixed `+02:30`/`-05:00` rows within and
  across chunks; named-zone hints on single and mixed rows; and exact offset,
  local time, UTC instant, and name after typed-JSON round-trip. Rust API docs
  and runnable examples must describe both representations and errors.
- Full-result inference chooses `typed-json-v1` for any column containing a
  3D Point, including 2D/3D and Point/Geometry mixes across chunks; single
  and chunked roundtrip tests must assert `z`, coordinates and per-value SRID
  as well as identical field schemas. Tests should also cover different
  per-cell SRIDs (including in a later chunk) that are valid in a typed-JSON
  column; WKB-only field/nested SRID and depth checks must not reject these
  values. Otherwise-native 2D-only spatial columns keep Binary WKB with
  their field SRID and existing roundtrip tests.
- Mixed Int/Float columns with an inexact integer use typed JSON; tests in
  both row orders and across chunks must cover `2^53 + 1`, `i64::MAX`,
  exact large integers such as `i64::MIN`, nulls, and the native Float64 path
  for exact mixes. The predicate must not mistake a saturating float-to-i64
  cast for proof of exactness.
- Native spatial writes must reject non-finite coordinates at every geometry
  depth with column/row/coordinate context before emitting single or chunked
  batches. Raw and array WKB reads must reject external NaN/Infinity in both
  byte orders, including Multi* and GeometryCollection descendants, with
  member and array-row context. Tests must retain finite WKB and null-cell
  roundtrips and cover the potential Point EMPTY sentinel rejection.
- Zoned Time columns, including homogeneous offset-bearing rows and mixed
  zoned/unzoned Time, now expose a `typed-json-v1` Utf8 field instead of
  native Time64; Arrow consumers expecting Time64 must handle that schema
  migration. Tests must confirm exact encoding metadata and Time offset
  round-trips, unzoned-only native Time64 round-trips, nested/mixed zoned Time
  round-trips, and model-propagated errors for `+1` and `-1` second offsets.
  Rust API docs and runnable examples must describe both outcomes.
