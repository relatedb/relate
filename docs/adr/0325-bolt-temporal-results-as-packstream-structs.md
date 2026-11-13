# ADR-0325: Bolt temporal results encode as PackStream temporal structures

Status: Proposed
Claim: b5196294892eeedc1608edede9e25e358929bc54

## Context

The Bolt server speaks Bolt 4.4 only (ADR-0057, ADR-0303). ADR-0272 made
Path a PackStream structure but kept "Temporal arms remain string
encodings", and ADR-0177 item 7 kept that for results while it taught the
parameter direction to decode the seven temporal structures. So a driver
that sends `date('2024-01-02')` as a parameter gets the string
`"2024-01-02"` back.

The Bolt compatibility probes against Neo4j 2026.09.0 at Bolt 4.4 show the
gap (LABS-323). Neo4j answers every temporal value as a structure: F-51
`date('2024-01-02')` is `0x44(19724)`, F-52 a time is `0x54`, F-53 a
localtime `0x74`, F-54 an offset datetime `0x46`, F-55 a zoned datetime
`0x66`, F-56 a localdatetime `0x64` and F-58 a duration `0x45`. CypherLite
sends strings. The one-time neo4rs 0.9.0-rc.10 driver probe shows the
consequence: neo4rs yields `BoltType::String` instead of the temporal
type (D-09..D-15), and a typed `row.get` of time, zoned datetime and
duration fails with `DeserializationError InvalidType`. The parameter
echoes D-26..D-32 decode correctly and come back as strings for the
same reason.

The engine's `Temporal` (ADR-0021) has `Date`, `CivilDate` (out-of-range
proleptic dates), `LocalTime`, `Time`, `LocalDateTime`, `CivilDateTime`,
`DateTime { utc, offset, zone }` and `Duration { months, days, nanos }`.
ADR-0177's decoders already define the wire layout of every structure, so
the encoding is their inverse.

## Decision

Without the `utc` patch (ADR-0329), `Value::Temporal` results encode as the
Bolt 4.4 PackStream structures, at any nesting depth, never as strings:

| Engine value | Tag | Fields |
| --- | --- | --- |
| `Date` | `0x44` Date | `[days]`: days since 1970-01-01 |
| `CivilDate { year, month, day }` | `0x44` Date | `[days]`, computed exactly in the proleptic Gregorian calendar |
| `Time { time, offset }` | `0x54` Time | `[nanoseconds since midnight of the local time, offset seconds]` |
| `LocalTime` | `0x74` LocalTime | `[nanoseconds since midnight]` |
| `LocalDateTime` / `CivilDateTime` | `0x64` LocalDateTime | `[epoch seconds of the wall clock read as UTC, nanoseconds]` |
| `DateTime` with an offset only | `0x46` DateTime | `[UTC epoch seconds + offset, nanoseconds, offset seconds]` |
| `DateTime` with an IANA region | `0x66` DateTimeZoneId | `[UTC epoch seconds + offset, nanoseconds, zone id]` |
| `Duration { months, days, nanos }` | `0x45` Duration | `[months, days, nanos div_euclid 10^9, nanos rem_euclid 10^9]` |

1. **Offset or region.** A `DateTime` is a region datetime when its zone
   names an IANA region; every other zone (none, `Z`, a numeric offset,
   any other spelling) is offset only. The engine helper
   `cypherlite_core::temporal::zone_region` makes the call on the same
   `zone_kind` classification the ISO formatter uses, so the Bolt surface
   compares no strings.
2. **Exact inverses.** The fields are exactly what ADR-0177's decoders
   read, so a temporal parameter echoes back as the same structure.
   Examples: `date('2024-01-02')` is `0x44(19724)`,
   `time('03:04:05.123456789+01:00')` is `0x54(11045123456789, 3600)`,
   `datetime('2024-01-02T03:04:05.123456789+01:00')` is
   `0x46(1704164645, 123456789, 3600)`,
   `datetime('2024-01-02T03:04:05[Europe/Stockholm]')` is
   `0x66(1704164645, 0, "Europe/Stockholm")`, and
   `duration('P1Y2M3DT4H5M6.5S')` is `0x45(14, 3, 14706, 500000000)`.
3. **Civil days.** `CivilDate` and `CivilDateTime` reuse the engine's one
   days-from-civil count, `cypherlite_core::temporal::civil_epoch_days`
   (the count civil-date arithmetic already used).
4. **Range.** A component that does not fit a PackStream Int (i64), for
   example a duration whose seconds exceed i64, fails the encode with an
   error naming the structure. It never falls back to a string or a
   clamped value.
5. Vector and UUID results are unchanged: they stay strings (ADR-0272).
   Point results are ADR-0326.

## Alternatives considered

- **Keep strings (ADR-0272, ADR-0177 item 7).** No change for clients that
  parse strings. Rejected: every driver that hydrates temporal types gets
  a string. neo4rs then fails typed reads, and a parameter does not round
  trip as its own type. This is the divergence the probes found.
- **Encode a region datetime as `0x46` with its resolved offset.** One
  structure fewer. Rejected: the zone id is lost, and Neo4j sends `0x66`,
  so a driver could not rebuild the zoned value.
- **Classify the zone by looking for a `/` in the Bolt surface.** Simple.
  Rejected: it re-splits a fact the engine already owns (`zone_kind`) and
  is the string sniffing the code-shape rules forbid.
- **Clamp or stringify an out-of-range component.** Never fails a PULL.
  Rejected: a silently wrong value is worse than a loud failure, and the
  engine produces such values only at its range edges.

## Consequences

- Drivers hydrate temporal results as their native types
  (`neo4j.time.*` in Python, `BoltType::Date` and friends in neo4rs).
  Clients that parsed the old strings must read the native type instead.
- `docs/api/bolt/spec.md`'s result structure table lists the seven
  temporal structures, and the parameter section's "results stay strings"
  sentence is gone.
- ADR-0272's "Temporal arms remain string encodings" and ADR-0177 item 7
  are superseded for temporals; both get a Later references note.
- The compat script `tests/compat/scripts/values/temporal-structs.bolt`
  and unit tests in `value_structs_tests.rs` pin every row above.
