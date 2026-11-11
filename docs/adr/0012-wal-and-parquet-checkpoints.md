# 0012. WAL-first durability with Parquet checkpoints

- **Status**: Superseded in part by ADR-0014
- **Date**: 2026-08-28
- **Deciders**: MemoryDB maintainers

## Context

ADR-0004 made the in-memory graph the source of execution and Parquet
snapshots the only durable form. A crash or process exit without an
explicit `checkpoint()` lost every write since the last snapshot.
`:memory:` remains a valid ephemeral mode, but persistent `file://` and
`s3://` URIs need commit durability. Object storage is still a poor
place to fsync a per-row log.

## Decision

- **Unit of commit** is one auto-commit Cypher statement. Bolt
  `BEGIN`/`COMMIT` stay no-ops until a transaction ADR.
- **WAL-first:** a write collects physical redo ops, appends them to a
  local WAL, `sync_all`s, then publishes `commit_ts` so readers can see
  the statement. Replay on open restores commits that fsynced before a
  crash.
- Redo is **physical graph ops** (create/delete node and rel, set/remove
  property, add/remove labels), not Cypher text.
- **`:memory:`** is ephemeral: no WAL, `checkpoint()` is a no-op.
- **Filesystem URI:** WAL segments live next to the snapshot:

  ```
  <dir>/manifest.json
  <dir>/nodes.parquet
  <dir>/relationships.parquet
  <dir>/wal/00000001.log
  ```

- **`s3://`:** Parquet and `manifest.json` stay on the object store. The
  WAL is always a **local directory** (`--wal-dir` or `MEMORYDB_WAL_DIR`;
  default `{tmp}/memorydb-wal/<hash(uri)>`). Open downloads the latest
  snapshot and replays the local WAL.
- **Checkpoint** writes ADR-0005 Parquet tables for the latest committed
  visible rows, records `checkpoint_lsn` and `commit_ts` in
  `manifest.json`, then truncates the WAL. Local checkpoints use tmp +
  rename. S3 writes Parquet first, manifest last. Persistent URIs
  checkpoint on close. Auto-checkpoint when the WAL exceeds a size or
  commit-count threshold.

## Alternatives considered

- **Keep snapshot-only durability (ADR-0004)** — simple, loses uncheckpointed
  writes. Rejected; persistent URIs need commit durability.
- **WAL as S3 objects** — `put` has no `fsync`; not compatible with
  fsync-per-commit. Rejected; WAL stays local.
- **Redo as Cypher text** — planner/`timestamp()`/RNG make replay
  non-deterministic. Rejected; physical ops only.
- **Group commit / delayed fsync** — smaller fsync cost, a window of lost
  commits. Rejected for this ADR; every successful write `sync_all`s.
- **Parquet as the live store** — still out of scope (ADR-0004).

## Consequences

- Open of a persistent URI is snapshot load plus WAL replay.
- `s3://` without a writable local WAL directory cannot offer
  fsync-per-commit.
- Manifest format_version bumps when checkpoint metadata fields are added
  (ADR-0005).
- ADR-0004’s “custom WAL rejected” alternative no longer holds; this ADR
  supersedes that durability decision.

## Later references

ADR-0014: `:memory:` uses the same WAL + snapshot path (in-process
store). The live graph is materialized from Parquet + WAL replay; commits
are WAL append then `apply_redo`, not in-place mutation plus optional
log. See ADR-0014.

ADR-0019: waiting independent WAL records share one `sync_all`. Delayed
fsync (SUCCESS before durability) stays rejected. Every SUCCESS is still
covered by a fsync.

ADR-0020: checkpoint rotates to a new WAL segment, dumps Parquet at a
pinned LSN/`snapshot_ts` without holding the WAL mutex, then deletes
closed segments with `max_lsn <= checkpoint_lsn`. The single-file
truncate in this ADR is superseded.

ADR-0022: the Parquet dump is a set of id-window shards, not always
`nodes.parquet` + `relationships.parquet`. WAL layout and
`checkpoint_lsn` are unchanged.

ADR-0028: the unit of commit is still one WAL record, but that record
may cover an explicit multi-statement transaction. Auto-commit
statements are unchanged. Bolt `BEGIN` / `COMMIT` are no longer no-ops.

ADR-0044 withdraws engine `s3://` (the local-WAL-plus-remote-Parquet
split in this file). Persistent URIs in this repo are filesystem
paths / `file://` only. Object storage belongs to clients implementing
embedded Relate.
