# 0153. `db.schema.visualization()` semantics

- **Status**: Accepted
- **Date**: 2026-09-30
- **Deciders**: CypherLite maintainers (Linear LABS-150)

## Context

browser.neo4j.io's Database information sidebar (LABS-150) calls
`CALL db.schema.visualization()` to draw the schema graph, so the procedure
shipped on the browser-sidebar branch (commit `42043f1c`) as the third
member of the `db.schema.*` family ADR-0112 opened. ADR-0112's Consequences
state that "any additional schema procedure or column needs a follow-up
ADR"; this is that follow-up, pinning the semantics the implementation
already has rather than proposing new ones.

ADR-0112 established the shared rule for the family: the schema is
**inferred by scanning stored entities**, not sourced from the declared
graph type spec, and procedures carry Neo4j-compatible signatures and
return columns with no CypherLite-specific additions.
`db.schema.visualization()` follows both rules; what needs pinning is the
shape of the virtual entities it returns and its empty-graph behavior,
because the returned `nodes`/`relationships` values flow back through the
Cypher value system (`startNode()`/`endNode()`, writes) where the
virtual-entity rules apply.

The implementation reuses the schema-visualization virtual-entity
machinery from `SHOW CURRENT GRAPH TYPE AS GRAPH`
(`cypherlite/cypherlite-executor/src/graph_type_visualize.rs`): a
thread-local side table mapping negative ids to full node values, consulted
by `startNode()`/`endNode()` as a fallback once the store lookup misses.

## Decision

`db.schema.visualization()` is implemented with the Neo4j-compatible
signature and return columns, no CypherLite-specific additions:

```
db.schema.visualization() ::
  (nodes LIST<NODE>,
   relationships LIST<RELATIONSHIP>)
```

### Semantics

- **Inferred by scanning stored entities** (ADR-0112's rule). The schema
  graph is derived from the entities actually stored; a declared graph type
  element that no stored entity exercises contributes nothing.
- **One virtual node per stored label.** The set of labels is the union of
  labels over all stored nodes; each contributes exactly one virtual node,
  sorted alphabetically.
- **One virtual relationship per distinct `(startLabel, relType, endLabel)`
  triple.** A stored relationship contributes one triple per label
  combination of its endpoints: a multi-label endpoint is expanded over
  every label it carries (Neo4j's shape). Triples are sorted by
  `startLabel`, `relType`, `endLabel`. Virtual relationships carry no
  properties.
- **Virtual entities carry negative ids**, allocated from `-1` down
  (nodes first, then relationships). Real, store-backed entities start at
  `0` and count up, so a negative id unambiguously marks "no real backing";
  writes against these entities follow the virtual-entity rules
  (`DELETE` / property `SET`/`REMOVE` are no-ops, label `SET`/`REMOVE` and
  `CREATE` off of one fail).
- **Virtual nodes are registered in the virtual-node side table**
  (`graph_type_visualize::register_virtual_node`), so
  `startNode()`/`endNode()` on a virtual relationship resolve its endpoints
  without a store lookup.
- **Node property shape is Neo4j's `{name, indexes, constraints}`.**
  `name` is the label itself; `indexes` and `constraints` are empty lists,
  documented placeholders (constraint rendering has its own catalog
  surface; index introspection is not part of this procedure).
- **No stored labels yields zero rows.** An empty graph — including a
  graph containing only unlabeled entities — returns no rows, because there
  is no label set to visualize.

## Alternatives considered

- **Neo4j's one-row-with-empty-lists empty behavior.** Neo4j returns one
  row with `nodes: []` and `relationships: []` when there is nothing to
  visualize. Rejected: zero rows is the more truthful Cypher answer for
  "there is no schema graph", it composes better with client code that
  treats the row count as the emptiness signal, and it matches how the
  sibling `db.schema.*` procedures behave on an empty store (ADR-0112
  emits rows only for categories that exist). Clients consuming the result
  iterate the row's two lists, so both spellings drive the same rendering
  path; the difference is observable only to callers counting rows.
- **Collapse multi-label endpoints to a single representative label** (or
  emit one triple per observed label-set pair rather than per label
  combination). Rejected: Neo4j expands over every label combination, and
  the sidebar draws one edge per triple; collapsing would draw a schema
  graph that does not match what `MATCH (:A)-[:R]->(:B)` can actually hit.
- **Populate `indexes`/`constraints` from the catalog.** Rejected: index
  and constraint introspection is a separate catalog surface with its own
  procedures; duplicating it here couples this procedure's contract to two
  unrelated ones and commits us to keeping three renderings of the same
  facts in sync. Empty placeholder lists keep the Neo4j property shape
  without that coupling.
- **Source the schema graph from the declared graph type spec.**
  Rejected for the same reason ADR-0112 rejected it for the property
  procedures: the spec describes what is permitted, not what exists, so it
  would report labels and triples absent from the data and miss inferred
  shapes the declaration does not cover.

## Consequences

- ADR-0112's "follow-up ADR for any additional schema procedure"
  requirement is satisfied for `db.schema.visualization()`; the procedure
  is no longer an undocumented extension of the ADR-0112 surface.
- The virtual-entity contract (negative ids from `-1` down, side-table
  registration, virtual-entity write behavior) is pinned: changing it, or the
  zero-rows empty behavior, requires a new ADR.
- `startNode()`/`endNode()` correctness for these results depends on the
  thread-local side table; the table is per-thread, so virtual
  node resolution is valid within the query execution that produced them.
- Schema inference costs a scan of stored nodes and relationships at call
  time; there is no cached or declared-schema fast path (same cost model
  as ADR-0112's procedures).

## Later references

- **ADR-0166** reverses this ADR's rejected "Populate
  `indexes`/`constraints` from the catalog" alternative: the virtual
  nodes' `indexes`/`constraints` lists are now populated from the catalog
  (the "empty documented placeholders" bullet under Decision is
  superseded), because the graph-examples differential suite (LABS-172)
  flagged the empty lists as a mismatch against the Neo4j reference.
  Every other decision here — inferred schema, one virtual node per
  label, triple expansion, negative virtual ids, zero-rows empty
  behavior, property-less virtual relationships — remains in force.
- **ADR-0187** supersedes this ADR's "Virtual relationships carry no
  properties" sentence: `db.schema.visualization()` virtual relationships
  now carry a single `name` property equal to the relationship type,
  matching Neo4j (LABS-185). Every other decision here — inferred schema,
  one virtual node per label, triple expansion, negative virtual ids,
  zero-rows empty behavior — remains in force.
