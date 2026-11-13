# 0226. Pin the ontology mappings: document to graph, ontology to graph type

- **Status**: Accepted
- **Date**: 2026-10-12
- **Deciders**: CypherLite maintainers (Linear LABS-132)

## Context

ADR-0120 §1 makes the validated ontology document the source of truth and
derives the `system_ontologies` graph from it; §5 carries the document as
one immutable snapshot object, so the store formats never see the mapping.
What the mapping *is* was deferred: ADR-0121 §5 decided that the
document-to-graph mapping and the ontology graph type would implement
Neo4j's published mapping, that CypherLite ships no mapping of its own, and
that implementing the procedures waits until Neo4j publishes it; ADR-0121 §1
defined `db.ontology.apply` (graph-type conversion with `ALTER CURRENT
GRAPH TYPE SET` semantics, failing when the data violates the resulting
graph type) without saying how that failure is produced.

Neo4j's mapping is still unpublished — the persistence proposal's
JSON-to-graph and graph-to-graph-type sections remain TODO — while the
Ontology Graph Spec v1 itself has converged: the proposal (2026-10-12
state) and its pinned draft schema (`neo4j/graph-spec` @
`2f8dcbdf2dab51ebf2d760e00904a8f2b0518f59`) fix the document shape —
nodes/relationships keyed by local ids, `label`/`labels.identifier`,
property type tokens, constraint objects plus shorthand flags, endpoint
cardinality, the extensions envelope — and an explicit
enforceable-vs-descriptive table ("Enforcement mapping"). CypherLite's
implementation of ADR-0120/ADR-0121 (LABS-132) is designed to feed back
into Neo4j's WS2 storage work, and because the graph is derived, adopting a
different Neo4j mapping later costs no data migration (ADR-0120
Consequences). Waiting buys nothing.

One more gap: today's SET path never validates existing data.
`sync_derived_constraints`
(`cypherlite/cypherlite-executor/src/exec/ddl.rs:324`) layers a
schema-equivalence skip in front of `create_constraint_core`
(`ddl.rs:196`), which inserts catalog rows and backing indexes without
looking at data, and `alter_current_graph_type`
(`cypherlite/cypherlite-executor/src/exec/catalog_schema.rs:449`) adds
nothing else. `db.ontology.apply` must fail when existing `neo4j` data
violates the resulting graph type (ADR-0121 §1), so the mechanism needs
deciding.

## Decision

### 1. Document → `system_ontologies` graph

Every node and relationship in the derived graph carries the property
`ontology :: STRING` with the value `'neo4j'` (ADR-0121 §5), naming the
database the stored ontology belongs to.

The graph mirrors the document as written, one graph element per document
construct:

| Document construct | Graph element | Properties (besides `ontology`) |
|---|---|---|
| document root | `(:Ontology)` | `schema`, `id`, `version`, `name`, `description` |
| `nodes` entry | `(:OntologyNode)` | `key` (the map key), `label` (the identifying label: `label`, or `labels.identifier`), `implied` (list, from `labels.implied`), `optional` (list, from `labels.optional`), `description`, `aliases` (list), `reference` |
| `relationships` entry | `(:OntologyRelationship)` | `key`, `type`, `from_count`, `from_min_count`, `from_max_count`, `to_count`, `to_min_count`, `to_max_count` (integers), `description`, `aliases`, `reference` |
| property of a node or relationship entry | `(:OntologyProperty)` | `name` (the map key), `type` (the token), `dimension` (integer), `mustExist`, `unique`, `key` (booleans), `one_of` (canonical JSON string), `pattern`, `description`, `aliases`, `reference` |
| constraint object | `(:OntologyConstraint)` | `constraint_type` (`key` \| `unique` \| `mustExist`), `name` |
| named extension | `(:OntologyExtension)` | `key` (e.g. `neo4j:index`), `custom: false`, `payload` (canonical JSON string of the value) |
| custom extension (envelope) | `(:OntologyExtension)` | `key: 'custom'`, `custom: true`, `type`, `schema`, `name`, `definition` (canonical JSON string) |

Absent optional fields produce absent graph properties (the spec's "absent
means none"). The shorthand flags stay flags: they are boolean properties
on `:OntologyProperty`, and only explicit constraint objects become
`:OntologyConstraint` nodes. Arbitrary JSON subtrees (`one_of` entries,
named-extension payloads, custom `definition` values) are stored as
canonical JSON text — compact, object keys sorted — so a JSON and a YAML
submission of the same logical document derive the same graph.

Relationships in the derived graph:

- `(:Ontology)-[:HAS_NODE]->(:OntologyNode)` and
  `(:Ontology)-[:HAS_RELATIONSHIP]->(:OntologyRelationship)`;
- owner `-[:HAS_PROPERTY]->(:OntologyProperty)`,
  `-[:HAS_CONSTRAINT]->(:OntologyConstraint)` and
  `-[:HAS_EXTENSION]->(:OntologyExtension)`, where the owner is the root, a
  node entry, a relationship entry or (for extensions) a property;
- `(:OntologyRelationship)-[:FROM]->(:OntologyNode)` and `-[:TO]->`,
  resolving `from.node` / `to.node` to the entry with that `key`;
- `(:OntologyConstraint)-[:CONSTRAINS]->(:OntologyProperty)` for each
  listed property.

**Deterministic ids.** Node ids are assigned 0, 1, 2, … in a fixed
traversal: the root first; then each `nodes` entry in document order — the
entry node, then its property, constraint and extension nodes, each group
in document order (`custom` entries in list order at the position of the
`custom` key); then each `relationships` entry likewise; then the root's
extension nodes. Relationship ids are assigned 0, 1, 2, … in creation
order: each element's `HAS_*` and `CONSTRAINS` edges are created with its
child node, and a relationship entry's `FROM`/`TO` edges when its node is
created (node entries always exist by then, `nodes` being traversed first).
The build is a pure function of the parsed document with source key order
preserved, so an unchanged document yields the same graph with the same
element ids (ADR-0116 §7) across restarts, and ids change only when the
ontology is replaced (ADR-0120 §2).

**Fixed graph type.** The derived graph carries a fixed graph type
— an engine constant in `cypherlite/cypherlite-ontology` — describing the
shapes above: the six labels with their mandatory properties `NOT NULL`
(`key`, `label`, `type`, `name`, `constraint_type` where the table lists
them, and `ontology` everywhere), and the seven relationship types with
their endpoints. It is installed as the namespace's current graph type
whenever the derived graph is built (ADR-0120 §2), so `SHOW CURRENT GRAPH
TYPE` on `system_ontologies` describes the ontology model. Because the
namespace is read-only to Cypher (ADR-0116 §1/§4), the type never gates
writes; it is descriptive and introspectable.

### 2. Ontology → graph type (`db.ontology.apply`)

The split follows the spec's enforcement table. **Enforceable** fields
compile into the graph-type body:

- Node entries: identifying label from `label` or `labels.identifier`,
  implied labels from `labels.implied` — the graph-type
  `(:<Ident> => :<Implied&…> { … })` form, with the local id as the element
  alias where endpoint references need it.
- Property `type` tokens map to the graph-type value type of the same name:
  bare scalars as-is, `LIST<X>` to the corresponding list type, `LIST<ANY>`
  to `LIST<ANY>`, `VECTOR<INTEGER|FLOAT>` to the vector type, with
  `dimension` as its dimension when present (absent = unconstrained).
  `type: ANY` or an absent `type` declares nothing — the property is
  unconstrained, as the spec prescribes for `ANY`.
- `mustExist: true` → the declaration is `NOT NULL` (or a standalone
  `REQUIRE <el>.<p> IS NOT NULL` graph-type constraint when the property
  declares no type). `unique: true` / `key: true` → single-property unique
  / key constraints, `key` implying `unique` + `mustExist` per the spec.
- Constraint objects → graph-type constraints of the same kind over the
  listed properties — composite when the list has more than one entry —
  keeping the object's `name` when present.
- Relationship entries → one edge type spec per entry,
  `(<from>)-[:<TYPE> => { … }]->(<to>)`, endpoints resolved through the
  referenced node entries' identifying labels. Entries sharing a `type`
  across endpoint pairs stay distinct specs; graph-type enforcement keys on
  the type alone, the spec's documented caveat.

**Descriptive** fields are never applied: `description`, `aliases`,
`reference`, `labels.optional`, endpoint `count`/`min_count`/`max_count`,
`one_of`, `pattern`, every extension (named and custom), and the document
metadata (`schema`, `id`, `version`, `name`, `description`). They remain
queryable in `system_ontologies` through the mapping in (1).

### 3. How apply fails on violating data

Like all four procedures (ADR-0227, following the
ontology-privileges proposal),
`db.ontology.apply` takes no database argument: it reads the stored
ontology of, and sets the graph type on, the database it is executed
against — the session database or the `USE` target — which ADR-0121 §2's
rule restricts to `neo4j` (`system` and `system_ontologies` reject the
procedures before this point). It validates first, then commits:

1. It builds the resulting graph-type body through the mapping in (2) and
   derives its constraint set with the same functions the `ALTER CURRENT
   GRAPH TYPE SET` path uses (`apply_verb`, `derive_constraints_typed`).
2. Before any catalog redo is pushed, it scans the calling transaction's
   captured `neo4j` snapshot (ADR-0116 §5) against every derived
   constraint: key constraints check presence and uniqueness; unique
   constraints check uniqueness (absent properties exempt, as in Cypher);
   existence (`NOT NULL` / `mustExist`) checks presence; property-type
   constraints check that every present value conforms, including vector
   dimensions; endpoint (`sourceLabel`/`targetLabel`) constraints check
   that every relationship of the type touches the identifying labels.
3. The first violation fails the statement with a data-violation error
   naming the constraint and the offending element's element id. No
   `SetCurrentGraphType` redo is written and no catalog change commits; in
   an explicit transaction (ADR-0121 §3) the failure is a statement error
   and the transaction's other work is untouched.
4. Only on success does the existing SET path run: the SET purge,
   `sync_derived_constraints`, and the `RedoOp::SetCurrentGraphType` push
   (`TheaterData::set_current_graph_type` on commit). Validation and the
   catalog write are computed from the same captured snapshot and commit
   atomically under the store's normal commit protocol.

The mapping itself (document model → graph-type body) lives in
`cypherlite/cypherlite-ontology` (ADR-0121 §6); the snapshot scan lives in
`cypherlite-executor` next to `alter_current_graph_type`, the only layer
that can see the data.

## Alternatives considered

- **Wait for Neo4j's published mapping** (the ADR-0121 §5 stance).
  Rejected: the mapping is still TODO in Neo4j's persistence proposal, and
  CypherLite's mapping is designed as input to Neo4j's WS2 work rather than
  a copy of its output. Derivation makes a later alignment migration-free,
  so waiting only delays LABS-132.
- **Persist the derived graph instead of deriving it.** Rejected: ADR-0120
  already weighed this — per-namespace shards, namespace-tagged WAL
  records, and rebuilding the document on `get` — and the graph stays a
  pure function of the document.
- **Materialize the shorthand flags as constraint nodes.** Rejected: the
  graph mirrors the document as written; expanding the flags is apply's
  job, through the spec's documented shorthand-to-object equivalence.
- **Merge same-type relationship entries into one edge type spec.**
  Rejected: the document keys entries by local id precisely to keep
  endpoint pairs distinct; merging would lose the structure `SHOW CURRENT
  GRAPH TYPE` should reflect.
- **Apply via `CREATE CONSTRAINT` redo ops instead of the graph-type
  path.** Rejected: apply is defined with SET semantics (ADR-0121 §1), and
  derived constraints must stay distinguishable from named ones in the
  theater (`TheaterData.derived_constraints`).
- **Make SET itself validate existing data instead of adding an apply-only
  scan.** Rejected: SET's non-validating behavior matches Neo4j, and
  changing it would break the graph-type corpus; the scan belongs to apply
  alone.

## Consequences

- Queries against `system_ontologies` get a stable, documented shape, and
  element ids for an unchanged document are stable across restarts
  (ADR-0116 §7).
- The mappings are CypherLite's own. If Neo4j publishes a different
  mapping, aligning is a new ADR plus a Later-references note here, and —
  per ADR-0120 — needs no data migration: only the derived graph changes.
  Queries written against this shape may then need updating.
- `apply` becomes the one graph-type path that validates existing data; SET
  stays non-validating. The scan is covered by tests over violating
  fixtures (presence, uniqueness, property type, endpoints).
- The enforceable/descriptive split is documented on the procedures'
  reference page: descriptive fields are queryable but never enforced.
- The mapping covers exactly the pinned schema's grammar; a future spec
  construct it cannot express arrives with a new vendored schema and its
  own ADR (ADR-0227 §5).
- ADR-0120 and ADR-0121 gain Later references notes.

## Later references

- ADR-0228 corrects §2's list-token mapping: `LIST<X>` maps to the closed
  `LIST<X NOT NULL>` (graph-type property types have no nullable inner
  type) and `LIST<ANY>` declares nothing, having no graph-type property-type
  counterpart. The rest of §2 stands.
- ADR-0229 removes §1 from the shipped crate: the
  document→`system_ontologies` mapping, its deterministic build and its
  fixed graph type are deleted — the namespace is listed but not
  queryable, and no graph is derived (superseding ADR-0120 §2). §1 stands
  as the design record, **not shipped** until Neo4j publishes its
  mapping; revive it from git history then, with a new ADR and a note
  here. §2 (with ADR-0228's correction) and §3 — the `db.ontology.apply`
  converter and its data-violation check — remain shipped.
- ADR-0364 re-bases §2's converter on main's document shapes: id-keyed
  constraint maps with an uppercase `type`, `must_exist`, display
  `name` fields, `RelationshipTarget` endpoints and the 39-token
  `Neo4jType` enum. The enforceable/descriptive split, the
  key/unique/must_exist rules and the object-replaces-flag rule stand.
  `PROPERTY_TYPE`, plain `FLOAT32`/`INTEGER8`/`INTEGER16`/`INTEGER32`,
  label-only or empty endpoints, empty property names that would be
  declared and empty constraint property lists fail loudly at `apply`;
  `UUID` and the vector coordinate types map to their graph-type forms.
- ADR-0487 re-bases §2 again on the published 1.1.0 shapes: constraints
  of both owners are one closed shape with no `label`, so a constraint
  always compiles against its owning element; node and relationship
  display `name`s no longer exist; property `aliases` returns as a
  descriptive member. Everything the ADR-0364 note above records
  otherwise stands.
