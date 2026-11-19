---
type: adr
area: engine
date: 2026-08-28
deciders: MemoryDB maintainers
name: 0014-single-wal-write-path
status: accepted
tags:
  - checkpoint
  - wal
  - write-path
title: "Single write path: WAL then snapshot; graph is a materialization"
REFERENCES:
  - "[[adr/0005-lpg-parquet-layout]]"
  - "[[adr/0012-wal-and-parquet-checkpoints]]"
  - "[[adr/0015-optimistic-wal-commits]]"
  - "[[adr/0020-segmented-wal-concurrent-checkpoint]]"
  - "[[adr/0022-id-window-checkpoint-shards]]"
SUPERSEDES:
  - "[[adr/0012-wal-and-parquet-checkpoints]]"
---

# 0014. Single write path: WAL then snapshot; graph is a materialization

## Context

[ADR-0012](./0012-wal-and-parquet-checkpoints.md) added a WAL for persistent URIs but left several write paths:
`:memory:` skipped the WAL; Cypher mutated the live graph then optionally
fsynced; Parquet could be written from that graph without a log record;
`StoreTarget::Memory` loaded an empty graph with no replay. Dataset load
and tests could not tell which path they were on.

## Decision

There is **one** write path and **one** way to obtain a live graph:

1. **Commit** = append physical redo to the WAL (fsync on a filesystem
   WAL; in-process buffer for `:memory:`) then **`apply_redo`** onto the
   in-memory graph. The live graph is never the source of a commit.
2. **Materialize** = load [ADR-0005](./0005-lpg-parquet-layout.md) / [ADR-0022](./0022-id-window-checkpoint-shards.md) Parquet (manifest parts or
   legacy `nodes.parquet` / `relationships.parquet`) then replay WAL records
   with `lsn > checkpoint_lsn`. Open always does this. No empty-graph
   shortcut that skips the store.
3. **Snapshot** = checkpoint writes those Parquet files from the
   **already applied** committed graph, then truncates the WAL. Parquet
   is not written except as a snapshot of WAL-committed state.

`:memory:` uses the same types: an in-process object store for Parquet
and an in-process WAL. It is still process-ephemeral (nothing on disk)
but there is no second code path.

`wal: Option<_>` is gone. Every `Database` has a WAL and a store.

## Alternatives considered

- **Keep `:memory:` as a no-WAL bypass** — fewer fsyncs, two engines.
  Rejected; the bypass is what this ADR removes.
- **Snapshot Parquet on every commit** — true “WAL then files” per
  statement, too slow for dataset load. Rejected; checkpoint remains
  periodic/on-close compaction of the WAL.
- **Execute Cypher by replaying into the live graph only after WAL**
  without a private apply — MATCH-after-CREATE in one statement needs
  tentative state. Decision: execute may use unpublished versions to
  **compute redo and the result**, then those versions are discarded
  (`abort_write`); durability and the live graph follow WAL +
  `apply_redo` only.

## Consequences

- Crash recovery and “open an existing URI” are the same function.
- Loadtests with `--uri :memory:` still hit the WAL (volatile).
- [ADR-0012](./0012-wal-and-parquet-checkpoints.md)’s “`:memory:` has no WAL” bullet is superseded.

## Later references

[ADR-0015](./0015-optimistic-wal-commits.md): execute still computes redo on unpublished versions, but on a
private snapshot. The shared graph is updated only by WAL + `apply_redo`.
`commit_ts` is assigned at WAL append, not at `begin_write`.

[ADR-0020](./0020-segmented-wal-concurrent-checkpoint.md): Parquet checkpoint dumps a pinned `snapshot_ts`; closed WAL
segments are deleted after the manifest is durable. Truncating the live
segment under the commit lock is superseded.

[ADR-0022](./0022-id-window-checkpoint-shards.md): the snapshot is a list of id-window parts in `manifest.json`,
not always two monolith Parquet files. Replay is unchanged.
