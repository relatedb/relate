# Multi-threaded integration of an immutable mmap columnar store — findings (2026-11-24)

Question: how do other databases — and vortex-data itself — share a
columnar/mmap property store across engine threads, and what does it cost
in locks? Drives the thread-safe redesign of the engine-side `VortexStore`
(the `!Send + !Sync` failure of `engine-ab-eval`).

## 1. vortex-data itself: lock-free COW session sharing (the fork's own answer)

- `VortexSession(SharedSessionVars)` is *built* for cross-thread sharing:
  clone-cheap, backed by **`ArcSwapMap`** (`vortex-session/src/arc_swap_map.rs`)
  — "a concurrent HashMap backed by an ArcSwap, offering **lock-free reads
  and copy-on-write writes**", with `insert_if_absent` for first-writer-wins
  fill-once semantics. `SessionVar: Any + Send + Sync + 'static`. This is
  the upstream-blessed lock-free pattern for exactly our cache shape, and
  `arc-swap` is already a transitive dep.
- **`vortex-datafusion`**: a `static VX_SESSION: LazyLock<VortexSession>`
  global, `Arc<dyn ObjectStore>`, running under multi-threaded Tokio —
  everything shared is `Arc`'d, nothing is `Rc`.
- **`vortex-duckdb`**: scan/projection/copy against DuckDB's multi-threaded
  C++ engine; the Rust side keeps state in plain shared structures.
- The layout scan is already parallel: `multi.rs` runs up to `concurrency`
  file opens as spawned tasks **on the session runtime** (a pool), so the
  pool runtime is not just Send — it is the runtime the scan machinery is
  designed around.

## 2. Arrow: the Rc→Arc refactor is a solved, precedented move

- `arrow::Buffer` is `Send + Sync` **by construction**: an immutable,
  atomically reference-counted byte region; slices are offset+length over
  shared buffers (zero copy, no lock — immutability makes locks pointless).
- arrow-rs made its parquet readers `Send` in ARROW-9674 (PR #8545, 2020):
  the commit list is literally our TODO — "Use **Arc everywhere instead of
  Rc**; **remove RefCell**; use **atomics** for MemTracker". The refactor we
  face is the one Arrow already did, with the same three moves.

## 3. DuckDB: shared state tiny and locked, private state big and lock-free

- Scan parallelism: one task per *thread*, all pulling from a single
  mutex-guarded cursor. The critical section is pointer bookkeeping only —
  everything expensive (per-column init, decompression, filtering) happens
  outside the lock; each thread has its own `CollectionScanState`.
  Stats (`skipped_rows`) are relaxed atomics, never locks.
- The design rule stated in their own terms: "the shared state is tiny and
  is only about which work comes next; the private state is big and is
  about the work itself."
- The shared BufferPool exists (even shareable across DatabaseInstances)
  but is a cache with eviction latching — a *mutable* cache, which is why
  it needs locks at all. Our base is immutable, so we do not inherit that
  problem.

## 4. Polars: mmap columns + rayon

`ColumnStore::Local(MemSlice)` — mmap'd column byte ranges, prefetched,
then deserialized on rayon worker threads. Sharing the mapping across
threads is free; parallelism lives in the deserialize fan-out, not in
shared mutable state.

## 5. LMDB (from its documented design, not re-measured here): the
architectural cousin

Read-only mmap of a copy-on-write B-tree: readers never block readers or
the writer; the only shared mutable structure is a tiny reader-slot table;
snapshot isolation comes from immutable page generations. Our model is the
same shape at the property layer — immutable checkpoint files (COW
generations) + a single writer (WAL/checkpoint fold) + lock-free reads of
the base. The delta-arena overlay is the part LMDB puts in the write txn.

## Conclusion for the VortexStore redesign

1. **Runtime**: the fork's pool runtime (`Arc<SmolExecutor>`,
   `Arc<Mutex<PoolState>>`) — Send, and the one the scan machinery
   already targets.
2. **Caches**: per-column fill-once `Arc` published via `OnceLock`/`ArcSwap`
   (or one `ArcSwapMap` keyed by column id — vortex's own pattern).
   Hit path: lock-free read of an immutable `Arc<[i64]>`, same ~6 ns as the
   single-thread lab. Miss path: racing decodes possible; worst case is a
   duplicated 130 µs decode, never a blocked reader.
3. **Scan/expand state**: per-thread private (DuckDB morsel rule); the
   store hands out row-ranges, threads keep their own scan state.
4. **Counters**: relaxed atomics only (null counts, stats), never locks.

## What must be measured (the re-measurement requirement)

A lab concurrency kernel, N = worker-thread count, over the
point/batch read shape:

| arm | expectation |
|---|---|
| lock-free fill-once (`OnceLock<Arc>`) | hit ≈ single-thread (6 ns); one cold race per column |
| `RwLock<HashMap>` | hit degrades with N on write-lock episodes; readers mostly parallel |
| `Mutex<HashMap>` | hit serializes — the arm that must lose |
| per-thread caches (today's lab shape ×N) | hit unchanged; resident bytes ×N (the all-or-nothing finding priced per worker) |

Plus: duplicate-decode count under a cold-cache thundering herd, and
alloc/resident deltas per arm. Decides lock-free shared vs per-thread by
measurement, not taste — and prices the fallback (RwLock) if the lock-free
arm surprises.
