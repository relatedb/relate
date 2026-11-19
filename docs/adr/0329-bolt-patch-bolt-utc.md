---
type: adr
area: surface
claim: 209f3cce9bae795cb9e28c8fc72ea913f1b4219b
name: 0329-bolt-patch-bolt-utc
status: proposed
tags:
  - bolt
  - conformance
title: |-
  Bolt 4.4 patch_bolt utc: UTC DateTime structures when the client negotiates them
REFERENCES:
  - "[[adr/0177-bolt-parameter-conversion]]"
  - "[[adr/0178-engine-zone-resolver]]"
  - "[[adr/0325-bolt-temporal-results-as-packstream-structs]]"
SUPERSEDES:
  - "[[adr/0177-bolt-parameter-conversion]]"
---

# ADR-0329: Bolt 4.4 patch_bolt utc: UTC DateTime structures when the client negotiates them

## Context

Bolt 4.3 and 4.4 let a client opt into the Bolt 5.0 UTC DateTime
structures with HELLO extra `patch_bolt: ["utc"]`. A server that applies
the patch echoes it in HELLO SUCCESS, and from then on that connection
uses `0x49` DateTimeUTC and `0x69` DateTimeZoneIdUTC in both directions
instead of the legacy `0x46`/`0x66`, whose seconds field is a local wall
clock. Neo4j 2026.09.0 does this (F-10: SUCCESS carries
`patch_bolt: ["utc"]`; F-57: an offset datetime is
`0x49(1704161045, 123456789, 3600)`).

CypherLite ignores `patch_bolt`, so a patching client keeps getting legacy
structures it did not ask for. [ADR-0177](./0177-bolt-parameter-conversion.md) rejects `0x49`/`0x69` and rejected
the alternative of accepting them, because no negotiated client could send
them. [ADR-0325](./0325-bolt-temporal-results-as-packstream-structs.md) now encodes datetime results as `0x46`/`0x66`.

## Decision

1. **Negotiation.** When HELLO's extra has `patch_bolt` as a list and one
   of its string entries is `utc`, the connection applies the utc patch.
   HELLO SUCCESS then carries `patch_bolt: ["utc"]`. Only patches the
   server applies are echoed: unknown entries, non-string entries and a
   non-list `patch_bolt` apply nothing, and with no patch applied there is
   no `patch_bolt` key. The patch set is a typed per-session value, set by
   the HELLO that validates.
2. **Results under the patch.** An offset datetime encodes as `0x49`
   DateTimeUTC `[UTC epoch seconds, nanoseconds, offset seconds]`, a
   region datetime as `0x69` DateTimeZoneIdUTC `[UTC epoch seconds,
   nanoseconds, zone id]`. `datetime('2024-01-02T03:04:05.123456789+01:00')`
   is `0x49(1704161045, 123456789, 3600)`. Every other temporal structure
 ([ADR-0325](./0325-bolt-temporal-results-as-packstream-structs.md)) is unchanged.
3. **Parameters under the patch.** `0x49` decodes with `utc = seconds +
   nanoseconds` and `zone: None`. `0x69` decodes the UTC instant and
   resolves the zone's offset at that instant, which is never ambiguous
 (the new engine helper `zone_offset_at_utc`, beside [ADR-0178](./0178-engine-zone-resolver.md)'s
 wall-clock `zone_offset_at`). An unknown zone fails with [ADR-0177](./0177-bolt-parameter-conversion.md)'s
   unknown-timezone message. The legacy `0x46`/`0x66` are rejected with
 [ADR-0177](./0177-bolt-parameter-conversion.md)'s unsupported-struct message.
4. **Without the patch** nothing changes: `0x49`/`0x69` stay rejected and
   results use `0x46`/`0x66`.
5. The session only stores the flag (set by the HELLO that validates),
   echoes it and hands it to the one RECORD encoder
   (`results::write_record`) and to parameter decoding. The patch
   vocabulary and the structure choice live in the `patches` and
   `value_structs` modules.

## Alternatives considered

- **Ignore `patch_bolt` (status quo).** Rejected: the client asked for UTC
  structures and Neo4j grants them; a client that relies on the patch,
  for example to avoid the legacy DST ambiguity of `0x66`, gets the other
  encoding.
- **Always send the UTC structures.** Rejected: a Bolt 4.4 client that
  did not negotiate the patch cannot decode `0x49`/`0x69`.
- **Accept both legacy and UTC parameters under the patch.** More lenient.
  Rejected: the patched protocol defines only the UTC structures, and a
  legacy structure on a patched connection is a client bug that would be
  decoded with the wrong seconds meaning.
- **Reject a malformed `patch_bolt` HELLO.** Rejected: the field is a
  hint list, the server applies the patches it knows, and HELLO
  validation is owned by the protocol contract, not by this patch.

## Consequences

- Drivers that send `patch_bolt: ["utc"]` (the official Python driver
 does on Bolt 4.4, see [ADR-0177](./0177-bolt-parameter-conversion.md)'s Context) get the UTC structures they
  expect, and zoned datetimes no longer pass through a wall-clock reading.
  The Python driver smoke and `temporal_params.py` therefore exercise the
  patched encoding. The Python driver's (6.3.1) patched encoder mishandles
  a `zoneinfo.ZoneInfo` datetime parameter (it crashes or computes a wrong
  offset nondeterministically, against Neo4j too), so the example sends
  its zoned parameter with a pytz zone.
- [ADR-0177](./0177-bolt-parameter-conversion.md)'s rejection of `0x49`/`0x69` (item 5) and its "accept the 5.0
 UTC tags" alternative are superseded under the patch; [ADR-0177](./0177-bolt-parameter-conversion.md) gets a
  Later references note.
- `docs/api/bolt/spec.md` documents the HELLO echo and both tables list the
  UTC structures.
- `tests/compat/scripts/values/patch-bolt-utc.bolt` and unit tests pin the
  echo, both result structures and a `0x49` parameter echo.
