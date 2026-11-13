# ADR-0388: The graph-type trio (optional properties, default values, implied-label inheritance) behind three preview gates

Status: Accepted

Claim: ddb2c047a5851e2d7cb354634e5382c2b81b8111

## Context

Three accepted Cypher extensions grow the already-implemented graph-type
sublanguage in one coherent direction — richer element types:

- **Optional properties**: a bare `ANY` value type in an *element type's*
  property declarations (`prop :: ANY`) — the optional property. It
  restricts nothing (useful with the key/uniqueness shorthands and for
  closed graph types later); independent property type constraints keep
  rejecting `ANY`.
- **Property default values**: `DEFAULT ON COMMIT <value expression>` on
  an element-type property, restricted to literals and the allowed type
  constructors. The default applies at the transaction boundary when an
  entity is created or gains the identifying label without the property;
  an explicit non-null value wins, a same-transaction removal is ignored,
  and two unrelated element types both defaulting one property on a node
  fail instead of picking one.
- **Implied-label inheritance**: an implied label may name another node
  element type's identifying label — inheritance. The child inherits the
  parent's implied labels and property definitions; a re-specification
  must repeat exactly or further restrict; a diamond intersects the
  parents' effective definitions (a parent not defining the property
  contributes `ANY`); inheritance must be acyclic; an inherited node
  element type cannot be dropped while a child implies it.

None of the three is part of the pinned openCypher TCK floor, so per
ADR-0371 each ships behind its own reserved `CYPHERLITE_PREVIEW` row —
`graph-type-opt-props`, `graph-type-default-values`,
`graph-type-implied-labels` — independently enableable, as the registry
already reserved.

The three interlock: default values' kernel motivation is the optional
property (the `ANY DEFAULT <value>` form), and the default-value
inheritance interaction (two unrelated declaring element types, two
declaring ancestors in one chain) is specified against implied-label
inheritance. One ADR covers the trio because they are one decision: the
same grammar family, one gate family, one semantic layer.

## Decision

Adopt all three, each behind its own ADR-0371 gate, as one graph-type
semantic layer:

- **Grammar (default values only; the other two need none)**: the
  parser's graph-type property list gains an optional `DEFAULT ON COMMIT
  <value expression>` clause after the value type, parsed as a full
  expression (grammar-only, like the rest of a graph-type body; the
  expression parser's existing ADR-0079 depth limit covers it — no new
  recursion family). `DEFAULT`/`COMMIT` match as non-reserved words; `ON`
  is the keyword token.
- **Gates (ADR-0371)**: two halves, the same authored error.
  - *In-body* (the statement's own text): the front's statement
    admission runs one `GraphTypeGates` collector over the parsed AST —
    a bare `ANY` element-type property, a `DEFAULT ON COMMIT` clause, an
    implied label naming another in-body node element type's identifying
    label — and each present construct requires its feature before
    anything of the statement lowers or plans.
  - *Stored-state* (constructs that only materialize against the current
    graph type — an `ADD` whose identifying label a stored node element
    type already implies): the store's resolved `PreviewFeatures`
    installs on the executor's shared `StoreServices` bundle
    (`Graph::configure_preview` at open, `Graph::preview()`), and the
    graph-type semantic layer (`validate_graph_type_body`,
    `apply_verb`) fails closed with the same authored error. WAL replay
    and the `db.ontology.apply` path install the spec directly and never
    re-validate, so a follower without the env var replays a gated
    store's graph type unchanged.
- **Optional-property semantics**: the bare `ANY` property type is
  allowed in element-type property sets (deriving a vacuous `ANY`
  property-type constraint, matching the proposal's SHOW CONSTRAINTS
  rows); `ANY NOT NULL` keeps its existing collapse to an existence
  constraint; independent property type constraints keep rejecting
  `ANY`; a default value on an `ANY`-typed property must still be
  storable (a map is not).
- **Default-value semantics**: one typed `DefaultValue` enum (in
  `graph_type::default_value`) is every consumer's single source — the
  shape check (literal or allowed constructor, `uuid()` without
  arguments), the storable check, the conformance check against the
  declared value type (the type-lattice subsumption `canon_allows`,
  shared with the implied-label restriction check), the canonical
  `DEFAULT ON COMMIT <CVE>` rendering (single-quoted strings), the serde
  persisted model, and the evaluation (rebuilt to an expression and
  evaluated through the engine's own evaluator, so `uuid()` behaves
  exactly as in a query). The default applies at the statement boundary
  — the statement is this engine's transaction, and a `CALL { ... } IN
  TRANSACTIONS` batch its own boundary inside the statement — after the
  writes and before the post-statement constraint validation, in
  `exec::property_defaults::apply_property_defaults`, reading the redo
  slice (created nodes' labels, `AddLabels`, created relationships) and
  writing through the overlay. A node carrying a parent's and a child's
  label takes the child's (descendant) default; two unrelated declaring
  element types on one node fail; two declaring ancestors in a child's
  inheritance chain fail at declaration time.
- **Implied-label semantics**: one inheritance module
  (`graph_type::inherit`) holds the graph — edges, the iterative
  three-color cycle check, the effective property resolution (a child's
  declaration or the pointwise intersection of its parents' effective
  definitions), and the effective default resolution with provenance
  (declaring labels). Enforcement rides the derived constraints: the
  child's derived label-existence constraint (child label requires
  parent label) makes the parent's own derived constraints apply to the
  child's nodes; implicit inheritance derives no duplicate child
  constraints. DROP protects a node element type a surviving child still
  implies; `SHOW CURRENT GRAPH TYPE` renders the canonical child
  declarations only (never inlined), and `SHOW CURRENT GRAPH TYPE AS
  GRAPH` points an implied label's `IMPLIES` edge at the parent's
  `NodeElementType` node.
- **Two catalog fixes the trio exposed** (pre-existing, latent): a node
  element type's *second* implied label used to look schema-equivalent to
  its first at derived-constraint registration and was silently skipped
  (never enforced), and the catalog's own duplicate rejection treated
  two same-label label-existence constraints as one rule. Both now
  account for the enforced label (`sync_derived_constraints` compares
  it through the theater's derived-constraint index; `Catalog::
  insert_constraint` exempts the enforced-label kinds from the
  equivalence rejection). Also, the persisted property-type model now
  carries `UUID` (the showcase is a UUID property defaulting to
  `uuid()`; the historical narrowing of UUID properties to `STRING`
  broke the SHOW/DROP roundtrip).
- **cip-eval**: the ratchet runs in two phases — every ungated suite's
  scenarios first (their pinned rejections of the gated constructs stay
  errors), then the env var set and only the gated suites' scenarios
  (`run_cip_eval_selecting`). The trio gains three scenario directories
  (45 doc-derived scenarios, 45 passing) and three baseline entries;
  the baseline only ratchets up.

## Alternatives considered

- **Gate everything at statement admission only.** Rejected: the
  stored-state construct (an `ADD` whose identifying label a stored type
  already implies) appears only against stored graph-type state, which
  the AST cannot see; the executor-side half of the gate is required to
  fail it closed with the same authored error.
- **Gate everything in the executor only.** Rejected: it would let a
  gated statement lower and plan before failing, and it would need the
  preview set threaded through every DDL dispatch layer; the front
  admission check (the ADR-0379 shape) fails earlier with less plumbing.
- **Apply defaults at write time instead of the statement boundary.**
  Rejected: the proposal fixes the observable semantics at transaction
  commit (the creating statement's RETURN still sees null); the
  statement is this engine's transaction, so the boundary placement
  matches exactly. Write-time application would make the creating
  statement see the value and would miss the same-statement-removal
  rule.
- **Inline inherited definitions in `SHOW CURRENT GRAPH TYPE`.**
  Rejected: the proposal keeps the canonical specification as declared;
  the inlined "effective graph type" is a future SHOW variant, not part
  of this trio.
- **Three separate ADRs.** Rejected: the three features are one coherent
  decision — one grammar family, one gate family, one semantic layer,
  interlocking semantics specified against each other; three ADRs would
  cross-reference the same modules and gates three times over.

## Consequences

- The three features are independently enableable via
  `CYPHERLITE_PREVIEW`; ungated, each construct fails closed with the
  authored preview error (the graph-type body validation's earlier
  rejections of these constructs, including the "non-storable value
  type" rejection, are superseded *only* under the flags).
- The statement-boundary default application adds one redo-slice pass
  per write statement on a store whose graph type has defaults (or
  inheritance); a cheap committed-theater gate
  (`Graph::has_property_defaults`) skips the overlay read on every
  other store.
- The persisted graph-type model carries `UUID` and the default value
  (serde-shaped, backward-compatible defaults); old persisted images
  deserialize unchanged.
- The two catalog fixes change pre-existing (ungated) behavior: a node
  element type with two implied labels now enforces both. No pinned
  scenario regressed (the cip-eval baseline diff is purely additive;
  the TCK stays 3892/3892).
- A later closed-graph-type extension will build on the optional-property
  form (its stated motivation).
