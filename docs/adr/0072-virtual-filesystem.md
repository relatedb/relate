---
type: adr
area: engine
date: 2026-09-20
deciders: CypherLite maintainers
name: 0072-virtual-filesystem
status: accepted
tags:
  - storage
title: Synchronous virtual filesystem for WAL and snapshots
REFERENCES:
  - "[[adr/0012-wal-and-parquet-checkpoints]]"
  - "[[adr/0014-single-wal-write-path]]"
  - "[[adr/0019-group-commit-shared-fsync]]"
  - "[[adr/0044-withdraw-engine-s3]]"
  - "[[adr/0050-snapshot-codecs]]"
  - "[[adr/0074-single-file-bundle-vfs]]"
  - "[[adr/0098-safe-atomic-replacement]]"
  - "[[adr/0145-wasm-opfs-vfs]]"
  - "[[adr/0574-withdraw-cdc-replication-slots]]"
---

# 0072. Synchronous virtual filesystem for WAL and snapshots

## Context

Durable CypherLite state is a fsync’d WAL plus codec-owned snapshot files
([ADR-0012](./0012-wal-and-parquet-checkpoints.md), [ADR-0014](./0014-single-wal-write-path.md), [ADR-0050](./0050-snapshot-codecs.md)). Those bytes were written through
`std::fs` in the WAL and Apache `object_store` (plus a local `fs_dir`
bypass) for checkpoints.

The product wants the same engine to run on several storage backings
selected by database URI: native OS directories (today’s default), OPFS
(WASM), a single host file that contains the virtual tree, and object
stores. That requires one filesystem abstraction the engine already uses,
before those backends exist.

[ADR-0044](./0044-withdraw-engine-s3.md) withdrew engine `s3://` and kept `object_store` only for local and
in-memory snapshots. [ADR-0050](./0050-snapshot-codecs.md) rejected a single-file *snapshot codec* URI
(`.db` as GraphData). Neither decision is a VFS.

## Decision

1. **One synchronous `Vfs`.** WAL segments and snapshot objects go through
   `Vfs` / `VfsFile` in `cypherlite-storage`.
   Paths are virtual `/` strings. The commit path stays synchronous
 ([ADR-0014](./0014-single-wal-write-path.md), [ADR-0019](./0019-group-commit-shared-fsync.md)). Future WASM OPFS uses
   `FileSystemSyncAccessHandle`, not an async `Database::open`.

2. **VFS owns how bytes are stored; codecs still own what is written.**
   `manifest.json`, Parquet shards, and `graph.json` keep their names and
 formats ([ADR-0050](./0050-snapshot-codecs.md)). A later single-file VFS still presents that tree
   (`wal/00000001.log`, `nodes/…`). That is not a single-file codec.

3. **URI selects the backend.** Advertise only schemes `Database::open`
 can open ([ADR-0044](./0044-withdraw-engine-s3.md)).
   - `:memory:` / empty → `MemoryVfs` (not durable; no checkpoint timer).
     WAL uses the same `MDBWAL01` framing as disk; process exit still
     discards everything.
   - Filesystem path or `file://…` → `NativeVfs` (default). Layout,
     `sync_all`, and Windows-safe tmp+close+rename stay as today.
   - Other schemes keep failing until a backend exists. Do not document a
     URI the engine rejects.

4. **`LOAD CSV` and other user paths stay on the OS filesystem.** They are
   not database files.

5. **Follow-on backends (not this change):** `opfs://<name>`; an
   object-store VFS (likely feature-gated, or owned by a client
   implementing embedded Relate, so the default crate still does not
 pull AWS). Naive object `put` is still not a WAL `fsync` ([ADR-0012](./0012-wal-and-parquet-checkpoints.md)).
 Single-file packing is [ADR-0074](./0074-single-file-bundle-vfs.md).

## Alternatives considered

- **Keep `object_store` for snapshots and VFS for WAL only** — rejected:
  atomic checkpoint writes would still bypass the abstraction.
- **Async `Vfs` trait** — rejected: group commit and `Database::open` are
  sync; an async trait would rewrite the engine for backends that can
  expose sync handles (native, OPFS sync access).
- **Implement OPFS / single-file / S3 in the same change** — rejected:
  the first cut is the interface plus Native (default) and Memory. Landing
 unimplemented advertised URIs violates [ADR-0044](./0044-withdraw-engine-s3.md).
- **Treat a `.db` file as a JSON codec** — already rejected in [ADR-0050](./0050-snapshot-codecs.md);
  a container VFS is a different layer.

## Consequences

- `cypherlite-storage` no longer depends on Apache `object_store` or a
  Tokio runtime for snapshot I/O.
- Public URIs are unchanged: `:memory:`, path, `file://`; `s3://` still
  errors.
- [ADR-0044](./0044-withdraw-engine-s3.md) still governs engine object storage until an object-store VFS
 exists. [ADR-0050](./0050-snapshot-codecs.md) still governs snapshot codecs.
- TCK, cip-eval, and in-tree loadtest gates must stay green; Native
  on-disk layout is unchanged.

## Later references

[ADR-0074](./0074-single-file-bundle-vfs.md) packs this virtual tree into one host file when the URI path
ends with `.cl`. Native directories remain the default for every other
filesystem path.

[ADR-0574](./0574-withdraw-cdc-replication-slots.md) withdraws CDC slots. The VFS no longer carries `cdc/slots.json`.

[ADR-0098](./0098-safe-atomic-replacement.md) proposes to supersede the fixed `{path}.tmp` spelling implied by the
Native tmp+close+rename description: atomic replacement uses an unpredictable,
exclusively created temporary name and syncs the containing Native directory
after rename on Linux/macOS. The close-before-rename requirement remains.

[ADR-0145](./0145-wasm-opfs-vfs.md) implements the OPFS follow-on named above: `opfs://<name>` on wasm,
`FileSystemSyncAccessHandle` in a dedicated worker, and a synchronous slot-pool
`Vfs`. `Database::open` still rejects the scheme.
