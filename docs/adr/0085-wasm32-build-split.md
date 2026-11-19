---
type: adr
area: surface
date: 2026-09-24
deciders: CypherLite maintainers
name: 0085-wasm32-build-split
status: accepted
tags:
  - wasm
title: wasm32 build split for the engine crates
REFERENCES:
  - "[[adr/0022-id-window-checkpoint-shards]]"
  - "[[adr/0051-wasm-memory-database]]"
  - "[[adr/0077-embedded-durability-boundary]]"
  - "[[adr/0084-reinstate-wasm-distribution]]"
  - "[[adr/0105-optional-parquet-codec]]"
  - "[[adr/0259-query-execution-stack-provisioning]]"
---

# 0085. wasm32 build split for the engine crates

## Context

[ADR-0084](./0084-reinstate-wasm-distribution.md) reinstates `crates/cypherlite-wasm` as a distributed `wasm-pack`
package. That package depends transitively on `cypherlite` and
`cypherlite-storage`, but three things in those crates do not build on
`wasm32-unknown-unknown` as written:

- **`std::time::Instant`** — the wasm32 std does not implement a
  monotonic clock. Every WAL, commit-group, and query-worker timing
  path uses `Instant::now`.
- **`std::thread::spawn`** and `thread::scope` — wasm32 has no threads.
 The 64 MiB query-worker thread from [ADR-0259](./0259-query-execution-stack-provisioning.md) and the background
  checkpoint timer + `schedule_checkpoint` spawner in `cypherlite`
  both spawn.
- **`arrow` + `parquet`** — neither builds on wasm32 (native FFI, C++
  code paths). They power the on-disk Parquet snapshot codec that
 [ADR-0022](./0022-id-window-checkpoint-shards.md) introduced.

Additionally, the wasm32 default main-thread stack is 1 MiB. The
planner and executor recurse deep enough that even trivial queries
(`MATCH (n) RETURN n`) exhaust it, which the browser surfaces as
"memory access out of bounds" (a wasm trap).

`:memory:` on `MemoryVfs` ([ADR-0051](./0051-wasm-memory-database.md)) is the only browser database URI;
there is no wasm durability boundary ([ADR-0077](./0077-embedded-durability-boundary.md) already records this).
That constrains what the wasm build must include and lets us cut the
Parquet snapshot writer/reader out entirely.

## Decision

Split the engine build so the wasm32 target compiles a strict subset,
using `#[cfg(target_arch = "wasm32")]` at module and item boundaries.
No new cargo features.

1. **Time.** Replace `std::time::Instant` with `web_time::Instant` in
   `cypherlite-storage` (WAL append timing) and `cypherlite` (commit
   group, query lifecycle). `web-time` is a workspace dep; on native it
   is a re-export of `std::time`, on wasm32 it maps to
   `performance.now()`.
2. **Threads.** On wasm32, `spawn_checkpoint_timer` is a no-op (no
   background threads and no durability boundary for `:memory:`),
   `schedule_checkpoint` runs the dump inline, and `run_on_query_worker`
   calls the closure directly on the current thread. Native keeps the
 64 MiB worker thread from [ADR-0259](./0259-query-execution-stack-provisioning.md) unchanged. The two paths are
   cfg-gated at the function level, not sprinkled inside a shared body.
3. **Storage codec.** `arrow` and `parquet` move under
   `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` in
   `cypherlite-storage`. The Parquet shard writer/reader in
   `snapshot.rs` moves into a single `mod native { ... }` block gated
   at the module. `SnapshotCodecKind::Parquet` remains as an enum
   variant on wasm32 for serialization symmetry, but the wasm-side
   `parquet_checkpoint_at` / `parquet_load` /
   `parquet_snapshot_data_bytes` helpers return a fixed storage error
   (`WASM_PARQUET_UNAVAILABLE`). `SnapshotCodecKind::default()` is
   `Parquet` on native and `Json` on wasm32, so `:memory:` on wasm32
   opens the JSON codec by default.
4. **Filesystem URIs.** `parse_filesystem_uri` and
   `reset_filesystem_uri` fail on wasm32 with a fixed storage error
   (`WASM_FS_UNAVAILABLE`). Only `:memory:` opens.
5. **Stack size.** `.cargo/config.toml` sets a wasm32 linker flag
   `-zstack-size=16777216` (16 MiB), a browser-friendly middle ground
   between the 1 MiB wasm32 default and the 64 MiB native worker stack.
   Native builds are unaffected.
6. **CI gate (from [ADR-0084](./0084-reinstate-wasm-distribution.md)).** The `wasm` job runs
   `cargo check -p cypherlite-storage --target wasm32-unknown-unknown`
   and `cargo check -p cypherlite-wasm --target wasm32-unknown-unknown`
   in addition to `wasm-pack build`, so any dep upgrade that breaks
   wasm32 fails the merge.

## Alternatives considered

- **Introduce a `native` cargo feature** — rejected: it doesn't
  compose cleanly with the target check (wasm32 always turns it off,
  everything else always turns it on), and it doubles the matrix
  every downstream crate has to think about. `cfg(target_arch)` is
  precisely what varies.
- **Add a WASI backend for filesystem and threads** — rejected: no
  consumer today. The browser is the only wasm target that matters
  for this ADR.
- **Ship a wasm-only fork of the crates** — rejected: doubles the
  maintenance surface. The refactor lets one tree serve both.
- **Keep `wasm-opt` off to shrink build time** — rejected: `wasm-pack`
  runs `wasm-opt` on `--release` by default and the ~2 MiB size
  reduction is worth the ~15s.

## Consequences

- One tree builds native and wasm32. The `wasm` CI job ([ADR-0084](./0084-reinstate-wasm-distribution.md)) is
  the gate that enforces the split doesn't rot.
- `web-time = "1"` becomes a workspace dep of `cypherlite-storage` and
  `cypherlite`. It is a shim, not a runtime dep on native, so the
  binary footprint is unchanged.
- The wasm32 build has no Parquet codec, no filesystem, and no
  background threads. Any query that opens a filesystem URI or asks
  for the Parquet codec fails with a typed storage error, not a
  panic. The default codec swap in (3) means `:memory:` "just works"
  in the browser.
- The 16 MiB wasm32 linker stack raises the browser database's
  per-tab memory floor by that amount. For a `:memory:` browser DB
  this is acceptable; native is unaffected.
- Reverting the split (e.g. adopting an alternative timing crate)
  requires a follow-up ADR because the choice above is what the
  `wasm` CI job locks in.

## Later references

[ADR-0105](./0105-optional-parquet-codec.md) adds an additive `parquet` Cargo feature (default on) so native
builds can also leave the Parquet codec out. The target gates above stay
for platform limits; Parquet code compiles only under
`all(feature = "parquet", not(target_arch = "wasm32"))`.
