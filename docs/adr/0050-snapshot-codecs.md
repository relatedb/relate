# 0050. Pluggable snapshot codecs (Parquet default, CypherLite JSON)

- **Status**: Accepted
- **Date**: 2026-09-17
- **Deciders**: MemoryDB maintainers

## Context

Durable MemoryDB state is a fsync’d WAL plus a checkpoint snapshot
(ADR-0012, ADR-0014). The snapshot is currently Parquet only (ADR-0004,
ADR-0005, ADR-0022). Callers want other snapshot encodings (CypherLite
GraphData JSON, later Markdown) without changing commit durability.

WAL format, append/fsync, rotate, LSN pin, recycle, timed/close
checkpoint, and recycle must stay unchanged. Incremental vs
full rewrite is a property of the snapshot encoding, not of the engine.

## Decision

1. **Codec owns store files.** A snapshot codec implements `load` (open,
   before WAL replay) and `dump` (checkpoint, after LSN pin). Parquet
   keeps dirty id-window copy-on-write (ADR-0022). JSON rewrites
   `graph.json` in full. Different codecs may have different
   characteristics.

2. **One codec per database instance.** No mixing Parquet objects and
   `graph.json` in one URI. The codec is recorded in `manifest.json` as
   `"codec": "parquet" | "json"`. Absent `codec` on existing directories
   means parquet.

3. **Envelope.** `manifest.json` always carries `codec` and
   `checkpoint_lsn` (WAL recycle). Parquet keeps shard lists, `next_*`,
   indexes, and constraints in that file. JSON writes a slim envelope
   plus CypherLite GraphData in `graph.json` (layout: MemoryDB directory
   + `wal/`; not a drop-in `.db` file URI).

4. **Selection.** Empty URI: `OpenOptions.snapshot_codec`, else
   `MEMORYDB_SNAPSHOT_FORMAT`, else parquet. Existing URI: envelope (or
   inferred from files if the envelope is missing). A mismatched flag is
   an error.

5. **JSON schema.** `graph.json` is CypherLite `GraphData` (`nodes`,
   `relationships`, `indexes`, `constraints`, `next_node_id`,
   `next_rel_id`, optional `graph_type` / `read_only`) with
   `cypherlite-model` `__type` property tags. `read_only: true` fails
   open. Default JSON dump writes `read_only: false`.

WAL, redo, and checkpoint choreography are unchanged.

## Alternatives considered

- **Replace WAL with JSON** — rejected; commit durability stays WAL.
- **Single-file `.db` URI** — rejected for this ADR; import is copy into
  `<dir>/graph.json`.
- **Share GraphStore** — rejected; the live engine remains CSR/`Graph`.

## Consequences

- ADR-0004 / ADR-0005 “Parquet is the only durable snapshot” is
  superseded for the snapshot layer only. Parquet remains the default.
- Markdown and other codecs are follow-on work under this trait.
- Loadtests and TCK continue to use parquet unless a test opts into JSON.

## Later references

ADR-0072 adds a virtual filesystem under the snapshot files. ADR-0074
packs that tree into one host file when the URI ends with `.cl`; that
does not revive a single-file *codec* URI. Codecs still own `manifest.json`
/ shards / `graph.json`.

ADR-0095 adds optional typed schema theater metadata to both Parquet's
manifest and JSON GraphData; JSON's envelope remains unchanged.

ADR-0105 puts the Parquet codec behind an additive `parquet` Cargo feature
(default on). Builds without it default new stores to JSON and refuse
Parquet stores; an absent `codec` field still means Parquet.

ADR-0120 adds an optional inline `ontology` string to GraphData. A
`graph.json` that carries it is written with `value_encoding: 3`; the JSON
envelope is unchanged.
