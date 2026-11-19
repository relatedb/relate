---
type: adr
area: engine
claim: d6200ff2158118235258609bb593dc08e2796735
name: 0481-schema-as-files
status: proposed
tags:
  - catalog
title: Schema as files — the store's schema mirrored as canonical text
REFERENCES:
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0229-remove-system-ontologies-cypher-surface]]"
  - "[[adr/0406-markdown-vault-snapshot-codec]]"
  - "[[adr/0623-engine-sidecar-dir-rename]]"
---

# ADR-0481: Schema as files — the store's schema mirrored as canonical text

## Context

An agent (or a human) that wants to know a CypherLite store's schema today
has to boot the engine and ask it: `SHOW INDEXES` / `SHOW CONSTRAINTS` for
the catalog, `CALL db.ontology.get()` for the ontology, the graph-type
introspection for the graph type. None of that is readable from a checkout,
none of it diffs in a pull request, and none of it survives as reviewable
history in git. LABS-681's scope update asks for the opposite posture, the
one the pg-delta talk (~59:00) states as "the schema folder is the state, in
git, declaratively": a store carries its schema as committed, human-diffable
text files that a reader opens instead of a live database.

The storage map on `main` (dbc944d2) fixes what exists to build on:

- **Native stores** keep `manifest.json`, `nodes/`, `relationships/` and
  `ontology/<128-bit-hex>.json` at the store root. Native and JSON stores
 have no `.cypherlite/` directory. Only the markdown vault ([ADR-0406](./0406-markdown-vault-snapshot-codec.md)) has
  one: the `.cypherlite/manifest.json` sidecar, whose `CatalogWire` already
  carries the graph type as text.
- **Graph type.** `GraphTypeSpec` (serde) lives in the manifest, and a
  canonical TEXT copy already flows through `SnapshotContents.graph_type`
  (`Graph::graph_type_spec()`, restored via
  `Graph::restore_graph_type_spec`). The canonical form is produced by
  `canonicalize_graph_type_body` (`cypherlite-executor`
  `graph_type/canonicalize.rs`) and re-parses through
  `cypherlite_parser::parse_graph_type_body`.
- **Ontology.** The verbatim validated JSON/YAML document (≤ 16 MiB,
  `MAX_ONTOLOGY_SPEC_BYTES`) is published and loaded by
  `publish_ontology` / `load_ontology_document`
  (`snapshot/native/ontology.rs`) and opened by `open_store_with_ontology`
  (`open.rs`); the manifest's `ontology_path` selects the hash-named object.
- **Indexes and constraints.** The manifest carries `IndexDef` /
  `ConstraintDef`. Per-row `createStatement` renderers exist for `SHOW`
  (`exec/catalog_show/indexes.rs`, `index_create_statement`); there is no
  whole-store DDL dump. No gram serializer exists anywhere in the
  workspace.
- **Lifecycle seams.** Checkpoint dispatch: `cypherlite-storage/src/lib.rs`
  `checkpoint*` → `snapshot::checkpoint_at` / codec dumps. Open:
  `open_store*` → codec load → `catalog::restore`.

Every piece of schema the feature needs therefore already has a canonical
text rendering or a verbatim byte form; what is missing is a stable on-disk
location, a write at checkpoint, and an open-time check that the text and
the loaded catalog agree.

## Decision

Every CypherLite store persists its graph type, ontology, and
index/constraint schema as canonical text files under
`.cypherlite/schema/` inside the store directory. The layout contract below
is normative; implementation follows it verbatim.

1. **`.cypherlite/schema/graph-type.cypher`** — the store's graph type as
   its canonical DDL body: exactly the `canonicalize_graph_type_body` form
   (the text `SnapshotContents.graph_type` already carries), re-parsable
   by `cypherlite_parser::parse_graph_type_body`. The file is absent when
   the store has no graph type.
2. **`.cypherlite/schema/ontology.json`** — the ontology document
   byte-verbatim, exactly the validated bytes `publish_ontology` stores
   (no re-serialization, no pretty-printing). The extension is `.yaml`
   (`ontology.yaml`) when the document is YAML. The file is absent when no
   ontology is set; at most one of `ontology.json` / `ontology.yaml`
   exists.
3. **`.cypherlite/schema/schema.cypher`** — one `CREATE INDEX` /
   `CREATE CONSTRAINT` statement per *renderable* catalog entry, in the
   catalog's name order (the catalog keeps no creation sequence; this is
   the same order every checkpoint manifest persists), each rendered by
   the same renderer that produces the `SHOW INDEXES` /
   `SHOW CONSTRAINTS` `createStatement` column (one renderer, two callers —
   never a second copy). Entries whose `createStatement` the SHOW column
   renders as NULL — legacy detail-less index rows, constraints the graph
   type manages — have no standalone `CREATE` form and are omitted, not
   fabricated and not failed; their schema is carried by the graph type
   and the manifest. The file is always present for a checkpointed
   store, empty when the catalog has no renderable entries.
4. **Authority model.** `manifest.json` and the WAL stay authoritative at
   open: v1 never re-parses the text set as its storage source. The text
   set is the canonical mirror, rewritten at every checkpoint from the same
   in-memory state the checkpoint persists. At open, when the
   `.cypherlite/schema/` folder exists, each file is verified against the
   loaded catalog (graph type canonical text, ontology bytes, rendered
   statement list). The comparison basis is the state the manifest-named
   checkpoint persisted, before WAL replay: WAL records past the
   checkpoint LSN legitimately move the live schema past the mirror, so
   they are not drift — the next checkpoint rewrites the mirror from the
   state they produce. Any drift — a changed, missing, or extra file — fails
   the open closed with an authored error naming the drifted file. An
   absent `.cypherlite/schema/` folder means a store written before this
   ADR and never refuses the open; the next checkpoint creates the folder.
5. **Every codec writes the set.** Native Parquet, JSON, and the markdown
   vault all emit the same three-file contract at checkpoint and verify it
   at open. For native and JSON stores this introduces the `.cypherlite/`
   directory at the store root; the markdown vault writes
   `.cypherlite/schema/` beside its existing `.cypherlite/manifest.json`.
6. **No CLI flag in v1.** The write is unconditional for every checkpoint;
   there is no opt-in or opt-out switch. An export-as-DDL command (writing
   the set somewhere other than the store) stays an optional follow-up.

## Alternatives considered

- **Make the text files authoritative at open.** The store would rebuild
  its catalog by parsing `graph-type.cypher`, `ontology.json`, and
  `schema.cypher`, so a hand edit in git would change the schema.
  Rejected for v1: the text set is written at checkpoint while the WAL
  carries schema changes since the last checkpoint, so text-as-source
  breaks WAL replay ordering (schema operations would replay against a
  catalog sourced from a different point in time) and the
  manifest-commit crash-consistency guarantee, which covers the manifest
  but not a folder of independent files.
- **A gram-format serializer for the schema.** Rejected: no gram
  serializer exists anywhere in the workspace, and the graph-type DDL is
  already a canonical, re-parsable text form; inventing a second notation
  adds a codec to maintain with no reader that needs it.
- **An export command instead of native storage.** A `cypherlite schema
  export` verb would dump the same text on demand. Rejected: the ticket's
  point is that the schema lives natively in the store, so a checkout
  already holds it without running anything; an export command is the
  optional follow-up named in Decision §6, not a substitute.
- **Snapshot the per-namespace system graphs as files.** Mirror the
 `system` / `system_ontologies` namespace graphs ([ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md), [ADR-0229](./0229-remove-system-ontologies-cypher-surface.md)) too.
  Rejected: v1 covers the three schema facts an agent reads — graph type,
  ontology, indexes/constraints; the system namespaces carry no
  user-authored schema beyond the ontology document already mirrored.

## Consequences

- An agent or reviewer reads `.cypherlite/schema/*` from a checkout and
  sees the store's schema without booting the engine or calling
  `db.ontology.get`; schema changes show up as ordinary text diffs.
- Native and JSON stores gain a `.cypherlite/` directory at the store root;
  store-layout docs and any tooling that enumerates store contents must
  account for it.
- Every checkpoint does three extra small writes; the ontology file can be
  up to 16 MiB (`MAX_ONTOLOGY_SPEC_BYTES`) and is rewritten only with the
  same bytes the ontology object already holds.
- Open gains a verification step: a store whose text set was hand-edited
  (or partially copied) refuses to open with an error naming the file,
  rather than silently diverging from its catalog. Repair is deleting the
  `.cypherlite/schema/` folder, which the next checkpoint regenerates.
- There is a crash window in every checkpoint: the codecs write the mirror
  after publishing the manifest, so a crash in between leaves the mirror
  one checkpoint behind the manifest. The next open then refuses with the
  same drift error until `.cypherlite/schema/` is deleted — zero data is
  lost, since the manifest and the WAL, not the mirror, are the durable
  state. Self-healing at open (regenerating the stale mirror from the
  loaded catalog) was considered and rejected: a crash-stale mirror is
  indistinguishable from a hand-edited one — both are "mirror differs from
  checkpointed state" — so regenerating would silently forgive the hand
  edits Decision §4 requires to fail closed.
- The `createStatement` renderers and `canonicalize_graph_type_body`
  become persistence contracts: changing their output changes the on-disk
  mirror, so an output change must ship with the open-time verification
  tolerating (or migrating) the previous form.
- Stores written before this ADR open unchanged; the folder appears at
  their first checkpoint.

## Later references

- [ADR-0623](./0623-engine-sidecar-dir-rename.md) renames the engine directory the mirror lives under to
  `.relate/`, adopting existing stores by one directory rename at their
  first writable open; the mirror's layout beneath it is unchanged.
