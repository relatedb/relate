---
type: adr
area: engine
date: 2026-08-28
deciders: MemoryDB maintainers
name: 0004-memory-first-parquet-object-store
status: superseded
status_note: in part by [ADR-0012](./0012-wal-and-parquet-checkpoints.md)
tags:
  - parquet
  - storage
title: Memory-first graph with Parquet snapshots via object_store
REFERENCES:
  - "[[adr/0012-wal-and-parquet-checkpoints]]"
  - "[[adr/0014-single-wal-write-path]]"
  - "[[adr/0044-withdraw-engine-s3]]"
  - "[[adr/0050-snapshot-codecs]]"
---

# 0004. Memory-first graph with Parquet snapshots via object_store

## Context

The engine should feel like DuckDB: fast in-process analytics, durable files
that can live on a laptop or in S3. TCK queries mutate the graph
(`CREATE`/`DELETE`/`SET`). Object storage is not a good per-row WAL.

## Decision

- The **source of query execution** is an in-memory labeled property graph.
- **Durability** is a snapshot: `Database::open` loads Parquet (or starts
  empty for `:memory:`); `checkpoint` / close writes Parquet.
- Files are addressed through Apache `object_store`: `file://` (and plain
  paths) and `s3://` in v1. GCS and Azure are follow-on work, same crate.
- S3 is **snapshot-oriented**. No multi-writer protocol in v1.
- Credentials come from the environment or explicit config, never from the
  repository.

Out-of-core scans of Parquet without loading the graph are **not** v1.

## Alternatives considered

- **Parquet as the live store (query files in place)** — better for graphs
  larger than RAM, worse for TCK-style mutations and “super lightweight.”
  Rejected for v1; would be a new ADR.
- **Custom binary WAL + pages (Neo4j store)** — heavy, not lakehouse-friendly.
  Rejected; Parquet is the durable format.
- **JSON/CSV snapshots** — easier to debug, poor compression and typing.
  Rejected in favor of Parquet.
- **DuckDB as the storage engine** — would couple us to another
  product and license surface. Rejected; we own the graph and use Parquet
  libraries only.

## Consequences

- Graphs larger than RAM are not a v1 goal.
- Concurrent writers on S3 are unsupported; last checkpoint wins.
- `object_store` + `parquet`/`arrow` are allowed dependencies.
- Opening `s3://` without credentials fails with a documented error.

## Later references

[ADR-0012](./0012-wal-and-parquet-checkpoints.md) replaces snapshot-only durability. Execution is still an
in-memory LPG; `object_store` still addresses `file://` and `s3://`;
out-of-core Parquet scans are still out of scope. Durability is now a
fsync’d **local WAL** for every committed write, with Parquet as a
**checkpoint** (load snapshot, then replay WAL). The rejected “custom
WAL” alternative in this file no longer holds. `:memory:` remains
process-ephemeral but uses the same in-process WAL + snapshot path
([ADR-0014](./0014-single-wal-write-path.md)). S3 still has no multi-writer protocol; the WAL for `s3://`
is local (`MEMORYDB_WAL_DIR` / `--wal-dir`), not an object-store log.
[ADR-0050](./0050-snapshot-codecs.md): Parquet remains the **default** snapshot codec. Other codecs
(CypherLite GraphData JSON) may write different store files. WAL is
unchanged.

[ADR-0044](./0044-withdraw-engine-s3.md) withdraws engine `s3://`. `object_store` still addresses
`:memory:` and `file://` / filesystem paths; object storage belongs
to clients implementing embedded Relate.
