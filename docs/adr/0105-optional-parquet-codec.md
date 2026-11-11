# 0105. Parquet snapshot codec behind an additive `parquet` Cargo feature

- **Status**: Proposed
- **Date**: 2026-09-25
- **Deciders**: CypherLite maintainers

## Context

LABS-103 asks for a small, lean engine core. ADR-0103 made the engine
boundary enforceable (`cypherlite/` depends only on `cypherlite/`), and
ADR-0104 introduced additive feature bundles for the CLI. Its rule 7 says
engine-level features follow the same rules, each with its own ADR. This is
the first one.

The engine's dependency tree is dominated by one component: the Parquet
snapshot codec in `cypherlite-storage` (ADR-0004, ADR-0022, ADR-0050).

- `cargo tree -p cypherlite -e normal` lists about 112 unique crates.
  `arrow` + `parquet` account for up to 91 of them. About 50 crates, among
  them the heaviest to compile (`arrow-cast`, `arrow-json`, `parquet`), are
  reachable only through them.
- No other engine crate uses Arrow or Parquet. `cypherlite-arrow` and
  `cypherlite-python` (both `surfaces/`) use Arrow for their own reasons.
- Even the `minimal` CLI bundle (ADR-0104) links all of it, because every
  consumer gets the engine's full dependency set.

The code already sits behind a boundary. ADR-0085 cut Parquet out of the
wasm32 build with `cfg(target_arch)`:

- `arrow`/`parquet` are target-gated dependencies.
- The shard writer/reader lives in `snapshot/native/`.
- `SnapshotCodecKind::Parquet` stays an enum variant everywhere.
- A stub returns a typed storage error when Parquet is unavailable.

ADR-0085 rejected a `native` Cargo feature because it would just mirror the
target check. This ADR is different: it lets a *native* build choose to leave
Parquet out, which no target check can express.

Four existing behaviours constrain the design:

1. **The absent-codec default.** ADR-0050 §2 says an absent `codec` in
   `manifest.json` means Parquet. Today that is implemented through
   `SnapshotCodecKind::default()`, which is also the default codec for new
   stores (Parquet on native, JSON on wasm32).
2. **Pull identity.** Its persistence (ADR-0080, ADR-0092) used to exist
   only in the Parquet snapshot path, so a repeated `cypherlite pull` into
   a JSON store duplicated nodes after a checkpoint. ADR-0106 closed that
   gap: JSON snapshots persist the pull-identity map inline in
   `graph.json`. Pull identity no longer depends on the codec.
3. **Native `:memory:` dumps.** A `:memory:` store keeps its WAL and
   snapshot files in a fresh, per-database `MemoryVfs`. Nothing ever reads
   that snapshot back: there is no restart, and the next open creates a new
   `MemoryVfs`. Native `:memory:` has no timed checkpoint (ADR-0041), but
   it still writes a full snapshot into RAM in three places:
   - on every WAL rotation (`finish_applied_commit` → `schedule_checkpoint`
     → `dump_and_recycle`);
   - on close (persist-on-drop);
   - on an explicit `Database::checkpoint()`.

   wasm32 skips the dump in `schedule_checkpoint` and only recycles the WAL
   (up to the checkpoint LSN) and GCs versions (ADR-0085). Without Parquet,
   the native rotation dump would become a full `graph.json` rewrite in
   RAM: more CPU and memory spent on bytes nobody reads.
4. **Workspace dependency plumbing.** A workspace member's
   `default-features = false` is ignored unless the
   `[workspace.dependencies]` entry sets it too.

## Decision

Put the Parquet snapshot codec behind an additive Cargo feature named
`parquet`. It is on by default in the engine crates, so every existing build
and consumer keeps today's behaviour. Builds that do not want it opt out
explicitly. ADR-0104 rules 1, 2, 6 and 7 apply unchanged.

### Features

| Crate | Feature | Enables | Default |
| --- | --- | --- | --- |
| `cypherlite-storage` | `parquet` | optional `arrow`, `parquet`, `bytes` (still target-gated to non-wasm32), the `snapshot/native` shard codec, the Parquet orphan sweep | on |
| `cypherlite` (facade) | `parquet` | `cypherlite-storage/parquet` | on |
| `cypherlite-cli` | `parquet` | `cypherlite/parquet` | in `full`, not in `agent` or `minimal` (see below) |

- Parquet code is compiled only when the `parquet` feature is on and the
  target is not wasm32. `cypherlite-storage`'s `build.rs` names that
  condition once as `cfg(parquet_codec)`, and the source gates on it. The
  feature expresses capability and the target check expresses platform.
  Neither replaces the other, and the ADR-0085 gates that are about
  filesystems, locks and threads stay target-only.
- `SnapshotCodecKind::Parquet` remains a variant in every build (ADR-0085
  §3). Format detection (`resolve_codec`, `has_parquet_store_files`) stays
  compiled in every build, so a build without Parquet can recognise a
  Parquet store and refuse it.

### Codec selection without `parquet`

1. **New stores default to JSON.** The default for *new* stores becomes a
   function of the build: Parquet when the codec is compiled in, JSON
   otherwise.
2. **An absent `codec` field still means Parquet.** Serde decoding of
   `manifest.json` stops using `SnapshotCodecKind::default()` and gets an
   explicit default function that always returns `Parquet`. A pre-ADR-0050
   Parquet store therefore never decodes as JSON, and an empty-graph
   Parquet store is never silently converted.
3. **Fail closed at open.** A store whose resolved codec is Parquet
   (directory or `.cl` bundle), in a build without the codec, fails in
   `Database::open` with a typed `Storage` error. The message names the
   missing feature: `parquet snapshot codec is not compiled into this build
   (enable the "parquet" feature)`. It does not fail lazily at first
   checkpoint.
4. **No silent fallback for explicit requests.** Asking for Parquet through
   `OpenOptions.snapshot_codec`, `CYPHERLITE_SNAPSHOT_FORMAT=parquet`, or the
   CLI `--snapshot-format parquet` in a build without the codec returns the
   same error. The CLI `--help` lists only the codecs compiled into the
   binary (ADR-0104 rule 5).

### Pull does not require Parquet

- Pull identity is codec-neutral since ADR-0106, so `cypherlite-load`'s
  normal dependency does not enable `cypherlite/parquet` and the CLI `pull` feature is only
  `dep:cypherlite-load`. The `agent` bundle (`skill`, `pull`) builds without
  Arrow/Parquet.
- `pull` follows the same codec rules as every other open. A new `DATABASE`
  gets the build's default codec (JSON in `agent`), and a repeated pull into
  a JSON store reuses mapped elements. An existing Parquet store needs a
  build with `parquet`; without it the pull fails closed at open.

### Workspace plumbing

- The `[workspace.dependencies]` entries for `cypherlite` and
  `cypherlite-storage` set `default-features = false`.
- Every workspace member that needs Parquet enables it explicitly
  (`features = ["parquet"]`): `cypherlite-ffi`, `cypherlite-bolt`,
  `cypherlite-python`, `cypherlite-datasets`, the TCK and reference
  loadtests, and the CLI through its `parquet` feature. `cypherlite-wasm`
  does not; `cypherlite-load` enables it only as a dev-dependency, for its
  Parquet pull-identity tests.
- The requirement is then visible in each `Cargo.toml`, and feature
  unification cannot quietly re-enable Parquet in a build that asked for
  less.
- External path consumers such as memorydb-cloud depend on
  `cypherlite/cypherlite` directly. They get the crate's own defaults, so
  Parquet stays on with no change on their side.

### Public API

- Items that exist only to serve the Parquet codec are compiled only with
  the feature: the native checkpoint and load entry points, shard helpers
  and bounded Parquet reads.
- `SnapshotCodecKind`, `OpenOptions.snapshot_codec` and the manifest types
  stay in every build.
- `SnapshotCodecKind` gains `is_compiled(self) -> bool` (`const fn`) and
  `compiled() -> &'static [SnapshotCodecKind]` (new-store default first,
  `Json` last), so callers ask the build instead of repeating the cfg.
  `SnapshotCodecKind::default()` is `compiled()[0]`.
- `cypherlite_storage::memory_store_with_codec` returns
  `Result<StoreTarget>` and rejects an uncompiled codec with the fail-closed
  error at construction.
- `docs/api/rust/spec.md` gains a Cargo features section that lists
  `parquet`, the fail-closed error and the two `SnapshotCodecKind` functions.

### Automatic checkpoints skip the dump for non-durable stores

This applies on every target and in every build, with or without
`parquet`.

- **What skips the dump.** An automatic checkpoint of a store whose
  snapshot storage is not durable (`:memory:`, where the snapshot VFS
  reports `durable() == false`) writes no snapshot. It only recycles the
  WAL up to the checkpoint LSN and GCs versions. The two automatic
  checkpoints are the WAL-rotation checkpoint and the close
  (persist-on-drop) checkpoint.
- **One code path.** This generalises wasm32's current `schedule_checkpoint`
  behaviour. The platform split becomes a single path keyed on
  `durable()`, not on `target_arch`.
- **Explicit checkpoints still dump.** `Database::checkpoint()` still
  writes a real snapshot with the store's codec (JSON when Parquet is not
  compiled in). `Database::snapshot_data_bytes()` after an explicit
  checkpoint keeps its meaning. The caller asked for the snapshot, so they
  get it.
- **Durable stores are unaffected.** Directories and `.cl` bundles always
  dump before recycling, as today.

## Alternatives considered

- **Keep target-only gating (status quo).** Only wasm32 drops Parquet.
  Native consumers such as agent sandboxes, the `minimal` CLI and embedders
  that only use `:memory:` or JSON always pay for about 50 extra crates and
  their build time. Rejected: LABS-103 asks for a lean core on native too.
- **Replace Parquet with JSON entirely.** Parquet gives dirty-window
  incremental checkpoints (ADR-0022). memorydb-cloud and the frozen
  checkpoint baselines (ADR-0070, ADR-0071) are built on it. Rejected: it
  would regress persistence performance and break existing stores to save
  dependencies.
- **Parquet off by default (opt-in).** A plain `cargo build` or a new
  embedder would silently get JSON checkpoints, and existing Parquet stores
  would fail to open after a routine dependency bump. Rejected: ADR-0104
  rule 2 keeps defaults equal to today's behaviour.
- **Move the codec into its own crate behind a runtime codec registry**
  (for example a `cypherlite-parquet` surface crate that registers itself).
  The engine would need a stable, public codec trait and registration at
  open time, and the storage format, which the engine must own, would sit
  outside the engine boundary. Rejected for now: a Cargo feature gives the
  same dependency cut with no new public extension point. A codec plugin
  API can come later if a real third codec needs it.
- **Silently fall back to JSON when Parquet is requested but not compiled
  in.** The store would be written in a format the user did not ask for,
  and a later `full` build would refuse to mix codecs. Rejected:
  persistence choices fail closed.
- **Keep dumping automatic `:memory:` checkpoints on native.** The dump's
  only consumer is restart recovery, which `:memory:` cannot reach. Without
  Parquet it would turn every rotation into a full JSON rewrite in RAM.
  Rejected: it spends CPU and memory on output nobody reads, and it keeps a
  platform-specific split (`target_arch`) where the real distinction is
  durability.
- **Skip every `:memory:` dump, including explicit `Database::checkpoint()`.**
  This would change the documented meaning of `checkpoint()` and
  `snapshot_data_bytes()` for in-memory databases (the `open_options`
  example and existing tests rely on it). Rejected: explicit calls are
  rare and deliberate, so honouring them costs nothing on the hot path.
- **Fix JSON pull identity in this ADR.** That changes the JSON checkpoint
  format and interacts with ADR-0092, which is a separate decision.
  Rejected here to keep one decision per ADR; ADR-0106 makes it.
- **Keep `pull` coupled to `parquet`** (`cypherlite-load` enabling
  `cypherlite/parquet`). The coupling only existed because pull identity
  was Parquet-only. Rejected: after ADR-0106 it would keep Arrow/Parquet in
  the `agent` bundle for no capability.

## Consequences

- Default builds, release artifacts, memorydb-cloud and every client
  binding keep Parquet, so they see no behaviour change.
- The `minimal` and `agent` CLI bundles and any embedder that sets
  `default-features = false` drop about 50 crates, including Arrow and
  Parquet.
  - The binary-size and build-time savings are measured in the
    implementation PR and recorded in `docs/api/cli/spec.md`.
  - Such builds read and write JSON checkpoints. They refuse Parquet stores
    with a clear, typed error.
- **Moving data between builds.** A database created by a `full` build
  (Parquet by default) cannot be opened by an `agent` or `minimal` build.
  To share stores across builds, create them with `--snapshot-format json`.
  The CLI spec states this next to the bundle table.
- **New code and tests:**
  - `cypherlite-storage` needs an explicit serde default for the manifest
    codec, and a build-dependent default codec for new stores.
  - Tests that cover both codecs filter out Parquet in builds without it.
    Parquet-only tests are gated.
  - New tests: a Parquet store opened by a build without Parquet fails
    closed; a `.cl` bundle round-trips with JSON; persistence round-trips
    pass on Native and Memory VFS with JSON only (AGENTS.md §4).
- **CI** adds `cargo test` and `cargo clippy` for
  `-p cypherlite-storage --no-default-features` and
  `-p cypherlite --no-default-features`. The existing `minimal` CLI job now
  also covers the build without Parquet. The wasm32 checks from ADR-0085 are
  unchanged.
- **`:memory:` checkpoints (every build).** Automatic `:memory:`
  checkpoints get cheaper everywhere: no snapshot encode and no second
  encoded copy of the graph in RAM on WAL rotation or close.
  - This is a behaviour change for default native builds too, so it needs
    tests: a rotation-triggered checkpoint on `:memory:` leaves
    `snapshot_data_bytes() == 0` while the WAL is recycled; an explicit
    `checkpoint()` still produces a snapshot.
  - The TCK is re-run to confirm no regression.
  - The fraud and identity loadtests default to `--uri :memory:`, so their
    frozen envelopes (ADR-0046, ADR-0070) will shift: ops/s and p99 should
    improve because rotation no longer encodes the graph. Any
    logical-bytes field that counts snapshot bytes will change. If the
    `loadtest-baselines` gate flags the change, the envelopes are re-frozen
    through the CI workflow (ADR-0071), never from a host run.
  - The wasm32-only `schedule_checkpoint` branch is removed in favour of
    the shared path.
- `pull` works in every build that has it, with either codec, because
  ADR-0106 persists pull identity in JSON snapshots (LABS-104).
- ADR-0050 and ADR-0085 get Later references notes pointing here.
