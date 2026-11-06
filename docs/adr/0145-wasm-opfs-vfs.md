# 0145. WASM persistence is an OPFS sync-access VFS

- **Status**: Accepted
- **Date**: 2026-09-30
- **Deciders**: CypherLite maintainers

## Context

ADR-0072 defined one synchronous `Vfs` and named the browser follow-on:
`opfs://<name>` with `FileSystemSyncAccessHandle`, not an async
`Database::open`. ADR-0051 and ADR-0084 kept the shipped wasm package on
`:memory:` until that backend existed. Directory calls in the Origin
Private File System are asynchronous, and sync access handles exist only
inside a dedicated worker, so the page thread that
`examples/wasm-playground/` used cannot host the VFS.

A reload or `Worker.terminate` does not run Rust `Drop`. The checkpoint in
the database destructor is therefore not the crash boundary. `Full`
durability already barriers each acknowledged commit (ADR-0077).

## Decision

1. **`opfs://<name>` is a wasm-only open.** `name` is one segment of 1–64
   characters from `[A-Za-z0-9_-]`. `Database::open` / `parse_uri` still
   reject the scheme. The browser calls `CypherliteDb.open`, which prepares
   handles and then `Database::open_vfs`. `:memory:` stays
   `new CypherliteDb()`.

2. **Slot pool, then a synchronous VFS.** `prepareOpfs` (shipped as
   `js/opfs.js`) runs in a dedicated worker. It creates
   `cypherlite/<name>/` under the origin's private directory and takes a
   `readwrite` sync access handle on a catalog file plus 64 slot files.
   After that, `OpfsVfs` in `cypherlite-wasm` implements `Vfs`: the catalog
   maps a virtual path to a slot id (directories are prefixes), and every
   read, write, truncate, and flush is a sync handle call. `cypherlite-storage`
   does not depend on `wasm-bindgen`.

3. **Durability.** `durable()` is true. `sync_all` and `durability_barrier`
   flush the slot. Creating, renaming, or removing a name rewrites and
   flushes the catalog before returning, so a WAL segment exists in the
   namespace before its bytes are acknowledged. `sync_dir` and
   `create_dir_all_for_atomic` flush the catalog again. The catalog handle
   is exclusive across agents; a second opener fails with `Error::Locked`.
   An exhausted pool is a storage error. `reset()` on an OPFS handle drops
   the database, wipes the virtual tree, and reopens the same name.

4. **Engine hook.** `StoreTarget::from_vfs` / `from_vfs_with_codec` build a
   store whose snapshot and WAL share one VFS (`wal/…`, no native
   directory). `Database::open_vfs` is `open_with` after that construction.
   `wal_dir` and follow do not apply. The codec is the build default unless
   `OpenOptions.snapshot_codec` says otherwise (JSON on wasm32).

5. **Playground.** The example engine stays in a dedicated module worker
   and opens the default in-memory database (`new CypherliteDb()`). A
   reload starts empty. Cookbook recipes stay `:memory:` too. `open` remains
   available for a host that wants `opfs://<name>`.

## Alternatives considered

- **Async `Vfs` or a main-thread OPFS proxy** — rejected. ADR-0072 kept the
  commit path synchronous, and a SharedArrayBuffer proxy needs cross-origin
  isolation the playground does not have.
- **IndexedDB as a snapshot codec** — already rejected by ADR-0051. The WAL
  still needs a durable device.
- **One OPFS file per virtual path, created inside `Vfs::open`** —
  rejected. `getFileHandle` is async and cannot be awaited inside the
  synchronous trait. The pool is allocated before the engine runs.
- **Rely on `Drop` to checkpoint when the tab closes** — rejected. Terminating
  the worker skips destructors. Acknowledged commits are durable because
  `Full` flushes the WAL.

## Consequences

- Reloading the playground keeps committed data. Reset clears that store.
- Opening `opfs://` on the page thread fails with an error that says a
  dedicated worker is required. Native URIs are unchanged.
- A second tab cannot open the same name while the handles are held.
- The wasm bundle gains `js/opfs.js` and the wasm-bindgen snippet that
  imports it. The `wasm` CI job covers a fake-slot reopen, the page-thread
  failure, and a real OPFS reopen in a worker.

## Later references

- ADR-0530 moves the playground to the relate-web repository; the
  playground clause now describes that app, which still opens the default
  in-memory database in a worker.
