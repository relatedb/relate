---
type: adr
area: engine
date: 2026-10-07
deciders: Oskar Hane (agreed in the end-of-feature review conversation), CypherLite maintainers
name: 0178-engine-zone-resolver
status: accepted
tags:
  - values
title: Expose the engine's IANA wall-clock zone resolver
REFERENCES:
  - "[[adr/0103-engine-surfaces-testing-layout]]"
  - "[[adr/0177-bolt-parameter-conversion]]"
  - "[[adr/0260-trust-boundary-error-sanitization]]"
SUPERSEDES:
  - "[[adr/0177-bolt-parameter-conversion]]"
---

# 0178. Expose the engine's IANA wall-clock zone resolver

## Context

An IANA wall clock becomes a UTC offset through one rule in the engine:
`iana_offset_at` in
`cypherlite/cypherlite-core/src/temporal/construct.rs`. Its local-time arm
resolves a wall clock with `.single().or_else(earliest)` and converts the
occurrence through `.offset().fix().local_minus_utc()`; a zone name outside
the time-zone database and a wall time in a DST gap are errors, while a wall
time in a DST overlap resolves to the earliest (pre-transition) occurrence.

[ADR-0177](./0177-bolt-parameter-conversion.md) item 4 names that rule as the one the Bolt 0x66 legacy
DateTimeZoneId parameter decoder follows. The decoder was implemented by
restating the rule in `surfaces/cypherlite-bolt/src/param_structs.rs`
(`datetime_zone_id_to_value`), so the surface compiles its own copy of
`.single().or_else(earliest)` and of the offset conversion. The
end-of-feature stellar review flagged this as a drift hazard: a future
change to the engine's rule — or to how it converts the occurrence to
seconds — would silently diverge from the decode contract, and nothing
compiles or tests the two copies against each other. The same selection
rule is restated a second time inside the engine, in
`cypherlite/cypherlite-core/src/temporal/arith.rs` (`utc_in_zone`).

The engine already depends on `chrono` and `chrono-tz`, so exposing the
rule adds no dependency. The surface needs the wall-clock reading only; the
private `iana_offset_at` also has a UTC reading (`IanaAt::Utc`, used by
`offset_from_utc_map`) that is total for a valid zone and has no error
outcome.

## Decision

**The engine exports its IANA wall-clock zone resolver, and the sites that
apply the single-or-earliest rule — the engine's construction path, the
engine's temporal arithmetic, and the Bolt 0x66 decoder — call it.**

The exact surface is defined in
`cypherlite/cypherlite-core/src/temporal/construct.rs` and re-exported from
`cypherlite/cypherlite-core/src/temporal/mod.rs`'s `pub use construct::{…}`
list, so the public path is `cypherlite_core::temporal::zone_offset_at`:

```rust
pub fn zone_offset_at(zone: &str, local: NaiveDateTime) -> std::result::Result<i32, ZoneError>
```

(`NaiveDateTime` is `chrono::NaiveDateTime`; the return type is the
standard two-parameter `std::result::Result`, not `cypherlite_core::Result`.)
The error is a dedicated public enum:

```rust
/// Why an IANA wall clock could not be resolved to a UTC offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneError {
    /// `zone` does not name a zone in the engine's IANA database.
    UnknownZone,
    /// `local` falls in a DST gap: the wall clock never occurs in `zone`.
    NonexistentWallTime,
}
```

The function body is the rule that lives in `construct.rs` today: parse
`zone` as a `chrono_tz::Tz`, resolve `local` with
`.single().or_else(earliest)`, and return
`.offset().fix().local_minus_utc()` in seconds. It distinguishes exactly
three outcomes:

1. **a resolved offset** — `Ok(offset)`; a unique wall time yields its
   offset, and a DST overlap yields the earliest occurrence's offset;
2. **an unknown zone name** — `Err(ZoneError::UnknownZone)`;
3. **a wall time that does not exist** — `Err(ZoneError::NonexistentWallTime)`,
   a DST gap.

Callers map those outcomes to their own messages and error types by
matching the variants — never by matching error text. Inside the engine,
`iana_offset_at`'s local arm delegates to `zone_offset_at` and maps
`UnknownZone` back to today's `Error::ty(format!("unknown timezone
'{zone}'"))` and `NonexistentWallTime` back to today's
`Error::ty("ambiguous datetime")`, so no existing engine message or outcome
changes; `arith.rs::utc_in_zone` also delegates (`.ok()` plus a checked
`local - offset` subtraction), preserving its `Option<NaiveDateTime>`
contract, where a gap or an unknown zone still yields `None`. The
`IanaAt::Utc` reading stays private: only the wall-clock reading has a
"does not exist" outcome, and no caller needs the UTC reading. The
single-or-earliest rule and the `local_minus_utc` conversion then exist
once in the engine, inside `zone_offset_at`.

This ADR supersedes [ADR-0177](./0177-bolt-parameter-conversion.md) item 4 and its no-engine-change consequence.
Item 4's decode outcomes stay the pinned contract — a unique wall time
resolves, a DST overlap resolves to the earliest occurrence, a DST gap
fails the RUN, and an unknown `tz_id` fails the RUN — but the surface no
longer restates the rule: it calls `zone_offset_at` and maps the two error
variants to its own [ADR-0177](./0177-bolt-parameter-conversion.md) item 6 messages. `parse.rs`'s zoned-string
parser is deliberately not a caller: it rejects DST overlaps (`.single()`
only), a different rule recorded in [ADR-0177](./0177-bolt-parameter-conversion.md) item 4.

## Alternatives considered

- **Leave the rule restated in the Bolt surface (status quo).** Rejected:
  it is the reviewed defect. The surface would keep compiling its own copy
  of the engine rule, so an engine change would silently diverge from the
  decode contract, and no test compares the two copies.
- **Expose `iana_offset_at` as-is, with its `IanaAt` enum.** Rejected: it
  would leak an internal classification whose `Utc` arm the surface never
  uses, and force every caller to construct a private mode; the public
  function should take exactly the wall clock it resolves.
- **Return the engine's `Error`, or a `String`, and let callers match the
  message text.** Rejected: message matching is brittle and couples a
 surface to engine wording, which crosses [ADR-0260](./0260-trust-boundary-error-sanitization.md)'s trust boundary the
  wrong way (the surface must author its own caller-facing text); a typed
  two-variant error keeps the outcomes distinct without sharing strings.
- **Return a resolved `Temporal::DateTime` (or `chrono::DateTime<Tz>`)
  instead of the offset.** Rejected: the `wall - offset` subtraction and
  its range policy differ per caller — the engine errors with `range_err()`,
  the Bolt decoder with its path-qualified out-of-range message — so
  returning the offset keeps the shared rule shared and the caller-specific
  error handling caller-specific.
- **Encode the outcome positionally (`Option<Option<i32>>`, or expose
  chrono's `LocalResult`).** Rejected: it makes "unknown zone" and "does
  not exist" easy to confuse and invites a defaulting `unwrap_or`; a named
  enum states which outcome occurred.
- **Keep the resolver private and route it through the `cypherlite`
  facade.** Rejected: the Bolt surface already links `cypherlite-core`
  directly (for example `PointValue`), so a facade re-export would add a
  second public path to the same function with no new consumer; the rule
  belongs to the crate that owns the temporal model.
- **Also single-source `parse.rs`'s strict `.single()` parser.** Rejected:
 the string parser rejects DST overlaps on purpose ([ADR-0177](./0177-bolt-parameter-conversion.md) item 4
  records the difference between the string path and the wire/construct
  path); folding it into the earliest-on-overlap resolver would change
  documented parser behavior.
- **Change the engine rule to reject overlaps so one strict resolver
  serves every caller.** Rejected: the earliest-on-overlap choice is
 pinned by [ADR-0177](./0177-bolt-parameter-conversion.md)'s worked example (`0x66 [1635647400, 0,
  "Europe/Stockholm"]` → `+02:00`) and by existing engine behavior;
  changing the rule is a separate behavior decision, not this one.

## Consequences

- `cypherlite_core::temporal` exports `zone_offset_at` and `ZoneError`.
  `construct.rs::iana_offset_at`'s local arm and `arith.rs::utc_in_zone`
  delegate to it; every existing engine error message, value, and outcome
  is unchanged, and `parse.rs` is untouched.
- The Bolt 0x66 decoder calls `cypherlite_core::temporal::zone_offset_at`
 and maps `UnknownZone` / `NonexistentWallTime` to its [ADR-0177](./0177-bolt-parameter-conversion.md) item 6
  messages; it no longer parses `chrono_tz::Tz` or applies
  `.single().or_else(earliest)` itself. The surface's decode outcomes and
  tests are unchanged; `docs/api/bolt/spec.md`'s 0x66 rows and messages
 stay as pinned by [ADR-0177](./0177-bolt-parameter-conversion.md).
- Tests: `cypherlite-core` gains unit tests pinning the three outcomes and
  the overlap choice (for example `Europe/Stockholm` 2021-10-31T02:30 wall
  → `+02:00`; 2021-03-28T02:30 wall → `NonexistentWallTime`; `Not/AZone`
  → `UnknownZone`), plus coverage that the delegating engine callers keep
 their existing messages; [ADR-0177](./0177-bolt-parameter-conversion.md)'s Bolt decoder tests stay green.
- No dependency change: `chrono`/`chrono-tz` are already engine
 dependencies ([ADR-0103](./0103-engine-surfaces-testing-layout.md) R6 is not triggered); the workspace's dependency
  graph is unchanged.
- [ADR-0177](./0177-bolt-parameter-conversion.md) gains a Later references note naming this ADR; its item 4
  outcomes and worked examples remain the pinned decode contract.
- No client-facing document change: the export is an engine seam consumed
  by surfaces, not a new `cypherlite` facade API, so no `docs/api/rust`
  entry is added; `docs/api/bolt/spec.md` keeps the 0x66 contract it
 already documents ([ADR-0177](./0177-bolt-parameter-conversion.md)).

## Later references

- **LABS-785 (2026-11-02) — a DST gap resolves forward for construction and
  zoned-literal parsing.** Neo4j 2026.09 turns a wall clock inside a spring
  forward gap into the shifted post-transition value
  (`datetime('2020-03-29T02:30[Europe/Stockholm]')` →
  `2020-03-29T03:30+02:00[Europe/Stockholm]`, and the equivalent map
  constructor does the same); the measured acceptance suite fails otherwise.
  `zone_offset_at` — the exported resolver this ADR defines — is unchanged:
  it still reports `NonexistentWallTime`, and its Bolt/arithmetic callers
  keep their pinned outcomes. The constructors and `parse_zoned_dt` now
  resolve a wall clock through `zone::resolve_wall_forward`, which shifts a
  gap by the transition's offset delta and also lets an inherited `datetime`
  source's stored offset disambiguate an overlap. A zoned literal that names
  an explicit offset inside a gap stays an error, because no offset is valid
  there (Neo4j raises 22N05).
