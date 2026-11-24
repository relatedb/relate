# Ontology example

Run with `cargo run -p relate --example ontology`.

The example opts in by setting `RELATE_PREVIEW=ontology` before
`Database::open` (ADR-0378): the four `db.ontology.*` procedures are
preview-gated, so without that opt-in every call fails with:

```text
semantic error: SemanticError: preview feature 'ontology' is not enabled: set RELATE_PREVIEW=ontology
```

`SHOW PROCEDURES` omits the four rows while the opt-in is off and lists
them once it is set. Deployments that manage the ontology must export the
variable in the host or server environment before open.

The `db.ontology.*` procedures (ADR-0121, ADR-0226, ADR-0227, ADR-0229,
The `db.ontology.*` procedures (ADR-0121, ADR-0226, ADR-0227, ADR-0229,
ADR-0378) manage the store's single ontology document, their contracts
revised (ADR-0667 — see the
[ontology procedures reference](../../../reference/ontology-procedures.md)).
In the example: `set` validates a YAML document in the published
Ontology Graph Spec 1.1.0 shape and stores its canonical form; `get`
returns it re-rendered in the requested format (`yaml` by default,
`json`); `apply` converts it to the database's current graph type with
`ALTER CURRENT GRAPH TYPE SET` semantics and answers its delta row — the
applied model's name, the constraints created and dropped, and the parts
without a graph-type equivalent — failing when existing data violates it
before anything is applied; and a `null` `set` argument clears the stored
ontology, the same clear `drop` pushes. A relationship `type` the
document declares across several `relationships` entries — one type,
several endpoint pairs — defines no graph type at all: the document
applies, the type still seeds the vocabulary, and its relationships stay
writable under any labels, with no endpoint or property rows enforced for
it (ADR-0668). The stored document is the sole source of truth — the
`system_ontologies` namespace is visible under the same
`RELATE_PREVIEW=ontology` opt-in (ADR-0593) and not queryable
(ADR-0229): every other statement routed there, reads included, fails
with 42N17, so the example reads the namespace's listing and pins that
failure. Without the opt-in the namespace is invisible — the listing
shows the two CE rows and `USE` fails as an unknown database.
