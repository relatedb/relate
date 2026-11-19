---
type: adr
area: engine
date: 2026-08-28
deciders: MemoryDB maintainers
name: 0005-lpg-parquet-layout
status: accepted
tags:
  - parquet
  - storage
title: Labeled property graph stored as columnar node and relationship Parquet tables
REFERENCES:
  - "[[adr/0004-memory-first-parquet-object-store]]"
  - "[[adr/0012-wal-and-parquet-checkpoints]]"
  - "[[adr/0022-id-window-checkpoint-shards]]"
  - "[[adr/0033-compact-live-records-typed-csr]]"
---

# 0005. Labeled property graph stored as columnar node and relationship Parquet tables

## Context

OpenCypher is a labeled property graph: nodes have labels and properties,
relationships have a type and properties. TCK values mix types on the same
property key. Parquet wants columns. We need a physical layout that is
simple, checkpointable, and still round-trips TCK values.

## Decision

Logical model: labeled property graph (nodes, relationships, integer IDs,
labels, relationship types, property maps).

Physical snapshot (directory or object-store prefix):

- `manifest.json` — format version, next IDs, checkpoint metadata
- `nodes.parquet` — `id` (i64), `labels` (list of utf8), `properties` (utf8
  JSON object encoding MemoryDB values)
- `relationships.parquet` — `id`, `start_id`, `end_id`, `type` (utf8),
  `properties` (utf8 JSON)

Properties use a tagged JSON encoding so a key can hold int, float, string,
bool, list, map, or null across rows (TCK mixed types). In memory, adjacency
lists are rebuilt on load.

Schema-optional: labels and types do not require `CREATE` DDL.

## Alternatives considered

- **Kuzu node-table per label, rel-table per type, typed columns** —
  faster scans, hostile to TCK untyped mixed properties. Rejected for v1.
- **One property column per key with Arrow union** — more Parquet-native,
  larger schema evolution problem. Rejected until a catalog ADR revisits it.
- **Store properties as a separate wide table** — extra joins on every
  checkpoint. Rejected for v1 simplicity.
- **JSON document store for the whole graph** — fails the Parquet storage
 goal ([ADR-0004](./0004-memory-first-parquet-object-store.md)). Rejected.

## Consequences

- JSON property blobs are a known CPU cost at checkpoint; acceptable for v1.
- Changing the Parquet schema is a format-version bump in `manifest.json`.
- Catalog crate tracks labels/types for planning, not as a storage constraint.
- Derived CSR files are optional later; v1 rebuilds adjacency in RAM.

## Later references

[ADR-0012](./0012-wal-and-parquet-checkpoints.md) extends the snapshot directory. `manifest.json` gains
`checkpoint_lsn` and `commit_ts` (format_version bump as already
required by this ADR when the snapshot schema changes). Checkpoints
still write `nodes.parquet` and `relationships.parquet` as the **latest
committed visible rows** (MVCC versions are not stored in Parquet). WAL
segments live under `wal/` for filesystem URIs; they are not Parquet.
On-disk checkpoint is tmp+rename locally; S3 still publishes Parquet
first, manifest last.

[ADR-0022](./0022-id-window-checkpoint-shards.md): format_version 3 lists byte-budgeted id-window parts
(`nodes/{lo}-{hi}.parquet`, `relationships/{lo}-{hi}.parquet`) instead
of one file per entity kind. Row schema is unchanged. A checkpoint
rewrites only dirty windows (copy-on-write). v1–v2 monolith files still
load.

[ADR-0033](./0033-compact-live-records-typed-csr.md): the live RAM store uses compact records and typed CSR
chunks. Parquet row schema and “rebuild adjacency on load” are
unchanged.
