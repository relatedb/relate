---
type: adr
area: engine
claim: 40a18803fea67b1b821130f39f496631ad2de429
date: 2026-11-25
name: 0683-civil-zoned-datetimes
status: proposed
tags:
  - temporal
  - values
title: |-
  Far-year zoned datetimes carry a civil representation
---

# 0683: Far-year zoned datetimes carry a civil representation

## Context

`Temporal` keeps two internal representations for DATE and LOCAL
DATETIME: chrono's (`Date`/`LocalDateTime`, years −262143..=262143) and
the proleptic `CivilDate`/`CivilDateTime` (i64 years), so a
`date('-999999999-01-01')` or `localdatetime('-999999999-01-01T05:00')`
literal parses, stores, orders, and WAL-roundtrips today. ZONED DATETIME
has no wide twin: `parse_zoned_dt` feeds the body to `parse_naive_dt`
(chrono), so `datetime('-999999999-01-01T05:00+18:00')` fails with
"cannot parse localdatetime" and `datetime('-9999-01-01T05:00+18:00')`
with "year must be in 1..9999" — while live Neo4j 2026.10.1 parses every
far-year zoned literal (probed 2026-11-25; the Bolt driver merely cannot
carry the value back).

Nine spec-harness rows pin the gap: the TCK's
`TemporalIndexing.feature` "Open ranges include values beyond the former
zoned datetime sentinels" block (8 rows — far-year zoned values stored
as properties, compared with `>` under both `USING INDEX` and
`USING SCAN`) and `MiscAcceptance.feature`'s issue #13165
(`datetime('+275760-09-13T00:00Z')` as a relationship property).

## Decision

Add `Temporal::CivilZoned(Box<CivilZonedFields>)` — the zoned twin of
the existing civil variants: `CivilZonedFields` carries the wall clock as
civil fields (`year`, `month`, `day`, `time`), plus the offset and
optional zone spelling `DateTime` carries. The payload is boxed because
unboxed it outgrows the `DateTime` variant's 48 bytes, and `Temporal`
growing past 48 grows every row's `Value` with it (the memory-calibration
suites charge that size); a pinned size test guards the bound.

- **Parsing** — the chrono body parse stays first so its own rejections
  keep their texts; a far-year civil fallback claims the body only when
  the year is outside chrono's range, and only an offset or `Z` zone
  spec completes it: the offset applies through the wide day-number
  arithmetic, never a chrono conversion, and a malformed time part
  declines rather than substituting midnight. A bracketed IANA zone at a
  far year keeps today's rejection — the wall-clock resolution path
  (`resolve_wall_forward`) is chrono-bound and no corpus row exercises
  it.
- **Ordering** — `Temporal::compare` orders `CivilZoned` against itself
  and against `DateTime` by instant (epoch nanos), then offset, then
  zone — the `DateTime` rule, computed over the wide fields. `equals`
  compares the wall clock and zone identity, the `DateTime` relation.
- **Identity surfaces** — the kind (shared with `DateTime`), the type
  predicates (`IS DATETIME` …), the date/time capability flags (read
  from the kind, not re-enumerated), the constructor readers
  (`time(datetime(…))`, `datetime(…)` keeping a zoned source verbatim),
  and the renderings all answer for the civil form; the pyo3 bridge
  keeps its generic `iso_string()` route. Component accessors
  (`dt.year`, …) are the civil forms' one recorded divergence: like
  `CivilDate`/`CivilDateTime`, a far-year value carries no accessor
  vocabulary and the read is the typed rejection.
- **Persistence** — the WAL carries the value like the other civil
  variants (verified for `CivilDate`/`CivilDateTime`: create, reopen,
  replay). The JSON checkpoint bridge rejects it
  (`"unsupported CivilZoned"`), exactly as it rejects `CivilDate` and
  `CivilDateTime` — ADR-0087's recorded far-year limitation now covers
  three variants, and its "expand model temporal representation"
  follow-up stays open.
- **Index/group keys** — one more `NormTemporal` variant keyed by
  (instant, zone), mirroring `DateTime`'s (utc, zone): representation
  follows representation, as `CivilDate` already does.

## Consequences

Far-year zoned datetimes behave like far-year dates: parseable,
storable, orderable, WAL-durable, not checkpointable. The TCK sentinel
block and issue #13165 pass; nothing else changes — in-range zoned
literals keep the chrono path untouched. ADR-0087's model-expansion
follow-up gains the third variant, and the harness corpus keeps no
marking for any of these rows.
