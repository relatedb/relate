# 0229. Remove the `system_ontologies` Cypher surface

- **Status**: Accepted
- **Date**: 2026-10-13
- **Deciders**: CypherLite maintainers (Linear LABS-132)

## Context

ADR-0116 makes `system_ontologies` a fixed namespace: listed by `SHOW
DATABASES` (§6), its name accepted everywhere a namespace name is parsed
(§3), readable with ordinary Cypher, writes failing 42N18 (§1, §4).
ADR-0120 §2 derives its graph from the stored document at open and after
every committed set or drop; ADR-0226 §1 pins the mapping that builds it —
six labels, seven relationship types, document-order element ids, the
fixed graph type — implemented in
`cypherlite/cypherlite-ontology/src/mapping.rs` and the facade build
(`build_system_ontologies_graph`) in `cypherlite/cypherlite/src/ontology.rs`.

Neo4j's own document→graph mapping is still unpublished: the persistence
proposal's JSON-to-graph section remains TODO, and ADR-0226 §1 is
CypherLite's design, meant as input to Neo4j's WS2 work, not a copy of its
output. Neo4j's ontology-privileges proposal (revised 8 Oct 2026)
meanwhile steers away from a visible namespace: the `system_ontologies` database is
to be "not initially visible through any SHOW command or Cypher clause".

Phase 2 of this workstream probed a live `neo4j:2026.09-community`
container and verified that CE 2026.09 allows reads on `system_ontologies`
(writes fail 42N18); the database is a listed, read-only CE database. A
queryable namespace is therefore live-CE parity — and shipping it would
expose, as a public Cypher surface, a graph whose shape is CypherLite's
guess at an unpublished mapping. When Neo4j publishes its mapping, every
query written against ours breaks, and removing reads then is a
backward-incompatible tightening. ADR-0116's own rule applies: relaxing an
error later is backward compatible, tightening one is not — so the
tightening is taken now, before the surface ships.

The user has decided: the Cypher surface for `system_ontologies` is
removed "for now" — we do not know what the ontology graph will look like,
so the stored ontology is reachable only through the `db.ontology.*`
procedures. This ADR records that decision and the exact error that
replaces the surface.

## Decision

1. **Listed, not queryable.** `system_ontologies` stays a fixed, listed
   namespace. `SHOW DATABASES` keeps its row with ADR-0116 §6's values
   verbatim (`type` standard, `access` read-only, `currentStatus` and
   `requestedStatus` `online`, `default` and `home` false, empty `aliases`
   and `constituents`, the stored `defaultLanguage`), and the name stays
   accepted everywhere a namespace name is parsed: session-namespace
   selection over the embedding APIs and Bolt (ADR-0117), a static `USE`
   target (ADR-0116 §3), and the CLI `--namespace` flag (ADR-0118). But
   every statement routed to it fails — reads included.

   What still runs while routed to `system_ontologies` is exactly
   ADR-0116 §2's database-administration commands — `SHOW DATABASE[S]
   [name]`, `SHOW DEFAULT DATABASE`, `SHOW HOME DATABASE`, `ALTER
   DATABASE … SET DEFAULT LANGUAGE` — which act on the store catalog,
   never on a namespace's graph, and keep §2's any-namespace rule. Every
   other statement routed there — data clauses, DDL, procedure and
   function calls (`db.ontology.*` and `db.info()` included) — fails with
   the error in (2), whether it reads or writes.

   The `db.ontology.*` procedures are unaffected: they act on the stored
   document and on `neo4j`'s graph type, never on the namespace's graph.
   Run against `neo4j` they behave exactly as ADR-0227 §1 pins; routed to
   `system_ontologies` they now fail in (2)'s gate, before ADR-0227 §1's
   `Ontologies can only be stored for the user database 'neo4j'.` (that
   text now fires only on `system`).

2. **The error.** Every statement routed to `system_ontologies` — read or
   write, `USE`-routed or session-routed, wrapped in `EXPLAIN` or
   `PROFILE` included — fails at the plan-based semantic gate (the layer
   of ADR-0116 §4's gates, above the EXPLAIN early-return) with:

   - GQLSTATUS **42N17**, the family of `unsupported_system_clauses`
     (`Neo.ClientError.Statement.SemanticError`; the class-level
     `SemanticError` kind token, no new `ErrorKind` variant), raised by a
     new formatter in `cypherlite/cypherlite-core/src/ce_messages.rs`;
   - message text, exactly:
     `This database is not queryable. Use the db.ontology.* procedures on the user database 'neo4j' instead.`
     so the engine renders
     `semantic error: SemanticError: This database is not queryable. Use the db.ontology.* procedures on the user database 'neo4j' instead.`

   42N17 fits where 42N18 would not: the namespace rejects every clause,
   not just writes, and 42N17 is already the namespace-restriction code
   ("'MATCH' is not allowed on the system database"). The text follows the
   family's phrasing (`read_only_write`'s "this database",
   `ontology_neo4j_only`'s "the user database 'neo4j'",
   `element_id_no_namespace`'s imperative pointer) and is CypherLite's
   own: live CE has no counterpart error, because live CE allows reads.
   The ADR-0116 §4 42N18 arm for `system_ontologies` is removed with it;
   `WriteOnReadOnlyAccessDatabase` stays the recorded CE text for
   read-only databases (ADR-0116 §9).

3. **No derived graph.** ADR-0120 §2 is superseded: no
   `system_ontologies` graph is built at open or after a committed set or
   drop. The document stays the sole stored source of truth (ADR-0120 §1,
   as amended by ADR-0227: the validated ontology document, JSON or YAML,
   kept verbatim). The store formats do not change: ADR-0120 §3's redo ops
   and 16 MiB envelope, §4's `MDBWAL02` segment magic, §5's snapshot
   object, §6's inline `ontology` field, §7's bundles and §8's
   format-follows-content all stand, and a store holding an ontology is
   persisted exactly as today. ADR-0116 §10's `system_ontologies` slot
   holds an empty graph, permanently — never built, never replaced — and
   §5's transaction capture of it captures that empty graph; keeping or
   dropping the capture is an unobservable implementation choice.
   `set`/`drop` validation keeps ADR-0227 §4–§7's parse, structural
   validation and version gate; only the derived-graph build check goes.

4. **The mapping code is deleted; ADR-0226 §1 is not shipped.** The
   document→graph mapping code is deleted from the shipped crate:
   `build_ontology_graph` and its plan types
   (`cypherlite/cypherlite-ontology/src/mapping.rs`, `mapping/`), the
   fixed graph type `system_ontologies_graph_type()` (`graph_type.rs`),
   and the facade build and commit pre-build (`build_system_ontologies_graph`
   and the graph half of `CommittedOntology`,
   `cypherlite/cypherlite/src/ontology.rs`). ADR-0226 remains Accepted as
   the record of the mapping design, marked **not shipped** until Neo4j
   publishes its mapping: then it is revived from git history, with a new
   ADR and a Later-references note there, per ADR-0226's own Consequences
   rule. What stays shipped of ADR-0226 is §2's ontology→graph-type
   converter `to_graph_type_body` (with ADR-0228's correction) and §3's
   validate-then-commit data-violation check — both serve `apply` (6).

5. **Element ids: decode-only.** The `cypherlite.system_ontologies`
   segment of ADR-0116 §7 stays decode-only. No id in that namespace is
   ever produced now — no graph, no elements. `db.nameFromElementId` keeps
   decoding the segment to `system_ontologies`, with today's errors for
   anything else; `graph.byElementId` with such an id resolves to the
   namespace and fails in (2)'s gate. An id whose middle segment names no
   namespace keeps failing 42NA7 (ADR-0116 §3).

6. **`apply` is unchanged.** `db.ontology.apply` converts the stored
   document to a graph type through ADR-0226 §2's converter (ADR-0228's
   correction included), validates the calling transaction's captured
   `neo4j` snapshot against every derived constraint first (ADR-0226 §3),
   and applies it to `neo4j` with `ALTER CURRENT GRAPH TYPE SET`
   semantics. No namespace graph is involved — there never was one.

## Alternatives considered

- **Full invisibility — remove the `SHOW DATABASES` row too, the
  proposal's direction.** Rejected: the row keeps CE parity with the ADR-0116 model —
  three fixed namespaces, one catalog, one element-id decode — and hiding
  it is the larger surface churn: `SHOW DATABASES` consumers, §6's values
  and the routing surfaces would all need their own rules for a name that
  still parses. A listed-but-not-queryable row answers "what is this
  database" without exposing a guessed shape. Following the proposal the
  rest of the way is a new ADR.
- **Keep the derived graph, hidden behind the new gate.** Rejected: it
  pays the build cost at open and after every set or drop for a graph no
  statement can ever read, and it keeps an unpublished shape compiled into
  the shipped crate — re-derived and drifting with every vendored-schema
  bump — for nothing.
- **Keep the mapping code dormant — compiled, never called.** Rejected:
  dead API in a shipped engine crate — unexercised on the product path,
  pinned only by its own unit tests, one refactor away from being
  re-enabled by accident. Git history is the cheaper archive.
- **Follow live CE exactly — allow reads, keep ADR-0116 §4 as
  implemented.** Rejected: it ships a public Cypher surface over a
  guessed shape; queries written against it break when Neo4j publishes
  its mapping, and removing reads then would be the backward-incompatible
  tightening this ADR takes now, taken against more users.
- **Reject the name at routing with 42N00 instead.** Rejected: the
  database is listed, so `Graph not found: system_ontologies` is a lie; it
  breaks the session-selection surfaces that legitimately accept the name
  (ADR-0117, ADR-0118); and it gives no pointer to the procedures.
- **Reuse `read_only_write` (42N18) for reads.** Rejected: 42N18 means a
  write on a read-only database — neither its text nor its code fits a
  read, and the namespace rejects every clause, which is 42N17's class.

## Consequences

- **Deliberate divergence from live CE, stated plainly:** phase 2 verified
  that live Neo4j CE 2026.09 allows reads on `system_ontologies`.
  CypherLite now rejects reads too, moving toward the proposal's "not
  initially visible through any SHOW command or Cypher clause" while
  keeping the `SHOW` listing for CE parity with the ADR-0116 model. The
  fixture (`testdata/ce-errors/neo4j-ce.json`) gains no case for this
  error — there is no live-CE text to pin — so the authored 42N17-family
  text in (2) is the source of truth, and the divergence is recorded
  here, never edited away.
- The namespace's observable behavior collapses to: listed in `SHOW
  DATABASES`, accepted as a name, and every non-administration statement
  routed to it failing with (2)'s text — `EXPLAIN`-wrapped included, and
  `USE system_ontologies CALL db.ontology.get()` included, which
  previously failed with ADR-0227 §1's `Ontologies can only be stored for
  the user database 'neo4j'.` and now fails in the namespace gate.
- Open, set and drop get cheaper: no graph build, no commit pre-build, no
  fixed graph type installed. Store formats, WAL ops and snapshot layouts
  are untouched, and ADR-0120 §8's older-binary rules stand. The WASM
  artifact that ADR-0100 optimizes for size shrinks with the deleted
  mapping; the delta is measured when the removal lands.
- ADR-0226 §1's mapping, its fixed graph type and its deterministic ids
  are not shipped. When Neo4j publishes its mapping, reviving the derived
  namespace is a new ADR that restores the code from git history.
- Tests pin (2)'s exact code and text: reads and writes, `USE`-routed and
  session-routed, `EXPLAIN`-wrapped statements, the still-passing
  administration commands, `db.nameFromElementId` still decoding the
  segment, and `apply` unchanged.
- The "Cypher dialect" paragraph of `AGENTS.md` (the three fixed
  namespaces' behavior) needs its own follow-up update — deliberately not
  folded into this decision. `docs/limitations.md`, the `docs/api/*`
  specs that describe reading the namespace, and the
  `docs/api/rust/examples/ontology.md` example (which reads the derived
  graph) change with the implementation.
- ADR-0116, ADR-0120, ADR-0121 and ADR-0226 gain Later-references notes;
  so do ADR-0227 and ADR-0228, whose `system_ontologies` composition and
  queryability statements this supersedes.
