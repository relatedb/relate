---
type: adr
area: surface
claim: 9c3b77d7a9c93831cf399fb906d320c5b3454fd1
name: 0326-bolt-point-results-as-packstream-structs
status: proposed
tags:
  - bolt
  - values
title: Bolt point results encode as PackStream Point2D/Point3D structures
REFERENCES:
  - "[[adr/0177-bolt-parameter-conversion]]"
  - "[[adr/0272-bolt-path-struct]]"
SUPERSEDES:
  - "[[adr/0272-bolt-path-struct]]"
---

# ADR-0326: Bolt point results encode as PackStream Point2D/Point3D structures

## Context

[ADR-0272](./0272-bolt-path-struct.md) kept "Point arms remain string encodings": a point result is sent
as its Cypher literal, for example `"point({srid: 4326, x: 13, y: 55.6})"`.
[ADR-0177](./0177-bolt-parameter-conversion.md) decodes the `0x58` Point2D and `0x59` Point3D parameter
structures, so a point parameter goes in as a structure and comes back as
a string.

The Bolt 4.4 probes against Neo4j 2026.09.0 (LABS-323) show Neo4j sends
point structures with Float coordinates: F-59 `0x58(7203, 1.5, 2.5)` and
`0x58(4326, 13.0, 55.6)`, F-60 `0x59(9157, 1.0, 2.0, 3.0)`. A whole-number
coordinate is still a Float (`13.0`). The one-time neo4rs 0.9.0-rc.10
probe gets a string and fails typed reads of `Point2D`/`Point3D` (D-16,
D-17); the parameter echoes D-33/D-34 come back as strings.

The engine's `PointValue` holds `srid: i32`, `x`, `y` and an optional `z`,
all `f64`.

## Decision

`Value::Point` results encode as PackStream structures, at any nesting
depth, never as strings:

| Engine value | Tag | Fields |
| --- | --- | --- |
| `PointValue { z: None, .. }` | `0x58` Point2D | `[srid: Int, x: Float, y: Float]` |
| `PointValue { z: Some(z), .. }` | `0x59` Point3D | `[srid: Int, x: Float, y: Float, z: Float]` |

Coordinates are always PackStream Floats, bit for bit, including whole
numbers and non-finite values. The SRID is the stored one (4326, 4979,
7203 or 9157). These are the exact inverses of [ADR-0177](./0177-bolt-parameter-conversion.md)'s decoders, so a
point parameter echoes back as the same structure.

## Alternatives considered

- **Keep the Cypher literal string ([ADR-0272](./0272-bolt-path-struct.md)).** No change for clients
  that parse it. Rejected: drivers cannot hydrate a spatial type from a
  string, neo4rs typed reads fail, and point parameters do not round trip.
- **Send whole-number coordinates as Int.** Slightly shorter on the wire.
  Rejected: the Bolt layout types the coordinates as Float, Neo4j sends
  `13.0`, and drivers that check the field type would reject an Int.
- **Choose 2D or 3D from the SRID instead of `z`.** Rejected: the engine
  already ties `z` to the SRID's dimension, and reading `z` keeps the
  encoder total over every `PointValue`.

## Consequences

- Drivers hydrate point results as their spatial types
  (`neo4j.spatial.CartesianPoint`/`WGS84Point` in Python,
  `Point2D`/`Point3D` in neo4rs).
- [ADR-0272](./0272-bolt-path-struct.md)'s "Point arms remain string encodings" is superseded and gets
  a Later references note. Vector results stay strings.
- `docs/api/bolt/spec.md`'s result structure table gains the two rows.
- `tests/compat/scripts/values/point-structs.bolt` and unit tests in
  `value_structs_tests.rs` pin the encoding and one parameter echo.
