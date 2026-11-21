---
type: adr
area: engine
claim: 568e86ffc55bd6c7b55a2aca80a18e0f943ab0ab
date: 2026-11-21
deciders: the maintainers
name: 0649-vault-committed-ontology-file
status: proposed
tags:
  - storage
title: "A vault-committed ontology file: a reserved root document declares the vault's schema, and its declarations travel with version control"
REFERENCES:
  - "[[adr/0406-markdown-vault-snapshot-codec]]"
  - "[[adr/0226-ontology-mappings]]"
  - "[[adr/0120-ontology-document-persistence]]"
  - "[[adr/0487-ontology-spec-1-1-0-pin]]"
---

# 0649: A vault-committed ontology file

## Context

A markdown vault shared through version control carries its data — the
markdown files — but not its schema. The learned relationship-type
vocabulary, the per-label key properties, the catalog DDL and the
store's ontology document all live in the `.relate/` sidecar, which is
per-machine engine state: every fresh clone re-infers, and every pull
leaves the schema behind. The sharp edges are concrete: a map-form
link entry under a relationship type no bare wikilink anywhere
declares refuses the open (ADR-0406 §2's bootstrap ambiguity), so a
vocabulary learned on one machine does not travel; the key property is
inferred per machine from property values rather than declared; index
and constraint DDL exists only in each machine's sidecar.

The engine already owns a schema language for exactly this shape of
declaration: the published Ontology Graph Spec (ADR-0487's 1.1.0 pin)
— nodes with identifying labels and `key: true` properties,
relationships with Neo4j type tokens — plus ADR-0226's converter to
the engine's graph-type grammar and ADR-0120's document model. The
missing piece is a committed source for it.

## Decision

1. **The reserved file.** A vault may carry one committed ontology
   document at its root: `ontology.yaml` or `ontology.json` — never
   both (the pair is an ambiguous source and refuses). The file is
   user-owned content the engine reads and never writes: the walk
   takes `.md` files only, so it is invisible to the node load, the
   view regeneration and the checkpoint's deletion phase alike. It is
   untrusted input: read under the ontology document class's 16 MiB
   bound (ADR-0120 §3), metadata-first.
2. **Declarations seed the load.** The document's relationship types
   join the classification vocabulary — map-form frontmatter entries
   under a file-declared type are declared, the same evidence a bare
   wikilink gives (ADR-0406 §2) — and each node type's identifying
   label with its `key: true` property seeds the per-label key map.
   The file's declarations **win over the sidecar's stale records**
   (committed truth over per-machine derived state) and are binding,
   not advisory: a declared key property that disagrees with its
   filename refuses the open, where the value-matching inference might
   have picked an agreeing property. Labels the file does not declare
   keep whatever the sidecar recorded or the inference derives; the
   seeded facts converge into the sidecar through the load's ordinary
   re-baseline, so one machine's pull propagates the schema to its own
   sidecar and nothing else.
3. **Expressibility is proven at open.** ADR-0226's converter must be
   able to express the document's graph type — the same conversion
   `db.ontology.apply` runs — or the open refuses naming the file: a
   document that claims to be the store's schema must be a schema the
   engine can hold.
4. **Everything invalid fails closed naming the file**: a document
   past the bound, one that does not parse, one that does not validate
   against the pinned spec (ADR-0487), or one that does not convert.
5. **The committed half applies at the open tail, through the replay
   path's own machinery.** What the codec's load extracts but must not
   apply — because the schema text mirror's open-time verification
   (ADR-0481 §4) compares the *checkpointed* state, so a committed
   change landing inside the load would read as drift and refuse the
   first open after any file edit — rides one carrier,
   `CommittedSchema` (the document, its graph type, its
   extension-envelope indexes), from the load to the facade's open
   tail, which applies it after the verification and the WAL replay:
   the catalog facts as the redo ops DDL commits record
   (`CreateIndex` + `CatalogIndexDetail`, `SetCurrentGraphType`) —
   every invariant the redo applier enforces holds for a committed
   schema exactly as for a WAL-replayed one — then the document
   becomes the store's ontology state and `db.ontology.set`/`drop`
   refuse (one gate, in the commit preflight): the file is the
   document's source. The carrier is the reusable half: the markdown
   codec produces one from the vault's reserved file, and any future
   file-based store that defines committed schema files produces one
   through the same type — the extraction is the codec's own, the
   application is one shared path. The application is idempotent
   (every open re-applies, same-named committed facts overriding
   machine state the way a later commit would), the read-only and
   follow-reload opens run it too, and the in-memory commit-timestamp
   advance it takes is recorded durably only by the next publishing
   checkpoint — a clean session's no-op exit (ADR-0646) writes
   nothing.
6. **Indexes ride the spec's native extension envelopes.** The
   Ontology Graph Spec already carries database indexes: per-element
   `extensions.indexes` (`NodeIndex`/`RelationshipIndex`) with the
   `FULLTEXT | POINT | RANGE | TEXT | VECTOR` kind enum, labels,
   properties and deeply validated option payloads
   (`fulltext.analyzer`, `vector.dimensions`, the spatial bounds) —
   the committed index DDL needed no format invention, only the
   conversion into the theater's `index_details`/`index_options`
   through decision 5's path. The conversion rules: kind mapping
   direct; `LOOKUP`, multi-property entries and relationship-entry
   indexes refused as shapes the catalog row does not hold (until it
   holds them); the index name is the entry's `name` or its envelope
   id; options map verbatim minus the `type` discriminator; node
   entries install under their labels. One limitation is deliberate:
   the file **adds and overrides** but does not remove — a machine
   index the file no longer declares survives until a `DROP INDEX`
   retires it per machine, because the sidecar carries no provenance
   separating file-sourced from machine-created indexes.

## Consequences

The committed file makes the vault's schema travel, all of it: the
relationship vocabulary, the key properties, the ontology document
(`db.ontology.get` serves it; set/drop refuse), the current graph
type, and the extension-envelope indexes — a fresh clone or a pulled
edit opens with the declared schema applied, and the schema text
mirror's verification never reads a committed change as drift. The
file is optional — a vault without one behaves exactly as before.
The file's index option payloads get the DDL path's own per-kind
validation and defaults (an option a `CREATE INDEX` would reject
refuses the open naming the store; a declared `vector.dimensions`
is enforced by the engine's dimensionality guard, not displayed
only). **The trust boundary is the file's edit path:** anyone who
can commit to the repository changes the enforced schema for every
machine that opens the vault — binding key declarations, the
current graph type, the installed indexes — so a pulled
`ontology.yaml` change is DDL for the store and belongs in review
like one; the same trust already covers the vault's markdown (a
pulled edit is data), and the sidecar remains per-machine.
`db.ontology.apply` remains available for its validate-first data
scan, which the open-time install deliberately does not run (the
file's type is trusted the way a checkpointed current graph type
is; enforcement is at write time). What remains deliberately open:
the file's add-and-override reach does not remove machine indexes
(decision 6's provenance gap), relationship-entry indexes and `LOOKUP`
wait on the catalog row holding them, and a `db.ontology.apply` or
graph-type DDL on a file-backed store changes machine state the next
open re-wins — an eventual-consistency seam to close with provenance,
not with a second source of truth.
