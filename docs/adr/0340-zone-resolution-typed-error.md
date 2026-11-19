---
type: adr
area: engine
claim: 00fd540ef29803463d12e81e60307e8c5a57d40e
name: 0340-zone-resolution-typed-error
status: proposed
tags:
  - errors
  - values
title: Zone resolution at a UTC instant returns the typed ZoneError vocabulary
REFERENCES:
  - "[[adr/0329-bolt-patch-bolt-utc]]"
---

# ADR-0340: Zone resolution at a UTC instant returns the typed ZoneError vocabulary

## Context

The engine has two zone resolvers (`cypherlite-core/src/temporal/`):
`zone_offset_at(zone, local_wall_clock) -> Result<i32, ZoneError>` for a
wall clock (unknown zone, DST gap) and
`zone_offset_at_utc(zone, utc_instant) -> Option<i32>` for a UTC instant.
An instant is never ambiguous, so the UTC resolver's only failure is an
unknown zone — but it encodes that failure as `None`, a second failure
vocabulary for the same fact. The shared caller `iana_offset_at`
hand-writes the identical unknown-timezone message for both arms, and the
Bolt DateTimeZoneIdUTC decoder ([ADR-0329](./0329-bolt-patch-bolt-utc.md)) maps the `None` to its own
error.

## Decision

`zone_offset_at_utc(zone, utc) -> Result<i32, ZoneError>`: an unknown zone
is `Err(ZoneError::UnknownZone)`, matching `zone_offset_at`;
`ZoneError::NonexistentWallTime` is unreachable for a UTC instant and is
documented as such. `iana_offset_at`'s UTC arm applies the same one
`map_err` as its local arm, deleting the duplicated
`unknown timezone '<zone>'` message. The zone-resolution cluster moves
from `construct.rs` into a new `temporal/zone.rs` (re-exported from
`temporal/mod.rs` exactly as today: `zone_offset_at`,
`zone_offset_at_utc`, `zone_region`, `ZoneError`), so the
constructors module keeps to its name and stays below the file-size
budget; the truncate cluster is noted there as the next extraction.

## Alternatives considered

- **Keep `Option` and add a second `Result` wrapper.** Rejected: two
  public paths for one resolution and two failure vocabularies for one
  fact.
- **Return `Result<i32, Error>` with the rendered message inside the
  resolver.** Rejected: the resolver would own a user-facing message that
  differs per caller (core's `unknown timezone`, Bolt's own path error),
  re-splitting the vocabulary it just unified.
- **Make `zone_offset_at_utc` keep `Option` and have callers construct
  `ZoneError`.** Rejected: the conversion is mechanical and would be
  copied at every caller.
- **Fold the UTC resolver into `zone_offset_at` with an instant flag.**
  Rejected: the two reading semantics (single-or-earliest wall clock vs.
  unambiguous instant) are the public contract; a boolean mode hides it.

## Consequences

- `iana_offset_at` has one message for both arms.
- The Bolt `DateTimeZoneIdUTC` decoder (surfaces/cypherlite-bolt) adapts
  its `ok_or_else` to `map_err`; behavior is unchanged (only
  `UnknownZone` can occur).
- Tests: `temporal/tests.rs` retargets the UTC cases to `Ok(..)`/`Err(..)`;
  `temporal/zone.rs` carries the cluster's unit tests.
