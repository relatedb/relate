---
type: adr
area: engine
date: 2026-09-27
deciders: CypherLite maintainers
name: 0121-ontology-procedures
status: accepted
tags:
  - ontology
  - procedures
title: "`db.ontology.*` procedures manage the single ontology"
REFERENCES:
  - "[[adr/0100-wasm-size-optimized-build]]"
  - "[[adr/0103-engine-surfaces-testing-layout]]"
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0120-ontology-document-persistence]]"
  - "[[adr/0146-unknown-namespace-selection-is-graph-not-found]]"
  - "[[adr/0226-ontology-mappings]]"
  - "[[adr/0227-ontology-document-format]]"
  - "[[adr/0229-remove-system-ontologies-cypher-surface]]"
  - "[[adr/0371-preview-feature-gating]]"
  - "[[adr/0378-ontology-procedures-preview-gate]]"
  - "[[adr/0593-system-ontologies-visibility-preview-gate]]"
---

# 0121. `db.ontology.*` procedures manage the single ontology

## Context

Neo4j's ontology persistence proposal defines the procedures for ontologies
stored in `system_ontologies`:

- `db.ontology.set(database, ontology)` imports an Ontology Spec v1 JSON
  document;
- `db.ontology.get` exports it;
- `db.ontology.drop` deletes it;
- `db.ontology.apply` converts it to a graph type and applies it to the target
  database, replacing that database's current graph type like
  `ALTER CURRENT GRAPH TYPE SET { … }`.

Neo4j limits who may write with `EXECUTE BOOSTED` privileges, and plans
`ontology.fromGraphType()` and `ontology.fromGraph()` as extensions. The
proposal still marks its JSON-to-graph and graph-to-graph-type mappings as
TODO.

[ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) makes `system_ontologies` read-only to Cypher and limits it to one
ontology, for `neo4j`. [ADR-0120](./0120-ontology-document-persistence.md) persists that ontology as a document.
CypherLite has no role-based access control.

## Decision

1. **Signatures follow Neo4j.**
   - `db.ontology.set(database :: STRING, ontology :: STRING)` validates the
     document against a supported Ontology Spec version and replaces the
     stored ontology. A newer spec version fails, naming the highest
     supported version.
   - `db.ontology.get(database :: STRING) :: (ontology :: STRING)` returns the
     stored document unchanged, or `null` when none is set.
   - `db.ontology.drop(database :: STRING)` removes it. Dropping when none is
     set succeeds.
   - `db.ontology.apply(database :: STRING)` converts the stored ontology to a
     graph type and applies it to `neo4j` with `ALTER CURRENT GRAPH TYPE SET`
     semantics, replacing the current graph type. It fails when no ontology is
     set, or when the data violates the resulting graph type.

   Where Neo4j's final signatures differ, for example in the name of `get`'s
   output column, CypherLite follows Neo4j.
2. **One database.** `database` must name `neo4j`, matched case-insensitively.
   An unknown name fails with `DatabaseNotFound` and `Database does not exist.
   Database name: '<name>'.`. `system` and `system_ontologies` fail with
   `Ontologies can only be stored for the user database 'neo4j'.`
3. **Transactions.**
   - `set` and `drop` run only in implicit (auto-commit) transactions. Inside
     an explicit transaction they fail with `'db.ontology.set' can only be
     executed in an implicit transaction, but tried to execute in an explicit
     transaction.` (or the same text naming `drop`). The wording follows
     Neo4j's error for `CALL { … } IN TRANSACTIONS`.
   - A statement that calls `set` or `drop` may not write anything else.
     Otherwise it fails with Neo4j's `Writing to more than one database per
     transaction is not allowed.` (42N03).
 - `get` reads the calling transaction's captured snapshot ([ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §5).
   - `apply` writes only `neo4j` and may run in explicit transactions.
4. **No privileges.** Any caller may run every procedure. The proposal's
   `EXECUTE BOOSTED` grants and property-based access rules do not apply.
5. **The mapping follows Neo4j.** The document-to-graph mapping and the
   ontology graph type implement Neo4j's published mapping for Ontology Spec
   v1. That includes the `ontology` property naming the database on every node
   and relationship, so queries against `system_ontologies` work the same in
   both products. CypherLite ships no mapping of its own, and implementing
   these procedures waits until Neo4j publishes it.
6. **Always built.** The procedures are part of the Cypher surface in every
   build: all CLI bundles, the Rust crate, the language bindings, Bolt and
   WASM. They are not a Cargo feature. The mapping lives in a new engine crate,
   `cypherlite/cypherlite-ontology`, that `cypherlite` always depends on. That
 follows [ADR-0103](./0103-engine-surfaces-testing-layout.md)'s R1, and the crate adds no heavy dependency (R6).
7. **Listed like other procedures.** `SHOW PROCEDURES` lists them with their
   signatures and modes.

## Alternatives considered

- **Allow Cypher writes to `system_ontologies`.** Rejected: nothing enforces
  the structure of the ontology graph until closed graph types exist, which is
  the same reason the Neo4j proposal gives.
- **Several ontologies keyed by database.** Rejected: CypherLite has one user
  database.
- **Allow `set` and `drop` in explicit transactions.** Rejected: later
  statements in the transaction would need to read a graph derived from an
  uncommitted document.
- **Set and apply atomically in one call.** Rejected for now: the proposal
  keeps them separate, and applying can fail on data that does not match. The
  single WAL could make a combined call atomic later.
- **A Cargo feature.** Rejected: every build ships the procedures, so a
  feature would only add build combinations to test.
- **An interim CypherLite mapping.** Rejected: a different graph shape would
  break queries and tools that also run against Neo4j.

## Consequences

- Ontology tooling built for Neo4j can push, read and apply an ontology in
  CypherLite, over Bolt or any embedding API, without privileges.
- Implementation waits for Neo4j's Ontology Spec v1 and its mapping. The
 namespace work in [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) does not.
- The procedures need a reference page with runnable examples. Their code adds
 to the WASM artifact that [ADR-0100](./0100-wasm-size-optimized-build.md) optimizes for size, so the size change is
  measured when they land.
- `ontology.fromGraphType()` and `ontology.fromGraph()` remain future work,
  each needing its own ADR.

## Later references

- [ADR-0146](./0146-unknown-namespace-selection-is-graph-not-found.md): §2 quotes the unknown-database text [ADR-0146](./0146-unknown-namespace-selection-is-graph-not-found.md) supersedes (the
  future `db.ontology.*` database argument takes the live 42N00 form).
- [ADR-0226](./0226-ontology-mappings.md) supersedes §5's wait-for-Neo4j stance: CypherLite pins its own
  document→`system_ontologies` and ontology→graph-type mappings, designed
  to feed back into Neo4j's WS2 work, and pins how §1's "fails when the
  data violates the resulting graph type" is implemented (validate the
  captured `neo4j` snapshot against the derived constraints before
  committing the `SetCurrentGraphType` op).
- [ADR-0227](./0227-ontology-document-format.md) amends §1 and §2, following the ontology-privileges proposal
  (revision of 8 Oct 2026)
  under §1's own follow-Neo4j rule: the database argument is removed — the
  procedures act on the database they are executed against (the session
  database or the `USE` target), so §2's `Ontologies can only be stored
  for the user database 'neo4j'.` now applies to the session/USE target
  and unknown names fail at `USE` routing (42N00). `db.ontology.get` gains
  a `format` parameter defaulting to `"yaml"`: it returns the stored bytes
  unchanged when the requested format matches the stored one and converts
  canonically otherwise. `db.ontology.set` accepts JSON or YAML and stores
 the submitted bytes verbatim. [ADR-0227](./0227-ontology-document-format.md) also pins the spec-version gate
  §1 sketches (pinned 1.0.0 draft URI; the error names the highest
  supported version). The database argument may return as an optional
  future extension.
- [ADR-0229](./0229-remove-system-ontologies-cypher-surface.md) voids §5's queryability premise ("queries against
  `system_ontologies` work the same in both products"): no statement
  routed there runs — the namespace is listed but not queryable — so the
  procedures are the only Cypher surface for the ontology. Routed to
 `system_ontologies`, the procedures now fail [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §4's namespace
 gate with [ADR-0229](./0229-remove-system-ontologies-cypher-surface.md)'s 42N17 text before [ADR-0227](./0227-ontology-document-format.md) §1's `Ontologies can
  only be stored for the user database 'neo4j'.` is reached; run against
  `neo4j` they are unchanged.
- [ADR-0378](./0378-ontology-procedures-preview-gate.md) gates §6's four procedures behind `CYPHERLITE_PREVIEW=ontology`
  while the Ontology Graph Spec contract is not final: with the
  opt-in off, `db.ontology.set`, `get`, `drop` and `apply` fail with the
 [ADR-0371](./0371-preview-feature-gating.md) authored preview error and `SHOW PROCEDURES` omits their rows;
  §6 still holds for the code (every build compiles it in), not for
  callability. §7's listing applies unchanged once the opt-in is on.
- [ADR-0593](./0593-system-ontologies-visibility-preview-gate.md) extends the same `ontology` opt-in to §7's namespace:
  `system_ontologies` is visible only while the opt-in is set —
  unlisted by `SHOW DATABASES` and refused as an unknown database
  otherwise — so §7's listing, like §6's callability, rides the opt-in.
