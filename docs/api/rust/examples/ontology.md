# Ontology example

Run with `cargo run -p relate --example ontology`.

The example opts in by setting `RELATE_PREVIEW=ontology` before
`Database::open` (ADR-0378): the four `db.ontology.*` procedures are
preview-gated, so without that opt-in every call fails with:

```text
semantic error: SemanticError: preview feature 'ontology' is not enabled: set RELATE_PREVIEW=ontology
```

`SHOW PROCEDURES` omits the four rows while the opt-in is off and lists
them with unchanged signatures and modes once it is set. Deployments that
manage the ontology must export the variable in the host or server
environment before open.

The `db.ontology.*` procedures (ADR-0121, ADR-0226, ADR-0227, ADR-0229,
ADR-0378 — Neo4j's CIP-287 surface) manage the store's single ontology
document: `set`
validates a JSON or YAML document against the published Ontology Graph
Spec 1.1.0 schema — the exact gate URI is
`https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json`
(ADR-0364, raised by ADR-0487) —
and stores the submitted bytes verbatim, `get` returns it in the
requested format (`yaml` by default, `json` converted canonically), `apply`
converts it to the database's current graph type with `ALTER CURRENT GRAPH
TYPE SET` semantics (failing when existing data violates it), and `drop`
removes it. The accepted document is 1.1.0-shaped: `version` is a string,
property existence is `must_exist`, and `constraints` is an id-keyed map
with `type: EXISTS | KEY | UNIQUE | PROPERTY_TYPE`; a pre-bump document is
rejected at `set`. The stored document is the sole source of truth — the
`system_ontologies` namespace is visible under the same
`RELATE_PREVIEW=ontology` opt-in (ADR-0593) and not queryable
(ADR-0229): every other statement routed there, reads included, fails
with 42N17, so the example reads the namespace's listing and pins that
failure. Without the opt-in the namespace is invisible — the listing
shows the two CE rows and `USE` fails as an unknown database.
