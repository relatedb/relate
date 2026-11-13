# 0187. `db.schema.visualization()`: virtual relationships carry a `name` property

- **Status**: Accepted
- **Date**: 2026-10-08
- **Deciders**: CypherLite maintainers (Linear [LABS-185](https://linear.app/neo4j/issue/LABS-185))

## Context

LABS-185: the graph-examples differential suite (ADR-0144) compares
cypherlite's output against a Neo4j reference on the public graph-examples
walkthroughs. The nightly run
(https://github.com/neo4j-labs/cypherlite/actions/runs/37567417871, commit
`e110fefe`) diffs the stackoverflow dataset's walkthrough query at
`stackoverflow.adoc:40`, a `CALL db.schema.visualization()`. Neo4j renders
each virtual relationship with a `name` property —

```
{":TYPE":"COMMENTED","name":"COMMENTED"}
```

— while cypherlite renders the same relationship property-less:

```
{":TYPE":"COMMENTED"}
```

(`":TYPE"` is the suite's normalization marker for the relationship type,
not a stored property.)

The target rendering was already recorded in ADR-0166's Docker probe of
`neo4j:2025-enterprise`:

```
relationships: [[:TRANSFER_TO {name: "TRANSFER_TO"}]]
```

ADR-0153 gave virtual nodes Neo4j's `{name, indexes, constraints}`
property shape but chose "Virtual relationships carry no properties"
deliberately; ADR-0166 reaffirmed that split in its Decision item 5
("Virtual relationships keep carrying no properties"). That leaves the
relationship half out of step with Neo4j and with clients that label edges
by `name` (Browser/Bloom-style schema views), so LABS-185 asks for the
relationship property shape to match too. This ADR is the follow-up that
supersedes exactly that point.

## Decision

Every virtual relationship returned by `db.schema.visualization()` carries
a property map with **exactly one entry**:

- `name`: a `STRING` equal to the relationship's type — `type(r)` — for
  every relationship `r` in the result's `relationships` list.

Reading `r.name` off the returned value yields the type, so `type(r)` and
`r.name` agree on the same virtual relationship.

Everything else in ADR-0153, as amended by ADR-0166, is unchanged:

- **Virtual node shape** stays Neo4j's `{name, indexes, constraints}`, with
  `indexes`/`constraints` populated from the catalog at call time
  (ADR-0166).
- **Negative virtual ids** stay allocated from `-1` down (nodes first,
  then relationships), and virtual nodes stay registered in the
  virtual-node side table so `startNode()`/`endNode()` resolve endpoints.
- **Triple expansion** stays: one virtual relationship per distinct
  `(startLabel, relType, endLabel)` triple, with multi-label endpoints
  expanded over every label combination.
- **Sort order** stays: nodes sorted alphabetically by label;
  relationships sorted by `startLabel`, `relType`, `endLabel`.
- **Zero-rows empty behavior** stays: a graph with no stored labels
  returns no rows.

The `SHOW CURRENT GRAPH TYPE AS GRAPH` producer is untouched: its
virtual relationships keep their own property shapes
(`{relationshipType, properties, constraints}` and friends), which are
pinned by that surface's own scenarios.

## Alternatives considered

- **Keep property-less virtual relationships (the ADR-0153/ADR-0166
  status quo).** Rejected: the differential-suite mismatch against the
  Neo4j reference is the bug being fixed (LABS-185); every schema view of
  a graph containing relationships diverges from the oracle, and clients
  that label edges by `name` render unlabeled edges.
- **Add further properties beyond `name`** (copy stored relationship
  properties onto the virtual relationship, or add that view's
  `properties`/`constraints` lists). Rejected: Neo4j's virtual
  relationship carries exactly `{name}`; any extra key re-creates the
  mismatch, and there is no stored-entity fact for a virtual relationship
  to expose — it is a schema triple, not a stored relationship, so copied
  values would be arbitrary (which stored relationship's?) or invented.
  The richer property shape belongs to that producer, not this
  one.
- **Widen the shared virtual-entity machinery so every producer emits
  `name`** (including `SHOW CURRENT GRAPH TYPE AS GRAPH`). Rejected:
  the graph-type view's relationship shapes are pinned by its own
  scenarios (`testdata/cip-eval/` visualization suites); the
  differential mismatch is scoped to `db.schema.visualization()` only,
  and changing a surface the differential suite does not compare would
  be an unrelated behavior change.

## Consequences

- Schema-visualization write rules still apply to these virtual
  relationships: `SET r.name = ...` / `REMOVE r.name` on a virtual
  relationship is a no-op (the write never reaches a store row), matching
  the "Write behavior on virtual entities" scenarios; only reads of the
  returned value see the new property.
- `properties(r)`/`keys(r)` on virtual entities still read the store by
  id and return empty (`{}` / `[]`), so they do not report the new `name`
  property. That is a **pre-existing gap** (the builtins resolve by id
  rather than reading the inline value) and is **out of scope** here;
  closing it needs its own decision.
- ADR-0153's "Virtual relationships carry no properties" sentence and
  ADR-0166's Decision item 5 are **superseded** by this ADR; **Later
  references** notes on both record it. Every other decision in both ADRs
  stays in force.
- `docs/reference/cypher-syntax.md`'s `db.schema.visualization()`
  paragraph (which documents property-less virtual relationships) must
  change in the same feature.
- The graph-examples differential suite (ADR-0144) baseline gains the
  `stackoverflow.adoc:40` expectation; the reported mismatch is removed.
