# Ontology Procedures Reference

The `db.ontology.*` procedures manage the store's single ontology document:
one Ontology Graph Spec document for the user database `neo4j`, stored
verbatim — the sole stored source of truth; no graph is derived from it, and
the `system_ontologies` namespace is visible only under the same
`RELATE_PREVIEW=ontology` opt-in (ADR-0593) and not queryable
([ADR-0116](../adr/0116-fixed-namespaces-community-edition-semantics.md),
[ADR-0120](../adr/0120-ontology-document-persistence.md),
[ADR-0121](../adr/0121-ontology-procedures.md),
[ADR-0226](../adr/0226-ontology-mappings.md),
[ADR-0227](../adr/0227-ontology-document-format.md),
[ADR-0228](../adr/0228-ontology-list-property-types-closed.md),
[ADR-0229](../adr/0229-remove-system-ontologies-cypher-surface.md),
[ADR-0364](../adr/0364-ontology-spec-main-gate.md),
[ADR-0378](../adr/0378-ontology-procedures-preview-gate.md)). The
signatures follow Neo4j's CIP-287 surface: no database argument — each
procedure acts on the database it is executed against (the session database
or the `USE` target). The procedures' code ships in every build: all CLI
bundles, the Rust crate, the language bindings, Bolt and the WASM artifact
(ADR-0121 §6); only callability is gated, because calling them requires the
`ontology` preview opt-in (ADR-0378).
Once the opt-in is set, any caller may run every procedure — there is no privilege check (ADR-0121 §4).
Enabling the preview on a served store — `relate serve` exposing Bolt,
HTTP or MCP — exposes the four procedures, including the store-level
writers (`set` and `drop`), to every connected client.
`SHOW PROCEDURES` lists all four with their signatures and modes once the
opt-in is set, and omits them while it is not (see
[Preview gate](#preview-gate)).
Always-on cost, measured at landing (ADR-0121 §6): the release WASM bundle
(`scripts/build-wasm.sh --mode release`, wasm-opt -Oz) grew from 4,741,408
to 4,929,088 bytes (+187,680 bytes, +4.0%).

## Preview gate

The four procedures require `RELATE_PREVIEW=ontology` (ADR-0378) — the
one procedure family ADR-0371's registry gates without extending syntax,
because its contract tracks a non-final external spec. `Database::open`
resolves the comma-separated list once; unset or empty means off, and an
unknown name is a fail-closed open error. While the feature is off, every
`db.ontology.*` call fails with:

```text
semantic error: SemanticError: preview feature 'ontology' is not enabled: set RELATE_PREVIEW=ontology
```

`SHOW PROCEDURES` omits the four rows while the feature is off — in every
composed form — and lists them with unchanged signatures and modes once it
is on, so discovery never advertises a procedure that cannot be called. The
gate governs callability, not stored state: a store holding an ontology
still opens, replays (including `MDBWAL02` segments), checkpoints and
reloads with the gate off, and the stored document is served again once the
store is reopened with the opt-in. WASM builds have no process environment,
so they cannot enable the feature; the procedures are unreachable there
until a programmatic opt-in exists. The code itself still ships in every
build — no Cargo feature and no `cfg` — and the ADR-0229 42N17 text is
unchanged.

The same registry row gates the `system_ontologies` namespace's visibility
(ADR-0593): while the feature is off the namespace is invisible — `SHOW
DATABASES` lists the two CE rows, and every route to the name (`USE`, the
`rl-onto` element-id segment, session connect, `set_namespace`, `ALTER
DATABASE`) fails with that site's ordinary unknown-database rejection,
never the authored preview error above — ADR-0371's name-gate rule: an
error must not advertise a name the catalog does not list. The durable
catalog entry is untouched (the opt-in is per-open, not durable), so an
existing store lists two rows while off and three once the opt-in is set.

## Procedures

| Signature | Mode | Summary |
| --- | --- | --- |
| `db.ontology.set(ontology :: STRING)` | WRITE | Validate and store the ontology document (JSON or YAML), replacing any current one. Implicit transactions only. |
| `db.ontology.get(format = "yaml" :: STRING) :: (ontology :: STRING)` | READ | Return the stored document in the requested format, or `null` when none is set. |
| `db.ontology.drop()` | WRITE | Remove the stored ontology. Succeeds when none is set. Implicit transactions only. |
| `db.ontology.apply()` | WRITE | Convert the stored ontology to a graph type and apply it with `ALTER CURRENT GRAPH TYPE SET` semantics. May run in explicit transactions. |

### `db.ontology.set(ontology :: STRING)`

`ontology` is the document as a string, in JSON or YAML 1.2 (YAML is a JSON
superset; both syntaxes denote the same document and behave identically
after parsing). The accepted shape is the published Ontology Graph Spec
1.1.0 document (ADR-0364, raised to 1.1.0 by ADR-0487). `set` validates
the document before anything is committed:

- The root `schema` field is checked first and must equal
  `https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json` exactly.
  Any other value, including a URI from another graph-spec dialect (the
  retired 1.0.0 main URI and the older draft URI included), fails with
  `Unsupported ontology spec version: '<uri>'. This version of Relate
  supports up to Ontology Graph Spec 1.1.0
  ('https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json').`
  A non-string `schema` is a type error on that field, not a URI mismatch.
- The parsed document is then validated structurally against the vendored
  published 1.1.0 schema (`neo4j/graph-spec` tag `release-spec/v1.1.0` @
  `5ff2c20aa7e428e7c563b17f573157293959a009`, ADR-0487 §2). Every object
  the schema closes is closed here too: unknown members are rejected,
  including the pre-bump spellings `mustExist`, constraint
  `constraint_type`, the 1.0.0 node and relationship `name`, the 1.0.0
  constraint `label`, and the 1.0.0 document extension keys `tables` and
  `mappings`. Anything outside the JSON-equivalent value model —
  non-string mapping keys, custom YAML tags, non-finite floats — is
  rejected at parse time.
- A document over 16 MiB is refused before any commit (the ADR-0086 object
  envelope, ADR-0120 §3).

#### Document shape

| Construct | Accepted shape |
| --- | --- |
| Root | required `schema`, `version`, `id`; `name` and `description` strings; `nodes` and `relationships` maps; `extensions`. `version` is any string, carried verbatim and never compared numerically. The root is closed: `$schema` and `$id` are not document members. |
| Node | keyed by node id; nullable `label` (shorthand for `labels.identifier`); `labels` with nullable `identifier`, `implied`/`optional` string arrays and `extensions`; `properties` and `constraints` maps; `extensions`; `description`, `aliases`, `reference`. No `name` member in 1.1.0. |
| Relationship | keyed by relationship id; required `type`, `from`, `to`; `properties` and `constraints` maps; `extensions`; `description`, `aliases`, `reference`. No `name` member in 1.1.0. |
| Endpoint (`RelationshipTarget`) | no required members; optional `node` and `label` strings; nullable `count`, `min_count`, `max_count` integers with no minimums. |
| Property | keyed by property id; optional `type` from the closed `Neo4jType` enum; nullable `dimension`; nullable `must_exist`, `unique`, `key`; `one_of` array of strings; nullable `name`; `aliases` string array (descriptive); `description`, `reference`, `pattern`; `extensions`. `mustExist` is rejected. |
| Constraint | id-keyed map, one shape for node and relationship owners; required `type` (`EXISTS`, `KEY`, `PROPERTY_TYPE` or `UNIQUE`) and `properties` (string array); nullable `name`; `extensions`. `constraint_type` and `label` are rejected. |
| Extensions | closed objects: node `indexes` map plus optional `display` and `custom`; relationship `indexes` map and `custom`; property, labels and constraint `custom` only; document `data_source_tables` and `data_source_mappings` maps plus `custom`. |

A main-shaped document, abbreviated:

```yaml
schema: https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json
version: "1"                     # string, not an integer
id: movies
nodes:
  Person:
    label: Person                # shorthand for labels.identifier
    properties:
      id:
        type: STRING
        key: true
      name:
        type: STRING
        must_exist: true
      role:
        type: STRING
        one_of: [actor, director]  # string items only
    constraints:
      person_role:
        type: EXISTS
        properties: [role]
        name: Person role
  Movie:
    label: Movie
    properties:
      title:
        type: STRING
        must_exist: true
relationships:
  ACTED_IN:
    type: ACTED_IN
    from: { node: Person }
    to: { node: Movie, max_count: null }
    properties:
      scene_id:
        type: STRING
    constraints:
      acted_in_scene:
        type: UNIQUE
        properties: [scene_id]
extensions:
  custom:
    owner: data-team
```

`one_of` items are strings; a non-string item is rejected. The
`Neo4jType` enum uses the spaced spellings `LOCAL TIME`, `LOCAL DATETIME`,
`ZONED TIME` and `ZONED DATETIME`; the pre-bump bare `TIME`, `DATETIME`,
`LOCALTIME`, `LOCALDATETIME` and `BYTES` tokens fail validation. `FLOAT32`,
`INTEGER8`, `INTEGER16`, `INTEGER32` and their list forms validate but fail
at `apply`, because the graph-type body has no plain 8/16/32-bit scalar
(ADR-0364 §5.2). Node, relationship and property `aliases` are all
accepted and descriptive. Nullable fields accept `null` and mean "no
value"; a `false` flag means "not asserted", and endpoint counts are
descriptive with no minimums.

The cutover is hard (ADR-0364 §1, repeated by ADR-0487 §1): there is no
dual gate, URI alias or automatic migration. A document carrying the
pinned URI but a pre-bump shape fails structural validation on its first
old member, and a document carrying an older URI — the 1.0.0 main URI or
the draft URI — fails at the gate. Resubmitting a 1.1.0-shape document
is the migration. The gate and structural validation are set-time policy
only (ADR-0364 §4): `get` never re-validates, and an already-stored
pre-bump document remains readable but fails `apply`.

The submitted bytes are stored verbatim — comments, key order and YAML
formatting included — replacing any currently stored ontology. The store
records no JSON-vs-YAML marker, only the bytes (ADR-0227 §2).

### `db.ontology.get(format = "yaml" :: STRING) :: (ontology :: STRING)`

Returns the stored document in the requested format, or `null` when no
ontology is set. `format` defaults to `"yaml"`; the other accepted value is
`"json"`, and any other value fails with
`InvalidArgumentValue: unknown db.ontology.get format '<value>'; expected
'yaml' or 'json'`.

When the requested format matches the stored one, `get` returns the stored
bytes unchanged — YAML in, YAML out, comments and formatting included;
likewise JSON. Otherwise it converts canonically (compact UTF-8 for JSON,
block style with two-space indentation for YAML), preserving the document's
key order. Conversion never touches the stored bytes (ADR-0227 §3), and
neither path re-runs the version gate or structural validation
(ADR-0364 §4). The stored format is detected at read time by a strict JSON
parse: a stored document that parses as JSON counts as JSON, anything else
as YAML.

`get` reads the document the calling transaction captured (ADR-0121 §3):
the begin-time capture inside an explicit transaction, so it keeps seeing
one consistent snapshot of the ontology.

### `db.ontology.drop()`

Removes the stored ontology and succeeds when none is set.

### `db.ontology.apply()`

Converts the stored ontology to a CIP-176 graph type and applies it to
`neo4j` with `ALTER CURRENT GRAPH TYPE SET` semantics, replacing the current
graph type (ADR-0226 §2). It fails when no ontology is set with
`OntologyNotFound: no ontology is set for database 'neo4j'.` It is the one
operation that interprets the stored document, so an already-stored
pre-bump document fails here with the gate's semantic error ending
`Unsupported ontology spec version: '<uri>'. …` (never an internal error);
`get` keeps returning its bytes unchanged (ADR-0364 §4).

`apply` validates first, then commits (ADR-0226 §3): before any catalog
change it scans the calling transaction's captured `neo4j` snapshot against
every constraint the resulting graph type derives — key constraints check
presence and uniqueness, unique constraints check uniqueness, `NOT NULL` /
`must_exist` checks presence, the property-type constraints derived from
`type` declarations check every present value (including vector dimensions),
and endpoint constraints check that every relationship touches the
identifying labels. The first violation fails
the statement with a data-violation error naming the constraint and the
offending element's element id; no graph-type change commits, and inside an
explicit transaction the transaction's other work is untouched. Validation
and the catalog write are computed from the same captured snapshot and
commit atomically. Unlike `apply`, a bare `ALTER CURRENT GRAPH TYPE SET`
does not validate existing data.

The enforceable/descriptive split follows main's conversion rules
(ADR-0226 §2, ADR-0364 §5). **Enforceable** fields compile into the
graph-type body: identifying and implied labels, property `type` tokens
(with `LIST<X>` mapping to the closed `LIST<X NOT NULL>` and `LIST<ANY>`
declaring nothing, ADR-0228), `must_exist` / `unique` / `key` flags,
constraint objects (id-keyed maps with uppercase `type`), and relationship
endpoint `node` references. **Descriptive** fields are never applied:
`description`, node/relationship/property `aliases`, `reference`,
`labels.optional`, endpoint `count`/`min_count`/`max_count`, `one_of`,
`pattern`, every extension, the document metadata (`version`, `name`,
`description`) and the property, constraint and index `name` fields
(1.1.0 carries no node or relationship `name`). (A constraint object's
`name` is carried as the graph-type constraint name when present.) The
descriptive fields remain available verbatim through `db.ontology.get` —
the stored document is the sole source of truth, and no graph is derived
from it (ADR-0229 §3).

Validation accepts every spec-valid document; `apply` fails loudly with
`OntologyError::Mapping` when a valid document cannot be expressed as a
CIP-176 graph type (ADR-0364 §3). That covers a node with no identifying
label, an endpoint with no `node` (label-only and empty targets),
`FLOAT32`/`INTEGER8`/`INTEGER16`/`INTEGER32` and their list forms, an
empty-named property that would be declared or constrained, an empty
constraint `properties` list, a `PROPERTY_TYPE` constraint, a
non-positive vector `dimension`, a constraint property id missing from
its owner, and an unknown endpoint node key. Each error names the owner,
the construct and, where applicable, the property or constraint id and
token. Nothing is silently coerced.

## Namespace rules

Each procedure acts on the database it is executed against — the session
database or the `USE` target (ADR-0227 §1) — which must be `neo4j`
(ADR-0121 §2):

- Executed against `system_ontologies` (visible only under the
  `ontology` opt-in, ADR-0593), every procedure — `get` included —
  fails that namespace's not-queryable gate first (ADR-0229 §1/§2), with
  `SemanticError: This database is not queryable. Use the db.ontology.*
  procedures on the user database 'neo4j' instead.` (GQLSTATUS 42N17,
  `EXPLAIN`/`PROFILE`-wrapped included). While the opt-in is off the
  namespace itself is invisible — the route fails as an unknown database
  before the procedure surface is reached.
- Executed against `system`, the procedures fail with
  `Ontologies can only be stored for the user database 'neo4j'.`
- An unknown `USE` target fails at routing with 42N00 before the procedure
  surface is reached (ADR-0116 §3).

## Transaction rules

- `set` and `drop` run only in implicit (auto-commit) transactions. Inside
  an explicit transaction they fail with
  `'db.ontology.set' can only be executed in an implicit transaction, but
  tried to execute in an explicit transaction.` (or the same text naming
  `drop`).
- A statement that calls `set` or `drop` may not write anything else;
  otherwise it fails with `Writing to more than one database per transaction
  is not allowed.` (42N03).
- `get` reads the calling transaction's captured document (snapshot
  semantics; see above).
- `apply` writes only `neo4j` and may run in explicit transactions.

## The `system_ontologies` namespace: gated visibility, never queryable

`system_ontologies` stays a fixed namespace of every store — its durable
catalog row is always written — but its dialect visibility rides the
`ontology` preview row (ADR-0593):

- **Opt-in off (the default): the namespace is invisible.** `SHOW
  DATABASES` lists the two CE rows, and every route to the name fails
  with the site's ordinary unknown-database rejection — `USE
  system_ontologies` (and `graph.byElementId` on the `rl-onto` segment)
  with `Graph not found: system_ontologies` (42N00), session-namespace
  connect and `set_namespace` the same, `ALTER DATABASE
  system_ontologies` as an unknown target (a no-op under `IF EXISTS`).
  The name-shape stays valid — the CLI `--namespace` flag and the client
  wire vocabularies still accept the spelling — and the refusal moves to
  the post-open connect. Never the authored preview error: an error must
  not advertise a name the catalog does not list (ADR-0371's name-gate
  rule).
- **Opt-in on: the namespace is visible and selectable** everywhere a
  namespace name is parsed — `SHOW DATABASES` keeps its row (ADR-0116
  §6's values), session-namespace selection over the embedding APIs and
  Bolt, a static `USE` target, the CLI `--namespace` flag, and
  `graph.byElementId` keeps decoding the `rl-onto` element-id segment to
  it. But every statement routed to it — reads
  included, `EXPLAIN`/`PROFILE`-wrapped included — fails at the plan-based
  semantic gate with (ADR-0229 §1/§2):

```text
semantic error: SemanticError: This database is not queryable. Use the db.ontology.* procedures on the user database 'neo4j' instead.
```

(GQLSTATUS 42N17.) What still runs while routed to `system_ontologies` is
exactly the ADR-0116 §2 database-administration commands — `SHOW
DATABASE[S] [name]`, `SHOW DEFAULT DATABASE`, `SHOW HOME DATABASE`,
`ALTER DATABASE … SET DEFAULT LANGUAGE`. No graph is derived from the stored
document (ADR-0229 §3 supersedes ADR-0120 §2): the document is reachable
only through the `db.ontology.*` procedures above — `get` reads it, and
`apply` turns it into `neo4j`'s current graph type. The ADR-0226 §1
document→graph mapping is not shipped; it is revived from git history when
Neo4j publishes its mapping. The divergence from live Neo4j CE 2026.09 —
which allows reads on `system_ontologies` — is deliberate and recorded in
ADR-0229.

## Storage behavior

The format follows the content (ADR-0120 §8): a store that never sets an
ontology stays byte-for-byte in the pre-ontology format and readable by
older binaries.

- The first committed `set` rotates the WAL to segments with the `MDBWAL02`
  magic: a segment created while the store holds an ontology — committed or
  still pending in the WAL — starts with `MDBWAL02`, and other segments
  start with `MDBWAL01`.
- A Parquet checkpoint of a store holding an ontology publishes one
  immutable `ontology/<128-bit-hex>.json` object and a manifest with
  `ontology_path` at `format_version: 4`; without an ontology the manifest
  stays at 3. A JSON checkpoint carries the document inline as the
  `graph.json` `ontology` field, written with `value_encoding: 3` (encoding
  2 plus that field); without an ontology it keeps 2.
- Reopening a store loads the stored document — a committed but not yet
  checkpointed `set` survives reopen through WAL replay. Nothing is
  derived from it at open (ADR-0229 §3); open still screens the folded
  document's syntax and value model, so a corrupt record fails open instead
  of deferring the failure to the first procedure that touches it. A
  pre-bump document is expected at rest and opens: the open
  path runs only the syntax/value-model screen — UTF-8, one document, the
  JSON-equivalent value model, the depth cap — never the version gate or
  the structural validator (ADR-0364 §4).
- After a `drop`, a checkpoint and WAL recycling, the store returns to
  `MDBWAL01` segments, manifest v3 and `value_encoding` 2, and older
  binaries can open it again.
- Older binaries fail closed on an ontology-carrying store — at the
  `MDBWAL02` segment, the v4 manifest or `value_encoding: 3` — before
  modifying any data (ADR-0114). A pre-bump ontology-aware binary likewise
  refuses a main-shape document at open, through its own stored-document
  validation and with no version-marker change; the bytes are intact and
  become readable again after upgrading (ADR-0364 §4).

One limitation: a `--read-only=live` reader does not refresh the ontology document
or any state derived from it while it follows the WAL. The store-level
document is written only at open and by the commit pipeline; the follow
paths apply records to the graph — an ontology op carries no graph
mutation, since no graph is derived from the document
([ADR-0229](../adr/0229-remove-system-ontologies-cypher-surface.md) §3) —
and reload the graph and WAL on a gap, never the document slot. A follower
therefore keeps seeing the ontology as of its own open until it
reconnects; reopening converges (the stored document is folded back out of
the snapshot or the WAL at open).

See the [storage engine guide](../guides/storage-engine.md) for the full
version-marker table.

## Examples

The statements below run in CI: they are exactly the ones the Rust API
example [`relate/relate/examples/ontology.rs`](https://github.com/relatedb/relate/blob/main/relate/relate/examples/ontology.rs)
executes (paired narrative:
[`docs/api/rust/examples/ontology.md`](../api/rust/examples/ontology.md)),
which the CI "Documented Rust API examples" job builds and runs on every
change (AGENTS.md §2).

Each statement needs the opt-in: the Rust example sets
`RELATE_PREVIEW=ontology` before `Database::open` (ADR-0378), and the CLI
form puts it in the process environment:

```sh
RELATE_PREVIEW=ontology relate query --database graph.rl \
  'CALL db.ontology.get() YIELD ontology RETURN ontology'
```

Without it, the call fails with the preview error above.

A small ontology in YAML — one node type with a key property and a required
property, in the published document shape:

```yaml
schema: https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json
id: example
version: "1"
nodes:
  Person:
    label: Person
    properties:
      id:
        type: STRING
        key: true
      name:
        type: STRING
        must_exist: true
```

`version` is a string and property existence uses `must_exist`; either
pre-bump spelling fails validation.

Store it from a parameter (`set` validates the published spec version and
the schema shape, and stores the submitted bytes verbatim):

```cypher
CALL db.ontology.set($ontology);
```

Read it back. With no argument, `get` returns the stored bytes unchanged
when the stored format is YAML; `'json'` converts canonically:

```cypher
CALL db.ontology.get() YIELD ontology RETURN ontology;
CALL db.ontology.get('json') YIELD ontology RETURN ontology;
```

The `system_ontologies` namespace is visible only under the same
`RELATE_PREVIEW=ontology` opt-in (ADR-0593) and not queryable — every other
statement routed there, reads included, fails with the 42N17 text above
(ADR-0229). Without the opt-in the namespace is invisible: two rows, and
`USE` fails as an unknown database:

```cypher
SHOW DATABASES YIELD name RETURN name;   -- two rows without the opt-in (ADR-0593), three with it
USE system_ontologies MATCH (n) RETURN n;  -- opt-in on: fails 42N17, not queryable; off: 42N00, unknown database
```

Apply the ontology as the database's current graph type. Data violating it
— here a `Person` without the key property `id` or the required `name` —
fails the write:

```cypher
CALL db.ontology.apply();
SHOW CURRENT GRAPH TYPE;
CREATE (:Person);                        -- fails: key id and must_exist name gate writes
CREATE (:Person {id: 'p1', name: 'Alice'});
```

Drop the ontology; `get` then yields `null`:

```cypher
CALL db.ontology.drop();
CALL db.ontology.get() YIELD ontology RETURN ontology;
```
