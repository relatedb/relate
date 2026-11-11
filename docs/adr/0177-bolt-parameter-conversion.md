# 0177. Bolt parameters decode temporal, duration, and point values — or fail the RUN

- **Status**: Accepted
- **Date**: 2026-10-07
- **Deciders**: CypherLite maintainers (Linear LABS-189)

## Context

`cypherlite-bolt` negotiates Bolt **4.4** (`[0x00, 0x00, 0x04, 0x04]`) and
its HELLO SUCCESS echoes no `patch_bolt`, so 4.4 drivers serialize
`ZonedDateTime` parameters with the **legacy** tags `0x46` (DateTime) and
`0x66` (DateTimeZoneId), not the 5.0 UTC tags `0x49`/`0x69`. This was
verified against the installed Python driver: it switches to `0x49`/`0x69`
only when the server echoes `patch_bolt`. A RUN's second field is a
PackStream map of parameters.

Today those parameters are converted by `Pack::to_mdb`
(`surfaces/cypherlite-bolt/src/packstream.rs:47-67`) and `pack_params`
(`surfaces/cypherlite-bolt/src/protocol.rs:715-725`):

- `Pack::to_mdb` ends in `_ => Value::Null`, so every temporal, duration,
  point, and unknown struct silently becomes `null`: a driver sending
  `datetime('2024-01-01T00:00:00Z')` stores `null`.
- Its `Map` arm skips a non-string key (`if let Some(ks) = k.as_str()`),
  silently dropping that entry at any depth.
- `pack_params` treats a present-but-non-map params field as an empty map
  (`p.as_map()` returning `None`), silently dropping every parameter.

The engine already represents every value the nine supported tags carry:
`Temporal` (ADR-0021) has `Date`, `LocalTime`, `Time`, `LocalDateTime`,
`DateTime`, and `Duration` variants, and `PointValue` carries
`srid/x/y/z`. LABS-189 tracks the faithful-parameter work; this ADR is its
contract.

Constraints from earlier decisions: Bolt is a transport (ADR-0057) and
does not claim Neo4j's status-code taxonomy; FAILURE text crosses a trust
boundary and is server-authored (ADR-0260); RESULT-side encoding is
unchanged (ADR-0272 — Temporal, Point, and Vector results stay strings);
and `Pack` is part of the library API clients implementing embedded
Relate link (ADR-0281).

## Decision

**A parameter value the engine can represent is decoded faithfully; one it
cannot fails the RUN — never a silent `null` and never a silently dropped
entry.**

1. **`Pack::to_mdb` becomes fallible.** The exact signature is

   ```rust
   pub fn to_mdb(&self) -> Result<cypherlite::Value, String>
   ```

   matching `Pack::from_mdb`'s `Result<Pack, String>`. The recursion
   threads the `Result` through List and Map, and the `String` error is
   the message template in item 6. This is a **breaking change to the
   ADR-0281 library API** (the exported `Pack` type): every caller,
   including a client implementing embedded Relate, must handle the
   `Err(String)` case.
   `Pack::from_mdb`, `decode`, `encode`, `try_struct_msg`, and
   `chunk_message` keep their signatures.

2. **The nine tags decode to engine values.** One row per tag:

   | Tag | Wire fields | Engine value | Field-to-component rule |
   | --- | --- | --- | --- |
   | `0x44` | `[days: Int]` | `Value::Temporal(Temporal::Date(NaiveDate))` | `date = 1970-01-01 + days` in the proleptic Gregorian calendar; a date outside chrono's representable range is rejected |
   | `0x54` | `[nanoseconds: Int, tz_offset_seconds: Int]` | `Value::Temporal(Temporal::Time { time: NaiveTime, offset: i32 })` | `time` = midnight + `nanoseconds` (`0..=86_399_999_999_999`); `offset = tz_offset_seconds` (must fit `i32`) |
   | `0x74` | `[nanoseconds: Int]` | `Value::Temporal(Temporal::LocalTime(NaiveTime))` | `time` = midnight + `nanoseconds` (`0..=86_399_999_999_999`) |
   | `0x46` | `[seconds: Int, nanoseconds: Int, tz_offset_seconds: Int]` | `Value::Temporal(Temporal::DateTime { utc: NaiveDateTime, offset: i32, zone: None })` | `utc = epoch(seconds - tz_offset_seconds) + nanoseconds`; `offset = tz_offset_seconds`; `zone = None` (item 3) |
   | `0x66` | `[seconds: Int, nanoseconds: Int, tz_id: String]` | `Value::Temporal(Temporal::DateTime { utc: NaiveDateTime, offset: i32, zone: Some(tz_id) })` | `wall = epoch(seconds) + nanoseconds` is the local wall clock in `tz_id`; resolve `tz_id` at `wall` to `offset`; `utc = wall - offset`; `zone = Some(tz_id)` (item 4) |
   | `0x64` | `[seconds: Int, nanoseconds: Int]` | `Value::Temporal(Temporal::LocalDateTime(NaiveDateTime))` | `local = epoch(seconds) + nanoseconds` (no zone) |
   | `0x45` | `[months: Int, days: Int, seconds: Int, nanoseconds: Int]` | `Value::Temporal(Temporal::Duration { months: i64, days: i64, nanos: i128 })` | `months` and `days` verbatim; `nanos = seconds as i128 * 1_000_000_000 + nanoseconds as i128` |
   | `0x58` | `[srid: Int, x: Float, y: Float]` | `Value::Point(PointValue { srid, x, y, z: None })` | `srid` must be `4326` or `7203` (2D); `x`/`y` stored verbatim |
   | `0x59` | `[srid: Int, x: Float, y: Float, z: Float]` | `Value::Point(PointValue { srid, x, y, z: Some(z) })` | `srid` must be `4979` or `9157` (3D); `x`/`y`/`z` stored verbatim |

   `nanoseconds` in `0x46`, `0x66`, and `0x64` is a sub-second component
   and must be `0..=999_999_999`; in `0x54` and `0x74` it is nanoseconds
   since midnight and must be `0..=86_399_999_999_999`. `0x45`'s
   `nanoseconds` is the signed sub-second remainder and is not bounded by
   the layout — it is combined into the i128 `nanos`. A point's `x`/`y`/`z`
   are stored bit-for-bit: the struct already names the SRID, so the
   `point()` map-construction rules (family inference, latitude range,
   longitude wrap) do not re-run at decode.

   `Null`, `Bool`, `Int`, `Float`, `String` decode to themselves; `List`
   and `Map` decode recursively. `Float` keeps non-finite values
   (NaN / ±Infinity) as `Value::Float` (ADR-0168). Every map — at any
   depth — must have string keys (item 5).

3. **Legacy 0x46.** `seconds` is UTC epoch seconds **plus**
   `tz_offset_seconds` — the wire carries local epoch seconds. The decoder
   computes `utc = epoch(seconds - tz_offset_seconds) + nanoseconds` and
   stores `offset = tz_offset_seconds` with **`zone: None`**, never
   `Some("Z")`. This matches how `cypherlite-core/src/temporal/parse.rs`
   represents `datetime('...Z')` and `datetime('...+01:00')`: an explicit
   zone suffix, including `Z`, produces `zone: None` (`offset = 0` for
   `Z`), while only a value with no zone suffix at all gets
   `zone: Some("Z")`. `iso_string()` still renders an offset of 0 as the
   `Z` suffix, so `0x46 [8100, 42, 3600]` is
   `1970-01-01T02:15:00.000000042+01:00` and `0x46 [4500, 42, 0]` renders
   `1970-01-01T01:15:00.000000042Z`.

4. **Legacy 0x66.** `seconds` is the **wall-clock** epoch seconds in
   `tz_id` (not UTC). The decoder resolves `tz_id` at the wall clock
   `wall = epoch(seconds) + nanoseconds` exactly like `iana_offset_at`
   (`cypherlite-core/src/temporal/construct.rs:262-275`):
   `tz.from_local_datetime(&wall).single().or_else(|| …earliest())`.
   - A unique wall time resolves to that offset:
     `utc = wall - offset`, `zone = Some(tz_id)`. `0x66 [8100, 42,
     "Europe/Paris"]` is
     `1970-01-01T02:15:00.000000042+01:00[Europe/Paris]`.
   - A wall time in a DST **overlap** resolves to the **earliest**
     occurrence (the pre-transition offset). `0x66 [1635647400, 0,
     "Europe/Stockholm"]` — 2021-10-31T02:30 wall — resolves to `+02:00`
     (CEST) and renders `2021-10-31T02:30:00+02:00[Europe/Stockholm]`.
   - A wall time in a DST **gap** is rejected. `0x66 [1616898600, 0,
     "Europe/Stockholm"]` — 2021-03-28T02:30 wall, which does not exist —
     fails the RUN with `invalid parameter structure for …: ambiguous
     local time 2021-03-28T02:30:00 in timezone 'Europe/Stockholm'`.
     (`construct.rs` rejects the gap the same way. `parse.rs`'s string
     parser rejects overlaps too, but the wire path follows the
     `iana_offset_at` precedent.)
   - `tz_id` must name a zone in the engine's timezone database
     (`chrono_tz::Tz`, the type behind `iana_offset_at`); anything else —
     including `Z` and fixed-offset spellings — is an unknown timezone.

   The worked examples below are the pinned contract; each
   `cypher_constructor` parses in the engine today.

   | Wire struct | Expected value (`iso_string`; Cypher literal for points) | Cypher constructor |
   | --- | --- | --- |
   | `0x44 [0]` | `1970-01-01` | `date('1970-01-01')` |
   | `0x45 [1, 2, 3, 4]` | `P1M2DT3.000000004S` | `duration('P1M2DT3.000000004S')` |
   | `0x46 [8100, 42, 3600]` | `1970-01-01T02:15:00.000000042+01:00` | `datetime('1970-01-01T02:15:00.000000042+01:00')` |
   | `0x66 [8100, 42, "Europe/Paris"]` | `1970-01-01T02:15:00.000000042+01:00[Europe/Paris]` | `datetime('1970-01-01T02:15:00.000000042+01:00[Europe/Paris]')` |
   | `0x58 [7203, 1.0, 2.0]` | `point({srid: 7203, x: 1.0, y: 2.0})` | `point({srid: 7203, x: 1.0, y: 2.0})` |
   | `0x59 [9157, 1.0, 2.0, 3.0]` | `point({srid: 9157, x: 1.0, y: 2.0, z: 3.0})` | `point({srid: 9157, x: 1.0, y: 2.0, z: 3.0})` |

5. **Rejected inputs.** `Pack::to_mdb` returns `Err` for:
   - `Bytes` anywhere;
   - every struct tag outside the nine — in particular the Bolt 5.0 UTC
     DateTime `0x49`, its DateTimeZoneId `0x69`, Node `0x4E`, Relationship
     `0x52`, UnboundRelationship `0x72`, Path `0x50`, Vector `0x56`, and
     every unknown tag;
   - a struct with the wrong field count, or a field whose Pack type is
     not the one the layout requires (`Int` vs `Float` vs `String`);
   - a temporal `nanoseconds` field outside its layout bound (item 2), an
     epoch/`days` value outside chrono's representable range, or a
     `tz_offset_seconds` that does not fit `i32`;
   - a point SRID outside `4326/4979/7203/9157`, or an SRID that does not
     match the tag's dimension (`0x58` requires a 2D SRID, `0x59` a 3D
     one);
   - an unknown `tz_id`, or a `0x66` wall time in a DST gap;
   - a map key that is not a `Pack::String` at any depth.

   `pack_params` (`surfaces/cypherlite-bolt/src/protocol.rs`) owns the RUN
   envelope one level above `to_mdb`:
   - a RUN whose params field is present but not a `Pack::Map` is
     rejected (an absent params field stays the empty map);
   - a non-string key of the top-level params map is rejected.

   Nested map keys are `to_mdb`'s job; the top-level params map is
   `pack_params`'s. Together they reject a non-string key at any depth.

6. **Failure contract.** The RUN arm converts parameters before namespace
   selection and before the engine call. On `Err(message)` the server
   sends a FAILURE with the pinned code **`CypherLite.Error`** (no new
   `Neo.*` code, ADR-0260), sets the connection's failed state so
   subsequent messages are IGNORED until RESET, and does not execute the
   query. The message is server-authored from the parameter path and the
   offending kind only — it never contains engine internals — so it is
   safe to echo to the untrusted peer; ADR-0260's genericization does not
   apply to this protocol-level failure.

   Paths are `$` for the params field itself, `$name` for a top-level
   parameter, `.key` for a map entry, and `[i]` for a list element (for
   example `$p`, `$p.x`, `$p.xs[0]`). Every message names the offending
   value by its parameter path. `{kind}` is the Pack variant name
   `Null`, `Bool`, `Int`, `Float`, `String`, `List`, `Map`, `Bytes`, or
   `Struct(0xNN)` (two uppercase hex digits). The templates:

   | Case | Message template |
   | --- | --- |
   | Bytes | `unsupported parameter value for {path}: Bytes` |
   | unsupported struct tag | `unsupported parameter value for {path}: Struct(0x{tag:02X})` |
   | wrong field count | `invalid parameter structure for {path}: Struct(0x{tag:02X}) has {count} fields, expected {expected}` |
   | wrong field type | `invalid parameter structure for {path}: field {index} of Struct(0x{tag:02X}) must be {expected}, got {kind}` |
   | field out of range | `invalid parameter structure for {path}: field {index} of Struct(0x{tag:02X}) is out of range: {value}` |
   | unknown point SRID | `invalid parameter structure for {path}: unknown srid {srid}: expected one of 4326, 4979, 7203, 9157` |
   | point SRID/dimension mismatch | `invalid parameter structure for {path}: srid {srid} does not match the coordinate dimension of Struct(0x{tag:02X})` |
   | unknown tz_id | `invalid parameter structure for {path}: unknown timezone '{tz_id}'` |
   | DST gap | `invalid parameter structure for {path}: ambiguous local time {wall} in timezone '{tz_id}'` |
   | non-string map key | `unsupported parameter map key for {path}: {kind} (map keys must be strings)` |
   | params not a map | `unsupported parameter value for {path}: {kind} (RUN params must be a map)` |

   For a non-string map key, `{path}` is the path of the map that
   contains the key (the params root is `$`). For the params-not-a-map
   case `{path}` is `$`, the field itself — no parameter exists to name.

7. **RESULT encoding is unchanged.** `Pack::from_mdb` and the encoder
   keep ADR-0272's shapes: Temporal, Point, and Vector results remain
   strings. This ADR changes only the parameter (client → server)
   direction.

## Alternatives considered

- **Keep the `Value::Null` catch-all and the key-skip (status quo).**
  Rejected: it is the LABS-189 defect — a client's `datetime`, `point`,
  or `duration` parameter silently becomes `null`, and a non-string map
  key silently vanishes, so data is corrupted instead of the RUN failing
  loudly.
- **Decode only the temporal tags and leave points null.** Rejected: a
  partial contract keeps silently corrupting the other half of the
  affected types, and the point structs are as cheap to decode as the
  temporal ones.
- **Accept the 5.0 UTC tags `0x49`/`0x69` as well.** Rejected: the server
  negotiates Bolt 4.4 and sends no `patch_bolt`, so no negotiated client
  sends them; accepting them would invent a wire surface and pre-empt the
  separate decision to negotiate Bolt 5.0.
- **Degrade unsupported values instead of failing the RUN** (Bytes to
  `null`, out-of-range nanos clamped). Rejected: silent data loss or
  silent value change is exactly the class of bug this ADR removes; a
  client cannot distinguish "the value was null" from "the server
  ignored it".
- **A new `Neo.ClientError.*` code for parameter conversion failures.**
  Rejected: Bolt is a transport (ADR-0057) and CypherLite does not claim
  Neo4j's status-code taxonomy; the pinned `CypherLite.Error` already
  covers caller-facing failures (ADR-0260).
- **Genericize parameter failures like internal-class errors
  (`internal error`, ADR-0260).** Rejected: the message is server-authored
  and describes the caller's own parameter path, so it is safe to echo;
  genericizing it would hide the one piece of information the client
  needs to fix the request.
- **Represent a 0x46 offset of 0 as `zone: Some("Z")`.** Rejected: it
  contradicts `parse.rs`, where `datetime('...Z')` and
  `datetime('...+01:00')` are both `zone: None`; a wire offset is an
  explicit suffix, so it maps to the explicit-suffix representation, and
  the zero offset still renders as `Z`.
- **Resolve a 0x66 DST gap by shifting to the nearest valid instant, or a
  DST overlap to the latest offset.** Rejected: the engine already has
  exactly one IANA resolution rule — `iana_offset_at`'s
  `.single().or_else(earliest)` — and keeping one rule in the engine is
  better than a second, Bolt-only policy; the gap has no valid instant to
  shift to, and the overlap's earliest occurrence is the documented
  choice.
- **Accept a non-map params field as an empty map (status quo).**
  Rejected: silently dropping every parameter is the same data-loss
  defect at the envelope layer; the RUN fails instead.
- **Stringify non-string map keys instead of rejecting them.**
  Rejected: Bolt maps require string keys, and an invented key (`"1"`)
  can collide with a real one, hiding a client bug behind wrong data.
- **Reject only in `to_mdb` and leave `pack_params` permissive.**
  Rejected: a non-map params field never reaches `to_mdb`'s `Map` arm, so
  it would still silently vanish; the envelope needs its own check.

## Consequences

- `Pack::to_mdb` returns `Result<cypherlite::Value, String>`. This is a
  breaking change to the ADR-0281 library API: the in-tree caller
  (`pack_params`) is updated in the same change, and a client
  implementing embedded Relate must handle `Err`. `Pack::from_mdb` and the
  encoder are unchanged (ADR-0272).
- Parameters of every engine temporal type — `date`, `time`, `localtime`,
  `datetime` (offset and zone forms), `localdatetime`, `duration` — and
  `point` round-trip from Bolt 4.4 drivers into the engine; unsupported
  values fail the RUN with a path-qualified message instead of becoming
  `null`.
- Parameter conversion failures are FAILUREs with `CypherLite.Error`,
  set the failed state, and never reach the engine; the query does not
  execute and no namespace selection happens. `docs/api/bolt/spec.md`
  gains the parameter conversion table and the failure messages.
- The decode is strict: wrong field count/type, out-of-range components,
  unknown zones, unsupported tags, `Bytes`, and non-string map keys all
  fail the RUN. `0x46` yields `zone: None`; `0x66` yields
  `zone: Some(tz_id)` with the earliest offset on a DST overlap and a RUN
  failure on a DST gap. The two Stockholm examples and every worked
  example above are pinned by tests.
- No new engine dependency and no engine change: the conversion lives in
  the Bolt surface and uses the workspace `chrono`/`chrono-tz`
  dependencies the engine already ships (ADR-0103 R6 is not triggered).
- Tests: `packstream_tests.rs` pins each tag's decode, each worked
  example (via its `cypher_constructor`), and each rejection message;
  `protocol_tests.rs` pins the RUN FAILURE code, the failed state, the
  no-execution behavior, and the `pack_params` envelope checks.

## Later references

- ADR-0178: supersedes item 4's in-surface restatement of the IANA
  wall-clock rule and the "no engine change" consequence. The engine
  exports `zone_offset_at`/`ZoneError`, and the 0x66 decoder calls it; the
  outcomes pinned here (earliest on overlap, gap rejected, unknown zone
  rejected) are unchanged.
- ADR-0179: supersedes item 1's `pub fn to_mdb` signature and its
  breaking-change consequence. The conversion is crate-internal
  (test-only `pub(crate)`); the decode contract of items 2–6, the failure
  messages, and every worked example pinned here are unchanged.
- ADR-0307: supersedes item 6's pinned code. A parameter conversion
  failure is a FAILURE with `Neo.ClientError.Request.Invalid`, not
  `CypherLite.Error`; the messages, the failed state and the no-execution
  contract are unchanged.
- ADR-0325: supersedes item 7 ("RESULT encoding is unchanged") for
  temporals: temporal results encode as the structures this ADR decodes,
  so a temporal parameter echoes back as the same structure. ADR-0326 does
  the same for points (`0x58`/`0x59`).
- ADR-0328: supersedes item 5's `Bytes` rejection and its message row. A
  PackStream Bytes parameter decodes to the engine's statement-scoped byte
  array (ADR-0327) and round-trips as Bytes.
- ADR-0329: supersedes item 5's rejection of `0x49`/`0x69` and the "Accept
  the 5.0 UTC tags" alternative for connections that negotiate the Bolt
  4.4 `patch_bolt` `utc` patch: there `0x49`/`0x69` decode and the legacy
  `0x46`/`0x66` are rejected. Without the patch this ADR is unchanged.
