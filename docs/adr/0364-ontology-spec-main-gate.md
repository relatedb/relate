# ADR-0364: Bump the ontology document gate to the main graph-spec schema

Status: Accepted
Claim: 0872cff15343509eed66966617919b69db51e2e8

## Context

ADR-0227 §6 pins the Ontology Graph Spec 1.0.0 draft (`neo4j/graph-spec`
@ `2f8dcbdf2dab51ebf2d760e00904a8f2b0518f59`) as the vendored schema
`cypherlite/cypherlite-ontology/schema/ontology-graph-spec.schema.json`,
hand-rolls validation against exactly that schema, and §7 gates documents
on the exact root `schema` URI
`https://neo4j.com/ontology-graph-spec/1.0.0/schema.json`. ADR-0226 §2
(with ADR-0228's list correction) compiles a validated document into the
graph type that `db.ontology.apply` sets. ADR-0227 §2/§3 store
the submitted bytes verbatim and return them unchanged in the stored
format.

Neo4j's graph-spec `main` has moved to
`066515593ae59317d758bcf2cef54195e74bde39`. Its `$id` is now
`https://neo4j.com/docs/ontology-graph-spec/1.0.0/schema.json`, and a
drift report between the pin and this commit enumerates 25 structural
changes, among them: `version` changed from an integer (minimum 0) to a
string; node, relationship and property entries gained nullable display
`name` fields and constraint `name` became nullable; node `label` and
`labels.identifier` became nullable; property `mustExist` was renamed
`must_exist` and the flags and `dimension` became nullable; property
`aliases` was removed; `one_of` items narrowed to strings; the
regex-validated property type token was replaced by a closed 39-token
`Neo4jType` enum (dropping `BYTES` and the bare `TIME`/`DATETIME`
spellings, adding `FLOAT32`, `INTEGER8`/`INTEGER16`/`INTEGER32`, `UUID`,
and the vector element types); constraint objects moved from arrays to
id-keyed maps with an uppercase `type` enum that adds `PROPERTY_TYPE`;
endpoints became `RelationshipTarget` (no required members, nullable
counts, no minimums); and the single open `extensionsMap` became closed
per-element `*Extensions` objects with typed `indexes`, `display`,
`tables` and `mappings`. The drift report is the authoritative list for
the implementation; its code and doc sites are the change checklist.

Two upstream facts shape the decision. First, main's root `schema` field
is an unconstrained string: neither `$id` equality nor a URI format ties
a document to the schema it claims, so the exact-URI gate is
**CypherLite policy**, not a schema-enforced marker. Second, main ships
no root-level `example.json` (the pinned README's curl URLs are stale),
and its `go/model/testdata/graph-spec-example.json` is main-shaped but
does not validate against main's own schema:
`relationships.ACTED_IN.from.property` and `.to.property` are extra
members, which `RelationshipTarget`'s closed shape rejects.

CypherLite must now choose between accepting both dialects (a
compatibility shim), migrating stored documents automatically, and a
hard cutover. This decision chooses **hard cutover**. The stored
document is the sole source of truth (ADR-0229 §3), so there is no
derived graph to migrate, and the product is pre-1.0 (0.12.x). The
decision still has to settle the gate, the accepted shapes, the places
where CypherLite stays stricter than main, the example fixture, and the
converter's token and constraint mapping.

## Decision

### 1. One gate URI, hard cutover

- The only accepted value of the root `schema` field is
  `https://neo4j.com/docs/ontology-graph-spec/1.0.0/schema.json` — main's
  `$id`. It is checked before structural validation, as ADR-0227 §7
  prescribes, on the `set` entry point. The old
  `https://neo4j.com/ontology-graph-spec/1.0.0/schema.json` and any other
  URI string fail with
  `Unsupported ontology spec version: '<uri>'. This version of CypherLite
  supports up to Ontology Graph Spec 1.0.0
  ('https://neo4j.com/docs/ontology-graph-spec/1.0.0/schema.json').`
  A non-string `schema` is not a URI at all and fails as a type error on
  that field. `PINNED_SCHEMA_URI` becomes the new URI;
  `MAX_SUPPORTED_SPEC_VERSION` stays `1.0.0`.
- **Hard cutover**: no dual gate, no URI alias, no rewrite of the
  submitted value, no automatic migration. A pre-bump document is not
  accepted by `set`; resubmitting a main-shape document is the migration.
  A document that carries the new URI but the old shape fails structural
  validation on its first old member (for example an integer `version` or
  `mustExist`); a document that carries the old URI fails at the gate
  before structure is looked at. In short, the raised gate together with
  the closed new shapes rejects every pre-bump document, whichever URI it
  carries.
- The vendored schema file's `$id` is the same URI; the gate is on the
  submitted document's `schema` field, not on the vendored `$id`.
- The exact-URI rule remains CypherLite policy: main's `schema` is a
  plain string, so the schema cannot enforce it. Keeping an exact marker
  is what makes a newer dialect fail loudly instead of being misread.
  Supporting a later spec version is still a new ADR (new vendored
  schema, validator update, raised gate).

### 2. Validation follows main's closed shapes

The vendored schema file is replaced in place with main @
`066515593ae59317d758bcf2cef54195e74bde39`, and the hand-rolled
validator checks exactly that document. The accepted shape is main's:

| Construct | Accepted shape |
|---|---|
| Root | required `schema`, `version`, `id`; `name`, `description` strings; `nodes`, `relationships` maps; `extensions`. `version` is any string (the integer/minimum rule is gone), carried verbatim. `schema` is gated per §1. The root is closed: no `$schema`/`$id` document members. |
| Node entry | keyed by node id; `label` nullable string; `labels` object with nullable `identifier`, `implied`/`optional` string arrays, `extensions` and no required members; `properties` map; `constraints` map; `extensions`; nullable `name`; `description`, `aliases`, `reference`. |
| Relationship entry | keyed by relationship id; required `type`, `from`, `to`; `properties` map; `constraints` map; `extensions`; nullable `name`; `description`, `aliases`, `reference`. |
| `RelationshipTarget` | no required members; `node` and `label` optional strings; `count`, `min_count`, `max_count` nullable integers with no minimums. |
| Property entry | keyed by property id; optional `type` from the closed `Neo4jType` enum; nullable `dimension` (integer); nullable `must_exist`, `unique`, `key`; `one_of` array of **strings**; nullable `name`; `description`, `reference`, `pattern`; `extensions`. `mustExist` and `aliases` are gone and are rejected as unknown members. |
| Node constraint | id-keyed map; required `type` (`EXISTS` \| `KEY` \| `PROPERTY_TYPE` \| `UNIQUE`) and `properties` (string array, no `minItems`); nullable `label`; `extensions`; nullable `name`. `constraint_type` is gone (renamed `type`). |
| Relationship constraint | as a node constraint without `label`. |
| Node extensions | `indexes` map of `NodeIndex`; optional `display` (null or `NodeDisplay`); `custom` object. |
| Relationship extensions | `indexes` map of `RelationshipIndex`; `custom` object. |
| Property, labels, node-constraint, relationship-constraint extensions | `custom` object only. |
| Document extensions | `tables` map of `Table`; `mappings` map of arrays of `Mapping`; `custom` object. |

`NodeIndex`/`RelationshipIndex`, `IndexType` (`FULLTEXT`, `POINT`,
`RANGE`, `TEXT`, `VECTOR`, `LOOKUP`), the nullable `options` union
(`FULLTEXT`/`POINT`/`VECTOR` payloads), `NodeDisplay`, `Table`,
`TableColumn`, `ForeignKey`, `ForeignKeyReference`, the tagged
`NODE`/`QUERY`/`RELATIONSHIP` `Mapping` union, `MappingMode`,
`PropertyMapping` and `TargetMapping` are validated **deeply** against
main's definitions — closed members, required fields, enum and scalar
types — not merely as opaque envelope keys. `custom` objects are the one
exception: main defines them as arbitrary JSON objects, so the validator
requires an object and does not interpret its members. This supersedes
ADR-0227 §6's "extension payloads are never validated", which described
the pinned open envelope; main's typed members are schema constructs,
and accepting a malformed `Table` or `NodeIndex` would accept a document
main itself rejects.

Every object is closed where main sets `additionalProperties: false`
(every `$defs` object type and the root). The schema's own metadata
(`$id`, `title`, `description`) imposes no document rule, and the string
fields main leaves unformatted (`reference`, `pattern`) carry no extra
checks. Nullable fields accept `null` and treat it as "no value" in the
typed model; the retained root value tree keeps the author's exact
`null` vs omission, so a cross-format `get` preserves the choice.

### 3. Where CypherLite stays stricter, and why

Validation answers one question — does the document match main's
document schema — and `apply` answers a different one: can the ADR-0226
converter express this document as a graph type. A main-valid
document is storable and readable even when it is not convertible; the
conversion failure is loud, at `apply`, never a silent coercion. This
keeps one notion of document validity instead of two, and matches the
precedent that unresolvable endpoint references are `Mapping` errors at
apply, not validation errors.

| Main-valid point | Ruling |
|---|---|
| `label` and/or `labels.identifier` null or absent | Accepted at validation. A node with neither has no identifying label; `apply` fails with a `Mapping` error naming the node. A node with only one of them uses it. When both are present they must be equal: `label` is main's shorthand for `labels.identifier`, and a conflicting pair is rejected at validation as an ambiguous identity (the schema is silent; silently choosing one would be a fallback). |
| Label-only or empty `RelationshipTarget` | Accepted at validation. `apply` fails with a `Mapping` error naming the relationship and the `from`/`to` side: an endpoint with no `node` cannot resolve to a node key, and resolving by `label` would be ambiguous because labels are not unique across entries. No label-based resolution is attempted. When `node` is present it is authoritative; a present `label` is not used for resolution and is not checked against it. |
| Empty property names (`propertyNames.minLength` dropped) | Accepted at validation. `apply` fails with a `Mapping` error when an empty-named property contributes a declaration or a constraint (it has a `type`, an asserted shorthand flag — `must_exist`, `unique` or `key` true — or is listed by a constraint object): a graph-type property reference is non-empty. A purely descriptive empty-named property is carried and compiles to nothing, exactly like any other property with no type and no flags. |
| Constraint `properties` with no `minItems` | Accepted at validation, including an empty array. `apply` fails with a `Mapping` error naming the owner and constraint when the list is empty: a graph-type constraint needs at least one property. |
| Nullable dimensions, flags and endpoint counts | Accepted; `null` means absent, `false` means not asserted. Counts have no minimums and are descriptive; a negative count is carried verbatim. `count` combined with `min_count`/`max_count` is rejected at validation, as today: the model represents one cardinality, and choosing one side would silently drop the other. |
| `one_of` items narrowed to strings | Enforced at validation. Non-string items are rejected; the old "any JSON value" acceptance is what main invalidated. |
| Property `aliases` removed | Enforced at validation as an unknown member. Node and relationship `aliases` remain accepted and descriptive. |
| `PROPERTY_TYPE` constraint | Accepted at validation (it is in main's enum) and fails at `apply` with a `Mapping` error: the constraint object carries no type token, and a property-type constraint the graph-type body cannot construct is not silently dropped. |
| Node constraint `label` | Accepted at validation. At `apply`, null/absent compiles against the owning node; a non-null value that is not one of the node's labels (identifying or implied) fails with a `Mapping` error naming both. |
| Vector dimension with no minimum | Any integer or null is accepted at validation. `apply` fails with a `Mapping` error when a present dimension is not positive, because a graph-type vector needs a positive dimension; absent/null stays unconstrained. A dimension on a non-vector token is carried and has no effect, as main describes it as the dimension "when it holds a vector". |

### 4. Stored documents are never re-validated

The gate and structural validation are a **`set`-time** policy. They are
not a property of documents at rest:

- `db.ontology.get` in the stored format returns the stored bytes
  verbatim; it does not parse them into the typed model or validate them.
- A cross-format `get` parses the stored bytes into ADR-0227 §4's value
  model (syntax only) and serializes canonically; it runs no version gate
  and no schema validation. A pre-bump document therefore still converts
  between JSON and YAML.
- The open path keeps only a syntax/value-model screen — UTF-8, one
  document, the JSON-equivalent value model, the depth cap — so a corrupt
  stored record still fails closed. It does not run the version gate or
  the structural validator. ADR-0120 §1's "validated" means validated
  when it was set, never re-validated at open.
- `db.ontology.apply` is the one operation that must interpret the
  stored document into the typed model. A pre-bump document fails there
  loudly — at the gate or on its first old-shape member — with no
  migration, coercion or partial conversion. `get` keeps returning its
  bytes unchanged.

No path except `set` and `drop` changes a stored document. Downgrading
after storing a main-shape document is fail-closed: a pre-bump binary
re-runs its own stored-document validation at open and refuses the
store; the bytes are intact and the document becomes readable again
after upgrading.

### 5. ADR-0226 §2 converter on the new shapes

#### 5.1 Renamed fields and identity

| Document | Typed model / converter |
|---|---|
| `version` (string) | Carried verbatim; descriptive. No numeric comparison anywhere. |
| node / relationship / property id | The map key. It is the element identity used for aliases, endpoint resolution and property references. The nullable `name` is display only and is never compiled. |
| `label` | Shorthand for `labels.identifier`; desugared at validation. The converter reads one identifying label, as today. |
| property `must_exist` | Renamed from `mustExist`; maps to `NOT NULL` or a standalone `EXISTS` constraint per §5.3. |
| constraint map key | The constraint's stable identity: carried in the typed model (arrays become ordered, id-keyed maps) and used in diagnostics; it is not compiled. The nullable `name` remains display, and, as in ADR-0226 §2, is carried as the graph-type constraint name when present. A duplicate graph-type constraint name is never silently renamed; it surfaces as a graph-type error. |
| constraint `type` | Uppercase enum: `EXISTS`, `KEY`, `UNIQUE` map to the corresponding graph-type verb; `PROPERTY_TYPE` fails per §5.3. |
| node constraint `label` | Null/absent → the owning node; a value matching one of the node's labels compiles against the owner; any other value fails per §5.4. |
| `RelationshipTarget` | `node` → the edge type's source/target alias; `label`/counts are descriptive and never resolved. Label-only and empty targets fail per §5.4. |
| `extensions` | Carried and deeply validated, never converted: node `indexes`/`display`, relationship `indexes`, document `tables`/`mappings` and every `custom` remain descriptive, exactly like the old extension envelopes. |

Source order of maps is preserved in the typed model, as ADR-0226 §1's
deterministic build and ADR-0227 §3's serialization require. Shorthand
flags and explicit constraint objects are compiled with the existing
rules: `key` subsumes `unique` and `must_exist`; an explicit object that
restates a flag of the same kind over the same single property replaces
the flag-derived constraint so the object (and its name) survives.

#### 5.2 `Neo4jType` token mapping

| Token(s) | Declaration in the graph-type body |
|---|---|
| `ANY`, `LIST<ANY>`, absent `type` | Declares nothing (ADR-0228): no property declaration; `must_exist` becomes a standalone `EXISTS` constraint. This is semantically faithful, not lossy — the document says "unconstrained". |
| `BOOLEAN`, `DATE`, `DURATION`, `FLOAT`, `INTEGER`, `LOCAL DATETIME`, `LOCAL TIME`, `POINT`, `STRING`, `ZONED DATETIME`, `ZONED TIME` | The graph-type predefined kind of the same name. |
| `LIST<X>` for each mapped `X` above | `LIST<X NOT NULL>` — graph-type property types have no nullable inner type (ADR-0228). |
| `UUID` | `PredefinedKind::Uuid`, the parser's existing token. |
| `VECTOR<FLOAT>` / `VECTOR<FLOAT32>` / `VECTOR<INTEGER>` / `VECTOR<INTEGER32>` / `VECTOR<INTEGER16>` / `VECTOR<INTEGER8>` | `VectorCoordKind` `Float64` / `Float32` / `Int64` / `Int32` / `Int16` / `Int8`, with the property's `dimension` when present. |
| `FLOAT32`, `LIST<FLOAT32>`, `INTEGER8`, `LIST<INTEGER8>`, `INTEGER16`, `LIST<INTEGER16>`, `INTEGER32`, `LIST<INTEGER32>` | **Fail loudly** with an `OntologyError::Mapping` naming the owner, property id and token. The parser's `PredefinedKind` has no plain 8/16/32-bit scalar and no plain `FLOAT32`; these exist only as vector coordinates. Widening them to `FLOAT`/`INTEGER` or declaring nothing would silently change the declared constraint, which the no-silent-fallbacks rule forbids. This is not the `ANY` case: `ANY` means unconstrained, while `FLOAT32` asserts a specific type the body cannot express. |

The old bare `TIME`, `DATETIME`, `LOCALTIME`, `LOCALDATETIME` and
`BYTES` tokens no longer exist in main's closed enum, so documents using
them are rejected at validation; the converter keeps no compatibility
mapping for them.

#### 5.3 Constraint mapping

| Document | Graph-type constraint |
|---|---|
| `must_exist: true` on a typed property | `NOT NULL` on the declaration. On a property whose token declares nothing (`ANY`, `LIST<ANY>`, absent), a standalone `EXISTS` constraint — the same split as today. |
| `unique: true` / `key: true` | Single-property `UNIQUE` / `KEY`; `key` subsumes `unique`. |
| Explicit `type: EXISTS` / `KEY` / `UNIQUE` | The corresponding verb over the listed property ids, composite when more than one, in listed order, with the object's `name` when present. The object-replaces-flag rule is unchanged. |
| Explicit `type: PROPERTY_TYPE` | **Fail loudly** at `apply` with `OntologyError::Mapping` naming the owner and constraint id. The constraint object carries no type token to compile, and the graph-type body cannot express the assertion; treating it as descriptive would silently under-enforce. |
| `properties: []` | **Fail loudly** at `apply` naming the owner and constraint id (see §3). |
| A property id in `properties` that the owning entry does not declare | **Fail loudly** at `apply` naming the owner, constraint and property: the document schema cannot express the cross-reference, and a dangling constraint would otherwise reach the graph-type body. |

Constraint `extensions` are carried and validated, never converted.

#### 5.4 The loud mapping errors

`apply` fails with `OntologyError::Mapping` (never a warning, silent
skip or default) for: an endpoint with no `node`; a node with no
identifying label; a property token with no plain graph-type counterpart
(`FLOAT32`, `INTEGER8`/`INTEGER16`/`INTEGER32` and their list forms); an
empty-named property that would be declared or constrained; an empty
constraint property list; a `PROPERTY_TYPE` constraint; a node
constraint `label` that is not one of the node's labels; a non-positive
vector dimension; a constraint property id missing from its owner; and
the existing unknown endpoint node key. Each error names the owner (node,
relationship or document), the construct and, where applicable, the
property or constraint id and token.

### 6. Example fixture: CypherLite-authored, main-shaped, validation-only

`schema/example.json` is replaced with a CypherLite-authored document in
main's shape. It must cover:

- every `Neo4jType` token, including the list forms, the vector element
  types, `UUID`, `ANY`/`LIST<ANY>`, and the four plain scalars that fail
  at `apply` (`FLOAT32`, `INTEGER8`, `INTEGER16`, `INTEGER32`) — the
  example pins the validator, not convertibility;
- all four constraint types (`EXISTS`, `KEY`, `PROPERTY_TYPE`,
  `UNIQUE`), composite and single-property, with names and null names;
- nullable and absent forms: a node using `label`, a node using only
  `labels.identifier`, a node with a null identifier, a null property
  flag and dimension, a null constraint name, and a null/absent endpoint
  count;
- endpoint forms: node-referenced, label-only (accepted, not
  convertible) and empty;
- empty property names and an empty constraint `properties` array (both
  accepted, neither convertible);
- every typed extension member: node indexes covering each `IndexType`,
  with `FULLTEXT`/`POINT`/`VECTOR` options and a null `options`, node
  `display`, relationship indexes, document `tables` with columns and
  foreign keys, `mappings` of all three kinds with
  `TargetMapping`/`PropertyMapping`, and `custom` objects at each level.

The example is deliberately not an apply fixture; `apply` keeps its own
convertible fixtures. `schema/README.md` records the provenance: main @
`066515593ae59317d758bcf2cef54195e74bde39`, no root-level upstream
example (the pinned README's curl URLs are stale), the upstream Go
testdata file's two invalid endpoint `property` members, and this
fixture's ownership and validation-only role. The upstream file is not
vendored — verbatim it is invalid, and stripped it duplicates coverage
the authored example already provides.

## Alternatives considered

- **Follow main and accept any `schema` string.** Rejected: it removes
  the fail-loud version gate ADR-0121 §1 promises; a document from a
  later dialect would be misread as this one.
- **Accept both the old and new URIs (dual dialect).** Rejected: the
  shapes are incompatible, so the old URI would still fail structural
  validation member by member; a dual gate would only obscure which
  dialect was accepted, and maintaining two validators doubles the
  surface.
- **Accept any `…/1.0.0/…` URI or a prefix match.** Rejected: the draft
  and main both call themselves 1.0.0 but are different dialects; the
  exact URI is the only discriminator. ADR-0227 §7 already rejected
  lenient matching.
- **Migrate stored documents at open or `get`.** Rejected: it rewrites
  bytes the store promises to keep verbatim, needs a version marker the
  store does not have, and makes an old binary's read path the migration
  engine. Hard cutover plus "stored documents are never re-validated" is
  simpler and keeps every byte.
- **Reject pre-bump documents at open.** Rejected: it evicts readable
  stored data. Hard cutover governs what `set` accepts, not what the
  store may still return.
- **Keep CypherLite's draft strictness everywhere** (reject nullable
  labels, label-only endpoints, empty property names and empty constraint
  lists at validation). Rejected: main-valid documents the store can
  represent would be refused; validation would encode the converter's
  limitations. The strictness that remains is only where the converter
  genuinely cannot express the construct, and it fires at `apply`.
- **Resolve label-only endpoints by label, skip empty constraints, and
  prefer one side of a conflicting `label`/`labels.identifier` pair.**
  Rejected: labels are not unique, and each choice silently discards
  author intent. Named mapping errors are the honest outcome.
- **Map `FLOAT32`/`INTEGER8`/`INTEGER16`/`INTEGER32` to
  `FLOAT`/`INTEGER`.** Rejected: silent widening changes the declared
  type and violates the no-silent-fallbacks rule.
- **Treat `PROPERTY_TYPE` as descriptive.** Rejected: it asserts an
  enforceable type constraint; dropping it would under-enforce while
  reporting success.
- **Validate only the extension envelope keys.** Rejected: main defines
  the payloads, so envelope-only validation accepts documents main
  rejects and lets a malformed `Table` or `NodeIndex` through `set`.
- **Vendor the upstream Go testdata example, verbatim or with the two
  invalid `property` members stripped.** Rejected: verbatim it is an
  invalid document, and stripped it duplicates coverage the authored
  example already has; a README note records the upstream defect instead.
- **Keep the pinned draft's `example.json`.** Rejected: it no longer
  validates against the new gate and would mislead as the canonical
  example.
- **Adopt a full JSON Schema engine.** Already rejected in ADR-0227 §6:
  a complete 2020-12 implementation and its transitive tree to validate
  one fixed, vendored schema. The bump does not change that trade-off.

## Consequences

- Every pre-bump document fails `set` when submitted. Users resubmit a
  main-shape document; there is no shim, no dual dialect and no rewrite.
  Pre-bump stored documents remain readable byte-for-byte via `get` in
  the stored format and convertible across formats, but `apply` refuses
  them until they are replaced.
- The stored document format does not change: no WAL magic, snapshot
  version or envelope change. A store holding a main-shape document is
  fail-closed on pre-bump binaries (their open-path validation rejects
  it), like any other forward-only content; the bytes are never lost.
- The implementation touches the 25 drift items and their code/doc
  sites: the document model (`version: String`, display names, id-keyed
  constraint maps, `RelationshipTarget`, closed extensions, the
  `Neo4jType` enum and its token mapping), the validator, the converter,
  the error text and constants, the executor's procedure path, the
  open-path screen, the examples, and the tests and docs that write
  documents.
- The authored `schema/example.json` is a **validator** fixture, not an
  apply fixture: it deliberately contains main-valid constructs that
  `apply` rejects (all tokens, all four constraint types, label-only and
  empty endpoints, empty property names and constraint lists), so it
  proves the accepted schema rather than convertibility. Apply keeps
  separate, convertible fixtures.
- `schema/README.md` records the new source pin, the new `$id`, the
  absence of a root-level upstream example, the upstream Go testdata
  defect (the two extra endpoint `property` members), and the
  CypherLite-authored fixture's role.
- ADR-0227's §7 gate text and §6 validation description are amended by
  §1/§2 above; ADR-0226 §2's converter rules are re-based on the new
  document shapes; ADR-0228's "declares nothing" rule survives and its
  token vocabulary is superseded by §5.2. ADR-0120's "validated" is
  clarified as set-time by §4. Each gains a Later references note.
- The spec version string stays `1.0.0`: the URI, not the semantic
  version, identifies the dialect. The gate error names the new URI so
  the distinction is visible to users.

## Later references

- ADR-0487 raises §1's gate to the published
  `https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json`
  (`neo4j/graph-spec` tag `release-spec/v1.1.0` @
  `5ff2c20aa7e428e7c563b17f573157293959a009`) and re-bases §2's shapes
  on it: node and relationship entries lose their display `name`,
  property `aliases` returns as a descriptive member, constraints of
  both owners lose `label` and share one closed shape, and document
  extensions rename `tables`/`mappings` to
  `data_source_tables`/`data_source_mappings`. §5.1/§5.4's
  constraint-`label` rules and §6's example are superseded; the hard
  cutover, the set-time-only policy (§4) and the §3 rulings stand.
