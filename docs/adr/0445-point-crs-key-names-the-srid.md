---
type: adr
area: cypher
claim: 904a8b42a7e6a406e7a76c198eddd916ff0883dc
name: 0445-point-crs-key-names-the-srid
status: proposed
status_note: 2026-10-29
tags:
  - conformance
  - functions
title: "`point` reads the `crs` key as the name spelling of `srid`"
REFERENCES:
  - "[[adr/0167-point-accepts-map-like-entities]]"
---

# ADR-0445: `point()` reads the `crs` key as the name spelling of `srid`

## Context

[ADR-0167](./0167-point-accepts-map-like-entities.md) records that Neo4j also reads a `crs` key (e.g. `crs: 'wgs-84'`)
from `point()` maps and entities, defers it, and leaves the note "a later
ADR can add it". CypherLite has ignored the key, so
`point({x: 2.3, y: 4.5, crs: 'WGS-84'})` builds a cartesian 7203 point:
the harness's `Spatial` `[1]`/`[2]` rows expecting 4326/4979 fail, and
`TernaryEqualityAcceptance`'s row expecting
`point({x: 1, y: 2, crs: 'wgs-84'})` to differ from `point({x: 1, y: 2})`
does not. The same ignored key also makes `TernaryComparisonAcceptance`'s
cross-CRS ordering comparisons answer: with the ordering operators'
equality fallback (`<=`/`>=` hold for equal points) the two points compare
equal and `<=`/`>=` return true, where Neo4j (different CRSs are
incomparable) returns null.

## Decision

`point()` reads `crs` from the map as the *name* spelling of the same
four-code CRS domain `srid` selects:

- `'wgs-84'` = 4326, `'wgs-84-3d'` = 4979, `'cartesian'` = 7203,
  `'cartesian-3d'` = 9157, matched case-insensitively (the harness spells
  `'WGS-84'` and `'wgs-84'`).
- A non-string `crs`, or a string outside that set, is an error naming the
  accepted values.
- `crs` and `srid` may both be present only when they name the same code;
  a conflict is an error.
- The coordinate keys still decide the family (`x`/`y` cartesian,
 `latitude`/`longitude` geographic) exactly as [ADR-0167](./0167-point-accepts-map-like-entities.md) ruled:
  `{x, y, crs: 'wgs-84'}` is a 4326 point whose `x`/`y` are its
  coordinates, per the harness rows. The dimension check (3D code iff `z`
  is present) and the geographic-keys-reject-cartesian-code rule apply to
  the resolved code unchanged, and every other extra key stays ignored.

## Alternatives considered

- **Keep ignoring `crs` ([ADR-0167](./0167-point-accepts-map-like-entities.md)'s deferred state).** Rejected: the
  harness's `Spatial` and `TernaryEquality` rows require the name to
  select the CRS, and ignoring it makes points from different CRSs equal,
  so the ordering operators answer on incomparable values.
- **Derive the coordinate family from `crs` (wgs-84 requires
  `longitude`/`latitude`).** Rejected: the harness constructs
  `{x: 2.3, y: 4.5, crs: 'WGS-84'}` and expects 4326 with x = 2.3,
  y = 4.5, so the key spelling and the CRS name are independent.
- **Accept any string and fall back to the default srid on a typo.**
  Rejected: a misspelled CRS would silently produce a point in the wrong
  CRS — a wrong answer, not a lenient one — and Neo4j rejects unknown
  names.

## Consequences

- `Spatial` `[1]`/`[2]` and `TernaryEqualityAcceptance` gain their `crs`
  rows; `TernaryComparisonAcceptance`'s cross-CRS ordering comparisons
  return null (incomparable points).
- Maps that carry both `crs` and `srid` today (the cookbook's place
  locations do) keep their meaning while the two agree.
- `crs` is no longer an ignored extra property: a map that carried an
  unknown or non-string `crs` now fails construction.
