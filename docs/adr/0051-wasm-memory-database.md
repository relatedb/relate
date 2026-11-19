---
type: adr
area: surface
date: 2026-09-17
deciders: CypherLite / MemoryDB merge
name: 0051-wasm-memory-database
status: accepted
tags:
  - wasm
title: WASM product path is Database `:memory:`
REFERENCES:
  - "[[adr/0050-snapshot-codecs]]"
  - "[[adr/0065-withdraw-wasm-swift-surfaces]]"
  - "[[adr/0084-reinstate-wasm-distribution]]"
  - "[[adr/0145-wasm-opfs-vfs]]"
---

# 0051. WASM product path is Database `:memory:`

## Context

The browser crate (`cypherlite-wasm`) previously wrapped GraphStore
(`Engine` + `MemoryStore`) with write-behind IndexedDB: `OpenDb::open_with_backend`,
`PersistBackend`, `IdbBackend`, `FakeBackend`, and a whole-graph blob flushed
on a debounce timer.

Product query execution is `memorydb::Database` (WAL, CSR, planner, executor).
IndexedDB is not a snapshot codec ([ADR-0050](./0050-snapshot-codecs.md)). A codec owns checkpoint files
next to an unchanged WAL; IndexedDB is an async browser object store.

Keeping GraphStore + IDB only on wasm would freeze a second runtime in the
browser while every other language binding moved to Database.

## Decision

1. **WASM opens `Database::open(":memory:")` only.** JS `CypherLiteDb.create()`
   accepts omitted / `memory://` / `:memory:`. `idb://…` and bare paths error
   (use `memory://`). `flush()` is a no-op Promise.

2. **Delete the persist stack.** `persist/`, `OpenDb`, IndexedDB crates,
   `browser_idb` tests, and native file-backed JsonStore wasm tests are gone,
   not dormant.

3. **Portable snapshots are GraphData.** `exportJson` / `importJson` use the
 [ADR-0050](./0050-snapshot-codecs.md) JSON GraphData helpers. Hosts may put that string in
   `localStorage`. That is not IndexedDB durability.

4. **IndexedDB later, if at all, is an ObjectStore** behind Database — not a
   codec and not a whole-graph KV blob.

5. **Playground is memory-only.** No Storage switch. CI browser smoke is
   `:memory:` (`RETURN 1`, CREATE/MATCH), not IndexedDB. wasm32 omits the
   Parquet/Arrow snapshot codec so the playground binary stays within the
   size budget (JSON GraphData only).

## Alternatives considered

- Treat IndexedDB as a snapshot codec — rejected (wrong layer; WAL still
  needs a durable device).
- Keep GraphStore + IDB until ObjectStore exists — rejected (second runtime).
- Ship a huge Parquet wasm artifact without a size gate — rejected; if the
  playground `.wasm` exceeds the Makefile budget, add a json-only wasm
  feature rather than falling back to GraphStore.

## Consequences

- Reloading the playground loses the graph unless the host saved GraphData.
- File-backed and `idb://` URIs fail until a future ObjectStore lands.
- Native `cargo test -p cypherlite-wasm` covers Cypher, wire, dots, and
  export/import; wasm32 CI checks the crate and runs headless Chrome smoke.

## Later references

- [ADR-0065](./0065-withdraw-wasm-swift-surfaces.md) withdraws the
  WASM-scaffolding half of this ADR's consequences: the `cypherlite-wasm`
  crate, the playground, and the wasm32 CI smoke it describes are removed from
  the tree. The memory-only WASM decision contract above is not contradicted —
  it now governs a crate that no longer exists and would have to be restated
  by any future ADR that reinstates a browser surface.
- [ADR-0084](./0084-reinstate-wasm-distribution.md) reinstates the crate as a
  distributed `wasm-pack` package with a `wasm` CI job, and restates the
  memory-only browser contract above in force. `examples/wasm-playground/`
  is one example consumer.
- [ADR-0145](./0145-wasm-opfs-vfs.md) adds `opfs://<name>` for a dedicated
  worker. `new CypherliteDb()` remains `:memory:`. The playground worker
  uses that default, so a reload starts empty.
