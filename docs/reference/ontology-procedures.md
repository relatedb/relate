# Ontology Procedures Reference

The `db.ontology.*` procedures manage the store's single ontology document:
one Ontology Graph Spec document for the user database `neo4j` — the sole
stored source of truth; no graph is derived from it, and
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
[ADR-0378](../adr/0378-ontology-procedures-preview-gate.md),
[ADR-0667](../adr/0667-ontology-procedure-parity.md)). No database
argument — each procedure acts on the database it is executed against
(the session database or the `USE` target). The procedures' code ships
in every build: all CLI bundles, the Rust crate, the language bindings,
Bolt and the WASM artifact (ADR-0121 §6); only callability is gated,
because calling them requires the `ontology` preview opt-in (ADR-0378).
Once the opt-in is set, any caller may run every procedure — there is no privilege check (ADR-0121 §4).
Enabling the preview on a served store — `relate serve` exposing Bolt,
HTTP or MCP — exposes the four procedures, including the store-level
writers (`set` and `drop`), to every connected client.
`SHOW PROCEDURES` lists all four once the opt-in is set, and omits them
while it is not (see [Preview gate](#preview-gate)).

The four procedures' contracts — signatures, arguments, result columns,
error behavior, the stored document's form and the transaction rules —
are revised
([ADR-0667](../adr/0667-ontology-procedure-parity.md)): `SHOW
PROCEDURES` is the live rendering of the signatures and modes, and this
reference documents what the signatures do not: the
preview and namespace gates, the accepted document shape, the storage
and checkpoint behavior, and `apply`'s validate-first data check.
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

The four procedures — `set`, `get`, `drop`, `apply` — and their contracts
are revised
([ADR-0667](../adr/0667-ontology-procedure-parity.md)): the
signatures, arguments and result columns are what `SHOW PROCEDURES`
renders under the opt-in, the error behavior is the current one, and
`apply` answers one result row describing what the statement applied.
What this reference adds on top of the live catalog is the accepted
document shape (below), the namespace and transaction framing, the
storage behavior, and `apply`'s validate-first data check.

### `db.ontology.set(ontology :: STRING)`

`ontology` is the document as a string, in JSON or YAML 1.2 (YAML is a JSON
superset; both syntaxes denote the same document and behave identically
after parsing). The accepted shape is the published Ontology Graph Spec
1.1.0 document (ADR-0364, raised to 1.1.0 by ADR-0487). `set` runs the
document through the codec before anything is committed (ADR-0667):
a `null` or empty argument clears the stored ontology — the same clear
`drop` pushes — and a non-empty argument is validated and stored in the
codec's canonical form. An
invalid document fails the statement with the codec's user-facing
envelope, before any commit; a document whose canonical form exceeds
16 MiB is refused by the object envelope (ADR-0086, ADR-0120 §3). What
`set` does with a member the published shape does not carry — the
pre-bump spellings included — is the revised handling
([ADR-0667](../adr/0667-ontology-procedure-parity.md)).

#### Document shape

| Construct | Accepted shape |
| --- | --- |
| Root | required `schema`, `version`, `id`; `name` and `description` strings; `nodes` and `relationships` maps; `extensions`. `version` is any string, carried verbatim and never compared numerically. The root is closed: `$schema` and `$id` are not document members. |
| Node | keyed by node id; nullable `label` (shorthand for `labels.identifier`); `labels` with nullable `identifier`, `implied`/`optional` string arrays and `extensions`; `properties` and `constraints` maps; `extensions`; `description`, `aliases`, `reference`. No `name` member in 1.1.0. |
| Relationship | keyed by relationship id; required `from`, `to`; `type` optional; `properties` and `constraints` maps; `extensions`; `description`, `aliases`, `reference`. No `name` member in 1.1.0. |
| Endpoint (`RelationshipTarget`) | no required members; optional `node` and `label` strings; nullable `count`, `min_count`, `max_count` integers with no minimums. |
| Property | keyed by property id; optional `type` from the closed `Neo4jType` enum; nullable `dimension`; nullable `must_exist`, `unique`, `key`; `one_of` array of strings; nullable `name`; `aliases` string array (descriptive); `description`, `reference`, `pattern`; `extensions`. |
| Constraint | id-keyed map, one shape for node and relationship owners; required `type` (`EXISTS`, `KEY`, `PROPERTY_TYPE` or `UNIQUE`) and `properties` (string array); nullable `name`; `extensions`. |
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
`LOCALTIME`, `LOCALDATETIME` and `BYTES` tokens fail validation. Node,
relationship and property `aliases` are all accepted and descriptive.
Nullable fields accept `null` and mean "no value"; a `false` flag means
"not asserted", and endpoint counts are descriptive with no minimums.
How a validated token converts at `apply` — which kinds widen, which
constructs have no graph-type equivalent — follows the revised mapping
([ADR-0667](../adr/0667-ontology-procedure-parity.md)); `apply`'s result
row reports the parts it skipped.

The cutover is hard (ADR-0364 §1, repeated by ADR-0487 §1): there is no
dual gate, URI alias or automatic migration. A document carrying an
older URI — the 1.0.0 main URI or the draft URI — fails at the gate, and
resubmitting a 1.1.0-shape document is the migration. The gate and
structural validation are set-time policy (ADR-0364 §4): an
already-stored pre-bump document opens (the open path screens syntax
only) but fails the codec's version gate in `get` and `apply`, each with
its own envelope, until it is replaced.

A `set` replaces any currently stored ontology. The stored form — what
the codec keeps and what `get` re-renders from — is the revised one
([ADR-0667](../adr/0667-ontology-procedure-parity.md)).

### `db.ontology.get(format = "yaml" :: STRING) :: (ontology :: STRING)`

Returns the stored document in the requested format, or `null` when no
ontology is set — exactly one row either way. The `format` argument and
the rendering of the stored document follow the revised contract
([ADR-0667](../adr/0667-ontology-procedure-parity.md)).

`get` reads the document the calling transaction sees; the transaction
framing is the revised one (ADR-0121 §3, as amended by
[ADR-0667](../adr/0667-ontology-procedure-parity.md)).

### `db.ontology.drop()`

Removes the stored ontology and succeeds when none is set.

### `db.ontology.apply()`

Converts the stored ontology to a CIP-176 graph type and applies it to
`neo4j` with `ALTER CURRENT GRAPH TYPE SET` semantics, replacing the
current graph type (ADR-0226 §2). The result row and the conversion's
skip reports are part of the revised contract
([ADR-0667](../adr/0667-ontology-procedure-parity.md)). It is the one
operation that interprets the stored document: a stored document that no
longer decodes, or fails validation, fails here with its own envelope —
never an internal error.

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

The enforceable/descriptive split frames the conversion (ADR-0226 §2):
**enforceable** fields compile into the graph-type body, **descriptive**
fields never do — `description`, `aliases`, `reference`,
`labels.optional`, endpoint `count`/`min_count`/`max_count`, `one_of`,
`pattern`, every extension, the document metadata (`version`, `name`,
`description`) and the property, constraint and index `name` fields.
Which enforceable fields map to which graph-type constructs — the type
widening, the flag-derived constraints, the endpoint forms — and which
parts are skipped as having no equivalent follow the revised mapping
([ADR-0667](../adr/0667-ontology-procedure-parity.md)); the result row
reports the skipped parts. The descriptive fields
remain available through `db.ontology.get` — the stored document is the
sole source of truth, and no graph is derived from it (ADR-0229 §3).

Validation accepts every spec-valid document; `apply` fails loudly when
a valid document cannot be expressed as a CIP-176 graph type
(ADR-0364 §3) — a construct the conversion cannot express is a named
failure, never a silent coercion, while a part that merely has no
graph-type equivalent is skipped and reported.

Relationship declarations resolve by type (ADR-0668): a type declared
once — or by entries restating one resolved endpoint pair identically —
defines one edge type spec, and a restatement of a declared pair with a
different declaration is a named failure, never a silent first-wins. A
type declared across several distinct endpoint pairs defines no graph
type at all: nothing enforces for the type, its relationships stay
writable under any labels, the type still seeds the load's vocabulary,
and the result row reports it as skipped. Two node entries sharing one
identifying label are a named failure too — one label, one node type.

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

- The transaction rules for `set` and `drop` — which transactions they
  run in and what one may share with them — are revised
  ([ADR-0667](../adr/0667-ontology-procedure-parity.md)).
- A statement that calls `set` or `drop` may not write anything else;
  otherwise it fails with `Writing to more than one database per
  transaction is not allowed.` (42N03).
- `get` reads the document the calling transaction sees (see above).
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

A small ontology in YAML — one node type with a key property, in the
published document shape:

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
```

`version` is a string; an integer fails validation.

Store it from a parameter (`set` validates the published spec version and
the schema shape, and stores the codec's canonical form):

```cypher
CALL db.ontology.set($ontology);
```

Read it back — the default `get` renders the stored form as YAML,
`'json'` as JSON:

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
USE system_ontologies MATCH (n) RETURN count(n) AS c;  -- opt-in on: fails 42N17, not queryable; off: 42N00, unknown database
```

Apply the ontology as the database's current graph type. The call answers
one result row — the applied model's name else id, the constraints the
statement created and dropped, and the parts without a graph-type
equivalent. The derived key then gates writes — here a `Person` without
the key property `id` fails the `CREATE`:

```cypher
CALL db.ontology.apply() YIELD ontology, constraintsCreated, constraintsDropped, skipped;
SHOW CURRENT GRAPH TYPE;
CREATE (:Person);                        -- fails: the derived key id gates writes
CREATE (:Person {id: 'p1'});
```

A `null` (or empty) `set` argument clears the stored ontology — the same
clear `drop` pushes; `get` then yields `null`:

```cypher
CALL db.ontology.set(null);
CALL db.ontology.get() YIELD ontology RETURN ontology;  -- null
```
