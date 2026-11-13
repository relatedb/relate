# 0120. Persist the ontology as a document and derive `system_ontologies`

- **Status**: Accepted
- **Date**: 2026-09-27
- **Deciders**: CypherLite maintainers (Linear LABS-102)

## Context

ADR-0116 makes `system_ontologies` a fixed, read-only namespace that holds at
most one ontology, the one for `neo4j`. ADR-0121 defines the procedures that
set and drop it. Neo4j's ontology proposal persists ontologies as a real
subgraph of their own database; its persistence, backup and privilege sections
are specific to Neo4j's DBMS and do not bind CypherLite.

Only procedures write the namespace, so its graph is a pure function of the
ontology document and CypherLite's mapping. Persisting that graph would need
per-namespace shards, WAL records tagged with a namespace, and per-namespace
id allocators, and `db.ontology.get` would have to rebuild the document from
the graph.

New persisted state must make older binaries fail rather than lose it. Older
binaries ignore unknown JSON fields: the manifest, `GraphData` and
`CheckpointTheater` do not use `deny_unknown_fields`. Before ADR-0114, their
WAL recovery also trims a checksum-valid but undecodable final record as if it
were torn (ADR-0086 §1). The checks they run before modifying anything are the
WAL segment magic, checked as each segment is scanned; the Parquet manifest's
`format_version`, where 1 to 3 are accepted; and the JSON `value_encoding`,
where absent and 2 are accepted. ADR-0059 declined to version the WAL while no
persisted change needed it.

## Decision

1. **The document is the source of truth.** The stored state is the validated
   ontology JSON for `neo4j`, kept verbatim. There is at most one.
2. **The graph is derived.** The `system_ontologies` graph is built from the
   document at open and after each committed set or drop, and then replaces
   the previous graph atomically. Statements and transactions keep the graph
   they captured (ADR-0116 §5). The build is deterministic: node and
   relationship ids follow document order, so element ids (ADR-0116 §7) stay
   the same across restarts and change only when the ontology is replaced. The
   derived graph carries a fixed graph type that describes the ontology model.
3. **WAL operations.** Two new store-level redo ops, which are not graph ops:
   `OntologySet { database, spec }` and `OntologyDrop { database }`. The
   document is limited to 16 MiB, the ADR-0086 object envelope, and the limit
   is checked before the commit is written.
4. **WAL segment magic `MDBWAL02`.** The frame layout does not change.
   - A segment created while the store holds an ontology, committed or still
     pending in the WAL, starts with `MDBWAL02`. Other segments start with
     `MDBWAL01`.
   - When an ontology op commits while the active segment is `MDBWAL01`, the
     WAL first rotates to a fresh `MDBWAL02` segment.
   - An ontology op in an `MDBWAL01` segment is corruption.
   - A short header repaired under ADR-0086 §1 becomes `MDBWAL01`. Such a
     segment holds no frames, and ontology ops are never appended to it.
   - Native ownership checks (ADR-0090) recognize both magics.
5. **Parquet snapshot.** The manifest gains an optional `ontology_path` that
   selects one immutable `ontology/<128-bit-hex>.json` object. The object is
   written and published under the pull-identity rules of ADR-0092 and
   ADR-0089: exclusive create, sync and a durable name before the manifest,
   and old objects retired only after publication. A manifest with
   `ontology_path` has `format_version: 4`; one without it stays at 3. Readers
   accept 1 to 4.
6. **JSON snapshot.** `graph.json` gains an optional inline `ontology` string,
   for the same reasons ADR-0106 inlines pull identity. A `graph.json` that
   carries it is written with `value_encoding: 3`, which means value encoding
   2 plus the ontology field; one without it keeps 2. The JSON
   `manifest.json` envelope is unchanged.
7. **Bundles.** A `.cl` store keeps the ontology object at a virtual path. The
   bundle container and its single WAL stream are unchanged (ADR-0074).
8. **The format follows the content.** A store that never sets an ontology
   stays byte-for-byte in today's format and readable by older binaries. Once a
   store holds an ontology, older binaries fail at the `MDBWAL02` segment, the
   v4 manifest or `value_encoding: 3`, each time before modifying that data.
   After the ontology is dropped and a checkpoint publishes, new segments are
   `MDBWAL01` again. Once the `MDBWAL02` segments are recycled and the
   manifest is back at v3, older binaries can open the store again.

## Alternatives considered

- **Persist `system_ontologies` as a real graph**, with per-namespace shards
  and WAL records tagged with a namespace. Rejected for now: it adds far more
  format surface than one document, and `get` would rebuild the document
  instead of returning what was stored. It becomes necessary only if Cypher is
  allowed to write the namespace, which needs its own ADR.
- **Inline the document in the manifest.** Rejected: the manifest is bounded
  metadata shared with schema theater (ADR-0086, ADR-0095). ADR-0092 rejected
  inline pull identity for the same reason.
- **Store it in the checkpoint theater image.** Rejected: older binaries
  ignore the unknown field and drop the ontology at their next checkpoint.
- **Add only the new redo ops, without a magic bump.** Rejected: binaries
  released before ADR-0114 trim an undecodable final record as if it were
  torn.
- **Bump every store's format on first open.** Rejected: stores that never use
  an ontology would lose compatibility with older binaries for no benefit.

## Consequences

- Existing stores need no migration. The change is visible only in stores
  that set an ontology.
- Upgrading CypherLite re-derives the ontology graph with the new mapping, so
  a mapping change needs no data migration. Queries written against the old
  graph shape may need updating.
- Replicas and WAL followers see the two new ops in `WalRecord`.
  Code that matches `RedoOp` exhaustively must add them.
- Docs that say the WAL is always `MDBWAL01` (`docs/reference/rust-library.md`,
  `docs/guides/storage-engine.md`, `docs/api/rust/spec.md`) change with the
  implementation.
- Tests cover:
  - set, drop and reopen on Native, Memory and Bundle for both codecs;
  - the ADR-0114 golden stores staying at v3 and `MDBWAL01` without an
    ontology;
  - rotation to `MDBWAL02` on the first set, and the return to v3 after a drop
    and recycle;
  - a crash between writing the ontology object and publishing the manifest;
  - rejection of unknown future versions and magics.
- ADR-0022, ADR-0050, ADR-0059, ADR-0086 and ADR-0090 gain Later references
  notes.

## Later references

- ADR-0226 pins the mappings §2's deterministic build uses: the
  document→`system_ontologies` graph mapping (shapes, labels, the
  per-element `ontology` property, document-order ids, the fixed graph
  type), the ontology→graph-type mapping for `db.ontology.apply`, and
  apply's data-violation check. It supersedes ADR-0121 §5's
  wait-for-Neo4j stance.
- ADR-0227 amends §1: the stored state is the validated ontology document,
  JSON or YAML, kept verbatim (not only JSON). Storage stays verbatim;
  following the ontology-privileges proposal, `db.ontology.get` returns
  the stored bytes unchanged
  when the requested format matches the stored one and converts canonically
  otherwise. The §5 snapshot object and §3 envelope limit are unaffected.
- ADR-0229 supersedes §2: no `system_ontologies` graph is built at open or
  after a committed set or drop — the §1 document (as amended by ADR-0227)
  stays the sole stored source of truth. The deterministic ids and the
  fixed graph type fall with it, and the Consequences re-derivation note no
  longer applies; §3–§8's store formats stand unchanged.
- ADR-0364 clarifies §1's "validated" as validated at `set`: stored
  documents are never re-validated against the current schema or its
  version gate. The open path keeps only the syntax/value-model screen,
  `db.ontology.get` returns stored bytes verbatim, and a pre-bump
  document remains readable (and cross-format convertible) but
  `db.ontology.apply` refuses it until it is replaced.
