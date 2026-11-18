# Read a store's schema from the files

Every Relate store writes its schema — graph type, ontology document,
index and constraint catalog — as text files under `.relate/schema/`
inside the store directory. The schema folder is the state, in git,
declaratively: an agent (or a reviewer) reads one file instead of booting
the database, running `SHOW INDEXES`, or calling `db.ontology.get`, and a
schema change shows up in a pull request as an ordinary text diff
([ADR-0481](../adr/0481-schema-as-files.md)).

## The three files

| File | Contents |
| --- | --- |
| `.relate/schema/graph-type.cypher` | The store's graph type as its canonical DDL body — the same canonical text the snapshot persists. Absent when the store has no graph type. |
| `.relate/schema/ontology.json` (or `.yaml`) | The ontology document, byte-verbatim as validated — never re-serialized or pretty-printed. The extension names the document's format; at most one of the two exists, and both are absent when no ontology is set. |
| `.relate/schema/schema.cypher` | One `CREATE INDEX` / `CREATE CONSTRAINT` statement per *renderable* catalog entry, in the catalog's name order, each rendered exactly as the `SHOW INDEXES` / `SHOW CONSTRAINTS` `createStatement` column renders it. Always present, empty when the catalog has no renderable entries. |

The renderable carve-out: some catalog entries have no standalone `CREATE`
form — legacy detail-less index rows, and constraints the graph type
manages. Those are omitted from `schema.cypher`, not fabricated; their
schema is carried by the graph type and the manifest.

Every checkpoint of every codec rewrites the folder — Parquet, JSON, and
the markdown vault (where it lands beside the `.relate/manifest.json`
sidecar). The write is unconditional; there is no flag to turn it off. A
store written before ADR-0481 has no folder, opens unchanged, and gains
the folder at its first checkpoint.

## The mirror is a read artifact

`manifest.json` and the WAL stay authoritative. The text files are a
mirror of the same in-memory state the checkpoint persists — they are
never re-parsed as a storage source.

That has one consequence worth stating plainly: **editing the schema text
does not change the store.** A hand-edited file does not alter the graph
type, the ontology, or the catalog — it fails the open instead (next
section). To change the schema, run the Cypher statements (`CREATE INDEX`,
the graph type DDL, `db.ontology.*` procedures) and checkpoint; the mirror
rewrites from the state they produce. This holds for agents too: the
schema folder is something to *read* (or diff in git), never to write.

## Drift fails the open closed

At open, when the `.relate/schema/` folder exists, each file is
verified byte-for-byte against the state the last checkpoint persisted. A
changed, missing, or extra file is *drift*, and the open refuses with an
authored error naming the file:

```text
schema mirror .relate/schema/schema.cypher differs from the schema this
store's checkpoint persisted; it was edited by hand or copied from another
store; delete the .relate/schema/ directory and reopen: the next
checkpoint regenerates it from the store's schema
```

The repair is in the message: delete the `.relate/schema/` directory
and reopen. The next checkpoint regenerates the mirror from the store's
schema — no data is lost, because the durable state (manifest, WAL,
snapshot objects) was never touched by the refusal.

Hand edits are the usual cause, but not the only one: a checkpoint
interrupted by a crash can leave the mirror one checkpoint behind (the
folder is rewritten after the manifest is published). That store hits the
same drift error on its next open, and the repair is the same — delete the
folder and reopen; nothing was lost. The open does not regenerate a stale
mirror on its own, because it cannot tell a crash-stale mirror from a
hand-edited one, and silently rewriting would forgive the hand edits the
check exists to catch.

Two things that are **not** drift:

- **WAL-pending schema changes.** The comparison is against the state the
  checkpoint persisted, before WAL replay. Schema statements committed
  since the last checkpoint live in the WAL and legitimately move the live
  schema past what the mirror reflects; the next checkpoint rewrites the
  mirror from the state they produce. A store with un-checkpointed schema
  changes opens normally.
- **An absent folder.** No `.relate/schema/` directory means a store
  written before ADR-0481 (or one repaired as above). The open proceeds.

## Why the files are not the storage source

Making the text files authoritative — re-parsing them at open so a hand
edit in git changes the schema — was considered and rejected for v1
(ADR-0481, Alternatives considered): the WAL carries schema changes since
the last checkpoint, so text-as-source would break WAL replay ordering and
the manifest's crash-consistency guarantee. The mirror plus the open-time
verification gives the git-diffable intent without that cost.
