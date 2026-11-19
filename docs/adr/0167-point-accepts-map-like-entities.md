---
type: adr
area: cypher
date: 2026-10-06
deciders: CypherLite maintainers
name: 0167-point-accepts-map-like-entities
status: accepted
tags:
  - functions
  - semantics
title: "`point` accepts map-like entity arguments"
REFERENCES:
  - "[[adr/0445-point-crs-key-names-the-srid]]"
---

# 0167. `point()` accepts map-like entity arguments

## Context

The `pole.adoc` walkthrough calls `point(l)` / `point(x)` with `Location`
nodes. CypherLite failed with `type error: point() requires a map` because
`point_construct` in `cypherlite-executor/src/spatial.rs` only accepted
`Value::Map`; Neo4j accepts the call. The map path itself also diverged
from Neo4j: key selection treated *any* `latitude`/`longitude` key as
geographic (so `{x, y, latitude, longitude}` went geographic instead of
cartesian), a non-integer `srid` was silently ignored, unknown srid codes
were accepted, srid/dimension mismatches were not checked, and geographic
ranges (latitude bounds, longitude wrap) were not enforced.

The semantics below were verified by direct probe against
`neo4j:2026.09-community` in docker.

## Decision

`point()` accepts **maps, nodes, and relationships**. Nodes and
relationships are map-like arguments: `bi_point` resolves them exactly like
`properties()` — `reject_deleted_access` first (a deleted entity errors
with `DeletedEntityAccess`), then fresh properties via `graph.node` /
`graph.rel`; a missing entity yields an empty map, which then errors on the
missing coordinates. `point(null)` is null; any other scalar is a type
error naming map/node/relationship.

Maps and entities alike share one construction core with the verified
Neo4j rules:

- **Key selection**: cartesian iff both `x` and `y` are present; else
  geographic iff both `latitude` and `longitude` are present; else an
  error. `height` is ignored for cartesian points; for geographic points
  `z` wins over `height` when both are present. Extra properties (e.g.
  `address`, `postcode`) are ignored. Geographic coordinates map
  longitude → x and latitude → y.
- **Coordinate values** must be floats or integers; anything else (e.g. a
  string) is an error.
- **`srid`**: absent → default by family and dimension (geographic 2D
  4326, geographic 3D 4979, cartesian 2D 7203, cartesian 3D 9157).
  Present → must be an integer (a float like `4326.0` or a string is an
  error), must be one of {4326, 4979, 7203, 9157}, and must match the
  coordinate dimension (2D srid with `z`, or 3D srid without `z`, is an
  error). Geographic keys with a cartesian srid are an error; cartesian
  keys accept any of the four srids, dimension-checked.
- **Ranges (geographic)**: latitude must be in [-90, 90]; longitude wraps
  into [-180, 180] keeping both endpoints (180.0 → 180.0, -180.0 →
  -180.0, 180.5 → -179.5, -200.0 → 160.0).

The output shape stays `Value::Point(PointValue { srid, x, y, z })`.

**Deferred**: Neo4j also reads a `crs` key (e.g. `crs: 'wgs-84'`) from
maps and entities. Supporting `crs` is out of scope here; the key is
ignored like any other extra property.

## Alternatives considered

- **Keep rejecting non-map arguments and fix the walkthrough instead.**
  Rejected: `point(node)` is documented Neo4j behavior, the reference
  accepts the call, and the walkthrough is the product's own teaching
  material — the engine, not the docs, was wrong.
- **Resolve entities to maps in the planner/front-end rather than in
  `bi_point`.** Rejected: function arguments are evaluated as expressions
  long before `bi_point` runs; the executor is where `Value::Node` /
  `Value::Relationship` and the `ExecGraph` meet, and `bi_properties`
  already establishes the exact resolution pattern (including
  deleted-entity rejection) to mirror.
- **Read entity properties from the `Value::Node` snapshot instead of
  re-fetching through the graph.** Rejected: a bound entity can carry
  stale properties (or none, after delete); `properties()` re-fetches via
  `graph.node`/`graph.rel`, and `point()` must agree with it.
- **Keep the lenient srid handling (ignore non-integers, accept unknown
  codes).** Rejected: silently ignoring `srid: 4326.0` produces a point
  with the *default* srid for the key family — a wrong answer, not a
  lenient one. Neo4j errors; so do we.
- **Implement `crs`-key support in the same change.** Rejected: it is a
  separate feature (string CRS names, alias resolution, srid/crs conflict
  rules) with no current user need; the probe-verified rules above are
  complete without it. Recorded here as deferred so a later ADR can add
  it.

## Consequences

- `bi_point` in `cypherlite-executor/src/eval/function/spatial.rs` resolves
  `Value::Node` / `Value::Relationship` through the graph exactly like
  `bi_properties`; `point_construct` keeps the `Value::Map` and
  `Value::Null` paths.
- `point_construct`'s core in `cypherlite-executor/src/spatial.rs`
  implements the verified rules for maps and entities alike; unit tests
  sit beside it, entity-resolution unit tests in `eval/function/spatial.rs`, and
  end-to-end coverage (including the pole-walkthrough shape and
  deleted-entity rejection) in `cypherlite/src/tests/queries.rs`.
- Behavior change for previously accepted map forms: non-integer or
  unknown `srid`, srid/dimension mismatch, out-of-range latitude, and
  mixed `{x, latitude, longitude}` key sets now error or resolve
  differently. These all produced wrong or unverified answers before;
  pre-1.0 this lands as a fix, recorded in the changelog.
- A future `crs`-key ADR must extend the shared core, not add a parallel
  path.

## Later references

[ADR-0445](./0445-point-crs-key-names-the-srid.md) adds the deferred `crs`-key support: `crs` is the name spelling
of the same four-code `srid` domain, read by the shared core in
`spatial.rs` (not a parallel path). The construction rules above are
unchanged; an unknown or non-string `crs` is an error, and a `crs`/`srid`
pair must agree.
