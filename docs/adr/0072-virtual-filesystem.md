# 0072. Synchronous virtual filesystem for WAL and snapshots

- **Status**: Accepted
- **Date**: 2026-09-20
- **Deciders**: CypherLite maintainers

## Context

Durable CypherLite state is a fsync’d WAL plus codec-owned snapshot files
(ADR-0012, ADR-0014, ADR-0050). Those bytes were written through
`std::fs` in the WAL and Apache `object_store` (plus a local `fs_dir`
bypass) for checkpoints.

The product wants the same engine to run on several storage backings
selected by database URI: native OS directories (today’s default), OPFS
(WASM), a single host file that contains the virtual tree, and object
stores. That requires one filesystem abstraction the engine already uses,
before those backends exist.

ADR-0044 withdrew engine `s3://` and kept `object_store` only for local and
in-memory snapshots. ADR-0050 rejected a single-file *snapshot codec* URI
(`.db` as GraphData). Neither decision is a VFS.

## Decision

1. **One synchronous `Vfs`.** WAL segments and snapshot objects go through
   `Vfs` / `VfsFile` in `cypherlite-storage`.
   Paths are virtual `/` strings. The commit path stays synchronous
   (ADR-0014, ADR-0019). Future WASM OPFS uses
   `FileSystemSyncAccessHandle`, not an async `Database::open`.

2. **VFS owns how bytes are stored; codecs still own what is written.**
   `manifest.json`, Parquet shards, and `graph.json` keep their names and
   formats (ADR-0050). A later single-file VFS still presents that tree
   (`wal/00000001.log`, `nodes/…`). That is not a single-file codec.

3. **URI selects the backend.** Advertise only schemes `Database::open`
   can open (ADR-0044).
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
   pull AWS). Naive object `put` is still not a WAL `fsync` (ADR-0012).
   Single-file packing is ADR-0074.

## Alternatives considered

- **Keep `object_store` for snapshots and VFS for WAL only** — rejected:
  atomic checkpoint writes would still bypass the abstraction.
- **Async `Vfs` trait** — rejected: group commit and `Database::open` are
  sync; an async trait would rewrite the engine for backends that can
  expose sync handles (native, OPFS sync access).
- **Implement OPFS / single-file / S3 in the same change** — rejected:
  the first cut is the interface plus Native (default) and Memory. Landing
  unimplemented advertised URIs violates ADR-0044.
- **Treat a `.db` file as a JSON codec** — already rejected in ADR-0050;
  a container VFS is a different layer.

## Consequences

- `cypherlite-storage` no longer depends on Apache `object_store` or a
  Tokio runtime for snapshot I/O.
- Public URIs are unchanged: `:memory:`, path, `file://`; `s3://` still
  errors.
- ADR-0044 still governs engine object storage until an object-store VFS
  exists. ADR-0050 still governs snapshot codecs.
- TCK, cip-eval, and in-tree loadtest gates must stay green; Native
  on-disk layout is unchanged.

## Later references

ADR-0074 packs this virtual tree into one host file when the URI path
ends with `.cl`. Native directories remain the default for every other
filesystem path.

ADR-0574 withdraws CDC slots. The VFS no longer carries `cdc/slots.json`.

ADR-0098 proposes to supersede the fixed `{path}.tmp` spelling implied by the
Native tmp+close+rename description: atomic replacement uses an unpredictable,
exclusively created temporary name and syncs the containing Native directory
after rename on Linux/macOS. The close-before-rename requirement remains.

ADR-0145 implements the OPFS follow-on named above: `opfs://<name>` on wasm,
`FileSystemSyncAccessHandle` in a dedicated worker, and a synchronous slot-pool
`Vfs`. `Database::open` still rejects the scheme.
