---
type: adr
area: engine
date: 2026-09-23
deciders: CypherLite maintainers
name: 0087-fallible-persistence-decoding
status: proposed
tags:
  - storage
  - values
title: Fallible persistence values and an escaped map form
REFERENCES:
  - "[[adr/0014-single-wal-write-path]]"
  - "[[adr/0050-snapshot-codecs]]"
  - "[[adr/0086-bounded-persistence-input]]"
  - "[[adr/0088-explicit-arrow-value-provenance]]"
---

# 0087. Fallible persistence values and an escaped map form

## Context

[ADR-0014](./0014-single-wal-write-path.md) establishes WAL recovery, and [ADR-0050](./0050-snapshot-codecs.md) makes JSON snapshots part of
checkpoint recovery. The
`cypherlite-model::serde_value` format stores temporal, spatial, duration, and
vector values as `{"__type":"point",...}`-style objects, while ordinary maps
are bare JSON objects. `json_to_cypher_value` currently defaults missing point
coordinates, duration components, and geometry contents, drops invalid nested
geometry members, narrows vector dimensions, and falls back to a map when many
recognized tags fail to parse. `parse_offset` can panic on untrusted UTF-8 or
overflow. `CypherValue::Deserialize` and JSON snapshot recovery use this
infallible path; a user map with `__type` can also be reinterpreted as a typed
value. [ADR-0086](./0086-bounded-persistence-input.md) bounds persisted input sizes but does not validate these values.

The persistence format is distinct from the untagged public JSON wire format
(`cypherlite-model::wire`). CLI and C-ABI query parameters are user JSON, not
snapshot bytes; they need an explicit boundary rule rather than inheriting the
snapshot tag inference accidentally.

The public `cypherlite-model::serde_value::cypher_value_to_json` writer is
currently infallible. `serde_json::Number::from_f64` failure becomes JSON
`null` for a non-finite scalar, while `json!` also substitutes `null` for
non-finite Point/Geometry coordinates. Public Vector fields permit invalid
dimensions, element kinds, ranges, and non-finite or inexact Float32 values;
the writer can emit bytes its strict decoder cannot accept. The JSON snapshot
writer currently publishes those bytes instead of failing the checkpoint.
[ADR-0088](./0088-explicit-arrow-value-provenance.md) embeds the Point/Geometry/Vector persistence objects in Arrow typed
JSON, so independent Arrow payload builders or Float32 checks can diverge from
this format.

A syntactically valid tagged Geometry decoded from a v1 or v2 JSON snapshot is
still not representable as a `cypherlite-core::Value`: the current
`cypher_to_value` bridge maps `CypherValue::Geometry` to `Value::Null`, including
when the Geometry occurs inside a list or map. Decoder validation alone cannot
prevent this loss during graph recovery.

The reverse bridge can also lose temporal property data *before* the fallible
model writer sees it. `value_to_cypher` converts core `Temporal::CivilDate` and
`CivilDateTime` through a narrowing `i64`-to-`i32` year cast, producing either a
different chrono date or a string. It narrows the `i128` Duration nanoseconds'
seconds quotient to `i64` with `as`; an invalid signed offset on `Time` becomes
an absent offset, while an invalid `DateTime` offset becomes a string. Valid
JSON emitted after those conversions cannot reveal the original temporal type
or magnitude, so checking only the model serializer is insufficient. Even a
constructible `FixedOffset` can lose data: the model writer formats `Time`
offsets as `HH:MM` and chrono's `DateTime::to_rfc3339` uses minute precision,
so an offset of one second is silently truncated in either JSON shape.

## Decision

Introduce a fallible, recursive persistence-value decoder returning
`Result<CypherValue, DecodeError>` (or an equivalent typed error). Use it for
`CypherValue::Deserialize`, snapshot property loading, and any other trusted
interpretation of persistence JSON. Deserialization maps the decode error to a
Serde error; a bad property fails the entire snapshot load and `Database::open`
as a storage error, before a partially loaded graph is exposed. Preserve the
existing JSON representations of scalars, lists, untagged maps, and valid
`date`, `time`, `datetime`, `localdatetime`, `duration`, `point`, `geometry`, and
`vector` tags. Do not turn a malformed recognized tagged object into a map or
substitute a null, zero, empty collection, or missing coordinate.

For new snapshots, add an optional top-level `value_encoding` to `graph.json`:
write integer `2`; absence means legacy version 1; any other value is an
unsupported-format error. This is an additive `GraphData` field, not a change
to the node/relationship/property layout. In version 2, encode **every**
ordinary `CypherValue::Map` that has a `__type` key (at any depth) as the exact
two-key object `{"__type":"map","value":{...}}`. The `value` object contains
the original map entries, whose *values* are recursively encoded; its own
`__type` entry is data and is never interpreted as an envelope. Maps without a
`__type` key keep the bare-object representation. For example, the map
`{"__type":"point","x":1}` is written as
`{"__type":"map","value":{"__type":"point","x":1}}` and decodes as a map,
not a malformed point. In version 2, a `map` tag must have exactly these two
keys and an object-valued `value`; otherwise decoding fails. Keep snapshot
writer/reader behavior symmetric, including nested maps and node/relationship
properties, on Native and Memory VFS.

For both versions, a recognized typed tag must have its required fields with
correct JSON types, valid ranges, and no unrecognized fields (optional fields
are accepted only where the existing writer emits them). Date/time strings must
parse; a time offset is either absent or exactly ASCII `[+-]HH:MM` with minute
`00..59`, hour within `FixedOffset`'s representable range, and checked seconds
arithmetic. Integer SRIDs must fit `i32`; point x/y and optional z are numeric
and representable, and duration has all four `i64` components. Geometry has a
known `geometry_type`, required shape-correct coordinates (each pair exactly
two numeric entries), and only geometry-valued collection children: reject a
bad child instead of filtering it out. Vector `coord` is one of the six
canonical names, `dim` is a representable nonnegative integer equal to the
length of `values`, and each coordinate has the required numeric variant and
range for its kind. Decode nested values recursively and propagate the first
error with property/value context. An unknown `__type` string or a non-string
`__type` remains ordinary map data; it is never treated as a known typed tag.

Make the numeric offset rule one public model API:
`cypherlite_model::serde_value::parse_numeric_offset(&str) -> Option<chrono::FixedOffset>`.
It accepts **only** six ASCII bytes of the form `[+-]HH:MM`: an explicit
leading sign, two decimal hour digits, a colon, and two decimal minute digits.
Minutes must be `00..59`; checked arithmetic and
`chrono::FixedOffset::east_opt` enforce the representable hour/total-seconds
range. Return `None`, without panicking, for a missing or invalid sign,
non-ASCII/multibyte characters, non-digits, a different width, invalid
minutes, or an out-of-range offset. In particular `+02:30` and `-05:45`
return `Some`, while `+00:60`, `+99:00`, a huge hour value, and `Z` return
`None`. Model persistence `Time` decoding calls this parser; [ADR-0088](./0088-explicit-arrow-value-provenance.md)'s Arrow
timestamp reader calls the same parser for signed timezone metadata. Arrow
separately dispatches `Z`/`UTC` and unsigned named zones, and reports an
invalid signed candidate as `ArrowError` rather than treating it as a zone.

Legacy snapshots (no `value_encoding`) retain their historical valid tag
interpretations and bare ordinary maps, including maps with unknown `__type`.
Do **not** interpret `__type=map` as the new escape in a legacy snapshot: it was
previously an ordinary map. A malformed *recognized* legacy tag now fails
open rather than becoming a map/defaulted value. A legacy bare map with a
recognized `__type` and a valid typed shape is intrinsically indistinguishable
from a typed value; retain the historical typed interpretation. There is no
safe automatic way to recover that map's original intent from old bytes. A
later checkpoint writes version 2 only after successful decode; it does not
silently repair a corrupted snapshot.

At the JSON snapshot property boundary, recursively check every decoded
`CypherValue` for representability as a core `Value` **before** calling
`cypher_to_value`. Reject a valid tagged Geometry at any depth, including in
lists and maps (and v2 escaped maps), for both legacy v1 and v2 snapshots.
Return a contextual storage error naming the node or relationship ID and the
full property path to the unsupported value (for example,
`node 7: invalid graph property $.payload.items[0]: unsupported Geometry`).
Fail `Database::open` without exposing a partially recovered graph or
rewriting the snapshot; never let this path convert Geometry to `Value::Null`.
This rule concerns the snapshot-to-core bridge: model-level Geometry decoding
and serialization remain valid for their other consumers. Do not expand core
`Value` with Geometry in this stream. Point, Vector, and other representable
values continue to recover normally.

For the standalone Serde `Deserialize` / fallible JSON-value API (no snapshot
header), recognize the exact new `map` envelope and valid historical typed
tags; malformed recognized tags error. An old *standalone* user map identical
to the new map envelope is likewise irreducibly ambiguous without a version
marker. CLI and C-ABI parameter JSON instead decodes bare JSON objects
recursively as ordinary maps **regardless of `__type`**, including complete
point-looking objects; no implicit persistence-tag conversion occurs there.
Typed query parameters must be constructed explicitly by the query (for
example, `point($coords)` or `date($iso_date)`); the former implicit
`__type`-parameter interpretation is a deliberate migration. Document both
parameter entry points and this change when implementing it.

Replace the infallible public writer with
`pub fn cypher_value_to_json(val: &CypherValue) -> Result<serde_json::Value,
EncodeError>` in `cypherlite_model::serde_value`. `EncodeError` is a
model-owned error with public `path: String` and `message: String` fields,
implementing `Display` and `std::error::Error` like `DecodeError`; nested
list/map, property, geometry-collection, and vector errors identify their
offending member. Change `pub fn props_to_json(props:
&IndexMap<String, CypherValue>) -> Result<serde_json::Map<String,
serde_json::Value>, EncodeError>` to use the same writer throughout. There is
no infallible compatibility wrapper that substitutes `null`, drops members,
or fabricates coordinates. `Serialize for CypherValue` converts an
`EncodeError` to a Serde serialization error rather than publishing a
different value. This is an intentional Rust source-API change; callers must
handle `Result`.

The writer recursively rejects non-finite Float scalars, Point x/y/z,
Geometry coordinates (including nested GeometryCollections), and Float64 or
Float32 Vector coordinates. It also rejects vectors with a mismatched
dimension/length, a coordinate with the wrong CypherValue variant, an integer
outside its coordinate kind's range, or a Float32 coordinate not exactly
representable. Node, Relationship, and Path are not persistence-property
values: reject them at any depth rather than stringifying a graph identity
or hiding an invalid nested value. For all valid persistence values keep the
existing v2 JSON shape, including `__type` map escaping and the exact Point,
Geometry, and Vector tagged payloads; neither the snapshot version nor the
CLI/C parameter and untagged wire contracts change.

The same public JSON writer rejects `CypherValue::Time` and
`CypherValue::DateTime` when their `chrono::FixedOffset::local_minus_utc()`
seconds are not divisible by 60, **before** formatting the Time offset as
`[+-]HH:MM` or the DateTime with `to_rfc3339()`. This applies whether the
DateTime has an additional timezone name or not, and at every list/map nesting
depth, including values passed through `props_to_json`; `EncodeError` carries
the complete value/property path (for example, `$.payload.items[0]`).
`serde::Serialize for CypherValue` propagates the same failure as a Serde
error, including for nested values, instead of emitting a changed offset or
instant. Exact minute-aligned offsets keep their existing v2 Time/DateTime JSON
shapes and round-trip unchanged. This model-level rule complements the earlier
core-to-model snapshot guard: direct model/Serde writes need protection even
when no core `Value` conversion or checkpoint is involved.

Before calling `to_rfc3339()` for `CypherValue::DateTime`, also check that
`dt.naive_utc().checked_add_signed(offset_duration)` succeeds, where
`offset_duration` is the fixed offset's signed seconds as a chrono `Duration`:
the UTC instant **plus** the offset must be representable as a chrono
`NaiveDateTime` local time.
An otherwise constructible, minute-aligned `DateTime<FixedOffset>` with UTC
`NaiveDateTime::MAX` and `+01:00`, or UTC `NaiveDateTime::MIN` and `-01:00`,
fails this check. Return a path-qualified `EncodeError` for either boundary,
including named-zone and nested list/map/property values, rather than allowing
chrono's local-time formatting to panic. `Serialize for CypherValue` must
surface the error as a Serde error. In-range local times, including those near
both bounds, keep the same RFC 3339/v2 JSON representation and round-trip.

Define one public model predicate
`pub fn float32_exact(f: f64) -> bool` in `cypherlite_model::serde_value`: it
returns true only for finite `f` where
`((f as f32) as f64).to_bits() == f.to_bits()`.
Use it for model Float32 persistence decode and encode; [ADR-0088](./0088-explicit-arrow-value-provenance.md) uses the
same predicate for Arrow's direct and composite vector writes. In particular,
both signed zeros retain their own bits, while `0.1`, overflow, and NaN or
infinity fail the check.

JSON snapshot property conversion, graph construction, and `dump_to_vec`
propagate `EncodeError` as a storage error with node/relationship and property
context. Finish validation and serialization of the entire graph before
writing `graph.json` or publishing the checkpoint manifest; an invalid value
fails the checkpoint without clearing the dirty state or advancing its
manifest. Preserve the previous committed snapshot for recovery. This also
applies on Native and Memory VFS. The checked DateTime local-range error follows
this same path even for nested properties: return the contextual StorageError
before publication on a fresh or existing checkpoint, leaving prior snapshot
and manifest bytes intact on an existing store.

At the **JSON snapshot write property boundary**, recursively validate core
`Value` properties before calling `value_to_cypher` or the model JSON writer.
For both node and relationship properties, descend through lists and maps at
every depth and reject:

- every `Temporal::CivilDate` and `Temporal::CivilDateTime`, even if its fields
  happen to fit chrono, because conversion can change the temporal variant or
  stringify/narrow its year;
- `Temporal::Duration` when `nanos / 1_000_000_000` (the signed `i128`
  quotient used by the bridge) is outside the inclusive `i64` range; and
- `Temporal::Time` or `Temporal::DateTime` whose signed offset seconds are not
  divisible by 60 **or** cannot be constructed with
  `chrono::FixedOffset::east_opt`. Both conditions must hold for an offset to
  survive the model writer's minute-precision JSON unchanged.

Report a storage error with the entity kind and ID and the full nested
property path, for example
`relationship 3: invalid graph property $.payload.items[0]: unsupported CivilDateTime`.
Do not pass a rejected value to `value_to_cypher`: model serialization cannot
detect a value already stringified, aliased, truncated, or stripped of its
offset. Continue to write core `Date`, `LocalTime`, `LocalDateTime`,
`Time`/`DateTime` with representable minute-aligned offsets, and Duration with
an `i64`-representable seconds quotient (including its signed nanosecond
remainder) through the existing v2
typed JSON representation; valid chrono-range, minute-aligned temporal values
remain supported. This guard is specific to snapshot properties, not a change
to the general core bridge API or the model JSON format. As with other write
errors, validate the entire graph before writing `graph.json` or advancing its
manifest: a failed fresh checkpoint publishes neither file, and a failed
subsequent checkpoint leaves the previous bytes and dirty state intact.

## Alternatives considered

- **Keep an infallible decoder with best-effort defaults and map fallback.**
  Rejected: corrupt persisted values can be silently rewritten as different
  values, and an offset can panic during public deserialization.
- **Treat every object with a `__type` key as a tagged value, without an
  escape or snapshot version.** Rejected: ordinary user maps collide with
  known tags, and a new `map` sentinel would also reinterpret old unknown-tag
  maps during recovery.
- **Reject all legacy snapshots or guess a map when a known tag is malformed.**
  Rejected: valid prior snapshots remain readable, while guessing would hide
  corruption and cannot resolve valid legacy tag/map ambiguity.
- **Apply snapshot tag inference to all CLI and FFI parameters.** Rejected:
  caller-controlled maps would turn into spatial/temporal values depending on
  their keys, violating the ordinary JSON parameter-map contract.
- **Keep the writer infallible and pre-validate only snapshot or Arrow callers.**
  Rejected: public `Serialize` and direct JSON callers would still lose data,
  and separate validators can drift from the bytes actually written.
- **Expose a new `try_` writer but retain the old null-coercing API.** Rejected:
  the old public entry point would continue to silently corrupt nested values.
- **Use numerical equality for the Float32 round-trip test.** Rejected: `+0.0`
  and `-0.0` compare equal but are distinct persisted coordinates.
- **Add lossless Geometry support to core `Value` now.** Rejected for this
  stream: it requires cross-layer public API and output-format expansion beyond
  persistence hardening. Defer it until that representation and its consumers
  are designed together; the snapshot bridge must reject unsupported values in
  the meantime.
- **Pass valid Geometry through the existing bridge and accept `Value::Null`.**
  Rejected: even perfectly formed legacy or v2 snapshot data would be silently
  lost, including Geometry nested inside lists and maps.
- **Expand model temporal representation to cover core CivilDate,
  CivilDateTime, and the full i128 Duration range.** Rejected for this stream:
  it changes typed persistence formats and their decoders and consumers, rather
  than closing the lossy snapshot boundary before the existing bridge.
- **Rely on bridge stringification, narrowing casts, or missing-offset
  fallbacks (or validate only after conversion).** Rejected: successful JSON
  serialization would silently persist a different temporal type, magnitude,
  date, or offset, and post-conversion validation cannot reconstruct the
  original value.
- **Check only `FixedOffset::east_opt` for Time and DateTime.** Rejected: a
  constructible one-second offset is not minute-aligned; the Time and DateTime
  JSON writers omit its seconds, so a checkpoint would silently change the
  offset despite passing bridge validation.
- **Keep formatting public model Time/DateTime offsets to minutes even when
  the offset contains seconds.** Rejected: a public JSON/Serde caller can lose
  an offset and change the represented instant without going through a
  checkpoint or the snapshot bridge.
- **Emit offset seconds in new Time/DateTime JSON strings.** Rejected for this
  stream: it changes the existing v2 tagged-value contract and requires a
  coordinated decoder and consumer migration; explicit errors preserve the
  current valid minute-precision representation without silent data loss.
- **Catch `to_rfc3339()` panics with `catch_unwind`.** Rejected: unwinding is
  a panic-strategy-dependent validation boundary and obscures the offending
  nested path; checking the local-time range reports an ordinary `EncodeError`
  before formatting or checkpoint publication.
- **Format DateTime without checking UTC plus offset.** Rejected: a
  minute-aligned `FixedOffset` can still place the local time beyond chrono's
  `NaiveDateTime` bounds; `to_rfc3339()` then panics even though the UTC instant
  and offset are individually valid, including during snapshot writes.
- **Keep private, independently maintained numeric-offset parsers in model and
  Arrow, or broaden the model parser to accept `Z`/`UTC`/zone names.** Rejected:
  duplicated validation can diverge on ASCII, width, minute and chrono bounds,
  while accepting nonnumeric forms in the persistence `Time` parser would
  weaken the existing exact `[+-]HH:MM` contract; Arrow owns its dispatch.

## Consequences

- Invalid tagged JSON produces a visible decode/Serde/storage error; snapshot
  recovery is atomic with respect to the decoded graph. Tests must cover the
  offset panic, missing/wrong/range-invalid fields, malformed nested geometry,
  vectors, and supported valid legacy tagged values.
- The public `parse_numeric_offset` adds a stable model API returning
  `Option<FixedOffset>` for exact signed numeric input; it does not change JSON
  bytes or admit zone names in persistence `Time`. Model tests must cover
  `+02:30`/`-05:45`, malformed signs/widths/digits, multibyte prefixes,
  `+00:60`, `+99:00`, and huge hour input without panic. Rust API `spec.md` and
  `description.md` must state the signature, `Some`/`None` contract, and Arrow
  reuse; a CI-run doctest or example must exercise a valid offset and invalid
 minute/multibyte inputs. [ADR-0088](./0088-explicit-arrow-value-provenance.md) specifies the Arrow error boundary.
- Even syntactically valid Geometry in a JSON snapshot property now fails
  recovery because core `Value` cannot represent it. This includes historical
  v1 snapshots and v2 snapshots, direct and nested map/list occurrences; they
  remain on disk rather than reopening as Null or being silently rewritten.
  Native and Memory VFS tests must cover both encodings, entity-ID and nested
  property-path storage errors, no partial graph accepted, and continued
  roundtrips for supported Point, Vector, and Map values.
- New snapshots preserve `__type` user maps, including point-looking and
  nested ones; version 1 remains readable subject to its documented
  irreducible known-tag collision and the Geometry bridge rejection. Test
  roundtrips on Native and Memory VFS.
- The public fallible decoder and parameter migration need Rust/CLI/C API
  specs, descriptions, and runnable examples when the implementation ships.
 [ADR-0088](./0088-explicit-arrow-value-provenance.md) separately specifies Arrow value provenance; it does not make the
  untagged public wire JSON into a persistence format.
- The public writer and property-map helper now return `Result`; direct
  `serde::Serialize` returns a serialization error for invalid values. Rust
  API specs and runnable examples must document the changed signatures and
  error behavior, including the intentional rejection of graph entities
  previously stringified by this persistence helper. Valid v2 persistence
  bytes remain readable without migration.
- Public model JSON/Serde callers with a non-minute `FixedOffset` on Time or
  DateTime now receive a path-qualified error rather than truncated bytes;
  callers must choose a minute-aligned offset before encoding. Model tests
  must cover `+1` and `-1` second offsets for both variants, nested maps/lists
  and `props_to_json`, Serde serialization errors, and offset/instant equality
  after round-tripping valid minute-aligned values. Rust API docs and runnable
  examples must describe this writer error boundary. Existing valid v2 shapes
  are unchanged.
- Public model JSON/Serde DateTime writes also return a path-qualified error
  when UTC plus the fixed offset exceeds chrono's local `NaiveDateTime` range;
  no `to_rfc3339()` panic escapes. Model tests must cover UTC
  `NaiveDateTime::MAX`/`+01:00` and `MIN`/`-01:00`, direct and nested paths,
  Serde errors, and successful near-boundary, minute-aligned round-trips.
  Native and Memory VFS tests must cover both invalid extremes (including a
  nested property) and valid near-boundary snapshots: fresh checkpoint errors
  publish no `graph.json` or manifest, while failed existing checkpoints
  preserve exact previous bytes and reopenable data. Rust API docs and a
  runnable example must describe the checked local-time boundary.
- Snapshot-write tests on Native and Memory VFS must show valid typed-value
  roundtrips and failed invalid writes with no published partial checkpoint.
  Model tests must cover nested non-finite numbers and coordinates, vector
 shape/ranges, and bit-preserving Float32 exactness. [ADR-0088](./0088-explicit-arrow-value-provenance.md) consumes the
  model-owned payloads and predicate instead of redefining them.
- Core-to-model temporal checks make JSON checkpoint writes fail with the node
  or relationship ID and full nested property path for unsupported CivilDate,
  CivilDateTime, out-of-range Duration seconds, and non-minute-aligned or
  unconstructible Time/DateTime offsets. Native and Memory VFS regression tests
  must cover rejected `+1` and `-1` second offsets at nested paths for both
  Time and DateTime, and a successful minute-aligned offset roundtrip for each.
  Failed fresh and existing checkpoints must publish no new
  `graph.json`/manifest on a fresh store and preserve exact prior bytes and
  reopenable data on an existing one. Tests also cover direct and
  list/map-nested properties and lossless roundtrips for valid chrono-range
  Date/LocalTime/Time/LocalDateTime/DateTime, Duration, Point, and Vector values.
