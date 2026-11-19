---
type: adr
area: surface
name: 0214-python-result-graph
status: accepted
tags:
  - python
title: "`Cursor.graph` — de-duplicated result-graph extraction"
REFERENCES:
  - "[[adr/0213-python-driver-result-ergonomics]]"
SUPERSEDES:
  - "[[adr/0213-python-driver-result-ergonomics]]"
---

# ADR-0214: `Cursor.graph()` — de-duplicated result-graph extraction

## Context

LABS-253 lists `result.graph()` (the Neo4j driver's `Result.graph()`) as a
gap. [ADR-0213](./0213-python-driver-result-ergonomics.md) deferred it because `cypherlite.viz.to_visualization_graph`
already walks result rows and collects graph entities — but that path
converts straight to a `neo4j_viz.VisualizationGraph`, needs the optional
neo4j-viz dependency, and cannot be used by callers who want the extracted
entities themselves (analysis, re-insertion, custom rendering). The
Stack Overflow notebook carries its own ad-hoc extraction for the same
reason.

## Decision

Add `Cursor.graph()` to the DB-API surface: it walks **all rows of the
cursor's result** (rows are materialized at execute time, so the fetch
position is irrelevant — unlike the driver, which builds from records
consumed so far), recursively collects every `Node`, `Relationship` and
`Path` found as a column value or nested in a list/tuple/set/map, and
returns a `cypherlite.ResultGraph` with:

- `nodes` — the unique `cypherlite.Node` entities, ordered by id
- `relationships` — the unique `cypherlite.Relationship` entities, ordered by id

De-duplication is by entity id (first occurrence wins, like
`to_visualization_graph`). A result with no graph entities returns an empty
`ResultGraph`, not an error — extraction is a pure function of the rows.
`ResultGraph` is a plain Python class in `cypherlite.result_graph`, no
optional dependency; `viz.to_visualization_graph` is refactored to reuse the
same walk so the two cannot drift.

The native `QueryResult` does not gain `graph()` in this change: its `data`
property yields plain dicts (graph entities arrive as dicts there, not as
`Node`/`Relationship` objects), so extraction would need a dict-shape walk
in addition to the object walk. That stays future work.

**neo4j-viz interop.** `neo4j_viz.neo4j.from_neo4j` gates on
`isinstance(data, neo4j.Result)` (also `neo4j.graph.Graph`, `EagerResult`,
`Driver`), so duck typing alone cannot get a CypherLite cursor in. The
supported path is our own wrapper, `cypherlite.viz.to_visualization_graph
(result)`, which accepts a `Cursor` (or `QueryResult`, or plain rows)
directly, needs only neo4j-viz — not the `neo4j` driver — and shares the
same extraction walk as `graph()`. A `neo4j.Result`-subclassing adapter
(`to_neo4j_result`) was implemented and then dropped: it would couple to
`from_neo4j`'s current internals (which happen to call only `.graph()`),
require the `neo4j` driver package, and duplicate what the wrapper already
does. `Node` and `Relationship` still gain the driver-parity members
`element_id` (the engine id, stringified — the driver's is a string) and
`items()` (the property pairs); `Relationship.start_node`/`end_node` stay
integer element ids, with `start`/`end` as short aliases (a CypherLite
convenience — the driver itself has only the long forms) for
edge-list-style consumers.

## Alternatives considered

- **Keep viz as the only extraction path** ([ADR-0213](./0213-python-driver-result-ergonomics.md)'s stance) — rejected:
  it makes the optional neo4j-viz dependency a prerequisite for reading
  back graph entities, and every consumer (the notebook) re-implements the
  walk anyway.
- **Return a `neo4j_viz.VisualizationGraph` directly** — rejected: couples
  the core DB-API surface to an optional dependency and a viz-specific
  shape (captions, coloring).
- **Driver-exact semantics (consumed records only, set-typed `Graph`)** —
  rejected: CypherLite cursors materialize all rows up front, so "consumed
  so far" would be an artificial restriction; and id-ordered lists are
  deterministic where sets are not.
- **Return plain dicts like the native path** — rejected: the DB-API path
  already yields rich `Node`/`Relationship` objects; returning dicts would
  lose ids and topology the caller asked for.

## Consequences

- `Cursor.graph()` is a new public API item: spec, description, example and
  the skill-bundle/website references update in the same change (this ADR).
- `viz` loses its private walk (moved to `cypherlite.result_graph`); its
  behavior is unchanged.
- [ADR-0213](./0213-python-driver-result-ergonomics.md)'s "not in scope" note on `graph` is superseded by this ADR.
- If the native `QueryResult` later yields rich objects, `graph()` can move
  down to shared code — a follow-up, not a promise.
