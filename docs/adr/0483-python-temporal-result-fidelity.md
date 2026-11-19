---
type: adr
area: surface
claim: c2141235992a9be9c94c6ea4d851a977f69d4770
name: 0483-python-temporal-result-fidelity
status: proposed
tags:
  - python
  - values
title: Python temporal results keep nanoseconds and named zones
REFERENCES:
  - "[[adr/0224-result-column-types]]"
  - "[[adr/0448-tostring-temporal-full-precision]]"
---

# ADR-0483: Python temporal results keep nanoseconds and named zones

## Context

LABS-756. The Python binding (`surfaces/cypherlite-python/src/convert.rs`)
mapped every engine temporal onto the stdlib type with microsecond precision
and a fixed offset. The engine value was correct; two things were lost before
any comparison happened:

1. **Sub-microsecond nanoseconds** — `datetime('2015-06-01T12:34:56.123456789')`
   reached Python as `.123456`.
2. **Named zones** — `datetime('2020-10-25T02:30+02:00[Europe/Stockholm]')`
   reached Python with a `+02:00` fixed offset and no zone name, so the
   canonical rendering and any DST-dependent offset were gone.

The engine's temporal value already carries both (`Value::Temporal`, the IANA
zone name beside the offset), and its canonical text has one renderer,
`Temporal::iso_string` ([ADR-0448](./0448-tostring-temporal-full-precision.md)). Measured on the scenario harness, this loss
was the single largest coverage blocker: of 532 failing temporal scenarios, 141
were nanosecond truncation and 105 named-zone loss.

The stdlib types remain the right answer wherever they carry the value: they
are what DB-API callers, pandas and Arrow interop expect.

## Decision

**The binding returns the stdlib type when it is lossless and a dedicated
`cypherlite.Temporal` when it is not.** Losslessness is read off the engine
value's own fields — `nanosecond % 1000 == 0`, and no zone name — never off the
rendered text.

One class serves the three affected kinds (`datetime`, `localdatetime`,
`time`) rather than one class per kind, because `cypherlite.Time` is already the
PEP 249 type constructor and `Date`/`Timestamp` are its siblings: per-kind
classes would either shadow the DB-API surface or need a prefix. A `Date` never
loses anything, and a lossy `Duration` falls back to its ISO text (below), so
neither needs the class.

`cypherlite.Temporal` carries:

- `.text` — the engine's canonical ISO-8601 rendering, verbatim. The binding
  formats nothing itself: it asks the engine (`cypher_to_value` →
  `Temporal::iso_string`), so the whole-minute seconds omission (`12:00`, not
  `12:00:00`), the zone-suffix spelling and the trimmed nanosecond digits stay
  one implementation.
- `.kind` — the engine's kind name (`datetime`, `localdatetime`, `time`).
- `.nanosecond` — sub-second nanoseconds, 0..999 999 999.
- `.zone_name` — the IANA name, or `None`.
- `.offset_seconds` — the UTC offset in seconds, or `None` when the value has
  no offset.
- `.to_native()` — the stdlib value floored to microseconds and (for a named
  zone) reduced to its fixed offset: the documented lossy projection.

`str()` and `repr()` show the canonical text; `==` and `hash()` are the text,
which is the engine's own `=` relation for these kinds (a named zone never
equals the offset it denotes, while `Z` equals `+00:00` — both render `Z`).

Durations whose nanoseconds are not microsecond-aligned now fall back to their
ISO-8601 text instead of a truncating `timedelta` — the fallback the binding
already documents for month-bearing and out-of-range durations.

## Alternatives considered

- **A `datetime` subclass carrying the extra fields.** Rejected: `datetime` is
  a C type with a fixed layout, so extra slots cannot be added from pyo3, and
  returning a different type for the value would break `isinstance` against
  the stdlib class.
- **Always return the rich type.** Rejected: it breaks DB-API, pandas and
  Arrow consumers for the common lossless value, where the fidelity gain is
  zero.
- **One class per temporal kind (`DateTime`, `LocalDateTime`, `Time`).**
  Rejected: `cypherlite.Time` is the PEP 249 constructor for `datetime.time`
  and `cypherlite.Date`/`Timestamp` are its siblings; per-kind classes would
  need a prefix to avoid shadowing the DB-API surface, with no gain over one
  class discriminated by `.kind`.
- **Expose only `.text`, no accessors.** Rejected: callers would have to parse
  ISO text to read a nanosecond or a zone name, although the engine holds both
  as typed fields.
- **Format the canonical text in the binding.** Rejected: a second copy of the
 zone/seconds/nanosecond rules drifts from `Temporal::iso_string` ([ADR-0448](./0448-tostring-temporal-full-precision.md)) —
  `CypherValue`'s own `Display` already had, writing seconds unconditionally.
- **Accept `Temporal` as a query parameter (round trip).** Rejected for now: no
  public temporal-text parser is exposed on this surface, and the stdlib types
  already cover the parameter direction. A later change can add it.

## Consequences

- DB-API compatibility holds for every lossless value: the change is additive
  there and a type change only where information was previously dropped.
- `Cursor.description` type codes are unaffected — they come from the engine's
 column types ([ADR-0224](./0224-result-column-types.md)).
- Callers that pattern-match on `datetime` must also handle `Temporal` for
  values with nanoseconds or a named zone; `Temporal.to_native()` is the escape
  hatch.
- Contract documented in `docs/api/python/spec.md` and `description.md`, with
  the runnable `docs/api/python/examples/temporal_fidelity.py`; the bundled
  library skill names the new type; `docs/reference/python-library.md` lists it
  in the returned-value table.
