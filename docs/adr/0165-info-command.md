---
type: adr
area: cli
date: 2026-10-05
deciders: CypherLite maintainers
name: 0165-info-command
status: accepted
tags:
  - commands
  - storage
title: |-
  Cold store-metadata read: `store_info` in cypherlite-storage, surfaced as `cypherlite info` and `.info`
REFERENCES:
  - "[[adr/0050-snapshot-codecs]]"
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0074-single-file-bundle-vfs]]"
  - "[[adr/0103-engine-surfaces-testing-layout]]"
  - "[[adr/0104-cargo-feature-bundles]]"
  - "[[adr/0127-store-version-compatibility]]"
  - "[[adr/0136-read-only-open]]"
  - "[[adr/0156-directory-clone]]"
  - "[[adr/0200-query-command-and-database-flag]]"
---

# 0165. Cold store-metadata read: `store_info` in cypherlite-storage, surfaced as `cypherlite info` and `.info`

## Context

Operators and agents routinely need to answer "what is in this store?"
before opening it: snapshot format and codec, native directory or `.cl`
bundle, size on disk, roughly how many nodes and relationships, which
indexes and constraints exist. No surface answers it today:

- The CLI's lifecycle commands (`clone`, `skill`, `upgrade`, `update`,
  `telemetry`, `pull`, `lab`, `info` —
  `surfaces/cypherlite-cli/src/flags.rs:309-318`) report nothing about
  store metadata; `--version` is the CLI's own version.
- The interactive shell has exactly seven dot-commands (`.help`,
  `.quit`/`.exit`, `.begin`, `.commit`, `.rollback`, `.checkpoint`,
  `.timer` — `docs/reference/dot-commands.md`); none reports store info.
- There is no count store anywhere in the engine:
  `Graph::live_node_count()` is `all_node_ids().len()`, an O(n)
  MVCC-visibility scan
  (`cypherlite/cypherlite-executor/src/graph/mod.rs:838-840`), and
  `Manifest.next_node_id`/`next_rel_id` are monotonic high-water marks
  (ids are never reused) — an upper bound, not a live count.

The data for a cheap answer already exists on disk. For Parquet-codec
stores `manifest.json` persists the format version, codec, checkpoint LSN,
per-shard row counts, and the index/constraint catalog
(`cypherlite/cypherlite-storage/src/lib.rs:114-153`, `ShardPart.rows` at
`cypherlite/cypherlite-storage/src/snapshot.rs:32-38`). But
cypherlite-storage has **no public manifest read**: `read_manifest` is
`pub(crate)` (`cypherlite/cypherlite-storage/src/snapshot/native/mod.rs:413`).
The only way at the metadata today is a full open — `Wal::open`, shard
load, WAL replay — which materializes the whole graph and takes the
store's locks. That is exactly what a pre-open diagnostic must not do;
even the [ADR-0136](./0136-read-only-open.md) read-only open is a loaded-graph affair.

The CLI already depends on `cypherlite-storage` directly
(`surfaces/cypherlite-cli/Cargo.toml`), which workspace rule R3
([ADR-0103](./0103-engine-surfaces-testing-layout.md)) allows, so surfacing this needs no engine (`cypherlite` crate)
API change.

## Decision

1. **One new public accessor in cypherlite-storage reads store metadata
   cold.** The exact public surface:

   ```rust
   /// What physical container a store path holds.
   pub enum StoreLayout {
       NativeDir,
       Bundle { superblock_version: u32 },
   }

   /// The snapshot metadata a cold read recovers without opening the
   /// store.
   pub enum StoreSnapshot {
       /// Parquet-codec store: the full selected manifest.
       Parquet(Manifest),
       /// JSON-codec store: `manifest.json` holds only the codec
 /// envelope ([ADR-0050](./0050-snapshot-codecs.md)); counts and the catalog live in
       /// `graph.json`, which a cold info read never parses.
       Json { checkpoint_lsn: u64, commit_ts: u64 },
       /// No snapshot yet: a fresh store before its first checkpoint
       /// has no `manifest.json`. This is a state to report, not an
       /// error — the typed signal callers match on instead of an
       /// error-message substring.
       None,
   }

   /// Cold-read store metadata: the selected snapshot metadata plus
   /// on-disk facts, gathered without opening the store.
   pub struct StoreInfo {
       pub layout: StoreLayout,
       pub snapshot: StoreSnapshot,
       pub total_bytes: u64,  // recursive dir sum, or bundle host-file length
       pub wal_segments: u32, // segments past the checkpoint, staleness hint
   }

   pub fn store_info(path: &Path) -> Result<StoreInfo>

   /// `store_info` from a store URI instead of a path: a `file://`
   /// prefix is stripped, remote schemes (`s3://`, …) are rejected,
   /// and `:memory:` fails with a typed error — an in-memory store has
   /// no on-disk layout to describe.
   pub fn store_info_uri(uri: &str) -> Result<StoreInfo>
   ```

   `store_info` works for native directories and `.cl` bundles
 ([ADR-0074](./0074-single-file-bundle-vfs.md)) through the Vfs layer: `is_bundle_path` selects the
   layout, and a bundle's superblock `VERSION` is read and classified
 through the [ADR-0127](./0127-store-version-compatibility.md) bundle registry. It never calls `Wal::open`, never loads a shard, and
   never replays the WAL. Failures are typed `cypherlite_core::Error`
   values, never panics: missing database, corrupt `manifest.json`, and
   an unreadable `format_version` rejected by the
 [ADR-0127](./0127-store-version-compatibility.md) manifest gate — the same gate `read_manifest` already applies
   on open. A missing `manifest.json` is not a failure: a fresh store
   before its first checkpoint reports `StoreSnapshot::None`.
2. **New CLI lifecycle command `cypherlite info DATABASE [--json]`.**
 Following [ADR-0069](./0069-cli-lifecycle-commands.md) it is an exit-after positional command recognized
   only as the first argument; a database path colliding with the verb
   stays reachable via `./info` or after `--`. It runs on a cold store
   via `store_info` and prints a human key/value table by default or one
   JSON object with `--json`: snapshot format version
   (`Manifest.format_version`, currently 3) and codec, layout (native
   directory vs `.cl` bundle, with the bundle superblock version), total
   size on disk, checkpoint-time node/rel counts, indexes (name, label,
   property) and constraints (name, kind, label/type, properties) from
   the manifest. Usage errors exit 2; runtime errors exit 1, mirroring
 `clone` and `pull` ([ADR-0156](./0156-directory-clone.md)).
3. **Counts are checkpoint-time, labeled, and never exact on the cold
   path.** Node/rel counts are the sum of `ShardPart.rows` over
   `Manifest.node_parts`/`rel_parts`, printed together with
   `checkpoint_lsn` so staleness is explicit; `wal_segments` (segments
   past the checkpoint) is the staleness hint. There is no exact/live
   mode on the cold path.
4. **New `.info` dot-command — the eighth.** In a session with an open
   database it prints the same manifest metadata through the shared
   accessor (the session knows the database path; reading `manifest.json`
   from the lock-holding process is a plain file read) **plus** live
   node/rel counts via Cypher (`MATCH (n) RETURN count(n)` — exact, since
   the graph is already materialized) and the current catalog via
   `SHOW INDEXES`/`SHOW CONSTRAINTS` (includes WAL-replayed DDL). On
   `:memory:` it prints live counts and the catalog only, with the store
   metadata marked not applicable.
5. **JSON-codec stores are envelope-only in v1.** With the JSON codec,
   `manifest.json` holds only the codec envelope
   (`cypherlite/cypherlite-storage/src/codec/mod.rs`: codec,
   `checkpoint_lsn`, `commit_ts`); counts and the catalog live in
   `graph.json`, which is the whole snapshot — parsing it is O(snapshot
   size) and defeats the cheap-info purpose. The envelope cannot even
   deserialize into `Manifest` (`format_version` is a required field
   with no serde default), and the JSON codec's version dimension is
 `value_encoding` inside `graph.json` ([ADR-0127](./0127-store-version-compatibility.md) Decision 1), so the
   manifest format version has no JSON-side meaning. The
   `StoreSnapshot::Json` arm encodes this in the type: for JSON-codec
   stores `info` shows layout, codec, size, and the envelope's
   checkpoint LSN, and prints format version, counts, indexes, and
   constraints as unavailable. Full JSON-codec support is possible later
   work.
6. **Feature gating follows [ADR-0104](./0104-cargo-feature-bundles.md).** The `info` command gets its own
   Cargo feature `info`, wired into the `full` and `agent` bundles — it
   is a read-only diagnostic useful to agents; `minimal` stays the query
   engine only. The word `info` joins `COMMAND_WORDS` in **every** build
 ([ADR-0104](./0104-cargo-feature-bundles.md): the set is the same in every build), so
   `cypherlite info ...` can never silently open a database of that
 name; a build without the feature fails with the [ADR-0104](./0104-cargo-feature-bundles.md)
   not-included error (exit 2). The `.info` dot-command is ungated —
   compiled into every bundle, `minimal` included. The session shell
   itself ships in every build (the `repl` feature only adds rustyline
   line editing), and `cypherlite-storage` is already a non-optional CLI
   dependency, so gating `.info` would gate nothing; the shared
   store-info projection and the `cypherlite-catalog` dependency it
   renders the manifest catalog through are non-optional for the same
   reason.

## Alternatives considered

- **Open the store read-only ([ADR-0136](./0136-read-only-open.md)) and scan for exact counts.**
  Rejected: full graph materialization dominates the cost — snapshot
  load plus WAL replay is precisely the expense a pre-open diagnostic
  exists to avoid. The cold path must stay a bounded manifest read.
- **Add a count store / running live counters to the engine.** Rejected:
  an engine-wide invariant change (every create/delete path maintaining
  durable counters) to serve a diagnostic. Revisit if exact cold counts
  become a requirement.
- **Expose everything through Cypher `SHOW` commands.** Rejected: format
  version, codec, on-disk size, and checkpoint LSN are not graph data;
  `SHOW` is the wrong surface, and a Cypher statement cannot run on a
  cold store at all.
- **`Database::info()` on the engine's public Rust API.** Rejected: the
  CLI may depend on `cypherlite-storage` directly (workspace rule R3,
 [ADR-0103](./0103-engine-surfaces-testing-layout.md)), so no engine API change is needed for a CLI diagnostic; a
  `Database` method would also imply an open handle, which the cold path
  deliberately avoids.
- **Parse `graph.json` for JSON-codec stores in v1.** Rejected: an
  O(snapshot) read defeats the purpose of the command; envelope-only
  output with an explicit "unavailable" marker is honest and cheap.
- **Gate `.info` behind its own Cargo feature.** Rejected: the
  dot-command is core session surface — the shell ships in every bundle
  (the `repl` feature only adds rustyline line editing) and
  `cypherlite-storage` is already a non-optional CLI dependency, so a
  feature would gate nothing while splitting the session experience
  across builds.

## Consequences

- `cypherlite info DATABASE` answers the pre-open "what is in this
  store?" question in bounded time for both layouts, without taking the
  store's locks or materializing the graph; `.info` gives the exact,
  live answer from inside a session.
- The public surface of `cypherlite-storage` grows `store_info`,
  `store_info_uri`, `StoreInfo`, `StoreLayout`, and `StoreSnapshot` —
  workspace-internal APIs documented in rustdoc; `Manifest` was already
  public.
- Checkpoint-time counts can be stale relative to the WAL; the output
  always carries `checkpoint_lsn` (and the `wal_segments` hint) so the
  staleness is visible rather than silent. Exact cold counts remain
  impossible until a count store exists.
- JSON-codec stores get honest envelope-only output in v1; full
  JSON-codec support can land later without changing the command shape
  or the accessor's cold-read contract.
- Documentation lands in the same change (AGENTS.md §3): the
 hand-written `--help` text, the flag table and [ADR-0104](./0104-cargo-feature-bundles.md) bundle table
  in `docs/api/cli/spec.md`, and `docs/reference/dot-commands.md`
  (seven dot-commands become eight).
- Tests accompany the change: accessor unit tests (native directory,
  `.cl` bundle, missing database, corrupt manifest, unreadable
  `format_version`), and CLI tests gated on the `info` feature across
  the `full`, `agent`, and `minimal` bundles.

## Later references

[ADR-0200](./0200-query-command-and-database-flag.md) moves the store from a positional `DATABASE` to required
`--database PATH`. `:memory:` stays rejected. The cold-read contract is
unchanged.
