# Rust crate — spec

Crate: `relate`. Errors: `relate::Error` / `relate::Result<T>`.

## `Database`

| Method | Contract |
| --- | --- |
| `Database::open(uri)` | Open `:memory:`, a filesystem path, or `file://…`. `s3://` is rejected (ADR-0044). Always materializes the live graph from the snapshot (if any) plus WAL replay (ADR-0014); corrupt JSON snapshot values, unsupported stored Geometry or unsupported `value_encoding` return `Error::Storage` instead of exposing a partially decoded graph (ADR-0087). The snapshot's format marker is validated before WAL recovery (ADR-0114): a Parquet manifest `format_version` of 1 to 4 is accepted (`unsupported snapshot version N` otherwise; 4 is a v3 manifest plus `ontology_path` selecting the store's immutable `ontology/<128-bit-hex>.json` object, ADR-0120 §5), and JSON `graph.json` must have `value_encoding` absent, 2 or 3 (`unsupported graph value encoding: N`; 3 is encoding 2 plus the inline `ontology` document field, ADR-0120 §6) and `read_only` false (`read-only snapshot not supported`); such a failure leaves every WAL segment unchanged. A supported-older store (Parquet manifest `format_version` 1 or 2, JSON `value_encoding` absent) is migrated to the current format in place before the first query is served — legacy-reader load, WAL replay, checkpoint at the current format, WAL rotated to a fresh current-magic segment — with no user action and no downgrade path (ADR-0127; see [Store version compatibility](#store-version-compatibility-adr-0127)). `OpenOptions.read_only` serves that legacy reader and does not rewrite the store (ADR-0136). A store holding an ontology — committed or still pending in the WAL — has `MDBWAL02` WAL segments where a store without one has `MDBWAL01`; both magics are readable and the WAL never migrates (ADR-0120 §4). Open loads the stored document (nothing is derived from it, ADR-0229 §3 — the document is reachable only through the `db.ontology.*` procedures), a `db.ontology.set` committed but not yet checkpointed survives reopen through WAL replay (ADR-0120 §2), and open still screens the folded document's syntax and value model — never the version gate or the schema validator — so a corrupt record fails open while a pre-bump document stays readable (ADR-0364 §4). An existing store keeps the codec its files record (`manifest.json` / `graph.json`); a manifest without a `codec` field means Parquet (ADR-0050). A new store uses Parquet when the `parquet` feature is compiled in and JSON otherwise. `OpenOptions.snapshot_codec: Some(SnapshotCodecKind::Markdown)` (ADR-0406) makes a directory an Obsidian-compatible vault — the markdown files with YAML frontmatter are the durable snapshot read at open and written at checkpoint, the WAL and engine state live under the vault's hidden `.cypherlite/` directory, and a fresh external vault bootstraps its schema (per-label key properties, the relationship-type vocabulary) into the sidecar on first open.  A store that resolves to Parquet, or an explicit Parquet request, fails here with `Error::Storage` in a build without that feature (see [Cargo features](#cargo-features), ADR-0105). `RELATE_SNAPSHOT_FORMAT` applies only to empty URIs. Storage I/O goes through a synchronous VFS (ADR-0072): `:memory:` is an in-process `MemoryVfs` (process-ephemeral); a path ending in `.rl` is a single-file `BundleVfs` (ADR-0074; `.cl` stays accepted as the legacy extension, ADR-0517 §5); other path/`file://` URIs are `NativeVfs` directories. Persistent ownership is acquired before bundle initialization or WAL recovery (ADR-0097). Negative or `i64::MAX` node/relationship IDs, relationship endpoints, and pull-identity local IDs in WAL, or pull-identity snapshot maps, return `Error::Storage` without applying the invalid record/map; the `i64::MAX` sentinel is also rejected in persisted Parquet and JSON graph IDs (ADR-0086). Persistent URIs start a checkpoint timer (ADR-0041) at `RELATE_CHECKPOINT_INTERVAL_SECS` (default 300; `0` off). Honors `RELATE_LOGICAL_GRAPH_SIZE_LIMIT` (`SIZE`: bytes or `mb`/`mib`/`gb`/`gib`; ADR-0042). Local `LOAD CSV` is denied unless `RELATE_LOAD_CSV_DIR` explicitly enables it (ADR-0081). `RELATE_PATTERN_ENGINE` (ADR-0519) is the diagnostic pattern-engine switch, read once here: `auto` (unset or blank) routes as today, `nfa` forces the frontier NFA rung (ADR-0478) onto every frontier-compilable clause, `walk` stands the traversal kernels down to the per-row walker; a value that is not `auto`/`nfa`/`walk` is `Error::Semantic` failing the open (`invalid RELATE_PATTERN_ENGINE: … expected auto, nfa, or walk`) — never a silent default. Diagnostic-only, never Cypher syntax; no stability promise. |
| `Database::open_with(uri, OpenOptions)` | Same as `open`, with `wal_dir`, `checkpoint_interval`, `durability_mode`, `logical_graph_size_limit`, `concurrent_logical_workspace_limit`, `max_traversal_depth`, `safe_mode`, `snapshot_codec`, `markdown_body_properties`, `load_csv_dir`, and `load_csv_max_bytes`. `durability_mode: None` uses `RELATE_DURABILITY_MODE` (full/everysecond/snapshot; a bad value is `Error::Semantic` failing the open) or `Full`; `Some(mode)` wins and is not persisted. `snapshot_codec: None` uses `RELATE_SNAPSHOT_FORMAT` or the build's default for empty URIs; existing directories must match. `Some(SnapshotCodecKind::Parquet)` in a build without the `parquet` feature is `Error::Storage`, never a JSON fallback. `load_csv_dir: Some(path)` overrides `RELATE_LOAD_CSV_DIR`; `None` uses it or denies local files. `safe_mode: true` (LABS-680, ADR-0477) refuses the agent-typical shapes listed under [`Database::is_safe_mode()`]. |
 `durability_mode: None` uses `RELATE_DURABILITY_MODE` (full/everysecond/snapshot; a bad value is `Error::Semantic` failing the open) or `Full`; `Some(mode)` wins and is not persisted. `snapshot_codec: None` uses `RELATE_SNAPSHOT_FORMAT` or the build's default for empty URIs; existing directories must match. `Some(SnapshotCodecKind::Parquet)` in a build without the `parquet` feature is `Error::Storage`, never a JSON fallback. `load_csv_dir: Some(path)` overrides `RELATE_LOAD_CSV_DIR`; `None` uses it or denies local files. |
| `Database::snapshot_data_bytes()` | Sum of the latest snapshot objects (`manifest.json` plus Parquet parts and the selected immutable pull-identity map, or `graph.json`). Legacy v3 manifests without a selector count the optional `pull-identity.json` sidecar. WAL is excluded. `0` if no checkpoint has been written. On `:memory:` only an explicit `checkpoint()` writes a snapshot, so the value stays `0` until one runs (see `checkpoint()`). A missing manifest-referenced part or selected map is `Error::Storage`; absent v1/v2 monoliths and unreferenced files are excluded. A failed metadata lookup for a present object is an error; JSON manifest and pull-map objects are capped at 16 MiB by VFS metadata and opened-handle length. |
| `Database::checkpoint()` | Write codec-owned store files for rows visible at a pinned `snapshot_ts`, including named schema and validated typed SHOW theater state (index details/options, the fixed namespaces' settings rows (ADR-0116 §8), graph types, derived constraints and enforced labels; ADR-0095). Theater-absent legacy Parquet/JSON snapshots or WAL replay with historically unknown enforced labels keep the theater field absent on checkpoint, preserving known legacy behavior through WAL recycling; a fully complete candidate emits the typed field. A new incomplete named label-existence constraint returns `Error::Storage` before writing any shard, map, graph or manifest. A commit or replication append waits for any admitted checkpoint to finish publication and recycle before the first frame write; new checkpoints wait during physical publication and fail closed while the handle is fenced (ADR-0094). Parquet rewrites dirty id windows under immutable generation-unique part paths (ADR-0022, ADR-0089); invalid persisted part metrics or a theater image that makes the manifest exceed 16 MiB return `Error::Storage` before replacing the checkpoint. Manifest serialization stops at the 16 MiB cap instead of allocating an unbounded JSON buffer. JSON rewrites `graph.json` in full (ADR-0050), including the inline `pull_identity` map (ADR-0106), rejecting negative next-ID counters, pull-identity local IDs outside `0..i64::MAX`, and unrepresentable core temporal properties or other invalid persisted values with `Error::Storage` before writing `graph.json` or advancing its manifest (ADR-0087). A store holding an ontology checkpoints it too (ADR-0120 §5/§6): Parquet writes the document as one immutable `ontology/<128-bit-hex>.json` object and publishes a manifest selecting it with `ontology_path` at `format_version: 4`, retiring the old object only after publication; JSON carries the document as the inline `graph.json` `ontology` string, written with `value_encoding: 3`. A store without an ontology keeps manifest v3 / `value_encoding` 2, so after a `db.ontology.drop()` and a checkpoint the store returns to the pre-ontology format (WAL segments return to `MDBWAL01` once the `MDBWAL02` segments are recycled), and a store that never set an ontology is byte-for-byte unchanged (ADR-0120 §8). Every checkpoint also rewrites the store's schema text mirror under `.cypherlite/schema/` inside the store directory (ADR-0481, unconditional, no flag): `graph-type.cypher` — the canonical graph type body, absent when the store has no graph type; `ontology.json` / `ontology.yaml` — the ontology document verbatim under the extension naming its format, at most one, both absent when none is set; and `schema.cypher` — one `CREATE INDEX` / `CREATE CONSTRAINT` statement per renderable catalog entry in the catalog's name order (entries with no standalone `CREATE` form are omitted, not fabricated), each the `SHOW INDEXES` / `SHOW CONSTRAINTS` `createStatement` rendering, always present and empty for a schema-less catalog. The manifest and the WAL stay authoritative; the text set is a mirror never re-parsed at open — when the folder exists, open verifies it against the checkpointed state and any drift (changed, missing, or extra file) fails closed with `Error::Storage` naming the file; schema changes still WAL-pending are not drift. Recycle closed WAL only after successful durable manifest publication (ADR-0020, ADR-0045). An explicit `checkpoint()` always writes a snapshot with the store's codec, `:memory:` included. Automatic checkpoints (WAL rotation and close/persist-on-drop) of a non-durable store (`:memory:`) write no snapshot: they only recycle closed WAL up to the CDC cut and GC old versions, on every target and in every build (ADR-0105). Durable stores (directories, `.rl` bundles) always dump before recycling. |
| `Database::is_read_only()` | `true` when the handle was opened with `OpenMode::ReadOnly` or `OpenMode::Follow` (ADR-0136). Surfaces use it to skip their own close-time checkpoint. |
| `Database::is_safe_mode()` | `true` when the handle was opened with `OpenOptions::safe_mode` (LABS-680, ADR-0477); fixed for the life of the handle and never auto-detected. Safe mode refuses, at admission, two agent-typical shapes as a `Syntax` error carrying `SafeModeViolation` (CLI exit 6, Bolt `Neo.ClientError.Statement.SyntaxError`, HTTP 400/`42N06`): a `CREATE`/`MERGE` of a new named node without a label; and an unbounded read (a `MATCH` body whose final `RETURN`/`WITH` has no `LIMIT` and no reducing aggregate — `collect()` does not count). |
| `Database::needs_reopen()` | `true` after a [`FollowGapPolicy::Yield`] follower hits a checkpoint gap or a torn WAL apply (ADR-0441). `connect` then returns `Error::ResourceLimit` named `follow-gap` with `retryable: true` (HTTP 429, `metrics.retryable: true`). The loaded graph is left in place. [`FollowGapPolicy::Reload`] never sets this. |
| `Database::clone(source, dest, CloneOptions)` | Copy a database into a new store across the full dir/`.rl` matrix (ADR-0156, ADR-0282). The format of each side is inferred from its path — a `.rl` suffix names a single-file bundle, anything else a native directory (the same classification `Database::open` applies) — so there is no format flag. `CloneOptions` is `wal_dir` plus `to_snapshot_format`; the source is opened `ReadOnly` (not `Follow`) at the LSN that load observes, then `checkpoint_at` and an empty WAL are published into `dest`. `to_snapshot_format: None` (the default) keeps the source's snapshot codec — json stays json, parquet stays parquet; `Some(kind)` transcodes the destination checkpoint to `kind` (ADR-0163). A requested codec not compiled into this build is `Error::Storage` from the storage layer's availability check, raised before the destination is created — no partial destination is left behind. Does not append, checkpoint, migrate, trim, or recycle `source`. A directory destination's WAL is always `<dest>/wal`; `cdc/slots.json` is not copied. `wal_dir` with a `.rl` source is `Error::Storage` from the storage layer (`wal_dir is not supported for .rl single-file databases`, ADR-0074) — a bundle embeds its WAL (ADR-0160, ADR-0163). A `:memory:` or empty source or destination is `Error::Storage`. A writer-held `.rl` source is `Error::Locked`. An existing destination, or a directory destination that is the source directory, is `Error::Storage`. Destination creation is exclusive (ADR-0161): a destination that appears concurrently — including a symlinked final component, which is never followed — is refused (`clone destination already exists`), never adopted, and never deleted. A failure after the destination file or directory is created deletes it; a removal failure is folded into the returned error. The next commit on the clone is the loaded LSN plus one. |
| `Database::open_with_wal_dir(uri, wal_dir)` | Same as `open`, with an explicit local WAL directory. Ignored for `:memory:`. Native directory URIs default to `<dir>/wal`. Rejected for `.rl` bundle URIs (ADR-0074). |
| `Database::open_vfs(vfs, OpenOptions)` | Open on a caller-supplied `Vfs` (ADR-0145). Snapshot and WAL share that VFS; the WAL prefix is `wal`. There is no native directory: `wal_dir` is ignored and `OpenMode::Follow` is `Error::Semantic` (`live freshness (WAL follow) is only supported for native directory databases`). `snapshot_codec: None` uses `SnapshotCodecKind::default()` (JSON on wasm32). `opfs://` is not parsed here; `Database::open("opfs://…")` stays a rejected remote scheme. |
| `StoreTarget::engine_dir()` | The directory holding engine state (CDC slots): `Some(".cypherlite")` for a markdown-codec store, `None` — never a sentinel string — for the snapshot-file codecs, whose engine state lives at the store root. |
| `StoreTarget::from_vfs(vfs)` / `from_vfs_with_codec(vfs, codec)` | Build that store without opening it. `from_vfs` uses `SnapshotCodecKind::default()` and cannot fail. `from_vfs_with_codec` returns `Error::Storage` when the codec is not compiled into this build. |
| `relate_storage::store_info(path)` / `store_info_uri(uri)` | Cold store-metadata read (ADR-0165): answer "what is in this store?" without opening it — no WAL open or replay, no shard load, no lock acquisition — for a native directory or a `.rl` bundle (`StoreInfo::layout`, `total_bytes`, `wal_segments`). The snapshot half is the public `StoreSnapshot`: `Parquet(Manifest)` for a Parquet store (the same manifest reader and version gate as open), `Envelope { codec, checkpoint_lsn, commit_ts }` for an envelope-only codec store — JSON (ADR-0050) or Markdown (ADR-0406, read from the `.cypherlite/` sidecar, bounded by the 16 MiB sidecar metadata limit checked from VFS metadata before any read, failing closed when the sidecar does not name the markdown codec) — and `None` for a fresh store before its first checkpoint, a state, not an error. `store_info_uri` accepts `file://`, rejects remote schemes and `:memory:` with `Error::Storage`. Other failures (missing database, corrupt `manifest.json`, unreadable `format_version`) are typed `Error::Storage` values, never panics. |
| `Database::memory_usage()` | `Result<(used, limit)>`. `used` is the live logical size. `limit` is `None` when unlimited. Acquires the publication read guard (ADR-0094): a poisoned/fenced handle returns `Error::Storage` rather than reporting an empty, unlimited graph (ADR-0099). |
| `Database::connect()` | New connection sharing the in-memory graph. Read-only queries check health, pin `commit_ts`, and copy named indexes, constraints and theater state (including the fixed namespaces' settings, ADR-0116 §8) under a short publication read guard, then execute without holding that guard (ADR-0013, ADR-0018, ADR-0094). A concurrent schema commit cannot change admitted SHOW INDEXES / SHOW CONSTRAINTS / SHOW GRAPH TYPE / SHOW CURRENT GRAPH TYPE / SHOW DATABASES rows. Writes execute on a private overlay of their snapshot (ADR-0016); `commit_ts` is assigned after conflict/capacity selection. Independent eligible waiters share one `sync_all`; success follows sync and application (ADR-0019). In `Full`, independent waiters share one barrier and SUCCESS waits for it; relaxed modes acknowledge after apply and rely on the later barrier/checkpoint (ADR-0077). |
| `Database::register_cdc_slot(name)` | Create a CDC slot at the current WAL LSN (ADR-0045). `name` is a lowercase DNS label. Idempotent: an existing name is returned, not reset. Both `registered_lsn` and `acked_lsn` start at that LSN. Persisted at `<data>/cdc/slots.json`. `:memory:` keeps slots in process. Cypher commits do **not** wait on a consumer poll or ack; slots only hold WAL recycle. |
| `Database::ack_cdc_slot(name, lsn)` | Advance `acked_lsn` (monotonic). `lsn` above the current WAL LSN is `Semantic`. Unknown name is `Semantic`. |
| `Database::drop_cdc_slot(name)` | Remove the slot. Next checkpoint recycles as if it were never registered. Returns whether a slot was removed. |
| `Database::cdc_slots()` | Registered slots (`name`, `registered_lsn`, `acked_lsn`). |
| `Database::cdc_records_after(lsn, limit)` | Copy WAL records with `lsn >` cursor, at most `limit`. Caught-up polls (`lsn >=` WAL LSN) and `limit == 0` return empty without I/O. File WAL skips closed segments whose `max_lsn <=` the poll cursor and seeks past the last delivered record. Brief WAL lock; does not append. In relaxed durability modes, a returned LSN can be lost by a later crash before its barrier/checkpoint. |
| `Database::set_apply_only(on)` / `apply_only()` | Fence Cypher writes in `try_wal_commit` (`Semantic`: `database is a CDC replica; writes are not accepted`). Reads, checkpoint, and `apply_replicated` still work. Persisted with slots. |
| `Database::apply_replicated(record)` | Replica write path. Under the exclusive publication boundary, checks the prospective destination LSN, advancing nonzero source `commit_ts` (below `u64::MAX`), all persisted IDs, exact bounded WAL JSON and ordered graph/catalog applicability before append; a missing or previously deleted relationship endpoint, duplicate schema or invalid options rejects the entire record with its typed graph error, unchanged WAL and graph. A new named node/relationship label-existence constraint requires matching nonempty derived enforcement metadata in the same record or existing theater; missing/mismatched metadata returns `Error::Storage` before append, leaving the WAL unchanged. Historical WAL replay still accepts old unaccompanied constraints. Invalid IDs/timestamps return `Error::Storage`, and a record over the per-record 64 MiB WAL payload cap returns the capacity refusal `Error::Capacity { kind: CapacityKind::WalPayload, .. }` (ADR-0492, reclassified ADR-0494); missing endpoints return `Error::Semantic`. Then follows the configured durability mode: appends once, barriers once for `Full` (relaxed modes acknowledge after apply and use the later barrier/checkpoint), and applies the same redo, including pull identities. Returns the assigned destination LSN. Growing redo can return `Error::Capacity`; source CDC slots exclude replication. Physical write, barrier, rotation or unexpected post-sync apply errors fence the handle with a distinguishable `Error::Storage` containing `uncertain WAL outcome`; close and reopen rather than retry in-process (ADR-0094). |
| `Database::register_procedure(proc)` | Register a TCK / user procedure visible to later `query` calls. The registry is store-level (ADR-0116 §10): the procedure is callable from every namespace session. |
| `Database::connect_namespace(name)` | New connection whose session namespace is `name` (ADR-0117 §2): `neo4j`, `system` or `system_ontologies` (ADR-0116 §1), matched case-insensitively. Any other name is `Error::DatabaseNotFound { name }`, whose `Display` is `semantic error: DatabaseNotFound: Graph not found: <name>` (ADR-0146, ADR-0307). Parses `name`, then delegates to `connect_typed`. See the [runnable namespace example](examples/namespace.md). |
| `Database::connect_typed(namespace)` | New connection in the typed session namespace (ADR-0150): no parsing, so no `DatabaseNotFound`. The canonical constructor — `connect()` is `connect_typed(Namespace::Neo4j)`. |

When `RELATE_COMMIT_TIMING=1`, each commit records histograms for six
rows — parse+plan, overlay execute, wal_lock wait, encode+write, `sync_all`,
and `apply_redo`. Barrier time is zero for relaxed commit paths; the
`EverySecond` background barrier is not charged to a commit. Printed to
stderr on last handle drop unless already reported.

## `Connection`

| Method | Contract |
| --- | --- |
| `query(cypher)` | Parse, plan, execute. Auto-commit unless `begin` is open. Conflicts (`Error::Conflict`) and memory capacity (`Error::Capacity`) exclude individual waiters; exact WAL size, LSN/timestamp and ordered redo applicability are checked for **all** eligible slots before any group frame is written. Any deterministic preflight failure rejects every eligible waiter without appending; a passing group has one durability barrier and ordered apply. A read already admitted on a pinned safe snapshot may finish after a later physical WAL uncertainty; subsequent queries, including reads and queries in an open transaction, fail admission with `Error::Storage("uncertain WAL outcome…")` until reopen (ADR-0094). At the next node or relationship ID `i64::MAX`, CREATE returns `Error::Execution` before WAL append. Inside an explicit transaction, writes remain in the overlay until `commit` (ADR-0028). |
| `query_with_params(cypher, params)` | `$name` parameters from a `BTreeMap<String, Value>`. Every parameter the planned statement references must be supplied (a `null` value is fine): a missing one fails the statement before anything executes with `Error::ParameterMissing`, `Display` `semantic error: ParameterMissing: Expected parameter(s): <names>` — each missing name once, in order of first appearance, joined by `, ` (ADR-0308). `EXPLAIN` plans without parameter values. In an explicit transaction the failure needs a rollback like any statement error. |
| `query_with_params_cancellable(cypher, params, token)` | Same as `query_with_params`, but a clone of `token` can stop the running query from another thread (ADR-0111). The executor checks the token at row-production points and inside its long loops. A cancelled query returns `Error::Cancelled`, whose `Display` is `query cancelled` (ADR-0307); the connection remains usable and a later query on it runs normally. The uncancelled `query` / `query_with_params` paths use a token that is never cancelled, so their signatures and behavior are unchanged. Two related entry points serve network transports: `query_staged(cypher, params, token, heartbeat)` reports whether a failure came before or during execution (ADR-0319), and `commit_with_heartbeat(heartbeat)` commits while ticking a caller-side keep-alive (ADR-0312) and returns the bookmark like `commit` (ADR-0502 §7); see [Staged runs and heartbeats](staged.md). |
| `assume_worker_stack_on_caller_thread()` | The connection-thread fast path (ADR-0504, the follow-up to ADR-0233's connection-thread-waits-on-the-worker shape): the caller asserts that every later call into this connection happens on a thread whose stack carries the engine query worker's own provision — 64 MiB, ADR-0259 — for the connection's remaining lifetime. With the assertion, a statement whose plan is bounded READ work (point and joined reads, catalog/admin commands, plain projections — no variable-length or quantified paths, no GPM patterns, no vector search, no cartesian multi-pattern MATCH, no `UNWIND`, `FOREACH`, `CALL {}`/`WHEN` branches, procedures, `LOAD CSV` or pattern expressions, and **no write clause of any kind**: an auto-commit write must never commit after its client gave up waiting, and writes already pay WAL/commit latency that dwarfs the handoff) runs **inline on the calling thread**: no worker handoff, no context switches. Everything else escalates to the connection's 64 MiB query worker exactly as an unasserted connection's statements do, preserving the `Heartbeat` wait shape (ADR-0312) for the statements that can outlive its interval. **The caller owns the guarantee**: the engine cannot check a thread's stack size, and a caller asserting from a smaller stack reintroduces the pre-ADR-0259 exposure for its own process (a deeply nested statement can abort it — the ADR-0257 depth cap is calibrated for ≥ 64 MiB). Parse, lowering, planning and execution recursion is as safe on an honestly asserted thread as on the worker because it is the same stack budget with the same margin; a panicking statement is contained to the same typed internal-error class on both paths, and cancellation behaves identically. One-way (there is no un-assert); the default is unset, and every statement runs on the worker. On wasm32 there is no worker — statements already run on the caller's thread — so the assertion changes nothing. |
| `execute(cypher)` | Same as `query`; alias for mutation-style calls. |
| `begin()` | Start an explicit transaction: pin entity `snapshot_ts` under the publication guard, allocate `txn_id`. Each later statement checks health and captures the named schema and full theater state under that short guard, then executes without holding it, using the begin-pinned entity view and its own uncommitted overlay. Schema is consistent per statement (changes committed between statements may become visible), not frozen at transaction begin. CREATE/DROP INDEX details, graph types, namespace settings (ADR-0116 §8) and derived constraint SHOW metadata remain private until commit; rollback discards them. Nested `begin` is `already in a transaction`. |
| `commit() -> Result<Option<String>>` | Publish accumulated redo as one WAL record and one `commit_ts`, and return the commit's bookmark (ADR-0502 §7): ADR-0310's string form `relate:<n>`, where `n` is the store's commit sequence — monotonically increasing across every connection and surface, so a bookmark from a later commit always sorts after an earlier one's. Empty redo is a no-op that returns `Ok(None)`: no bookmark is fabricated for a commit that crossed no WAL boundary. Without `begin`: `no transaction`. Failed txn: `transaction failed; rollback required`. Conflict discards the overlay (`Error::Conflict`). Deterministic invalid redo rejects the eligible group without WAL bytes; uncertain physical outcomes fence reads/writes, CDC, checkpoints and drop persistence until reopen (ADR-0094). |
| `rollback()` | Discard the overlay. Connection drop is implicit rollback. Without `begin`: `no transaction`. |
| `in_transaction()` | `true` after `begin` until `commit`, `rollback`, or drop. |
| `set_principal(principal: Option<&str>)` | Set (or clear, with `None`) the connection's authenticated principal (ADR-0273). **Display-only**: reported as the `username` column of `dbms.showCurrentUser()`; confers no authorization — Relate has no engine-level access control. Set it only with an identity the caller has itself verified (the Bolt server sets it after a validated HELLO); surfaces with no notion of identity never call it. `Database::connect()` yields a principal-less connection. |
| `principal() -> Option<&str>` | The principal set on this connection, if any. When unset, `dbms.showCurrentUser()` reports the documented placeholder `'relate'`. |
| `namespace()` | The session namespace, typed `Namespace` (ADR-0147): `Namespace::Neo4j`, `Namespace::System` or `Namespace::SystemOntologies` (ADR-0117 §2). A statement without a `USE` clause runs in this namespace. |
| `set_namespace(name)` | Change the session namespace between transactions (ADR-0116 §5, ADR-0117 §2). Names match case-insensitively; an unknown name is `Error::DatabaseNotFound { name }`, `Display` ending `DatabaseNotFound: Graph not found: <name>` (ADR-0146, ADR-0307). While an explicit transaction is open it fails with `Error::Semantic` ending `TransactionError: Cannot change the namespace while a transaction is open.` |
| `pull_lookup(source, kind, remote_id)` | `Result<Option<i64>>`. Local id for a pulled remote element if it still exists, checked against the current transaction overlay first, then committed graph state (ADR-0080). `Ok(None)` means no live mapping for this element; a poisoned/fenced handle returns `Error::Storage` and is never collapsed into `Ok(None)` (ADR-0099). |

## Schema (ADR-0040)

Standalone Cypher. Engine label / equality / prefix / trigram indexes stay always available (ADR-0010, ADR-0030; prefix/trigram postings materialize lazily on first seek, ADR-0063); these statements only name a `(label, property)` pair.

| Statement | Contract |
| --- | --- |
| `CREATE [RANGE] INDEX [IF NOT EXISTS] [name] FOR (n:Label) ON (n.prop)` | Catalog write. Omitted `name` is `{label}_{property}`. Duplicate name or schema is `Semantic` unless `IF NOT EXISTS`. |
| `CREATE [RANGE] INDEX [IF NOT EXISTS] [name] ON :Label(prop)` | Same, Neo4j 4 alias. |
| `DROP INDEX [IF EXISTS] name` | Catalog write. Missing name is `Semantic` unless `IF EXISTS`. |
| `DROP INDEX [IF EXISTS] ON :Label(prop)` | Drop by schema. |
| `SHOW INDEXES` / `SHOW INDEX` | Read. Columns `name`, `label`, `property`, `type` (`RANGE`). |

Must be the only clause in the query part. Durable via WAL + `manifest.json` `indexes`.

## Types

| Type | Contract |
| --- | --- |
| `OpenOptions` | `wal_dir: Option<PathBuf>`, `checkpoint_interval: Option<Duration>`, `durability_mode: Option<DurabilityMode>` (`None` reads `RELATE_DURABILITY_MODE` (`full`/`everysecond`/`snapshot`); unset or blank is `Full`; a value that does not parse is `Error::Semantic` naming the variable — ADR-0470), `logical_graph_size_limit: Option<u64>` (`None` reads `RELATE_LOGICAL_GRAPH_SIZE_LIMIT`; an unparseable value is `Error::Semantic` from `parse_memory_limit`, not unlimited; `Some(0)` is unlimited), `concurrent_logical_workspace_limit: Option<u64>` (`None` reads `RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT` with the same `SIZE` grammar as `logical_graph_size_limit`; unset or blank leaves admission off; a positive value turns admission on at that pool capacity and does not read the environment; `Some(0)` and a zero or unparseable value are `Error::Semantic`. A set `RELATE_TOKEN_ADMISSION` is `Error::Semantic` naming `RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT`. ADR-0452), `snapshot_codec: Option<SnapshotCodecKind>`, `markdown_body_properties: Option<MarkdownBodyProperties>` (`None` reads `RELATE_MARKDOWN_BODY_PROPERTIES` (the `--markdown-body-properties` grammar) or, for a markdown vault, the sidecar's recorded configuration; the unconfigured form is the single `body` property — ADR-0460: the list's first entry renders as the note body, each subsequent one as a `## <property>` section; a set configuration on a store whose codec is not markdown is `Error::Semantic`), `load_csv_dir: Option<PathBuf>`, `load_csv_max_bytes: Option<u64>`, `mode: OpenMode` (`Writable` default, `ReadOnly`, `Follow`, `Ephemeral` — LABS-903, ADR-0500: the turn-into, a read-only open that loads and then releases the source, so nothing is held open or locked and the source can be deleted or opened writable underneath; `Database::open_ephemeral(uri)` is the convenience twin; on `:memory:` it is the plain memory open). `ReadOnly` and `Follow` load once and refuse writes, checkpoints, CDC slot updates, and `apply_replicated` (`Error::Semantic`, `database is read-only`). They do not create a missing store, migrate, sweep orphans, or trim a torn WAL tail (ADR-0136). `Follow` is refused when the store has no native directory (`:memory:` and `.rl`); it tails a native WAL and reloads across a checkpoint or trim gap (ADR-0137). `follow_gap: FollowGapPolicy` defaults to `Reload`. `Yield` does not load a second graph: it latches `follow-gap` and [`Database::needs_reopen`] becomes true (ADR-0441). See `open_with`, ADR-0077, ADR-0050, and ADR-0081. |
| `CancelToken` | Cooperative query-cancellation flag (ADR-0111). `CancelToken::new()` is uncancelled; `cancel(&self)` sets the shared flag (idempotent); `is_cancelled(&self) -> bool` reads it. `Clone` shares the same underlying `Arc<AtomicBool>`, so a transport can hold a handle while the worker checks another clone. `Default` is uncancelled. Pass it to `Connection::query_with_params_cancellable`; a cancelled query returns `Error::Cancelled` (`Display` `query cancelled`). The staged-run types `Heartbeat`, `Executed` and `ExecutionFailure` are specified in [Staged runs and heartbeats](staged.md). |
| `relate_executor::CsvPolicy` | Immutable, validated internal-graph CSV capability. `CsvPolicy::new(root: Option<PathBuf>, max_bytes: Option<u64>) -> Result<CsvPolicy>` canonicalizes an existing directory, defaults to no root and 8 MiB, and rejects limits outside `1..=67_108_864` with `Semantic("LOAD CSV invalid configuration")`. An absent root denies local reads regardless of limit. A relative directory resolves against the process working directory. |
| `relate_executor::Graph::configure_load_csv(policy: CsvPolicy)` | Replaces the graph's CSV capability before sharing it with execution views. Only a policy constructed by `CsvPolicy::new` (or its default-deny `Default`) can be installed; failed construction leaves the existing policy unchanged. Embedded `Database::open_with` validates the policy before creating persistent storage. Must run before `Graph::share_services_from`: the store's `StoreServices` bundle is mutated in place, and calling it once the bundle is shared panics rather than silently detaching the graph from the store's one-bundle invariant (ADR-0116 §10). |
| `relate_executor::ExecGraph::create_node(labels, properties) -> Result<NodeId>` | On a write view, allocates the next signed 64-bit node ID, records the create in the private overlay, and returns the ID. At `i64::MAX` next ID, returns `Error::Execution("node ID exhausted")` without adding redo; the sentinel cannot be persisted. A read view has no write scratch. `create_rel` likewise returns `Error::Execution("relationship ID exhausted")` at its independent limit. See [runnable example](examples/id_exhaustion.md). |
| `relate_executor::Graph::create_node(labels, properties) -> Result<NodeId>` | Direct graph creation atomically reserves a node ID below `i64::MAX`; at exhaustion returns `Error::Execution("node ID exhausted")` without changing the graph or producing redo. Concurrent attempts at the final ID yield exactly one success. `Graph::create_rel(start, end, rel_type, properties) -> Result<RelId>` independently enforces the same limit (`relationship ID exhausted`); missing endpoints return `Error::Semantic`, and unsupported endpoint IDs return `Error::Storage`. See [runnable example](examples/id_exhaustion.md). |
| `relate_executor::Graph::begin_write() -> Result<()>` / `begin_write_at(write_ts: u64) -> Result<()>` | Start a direct graph write at `commit_ts + 1` or the supplied timestamp. `u64::MAX` is unsupported: an implicit next timestamp at that value (or overflowing) or an explicit `u64::MAX` returns `Error::Execution("write timestamp exhausted")` before changing write state, statistics or pending redo. A committed timestamp of `u64::MAX - 1` remains readable but cannot begin another implicit write. See [runnable example](examples/id_exhaustion.md). |
| `relate_executor::Graph::apply_redo(ops, commit_ts) -> Result<()>` | Apply physical WAL operations and publish `commit_ts`. The timestamp must advance past the current commit timestamp, be nonzero and be below `u64::MAX`; otherwise it returns `Error::Storage` before applying any operation or changing write state. A fresh graph accepts `u64::MAX - 1`, but replaying it on a graph already at that timestamp is rejected. See [runnable example](examples/id_exhaustion.md) and ADR-0094. |
| `relate_executor::Graph::schema_snapshot() -> SchemaSnapshot` | Copy named indexes, constraints and complete graph-backed SHOW theater metadata (index details/options, the fixed namespaces' settings rows (ADR-0116 §8), graph types, current graph type, derived constraints) for an admitted read. Capture together with `Graph::pin_now()` while serializing with graph publication. The embedded database does both under its short publication read guard. A direct Graph caller must provide its own synchronization against concurrent apply. Catalog and theater are copied under non-overlapping locks. Snapshot capture cannot fail; allocation follows schema size. See the [compiled pinned-read example](examples/pinned_read.md). |
| `relate_executor::Graph::checkpoint_theater() -> CheckpointTheater` / `checkpoint_theater_for_write() -> Result<CheckpointTheater>` / `restore_checkpoint_theater(image) -> Result<()>` | Storage-internal typed committed schema metadata. `checkpoint_theater` captures the committed state; the checkpoint writer must call `checkpoint_theater_for_write` first, which validates against the named catalog and returns a typed image containing all known theater plus only exact legacy-unknown named label constraint definitions, or `Error::Storage` for new/inconsistent omissions. After restoring named indexes and constraints, restore a present image before WAL replay or read admission. `CheckpointTheater::enforced_label(name) -> Option<&str>` inspects a named constraint. Restore rejects markers not matching a named label-existence definition, markers with a derived entry, unmarked incomplete label constraints, missing derived graph-type constraints, or index details without a named index with `Error::Storage`. Images without markers require full enforcement; missing images in legacy snapshots retain historical defaults. Malformed or explicit-null images fail open. See the [compiled pinned-read example](examples/pinned_read.md). |
| `relate_executor::Graph::restore_legacy_enforced_constraints(constraints) -> Result<()>` | Storage JSON import helper: install the optional legacy `enforced_label` annotations after the corresponding named constraints are loaded. Validates against the catalog and returns `Error::Storage` on inconsistent annotations. `restore_graph_type_spec(spec) -> Result<()>` reconstructs the current graph type and its derived constraints for legacy GraphData without a typed theater; malformed specifications return `Error::Storage`. |
| `relate_executor::Graph::mark_legacy_unenforced()` / `apply_replayed_redo(ops, commit_ts) -> Result<()>` | Storage recovery helpers: after restoring a theater-absent legacy catalog, record exact named label-existence definitions with unknown enforcement; historical WAL replay validates ordinary ordered redo applicability but permits an old record lacking a derived enforcement companion and records that legacy omission. Do not use these helpers for fresh writes: `apply_redo` enforces complete new records, and `checkpoint_theater_for_write` refuses untracked omissions. See the [compiled pinned-read example](examples/pinned_read.md) for a complete typed record. |
| `relate_executor::ExecGraph::read_at_with_schema(graph, snapshot_ts, schema) -> ExecGraph` | Construct a read-only executor view using the exact admitted timestamp and `&SchemaSnapshot`, without repinning or copying the schema. Keep the matching `Graph::pin_now()` guard alive for the view's execution; write operations require a scratch overlay and will error/panic if called on this read view. Caller serializes capture with publication. SHOW uses the admitted theater even if a later commit occurs. The constructor cannot fail. See the [compiled pinned-read example](examples/pinned_read.md). |
| `relate_executor::ExecGraph::write_with_schema(graph, scratch, schema) -> ExecGraph` | Construct a write-capable view from a `&RefCell<WriteScratch>` and statement-admitted `&SchemaSnapshot`; entity visibility uses `scratch.snapshot_ts()` (typically pinned at transaction begin), while named schema and theater come from statement admission plus the scratch redo in order. DDL changes stay private until committed apply; dropping the scratch rolls them back. Caller keeps its entity pin alive and serializes schema capture with publication. The constructor cannot fail; borrow conflicts panic under `RefCell` rules, and query/commit errors are returned by their respective operations. See the [compiled pinned-read example](examples/pinned_read.md). |
| `relate_executor::RedoOp::CatalogDerivedConstraint { constraint: relate_model::Constraint }` | WAL operation carrying the typed SHOW metadata for a standalone constraint with an `enforcedLabel`. Apply in the same commit after `CreateConstraint`; the constraint name identifies the metadata entry, and `DropConstraint` removes it. It is staged in an explicit transaction and discarded on rollback; replay restores it. Serialization uses the tagged `op` enum and the existing `Constraint` serde fields. Malformed WAL JSON fails decode/open; valid redo may fail deterministic preflight on accompanying catalog operations before append. See the [compiled pinned-read example](examples/pinned_read.md). |
| `relate_executor::execute_read_at(&ReadAdmission) -> Result<QueryResult>` | Run a read-only logical plan at the admitted snapshot without acquiring a fresh pin or schema. Write clauses error. `ReadAdmission` bundles the admission facts: `graph`, the caller-pinned `snapshot_ts`, the `schema: &SchemaSnapshot` captured with that pin, the `request: ExecRequest` (`plan`, `params`, `query_text`, `principal`), and `cancel`. The caller must hold a `Graph::pin_now()` guard for that graph with `pin.ts() == snapshot_ts` through execution; otherwise version GC can remove needed rows. Capture `schema` together with that pin at admission; SHOW INDEXES / SHOW CONSTRAINTS use it even if a schema commit follows. `cancel: Option<&CancelToken>` attaches cooperative cancellation (ADR-0111); `None` is never cancelled. `principal: Option<&str>` is the connection's authenticated principal (ADR-0273), read by `dbms.showCurrentUser`; `None` reports the documented placeholder. There is deliberately no graph-only wrapper that acquires both itself: the publication read guard lives on the database's `Shared` state, not on a bare `Graph`, so pin and schema cannot be captured atomically there; admission must happen under that guard at the caller. See the [compiled pinned-read example](examples/pinned_read.md). |
| `relate_executor::Graph::pin_now() -> Result<SnapshotPin>` | Claim a snapshot pin so GC cannot drop versions this statement still sees (ADR-0038). Returns `Error::Execution` containing `"snapshot pin"` when all 256 slots stay occupied through a bounded retry budget, instead of spinning forever (ADR-0111). The returned `SnapshotPin` unpins on drop; `detach()` keeps the pin held for an explicit transaction. |
| `relate_executor::Graph::load_checkpoint_node(node) -> Result<()>` / `load_checkpoint_rel(rel) -> Result<()>` / `finish_restore()` | Restore a checkpoint entity as a committed version visible at `commit_ts`. Valid only on a graph being restored from a checkpoint, before any write: calls after any write, open or committed, are unsupported. Two such calls are refused: a call while a write is open returns `Error::Storage` without changing the graph, and a call for an ID whose version chain a committed write has extended returns `Error::Storage` and leaves that chain unchanged. Any other call after a write is not detected and its effect is unspecified. Restoring an ID again replaces the earlier restored row — the later row wins: for a node its labels and properties, for a relationship its endpoints, type and properties, and with them its adjacency — so the ID keeps one version that a later delete removes entirely (overlapping legacy parts can hold an ID twice, ADR-0183). Neither call indexes the row or adds it to adjacency: after the last row a loader must call `finish_restore()` once, which builds relationship adjacency and the label index from the restored versions and clears the checkpoint dirty IDs, as the recovery codecs do. `finish_restore` is `#[doc(hidden)]`, like `apply_replayed_redo`: a loader-only step, documented here for loader authors rather than in rustdoc. WAL replay after it (`apply_replayed_redo`) keeps adjacency current, so replay ends in `Graph::rebuild_indexes()`, which rebuilds only the indexes. Unsupported IDs (including relationship endpoints) return `Error::Storage` before insertion. Recovery codecs propagate these failures. |
| `SnapshotCodecKind` | `Parquet` or `Json`. Both variants exist in every build. `SnapshotCodecKind::default()` is the codec for new stores, `SnapshotCodecKind::compiled()[0]`: `Parquet` with the `parquet` feature, `Json` without it and on wasm32. `is_compiled(self) -> bool` and `compiled() -> &'static [SnapshotCodecKind]` report which codecs this build has ([Cargo features](#cargo-features)). A manifest without a `codec` field decodes as `Parquet` in every build. Empty URI only; existing directories must match `manifest.json`. |
| `DurabilityMode` | `Full` (default), `EverySecond`, or `Snapshot`. `EverySecond` may lose the latest approximately one second of acknowledged writes; `Snapshot` may lose all writes since the latest completed checkpoint. |
| `CdcSlot` | `name`, `registered_lsn`, `acked_lsn`. |
| `WalRecord` | `lsn`, `commit_ts`, `ops` (physical redo). Input to `apply_replicated`; output of `cdc_records_after`. |
| `DEFAULT_CHECKPOINT_INTERVAL_SECS` | `300`. Used when `RELATE_CHECKPOINT_INTERVAL_SECS` is unset. |
| `Namespace` | One of the three fixed namespaces of a store (ADR-0116 §1): `Namespace::Neo4j`, `Namespace::System`, `Namespace::SystemOntologies`. `Namespace::parse` / `FromStr` match names case-insensitively; `as_str` / `Display` give the canonical lowercase name; `is_writable()` is true only for `neo4j` and `is_read_only()` only for `system_ontologies`; `Namespace::ALL` lists all three. Session APIs take `&str` names (see `Database::connect_namespace`) or a typed `Namespace` (see `Database::connect_typed`, ADR-0150). |
| `QueryType` | The statement's Neo4j Query API v2 `queryType` (ADR-0162): `QueryType::Read`, `QueryType::Write`, `QueryType::ReadWrite` or `QueryType::Schema`. `as_str()` / `Display` give the wire code — `"r"`, `"w"`, `"rw"`, `"s"` — exactly Neo4j's value set; a future fifth Neo4j value is a new ADR, not silent enum growth. Re-exported from `relate_planner`; carried by `QueryResult::header.query_type`. |
| `StatementEffect` | The engine's truthful write effect (ADR-0402 Decision 2), one of exactly three states: `None` (the statement's plan holds no mutation operator — a read), `Attempted` (the statement's plan holds a mutation operator and the engine applied nothing) and `Applied` (at least one change reached the write redo). That zero state has exactly one spelling, `StatementEffect::None`. `merge()` — the join a transaction ledger folds its statements through — is an explicit three-state table, so no variant order is load-bearing; `from_facts(attempted, applied)` builds an effect from a statement boundary's two execution facts. The accessors `attempted()`, `applied()` and `changed_nothing()` (`attempted` without `applied`, the CLI note's criterion) read the state. Re-exported from `relate_executor`; named at the statement boundary and carried by `QueryResult::effect`. |
| `relate_core::ElementIdRef` | The parsed shape of an element id (ADR-0116 §7, ADR-0139): `ElementIdRef::parse(id)` splits `<kind>:<segment>:<n|r><id>` into its three borrowed segments (`kind`, `segment`, `id`) or returns `None` when the id does not have exactly three colon-separated segments; `namespace()` maps the middle segment back to its `Namespace` (`None` when it names none). The entity segment stays opaque — validation of it is the caller's choice. |

## Helpers

| Function | Contract |
| --- | --- |
| `params_from_json(&str) -> Result<BTreeMap<String, Value>, String>` | Decode a JSON object of query parameters, as the bindings receive them (C ABI, WASM, script execution). Each value decodes with `try_json_to_param_value`: every nested object, even one with a `__type` key, is an ordinary map; typed values are built in Cypher (`point($p)`). Invalid JSON, a non-object, and unrepresentable numbers are `Err` naming the parameter. A thin front-end over `params_from_value`. |
| `params_from_value(&serde_json::Value) -> Result<BTreeMap<String, Value>, String>` | Decode query parameters from an already-parsed JSON value — the typed sibling of `params_from_json` for surfaces holding a parsed request body (the HTTP Query API, the MCP server), so they never round-trip a `Value` through a string. Same conversion and error style: the value must be an object; a non-object and unrepresentable numbers are `Err` naming the parameter. |
| `params_from_map(&serde_json::Map<String, serde_json::Value>) -> Result<BTreeMap<String, Value>, String>` | The map-taking fold `params_from_value` wraps (ADR-0425): callers that already hold the parameters as a JSON map (the HTTP request dialects' folds, the MCP tools) skip the `Value` re-wrap and its clone. Same conversion and error style: unrepresentable numbers are `Err` naming the parameter. |
| `execute_wire(&mut Connection, query: &str, params_json: Option<&str>) -> serde_json::Value` | Run one statement with `params_json` (decoded by `params_from_json`; `None` or blank means none) and return its `relate_model::wire` envelope: the result, or an error envelope for parameters that do not decode or a failing statement. The WASM `execute` is this function, and `run_script_wire` runs each statement through it. |
| `run_script_wire(&mut Connection, script: &str, params_json: Option<&str>) -> Vec<serde_json::Value>` | Run a script (ADR-0147): statements from `relate_splitter::split_executable_statements` (comment-only pieces skipped), in order, each with the same parameters (`params_json` decoded by `params_from_json`; `None` or blank means none), each committing on its own. Returns one `relate_model::wire` envelope per statement run and stops after the first with an `error`, so earlier statements stay applied. A blank or comment-only script returns `[]`; parameters that do not decode return one error envelope and run nothing. The WASM `executeScript` is this function. |
| `parse_memory_limit(s)` | Parse `RELATE_LOGICAL_GRAPH_SIZE_LIMIT` / `--logical-graph-size-limit` `SIZE` to logical bytes. Integer, or integer plus `b`/`bytes`/`mb`/`mib`/`gb`/`gib` (case-insensitive; optional space). `mb`/`gb` are SI; `mib`/`gib` are IEC. Empty, unknown unit, or overflow is `Semantic`. |
| `MAX_TRAVERSAL_DEPTH`, `MAX_TRAVERSAL_DEPTH_CEILING` | The traversal-depth admission default (500) and ceiling (2000) for an explicit `*1..N` / `{1,N}` hop bound (LABS-610, ADR-0466). Re-exported from `relate`. `require_traversal_depth(name, n)` validates `1..=ceiling` with a `Semantic` error naming `name`; `traversal_depth_exceeded_message(requested, limit)` is the user-facing rejection text. |
| `parse_and_lower_statement_with_limits(src, preview, max_traversal_depth)` | Parse and lower a statement with the store's resolved traversal-depth limit instead of the default; a bound above it is a `Syntax` error carrying `TraversalDepthExceeded`. The existing `parse_and_lower*` entries delegate with `MAX_TRAVERSAL_DEPTH`. |
| `parse_and_lower_statement_with_policy(src, preview, StatementPolicy { safe_mode, max_traversal_depth })` | The policy-carrying entry (LABS-680, ADR-0477): `safe_mode: true` also refuses the safe-mode shapes as a `Syntax` error carrying `SafeModeViolation`, at every nesting depth before lowering. `StatementPolicy::unsafened(limit)` is the historical behavior. |

## Cargo features

| Crate | Feature | Default | Gates |
| --- | --- | --- | --- |
| `relate` | `parquet` | on | Forwards `relate-storage/parquet`. |
| `relate-storage` | `parquet` | on | The optional `arrow`, `parquet` and `bytes` dependencies, the Parquet shard codec (checkpoint, load, bounded shard reads, orphan sweep), and the public items `relate_storage::window_width` and `relate_storage::DEFAULT_SHARD_TARGET_BYTES`. |

Parquet code compiles only with the feature on a non-wasm32 target. wasm32 is
JSON-only with or without it (ADR-0085). Parquet-specific contracts in this
spec (shards, part paths, shard limits) apply only to builds that compile the
codec (ADR-0105).

Check the build at runtime instead of repeating the cfg:

| Function | Contract |
| --- | --- |
| `SnapshotCodecKind::is_compiled(self) -> bool` | `const fn`. `true` for `Json` in every build. `true` for `Parquet` only with the `parquet` feature on a non-wasm32 target. A codec that returns `false` fails at open when requested or found on disk. |
| `SnapshotCodecKind::compiled() -> &'static [SnapshotCodecKind]` | Every codec compiled into the build, new-store default first: `[Parquet, Json]` with the codec, `[Json]` without it. Never empty; the last entry is always `Json`, and `compiled()[0] == SnapshotCodecKind::default()`. |

Both are re-exported from `relate` with `SnapshotCodecKind`. Their
rustdoc doctests in `relate-storage` (`src/codec/kind.rs`) are the
runnable examples and run in CI with and without the feature
(`cargo test -p relate-storage` and
`cargo test -p relate-storage --no-default-features`).

`relate-storage` also checks the codec when building an in-memory store
directly (not re-exported from `relate`):

| Function | Contract |
| --- | --- |
| `relate_storage::memory_store_with_codec(codec) -> Result<StoreTarget>` | A fresh `:memory:` store target (its own `MemoryVfs`) whose snapshots use `codec`. A codec that is not compiled into the build (`Parquet` without the feature) returns `Error::Storage` with the unavailable-codec message below at construction, not at the first checkpoint. `memory_store()` uses `SnapshotCodecKind::default()` and cannot fail. Its rustdoc doctest (`src/lib.rs`) is the runnable example. |

Without the feature:

- New stores, `:memory:` included, use `SnapshotCodecKind::Json`.
- `SnapshotCodecKind::Parquet`, `OpenOptions.snapshot_codec` and the manifest
  types still exist, and format detection still recognises Parquet stores.
- `Database::open` / `open_with` fail with `Error::Storage` when the store
  resolves to Parquet (directory or `.rl` bundle, including a manifest with
  no `codec` field), or when Parquet is requested through
  `OpenOptions.snapshot_codec` or `RELATE_SNAPSHOT_FORMAT=parquet`. The
  message is:

  ```text
  parquet snapshot codec is not compiled into this build (enable the "parquet" feature)
  ```

  `Display` prefixes it with `storage error: `. The failure happens at open,
  not at the first checkpoint, and never falls back to JSON.
- Nothing converts a store between codecs. A store shared between builds
  with and without the feature must be created with `SnapshotCodecKind::Json`.
- Pull-identity maps (ADR-0080) persist with either codec: Parquet in a
  selected sidecar (ADR-0092), JSON inline in `graph.json` (ADR-0106). Pull
  needs no Parquet, so `relate-load` does not enable `parquet` for
  consumers.

Depend on the engine without Parquet by turning off default features:

```toml
[dependencies]
relate = { git = "https://github.com/relatedb/relate", default-features = false }
```

Pin a `tag` or `rev` for reproducible builds ([Rust library reference](../../reference/rust-library.md#adding-it-to-your-project)). v0.7.2 and earlier predate the feature and always compile Parquet, so pin a commit or tag that includes ADR-0105.

Inside this workspace, the `[workspace.dependencies]` entries for `relate`
and `relate-storage` set `default-features = false`, so each member opts in
with `features = ["parquet"]`. `relate-ffi`, `relate-bolt`,
`relate-python` and `relate-datasets` enable it; `relate-cli`
enables it through its own `parquet` feature (in the `full` bundle only;
[CLI feature bundles](../cli/spec.md#build-bundles)). `relate-wasm` does
not, and `relate-load` enables it only for its own tests (a
dev-dependency). A consumer outside the workspace that depends on
`relate` directly gets the crate defaults, so Parquet stays on.

## `QueryResult`

- `header: ResultHeader` — the statement's result metadata (ADR-0319, ADR-0343), one struct shared with `ExecutionFailure`:
  - `columns: Vec<String>` — RETURN aliases / generated names
  - `column_types: Vec<String>` — the ADR-0224 per-column type names: one of the 18 canonical names (`STRING`, `INTEGER`, `FLOAT`, `BOOLEAN`, `NODE`, `RELATIONSHIP`, `PATH`, `LIST`, `MAP`, `POINT`, `DATETIME`, `DATE`, `TIME`, `DURATION`, `VECTOR`, `UUID`, `BINARY`, `ANY`) per column, same length and order as `columns`, derived from the FIRST row. A column whose first-row value is Null, or a result with no rows, stamps `ANY`; the name describes the first row only
  - `namespace: Namespace` — the namespace the statement ran in (ADR-0117 §7): the statement's resolved `USE` target, or the session namespace when it carries no `USE` (ADR-0116 §3). Typed as `relate::Namespace`, not a string: construction stamps it, so a result always carries its resolved namespace (ADR-0138)
  - `query_type: QueryType` — the statement's Neo4j Query API v2 `queryType` classification (ADR-0162), derived from the logical plan before execution and stamped at construction for every statement: plain runs, EXPLAIN and PROFILE alike. `QueryType::Read` (`"r"`) is a read-only statement; a write splits on its returned columns (ADR-0332): `QueryType::Write` (`"w"`) writes and returns no columns (e.g. `MATCH … SET` without RETURN), `QueryType::ReadWrite` (`"rw"`) writes and returns columns (e.g. `CREATE (n) RETURN n`); `QueryType::Schema` (`"s"`) is index/constraint DDL. EXPLAIN and PROFILE report the **wrapped** statement's type, so `EXPLAIN` answers "may this run on a read-only target?" without executing anything (its `stats` stay `QueryStats::default()`). See the [runnable query-type example](examples/query_type.md).
  - `elapsed: Duration` — the statement's wall-clock elapsed (ADR-0221): parse, plan, gates and dispatch (execution, or EXPLAIN/PROFILE rendering), measured once around `Connection::run_query`; never includes row serialization or surface-side output. See the [runnable elapsed example](examples/elapsed.md).
- `parse_ms() -> f64` / `plan_ms() -> f64` / `execute_ms() -> f64` — the statement's always-on PARSE / PLAN / EXECUTE split of `header.elapsed` (the ADR-0221 total, split per ADR-0491), in wall-clock milliseconds through the same single Duration→ms conversion as `elapsed_ms()`. PARSE is the front-end parse and lowering; PLAN is binding, logical planning and the admission classification; EXECUTE is the dispatch plus the result build (its `result_generation_ms()` tail) — or, for `EXPLAIN`, the plan render, that path's execute-equivalent stage (the wrapped statement never executes). The three phases are disjoint sub-windows of the measured window, so `parse_ms() + plan_ms() + execute_ms() <= elapsed_ms()`: the inter-stage gaps (the post-EXPLAIN gates, token reservation) stay in the total alone, which keeps its meaning unchanged. Always on — no PROFILE opt-in, no feature flag, no environment variable — and per statement exactly like `elapsed`: a session or transaction running several statements reports each statement's own split, never an accumulated one. See the [runnable phase-timing example](examples/phase_timing.md).
- `lex_parse_ms() -> f64` / `rewrite_ms() -> f64` / `lower_ms() -> f64` — the PARSE stage's fine sub-split (LABS-1120): `lex_parse` covers lexing plus the AST build (fused at the parser's one tokenizing constructor — splitting them would run the tokenizer twice), `rewrite` the CIP-231 `DEFINE`-block resolver pass, `lower` validation-and-lowering (fused: the validation battery runs inside the lowering walk). Sub-windows of `parse_ms()`, so their sum never exceeds it; `0.0` on a plan-cache hit — nothing was parsed, which `plan_cache()` says. Measured always-on like the coarse split: the four extra clock stamps cost ~130 ns, below the p50 resolution of a 16 µs point read (the LABS-1120 measurement).
- `result_generation_ms() -> f64` — the EXECUTE stage's result-generation sub-window (LABS-1120): the buffered executor result's conversion into the `QueryResult` — the column-type stamp, the header, a staged failure's derived columns. A sub-window of `execute_ms()`; `0.0` on `EXPLAIN`, whose execute-equivalent stage (the render) is itself its result generation.
- `plan_cache() -> PlanCacheOutcome` — which plan-cache stage (ADR-0503) served the statement (LABS-1120): `PlanCacheOutcome::Miss` (no cached plan served — the first run of a template, or a hit rejected for literal-shape safety), `HitRawFastPath` (stage 1, the raw text), `HitTemplate` (stage 2, the auto-parameterized template), `ReplanGenerationMismatch` (a hit re-planned under a fresh stats generation), `Bypassed` (not consulted — a caller parameter map carrying the reserved `__AUTO_*__` prefix, or a non-cacheable statement). `as_str()` is the one wire spelling every surface renders; the outcome is what explains a hit's near-zero `parse_ms()` and zero fine parse sub-windows.
- `rows: Vec<Vec<Value>>` — row-major, Cypher nulls as `Value::Null`; a statement without `RETURN` — including index, constraint, database and graph-type DDL — returns zero rows and zero columns. In a `NEXT` chain (CIP-213) such a schema/admin DDL stage still seeds the next stage with one unit row, exactly like a data write, so `CREATE CONSTRAINT c FOR (x:N) REQUIRE x.p IS UNIQUE NEXT MATCH (n) RETURN n` returns the graph's nodes (ADR-0287); row-producing catalog commands (the SHOW family, `TERMINATE TRANSACTIONS`) seed the next stage from their projected rows instead.
- `stats: QueryStats` — two counter families (ADR-0309). The openCypher graph-diff counters the TCK side effects check: `labels_added`/`labels_removed` count label names that appear in or vanish from the graph (`CREATE (:A), (:A)` adds 1), `properties_set`/`properties_removed` count property additions and removals (an overwrite is one of each; a deleted entity's properties are removed). Neo4j's update counters, which every wire `stats` object renders (`relate::query_stats_to_wire`): `node_labels_added` and `node_labels_removed` count labels per node (by `CREATE`/`SET`/`REMOVE`; a delete counts none), and `properties_written` counts one per property a `CREATE`, `SET` or `REMOVE` writes. Shared by both: `nodes_created`, `nodes_deleted`, `relationships_created`, `relationships_deleted`, and the schema DDL counters `indexes_added`, `indexes_removed`, `constraints_added`, `constraints_removed` (an `IF [NOT] EXISTS` no-op counts 0; the range index backing a `UNIQUE`/`KEY` constraint is not counted as an index)
- `effect: StatementEffect` — the engine's truthful write effect for the statement (ADR-0402 Decision 2), the fact the CLI's batch zero-effect write note reads, named once at the statement boundary. One of three engine-recorded states: `None` — the statement's plan holds no mutation operator, so nothing was applied (a read); `Attempted` — the statement's plan holds a mutation operator (a data-write clause, schema DDL, a catalog write — `ALTER DATABASE`, a graph-type change — or a writes-flagged procedure call), including a write inside a `CALL { … }` or `WHEN/THEN/ELSE` body that never dispatched, and the engine applied nothing, even when the row pipeline fed it no rows; `Applied` — at least one change reached the write redo (entity, schema and catalog changes alike, counted through the one redo funnel). `attempted()`, `applied()` and `changed_nothing()` (the note's criterion: `attempted` without `applied`) read the state. Neither the plan-derived `query_type` (deliberately conservative, ADR-0332) nor `stats` can answer it — a read-only `CALL { … }` is classified `w`/`rw` and moves no counter, and a catalog change moves no counter either. A read, and every `EXPLAIN` result (which does not execute), carries `StatementEffect::None`; `PROFILE` executes, so its effect is real — the CLI excludes a plan-producing run from its per-statement note only, and a `PROFILE` write still counts in the transaction summary. `merge()` is the join of the three-state lattice, which is how a transaction's ledger is accumulated. This is execution metadata of the same class as `receipt` — not announced result metadata (`header`) and not carried by any wire envelope (ADR-0402 Decision 5). The `QueryResult` doc comment carries a runnable example.
- `bookmark: Option<String>` — the statement's commit bookmark (ADR-0502 §7): ADR-0310's string form `relate:<n>` of the commit timestamp the WAL boundary assigned this statement's auto-commit write. `n` is the store's commit sequence — monotonically increasing across every connection and surface — so a bookmark from a later commit always sorts after an earlier one's, and the batched paths carry the last batch's commit (`CALL { … } IN TRANSACTIONS`, ADR-0210; ADR-0209 bulk batches). `None` on every statement that committed nothing: a read, a write whose row pipeline was empty, an `EXPLAIN` (nothing executes), and every statement inside an explicit transaction — those commit at `commit()`, which returns the bookmark. Execution metadata of the same class as `effect` (ADR-0402), not announced result metadata (`header`). The field's doc comment carries a runnable example.
- `plan: Option<Box<PlanArtifacts>>` — the plan tree and the query notifications derived with it (ADR-0344/0345, ADR-0363, ADR-0374) as one artifact: `Some` when the statement was `EXPLAIN` or `PROFILE`, `None` on every plain run, so a result can never carry notifications without the tree they derive from. `PlanArtifacts { root: PlanNode, notifications: Vec<Notification> }`; `QueryResult::plan_root() -> Option<&PlanNode>` and `QueryResult::notifications() -> &[Notification]` are the read accessors — the notification accessor is the empty slice on a plain run. See [EXPLAIN and PROFILE plan trees](#explain-and-profile-plan-trees-adr-03440345) and [Query notifications](#query-notifications-adr-0363).
- `receipt: Option<ExecutionReceipt>` — the statement's peak logical workspace, in bytes, when concurrent logical-workspace admission is on (ADR-0452, ADR-0501). `ExecutionReceipt { peak_logical_bytes, outcome }`. `peak_logical_bytes` is the high-water mark of the workspace the statement held. `None` when admission is off, and on `EXPLAIN`, which does not reserve. `PROFILE` stamps the same peak on the plan root, and the plan args carry it as `PeakLogicalBytes`. The receipt carries no schedule name.

## Namespaces (ADR-0116, ADR-0117)

Every store has three fixed namespaces; none can be created, dropped or
renamed, and names match case-insensitively (`relate::Namespace`):

| Namespace | Behaviour |
| --- | --- |
| `neo4j` | The user data graph; the only namespace Cypher can write. Existing stores are this namespace. |
| `system` | CE system database: SHOW commands, supported administration commands (`SHOW DATABASES`, `ALTER DATABASE … SET DEFAULT LANGUAGE`) and standalone procedure calls (`CALL … [YIELD …] [RETURN …]`) only. Anything else fails with `Error::Semantic` ending `SemanticError: The following unsupported clauses were used: <clauses>. \nThe system database supports a restricted set of Cypher clauses. The supported clause structure for procedure calls is: CALL, YIELD, RETURN. YIELD and RETURN clauses are optional. The order of the clauses is fixed and each can only occur once.` (the space before the newline is CE's) — except a `WITH` composing an administration command (`… YIELD … WITH … RETURN …`, in either order), which fails with `Error::Semantic` ending `SemanticError: The following clauses are not allowed on a system database: WITH.` It holds no graph data. |
| `system_ontologies` | Listed but not queryable (ADR-0229): the name stays accepted everywhere a namespace name is parsed and `SHOW DATABASES` keeps its row, but every statement routed to it — reads included, `EXPLAIN`/`PROFILE`-wrapped included — fails with `Error::Semantic` ending `SemanticError: This database is not queryable. Use the db.ontology.* procedures on the user database 'neo4j' instead.` (GQLSTATUS 42N17). Only the ADR-0116 §2 database-administration commands (`SHOW DATABASES`, `SHOW DEFAULT DATABASE`, `SHOW HOME DATABASE`, `ALTER DATABASE … SET DEFAULT LANGUAGE`) still run from there; the stored ontology document is reachable only through the `db.ontology.*` procedures. |

A connection's session namespace selects the namespace its statements run in
(`Database::connect_namespace`, `Connection::set_namespace`,
`Connection::namespace`). A statement's `USE` clause overrides it for that
statement (ADR-0116 §3): the target is resolved before execution — a static
name must be one of the three namespaces (anything else fails with
`Error::DatabaseNotFound { name }`, ending `DatabaseNotFound: Graph not found: <name>`),
`graph.byElementId('<kind>:relate[.system_ontologies]:<n|r><id>')`
resolves through the element id's middle segment (an id naming no namespace
fails ending ``SemanticError: No database is corresponding to `graph.byElementId(<id>)`. Verify that the elementId is correct.``, with `<id>` echoed as written), and `graph.byName` / `graph.names` / `graph.propertiesByName`
fail ending `SemanticError: Dynamic graph lookup not allowed here. This
feature is only available on composite databases.\nAttempted to access graph
<function call as written>`, as a `USE` target and as a function call.
Several distinct targets in one statement (across `UNION` arms, `CALL`
subqueries or `NEXT`) fail ending `SemanticError: Multiple graphs in the same
query not allowed here. This feature is only available on composite
databases.\nAttempted to access graph <second distinct target>`.
Database-administration commands run from any
namespace against the store's catalog — except through an explicit `USE`
clause targeting a non-system namespace (`USE neo4j SHOW DATABASES`,
`USE system_ontologies …`), which fails with `Error::Syntax` ending
``UseClauseWithAdministrationCommand: The `USE` clause is not required for
administration commands. Retry your query without the `USE` clause, and it
will be routed automatically.`` (CE 42N79); `USE system` + administration
command stays accepted (ADR-0116 §2's Later references). Explicit transactions read `neo4j`
and `system` and write only `neo4j`. Registered procedures and the LOAD CSV
policy are store-level and shared by every namespace; the memory limit
(`OpenOptions.logical_graph_size_limit`, `Database::memory_usage`) covers every
namespace.

### Ontology procedures (ADR-0121, ADR-0226, ADR-0227, ADR-0364, ADR-0378)

`db.ontology.set(ontology :: STRING)`, `db.ontology.get(format = "yaml" ::
STRING) :: (ontology :: STRING)`, `db.ontology.drop()` and
`db.ontology.apply()` manage the store's single ontology document, following
Neo4j's CIP-287 surface: no database argument — each acts on the session or
`USE`-target namespace, which must be `neo4j`; executed against
`system_ontologies` every procedure fails that namespace's not-queryable
gate first, with `Error::Semantic` ending `SemanticError: This database is
not queryable. Use the db.ontology.* procedures on the user database 'neo4j'
instead.` (ADR-0229 §1/§2), and executed against `system` they fail with
`Error::Semantic` ending `SemanticError: Ontologies can only be stored for
the user database 'neo4j'.` The four procedures require
`RELATE_PREVIEW=ontology` (ADR-0378): `Database::open` resolves the
comma-separated `RELATE_PREVIEW` list once (ADR-0371). With the feature
off, every call fails with `Error::Semantic`:

```text
semantic error: SemanticError: preview feature 'ontology' is not enabled: set RELATE_PREVIEW=ontology
```

`SHOW PROCEDURES` omits the four rows while the feature is off; with it on,
the catalog lists them and every call runs unchanged. The gate governs
callability only: a stored document opens, replays and checkpoints with the
gate off and is served again after reopening with the opt-in. `set` accepts a JSON or
YAML document, validates it against the published Ontology Graph Spec
1.1.0 schema (ADR-0487, succeeding ADR-0364) before anything is committed
(any `schema` URI other than
`https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json` fails ending
`Unsupported ontology spec version: '<uri>'. This version of Relate
supports up to Ontology Graph Spec 1.1.0
('https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json').`; a
document over 16 MiB is refused), and stores the submitted bytes verbatim.
The accepted shape is the published closed one: `version` is a string,
property existence is `must_exist`, constraints are id-keyed maps of one
closed shape with an uppercase `type`, `one_of` items are strings, and
extensions are closed objects (see
the [ontology procedures reference](../../reference/ontology-procedures.md)).
A pre-bump document fails at the gate or on its first old-shape member;
`get` never re-validates a stored document and returns it unchanged when the
requested format (`"yaml"` default, `"json"`) matches the stored one and
canonically converted otherwise, `null` when none is set; any other format
value fails ending `InvalidArgumentValue:
unknown db.ontology.get format '<value>'; expected 'yaml' or 'json'`. `drop`
succeeds when none is set. `apply` converts the document to a CIP-176 graph
type and applies it to `neo4j` with `ALTER CURRENT GRAPH TYPE SET` semantics.
It is the one operation that interprets the stored document, so a pre-bump
stored document fails there with the gate's semantic error ending
`Unsupported ontology spec version: '<uri>'. …` (never an internal error)
until it is replaced; `get` keeps returning its bytes unchanged. `apply`
fails when no ontology is set (ending `OntologyNotFound: no ontology is set
for database 'neo4j'.`), when a main-valid document cannot be expressed as a
graph type (a named `OntologyError::Mapping`, ADR-0364 §3/§5), or when
existing data violates the resulting graph
type (a validate-first scan; the error names the constraint and the offending
element's element id). `set`/`drop` run only in implicit transactions — in an
explicit transaction they fail ending `'db.ontology.set' can only be executed
in an implicit transaction, but tried to execute in an explicit transaction.`
(resp. `drop`) — and a statement calling them may not write anything else
(`SemanticError: Writing to more than one database per transaction is not
allowed.`). `get` reads the calling transaction's captured document (the
begin-time capture in an explicit transaction); `apply` may run in explicit
transactions. There is no privilege check (ADR-0121 §4). See the
[runnable ontology example](examples/ontology.md).

## `Value`

`Null`, `Bool`, `Int(i64)`, `Float(f64)`, `String(Arc<str>)`, `List(Arc<Vec<Value>>)`, `Map(Arc<BTreeMap<String, Value>>)`, `Node`, `Relationship`, `Path`, `Temporal` (openCypher date/time/localtime/datetime/localdatetime/duration; ADR-0021), `Point`, `Uuid(u128)` (CIP-246), `Vector { coord: VectorCoordKind, values: Arc<Vec<Value>> }`, `Bytes(Arc<[u8]>)`. Core `Value` has no `Geometry` variant.

`Bytes` is a statement-scoped byte array ([ADR-0327](../../adr/0327-engine-statement-scoped-bytes-value.md)).
No Cypher expression creates one: it enters as a parameter (`Value::Bytes`
passed to `query_with_params`, or a PackStream Bytes parameter over Bolt)
and flows through expressions, lists and maps into results. Equality,
DISTINCT and grouping are byte-wise; `<`/`>` compare byte-wise; ORDER BY
sorts BYTES directly after LIST; `valueType()` is
`LIST<INTEGER NOT NULL> NOT NULL` and `IS :: LIST<INTEGER>` holds; the column
type stamp is `BINARY`; `logical_bytes()` is 16 plus the length. Storing one
as a node or relationship property (CREATE, MERGE, SET), alone or nested in
lists at any depth, fails with `Error::TypeError` (`type error: TypeError:
byte array values cannot be stored as node or relationship properties; they
live only inside a statement (parameter to result)`) and writes nothing (the
one property-value verdict walk, ADR-0339); persistent
byte-array properties are the Linear follow-up "Engine: persist bytes
(byte-array) values as node and relationship properties (WAL, snapshot and
Parquet/JSON checkpoint codecs)". `value_to_cypher`, the Display / Cypher
literal form and `to_json_plain` render it as the list of its byte values
0–255 (`[0, 1, 255]`), so every `CypherValue` surface (C ABI, JS, Java, Go,
Python, WASM, HTTP, MCP, CLI) sees a `LIST<INTEGER>`; only Bolt sends real
bytes. See the [runnable byte-array example](examples/bytes_value.md).

The `String`, `List`, `Map` and `Vector` payloads are shared
`std::sync::Arc`s ([ADR-0154](../../adr/0154-value-clone-cost.md),
[ADR-0170](../../adr/0170-shared-value-payloads.md)). Cloning a `Value`, or
a row or `QueryResult` holding one, increments a refcount and shares the
payload in O(1) instead of copying it. Values are immutable: a shared
payload is never changed in place (`Arc::make_mut` copies a shared payload
first), so a clone never observes a change. `Value` stays
`Send + Sync`, `PartialEq` compares contents, and `Debug` output and
`logical_bytes()` (ADR-0042) are unchanged. The change is breaking only for
Rust code that constructs or destructures these four variants; the C ABI,
Bolt, persisted WAL/JSON/Parquet bytes and `relate_model::CypherValue`
are unchanged. There are no payload constructor or accessor helpers:

- **Construct** from an owned `String`/`&str`, `Vec` or `BTreeMap` with
  `.into()` or `Arc::new(..)`.
- **Read** through deref (`xs.iter()`, `xs.len()`, `m.get("k")`, `&**s`);
  existing methods keep their signatures (`as_string() -> Option<&str>`).
- **Take an owned payload** with `Arc::unwrap_or_clone`: it moves the `Vec`
  or `BTreeMap` out when the `Arc` is unique and copies it once otherwise.
  `Arc<str>` has no `unwrap_or_clone`; borrow the `&str`, or call
  `String::from(&*s)` where an owned `String` is required.

```rust
use std::collections::BTreeMap;
use std::sync::Arc;
use relate::Value;

let name = Value::String("Alice".into());
let tags = Value::List(vec![Value::Int(1), Value::Int(2)].into());
let person = Value::Map(Arc::new(BTreeMap::from([("name".to_string(), name.clone())])));

let copy = tags.clone(); // shares the Vec: one refcount increment
if let Value::List(items) = copy {
    // `tags` still shares the Vec, so this copies it once.
    let owned: Vec<Value> = Arc::unwrap_or_clone(items);
    assert_eq!(owned.len(), 2);
}
if let Value::Map(fields) = &person {
    assert_eq!(fields.get("name"), Some(&name));
}
if let Value::String(s) = name {
    let owned: String = String::from(&*s);
    assert_eq!(owned, "Alice");
}
```

See the [runnable value-sharing example](examples/value_sharing.md).

Display / Cypher literal form is used by the CLI and C ABI text export.

## Persistence JSON and query parameters (ADR-0087)

These are APIs of `relate_model::serde_value` (`CypherValue` is from
`relate_model`), not the untagged `relate_model::wire` result JSON.
The decoder takes a borrowed `serde_json::Value`; decoding does not mutate it.

| Function / type | Contract |
| --- | --- |
| `EncodeError { path: String, message: String }` | Public writer error (`Display` and `std::error::Error`); `path` starts at `$` and identifies the offending nested map/property key, list or geometry-collection index, vector coordinate, or typed field. |
| `cypher_value_to_json(&CypherValue) -> Result<serde_json::Value, EncodeError>` | Encode a v2 persistence value. Simple values use JSON scalars/lists/maps; temporal, duration, point, geometry and vector values use `__type` tags. Every ordinary map with a `__type` key, even nested, is escaped as `{"__type":"map","value":{...}}` with recursively encoded values. Reject non-finite scalar/spatial/vector numbers, invalid vector dimensions/coordinate types/ranges (including inexact Float32), Time/DateTime `FixedOffset`s containing seconds beyond whole minutes, DateTimes whose UTC instant plus offset is outside chrono's local `NaiveDateTime` range, and Node/Relationship/Path at any depth rather than writing null or stringifying graph identities. `serde::Serialize` for `CypherValue` reports the writer failure as a Serde error. |
| `try_json_to_cypher_value_v2(&serde_json::Value) -> Result<CypherValue, DecodeError>` | Decode version-2 persistence JSON, including the exact two-key `map` envelope. Known malformed tags return `Err`, never a repaired value or ordinary map. |
| `try_json_to_cypher_value_legacy(&serde_json::Value) -> Result<CypherValue, DecodeError>` | Decode version-1 snapshot values. `__type=map` remains an ordinary map; recognized valid historical tags retain their typed meaning; malformed recognized tags return `Err`. |
| `try_json_to_cypher_value(&serde_json::Value) -> Result<CypherValue, DecodeError>` | Standalone decoder using version-2 rules (there is no snapshot header). `serde_json::from_*::<CypherValue>` uses these rules and returns a Serde error on invalid tags. |
| `props_to_json(&IndexMap<String, CypherValue>) -> Result<serde_json::Map<String, serde_json::Value>, EncodeError>` | Encode every property with the same fallible persistence writer; errors include the property key in `path` (for example, `$.place.y`). |
| `float32_exact(f64) -> bool` | Retained public FLOAT32 convenience alias for `VectorCoordKind::Float32.is_exact` — it owns no second rule; persistence and Arrow call `is_exact` directly. Accepts finite input only, exactly when the f32 round-trip changes nothing (the `to_bits()`s of `(f as f32) as f64` and `f` match, so both signs of zero keep their own bits); `0.1`, overflow and non-finite inputs return `false`. |
| `VectorCoordKind::is_exact(f64) -> bool` | Whether the value survives that coordinate kind's narrowing unchanged (`is_finite() && narrow_float(f) == Some(f)`), the one exactness rule persistence vector reads/writes and Arrow float-vector writes share: FLOAT16/BFLOAT16 (widened into Arrow Float32), FLOAT32 and FLOAT64 all reject an inexact coordinate instead of silently rounding it. Always `false` for the integer kinds, whose coordinates take their own integer path. |
| `parse_numeric_offset(&str) -> Option<chrono::FixedOffset>` | Parse exactly six ASCII bytes in `[+-]HH:MM` form: explicit sign, two decimal hour digits, colon, two decimal minute digits (`00..59`). Checked arithmetic and chrono's `FixedOffset` range determine representability. `Some` contains the signed offset (e.g. `+02:30` is +9,000 seconds); `None` means malformed or out of range, including missing sign, wrong width, non-ASCII/multibyte text, non-digits, invalid minutes (`+00:60`), `+99:00`, or `Z`. Pure and stateless: no database/resource lifecycle, and malformed input returns `None` without panicking. |
| `try_json_to_props_v2`, `try_json_to_props_legacy`, `try_json_to_props` | Each accepts `&serde_json::Map<String, serde_json::Value>` and returns `Result<IndexMap<String, CypherValue>, DecodeError>`; the unqualified form uses v2. Property errors include the key in their path. |
| `try_json_to_param_value(&serde_json::Value) -> Result<CypherValue, DecodeError>` | Decode user query JSON recursively: **every object is an ordinary Map**, including complete/incomplete `__type` lookalikes and nested maps. Reject unrepresentable numbers (e.g. a `u64` above `i64::MAX` or non-finite float) with a path-bearing error. Rust callers using `Connection::query_with_params` can also pass typed `Value`s directly; JSON parameters needing typed values can use `point($coords)` / `date($iso)` in Cypher. |

Known persistence tags (`date`, `time`, `datetime`, `localdatetime`,
`duration`, `point`, `geometry`, `vector`) require correctly typed, complete
fields and reject extras; unknown or non-string `__type` remains map data.
Numeric components must fit their declared kind, vectors require a matching
dimension and coordinate type, geometry members must have valid shapes, and
time offsets must be ASCII `±HH:MM` in the representable range. Invalid nested
values produce `DecodeError`, without default coordinates or dropped members.
A syntactically valid tagged `Geometry` decodes as `CypherValue::Geometry` at
this model API boundary, but cannot be stored as a core `Value` property.
Persistence `Time` decoding uses `parse_numeric_offset` for its optional
numeric offset; an invalid offset in a recognized `time` tag therefore becomes
a path-bearing `DecodeError` (and a Serde or snapshot-open error at those
boundaries), rather than a guessed timezone. The parser itself is available to
Rust callers without opening a database; see the
[CI-run offset parser doctest](examples/decoder_formats.md).

On **model JSON/Serde write**, `Time` and `DateTime` with a
`chrono::FixedOffset` whose `local_minus_utc()` seconds are not divisible by
60 return `EncodeError` before formatting; this includes DateTimes with a
timezone-name hint. For `DateTime`, the UTC instant's `naive_utc()` plus the
fixed offset's signed seconds must also fit chrono's local `NaiveDateTime`
range before `to_rfc3339()` is called. Even a minute-aligned, constructible
`DateTime<FixedOffset>` at UTC `NaiveDateTime::MAX` with `+01:00` (or UTC
`NaiveDateTime::MIN` with `-01:00`) returns `EncodeError` with message
`DateTime local time out of range`, rather than panicking while formatting.
Both checks carry the full value path (for example, `$.payload.items[0]`, or
`$.clock` via `props_to_json`); `serde_json::to_value` and other Serde
serializers return errors with that path too. A constructible `+00:00:01`
offset therefore cannot silently become `+00:00` or change the UTC instant.
Minute-aligned Time offsets (for example `+02:30`) and DateTime offsets whose
resulting local time fits retain the existing tagged Time `offset: "+02:30"`
and DateTime RFC3339 `value` shapes, including the optional DateTime `timezone`
name. Valid near-boundary local times, including canonical chrono expanded-year
RFC3339 values, round-trip without changing offset, local time or instant (ADR-0087).
See the [CI-run boundary doctest](examples/decoder_formats.md).

**JSON snapshot lifecycle.** A new JSON `graph.json` checkpoint writes
`value_encoding: 2`; an absent field selects legacy v1; any other value fails
open with `Error::Storage`. The v2 `map` escape preserves arbitrary `__type`
maps through checkpoints on `NativeVfs` and `MemoryVfs`. A legacy bare map
identical to a valid typed tag cannot be distinguished from that typed value:
v1 keeps the historical typed interpretation. A malformed recognized tag in
either version fails the entire `Database::open` with property context. Even a
**valid** `__type: "geometry"` value anywhere in a node or relationship
property (directly or nested in lists/maps, including a v2 escaped map) fails
v1/v2 snapshot open: the snapshot-to-core bridge cannot represent Geometry and
rejects it before it could become `Value::Null`. The `Error::Storage` includes
the entity ID and nested property path, for example
`node 7: invalid graph property $.payload.items[0]: unsupported Geometry`.
Point and Vector are representable and continue to round-trip. No partial graph
is returned or rejected snapshot silently rewritten. Correct the stored data
or restore a valid checkpoint before reopening. See the
[executable decoder examples](examples/decoder_formats.md). On write, the
entire graph's JSON properties are encoded before `graph.json` is written or
the checkpoint manifest is published. A non-finite or otherwise unencodable
property fails the checkpoint with `Error::Storage` identifying the
node/relationship, property and nested value path; the previous committed
snapshot remains available and the graph stays dirty for a later retry. The
same rule applies to `NativeVfs` and `MemoryVfs`. This includes a DateTime
whose UTC instant plus offset exceeds chrono's local range: even a nested
property returns contextual `Error::Storage` (for example,
`relationship 3: invalid graph property $.payload.items[0].bad: DateTime local time out of range`)
before checkpoint publication. A fresh failed checkpoint publishes neither
`graph.json` nor its manifest; a failed checkpoint on an existing store
preserves their previous bytes for recovery. The optional top-level
`pull_identity` array holds the pull-identity map as
`{"source", "kind": "node"|"rel", "remote_id", "local_id"}` entries
(ADR-0106). It is omitted when the map is empty, so a store that never pulled
writes the same `graph.json` as before; an older `graph.json` without the
field loads with an empty map. The map is restored before WAL replay.

**Core temporal properties on JSON checkpoint write.** Before converting a
core `Value` to model `CypherValue` or serializing any graph property, the
checkpoint checks temporal values recursively through lists and maps. It
rejects every `Temporal::CivilDate` and `Temporal::CivilDateTime` (even with
chrono-representable fields); `Temporal::Duration` when its signed `i128`
`nanos / 1_000_000_000` seconds quotient lies outside `i64::MIN..=i64::MAX`;
and `Temporal::Time` / `Temporal::DateTime` when `offset` seconds are not
divisible by 60 or cannot be constructed as a `chrono::FixedOffset`. A
checkpoint failure is `Error::Storage` with the entity kind/ID and full path,
for example `relationship 3: invalid graph property $.payload.items[0]:
unsupported CivilDateTime`. These checks run before the lossy core-to-model
bridge, so a successful serialization cannot conceal a narrowed date, duration
or offset. A failed checkpoint leaves the previous `graph.json` and manifest
bytes intact; it does not undo WAL-committed writes. Core `Date`, `LocalTime`,
`LocalDateTime`, minute-aligned and constructible `Time`, and minute-aligned
`DateTime` whose resulting local time fits chrono's range, plus
`Duration` with an `i64`-representable seconds quotient (including its signed
nanosecond remainder) continue to round-trip, as do Point and Vector. See the
[CI-run checkpoint example](examples/decoder_formats.md).

## Store version compatibility (ADR-0127)

The contract is **migrate-on-open: hands-off, forward-only**
([ADR-0127](../../adr/0127-store-version-compatibility.md)). One registry in
`relate-storage` (`src/version.rs`) names, per durable subsystem, what
this binary writes (`CURRENT_VERSION`) and the oldest version it can read:

| Subsystem | Version marker | Writes | Readable |
| --- | --- | --- | --- |
| Parquet snapshot | manifest `format_version` | 3 (4 while the store holds an ontology) | 1, 2, 3, 4 |
| JSON snapshot | `graph.json` `value_encoding` | 2 (3 while the store holds an ontology) | absent (1), 2, 3 |
| WAL | segment magic | `MDBWAL01` (`MDBWAL02` while the store holds an ontology) | `MDBWAL01`, `MDBWAL02` |
| `.rl` bundle container | superblock magic + `VERSION` | `MDBCLB01` / 1 | `MDBCLB01` / 1 |

The ontology-dependent markers follow the content (ADR-0120 §8: a v4
manifest is a v3 manifest plus `ontology_path` selecting one immutable
`ontology/<128-bit-hex>.json` object; `value_encoding` 3 is encoding 2 plus
the inline `ontology` field; `MDBWAL02` heads segments created while the
store holds an ontology, committed or still WAL-pending). Only the pair the
writer emits is current — `(3, no ontology)` or `(4, ontology)` for the
manifest, `(2, no ontology)` or `(3, ontology)` for `value_encoding` — and
migrate-on-open normalizes any other readable pair to the content's
version. A store that never sets an ontology stays byte-for-byte in the
v3/encoding-2/`MDBWAL01` format, and after a drop, a checkpoint and WAL
recycling a store returns to it; older binaries fail closed on an
ontology-carrying store before modifying any data (ADR-0114).

There is one legacy reader per readable version plus the current writer
(the v1/v2 monolithic Parquet fallback and the absent-`value_encoding` JSON
arm are the first legacy readers); each legacy reader is removable when the
minimum readable version advances past it. Detection is the version marker
the codec loader already decoded — the loaders return it alongside the
decoded graph and checkpoint LSN, so a current-format open reads
`manifest.json` / `graph.json` exactly once and there is no separate
pre-read of `manifest.json`, `graph.json` or WAL headers. One per-marker
classification — `Current | Legacy(version) | Unreadable` — backs every
gate: the ADR-0114 unknown-version rejection, the WAL scan and ownership
admission, and migrate-on-open.

`Database::open` / `open_with` are one flow: load with the reader for the
reported marker, `Wal::open`, replay; then, only when a loader reported a
legacy marker and the VFS is durable, a checkpoint at the current format, a
WAL rotation to a fresh current-magic segment and recycling of the legacy
segments — all on the handles already open, with no second load, `Wal::open`
or replay. Behavior:

- On a current store the step is a no-op; a second open of a migrated
  store performs no migration.
- Every subsystem's markers are validated **before any byte mutates** (the
  ADR-0114 ordering). A store written by a **newer** Relate is not a
  migration input: it hard-fails at open with the unsupported version in
  the error, before any WAL or snapshot byte changes.
- A corrupt or partial old store fails with its original `Error` variant
  and message, the legacy version named in place:
  `{original message} (store snapshot is at manifest version {v})` or
  `{original message} (store snapshot is at value_encoding {v})`; a JSON
  envelope that does not parse fails as
  `failed to parse graph JSON: {serde error} (store snapshot is at value_encoding {v})`.
  Current-format stores keep their errors verbatim and no `storage error:`
  prefix is doubled.
- A crash mid-migration leaves the old store intact (manifest-last
  tmp/rename publication, ADR-0089/ADR-0092); the next open re-migrates
  from scratch.
- `:memory:` never migrates; it has no durable state.
- A `.rl` bundle migrates the payload files inside the container
  (manifest, shards, WAL segments); the container format itself
  (`MDBCLB01`, superblock `VERSION` 1, ADR-0074) is frozen and bumps only
  for a true container layout change.
- The WAL has two readable segment magics. `MDBWAL01` is the default write
  magic; `MDBWAL02` heads segments created while the store holds an
  ontology (ADR-0120, implemented), so older binaries fail closed before
  they could misread an ontology record. Both magics are current content,
  not format ages: the WAL scanner and the native-ownership admission
  accept either, and migrate-on-open never rewrites WAL segments — a fresh
  segment takes the magic matching the store's current ontology state. A
  future `RedoOp` shape change bumps the magic again **and** ships a
  legacy decoder for the previous one; legacy-magic detection lives inside
  `Wal::open`, which already reads every segment's magic and forces a
  fresh current-magic segment even when the WAL tail is empty. Older
  binaries keep failing at an unknown magic before modifying data.

Migration is forward-only: once a store is migrated, older binaries reject
it at its version markers. The `MigrationReport` type is `pub(crate)` — a
plain value whose entries mean "subsystem X was at version N, now current",
empty when no migration happened; there is no user-facing migration API,
and no log or trace line is emitted on open because `relate-storage`
has no logging facility yet (its own ADR under AGENTS.md rule R6).

## Arrow values and WKB (ADR-0088)

These APIs live in `relate_arrow`; their errors are
`arrow::error::ArrowError`. `result_to_record_batch(&[String],
&[IndexMap<String, CypherValue>]) -> Result<RecordBatch, ArrowError>` and
`result_to_record_batches(&[String], &[IndexMap<String, CypherValue>],
max_rows_per_batch: usize) -> Result<Vec<RecordBatch>, ArrowError>` are in
`record_batch`. The latter shares one full-result-inferred schema across
chunks; `max_rows_per_batch = 0` uses chunks of one row, and empty input
returns one empty batch. `from_arrow::record_batch_to_rows(&RecordBatch) ->
Result<DecodedRows, ArrowError>` returns `(column_names,
Vec<IndexMap<String, CypherValue>>)` (`DecodedRows` is that public tuple
alias). A bad field, non-null cell, or unsupported physical type fails the
**whole** batch with field/row context; a null cell decodes as `Null`.

Arrow **field** metadata is exact (no prefix or shape inference):

| Physical field | Required marker | Decodes as |
| --- | --- | --- |
| Node `Struct` | `relate.value_type=node-v1` | Node with non-null Int64 `id`, `List<Utf8>` labels and typed-JSON properties |
| Relationship `Struct` | `relate.value_type=relationship-v1` | Relationship with non-null Int64 `id`, `start_node`, `end_node`, Utf8 `type` and typed-JSON properties |
| Vector `FixedSizeList<primitive>` | `relate.value_type=vector-v1` | Vector with Int8/16/32/64 or Float32/64 child values |
| Composite/mixed `Utf8` | `relate.encoding=typed-json-v1` | Recursively typed values, including maps, lists, graph values and paths |
| Spatial `Binary` | `ARROW:extension:name=geoarrow.wkb` and `relate.srid=<signed i32>` | 2D WKB Point/Geometry at the field's SRID |

Each non-null `typed-json-v1` cell is an **exact** `{"type":T,"value":V}`
object. Supported uppercase `T` values are `NULL`, `BOOLEAN`, `INTEGER`,
`FLOAT`, `STRING`, `LIST`, `MAP`, `NODE`, `RELATIONSHIP`, `PATH`, `DATE`,
`TIME`, `DATE_TIME`, `LOCAL_DATE_TIME`, `DURATION`, `POINT`, `GEOMETRY`, and
`VECTOR`. Lists/maps contain recursively typed envelopes; graph envelopes
require actual identities and typed properties; temporal/spatial/vector
payloads use validated persistence-tag JSON. Missing/extra fields and wrong
types error. The Arrow encoder obtains Point, Geometry and Vector tagged
payloads from the model's fallible persistence writer, keeping their valid
shapes identical; nested writer errors become `ArrowError` with column/row and
member paths. The Arrow envelope and graph properties still have their own
typed-member format. This is an Arrow-only format, not public wire JSON.

Unknown or conflicting markers and markers on the wrong physical type error,
even in empty batches. Legacy `relate.encoding=json` and `tagged` error
because their untyped graph-shaped contents are ambiguous: regenerate those
batches with the new writer. Unmarked `Struct` decodes recursively as Map,
unmarked `FixedSizeList` as List, and unmarked `Utf8` as the verbatim string;
none infer graph identity from shape. Bare Binary without the WKB extension
errors. Marked graph structs require all correctly typed children and valid
properties, never a fabricated ID; parent null rows stay Null.

Both writers use native `Time64(Nanosecond)` for a column of **only unzoned**
`Time` values. Any offset-bearing `Time`, even in an otherwise homogeneous
column, makes the entire column `TypedJson`: `Utf8` with
`relate.encoding=typed-json-v1`. Every non-null cell has its own `TIME`
envelope, including its optional offset; a mix of zoned and unzoned Times
round-trips without dropping either. Offset-bearing Times with a non-minute
offset still return `ArrowError` with column/row and nested value path from the
model writer, including in a later chunk.

For `DateTime`, native `Timestamp(Nanosecond, "[+-]HH:MM")` (UTC instant and one
column-wide fixed-offset timezone) is selected **only if every non-null,
present value in the full result** is within the native nanosecond range, has
the **same minute-aligned fixed offset**, and has **no timezone-name hint**.
Null/absent rows do not affect that choice. Different fixed offsets (including
`+02:30` and `-05:00`) or **any** `Some(timezone_name)`, even in one row,
select the `typed-json-v1` `Utf8` column instead. Its per-cell `DATE_TIME`
envelopes preserve each row's fixed offset, local time, UTC instant and optional
name. The choice is made over the whole result before single-batch or chunked
writing, so chunks share a schema; it is never inferred from the first row or
chunk alone. A non-minute DateTime offset returns `ArrowError` with column/row
and nested value path **even if** differing offsets or a name already forced
the typed-JSON fallback: fallback is not an offset validation bypass (ADR-0088).

Both writers also infer native `Timestamp(Nanosecond)` for representable
`LocalDateTime` (no timezone) and native `Interval(MonthDayNano)` for
representable `Duration`. Nanosecond timestamps must fit `i64`; duration
months and days must each fit `i32`, and
`seconds × 1,000,000,000 + nanoseconds` must fit `i64` using checked
arithmetic. Ordinary out-of-range values cause the **column** to infer
lossless `typed-json-v1` `Utf8` instead of substituting epoch zero or
saturating the interval. If an invalid/forced native writer plan still reaches
conversion, the writer returns `ArrowError` with column/row context; it does
not fabricate a timestamp or interval. The chunked writer uses the same
full-result inference and schema for every batch.

On Arrow timestamp **read**, timezone metadata accepts `Z`, case-insensitive
`UTC`, or a fixed offset with exact ASCII `[+-]HH:MM` syntax, minute `00..59`
and hours representable by `chrono::FixedOffset`. A leading `+`/`-` with
malformed or out-of-range digits (for example `+00:60` or `-12:99`) returns
`ArrowError`, even for an empty/all-null batch; it is not treated as a named
timezone. Nonnumeric named zones retain the UTC instant and zone name for
display without timezone-database resolution. The Arrow reader delegates
signed offset parsing to the same public model `parse_numeric_offset` helper;
it handles `Z`/`UTC` and unsigned named-zone dispatch separately, mapping a
signed candidate's `None` to `ArrowError` during schema validation.

Both writers preflight the **whole-result** child-element budget for each
native FixedSizeList Vector column and the SRIDs for each native spatial
column before chunking. They do not preflight every Vector's validity over
the whole result: direct Vector dimensions, lengths, coordinate types/ranges,
and narrowing are checked while building each Vector array (or while encoding
typed JSON for composite columns). Invalid values return `ArrowError`.
Float32 coordinates must round-trip exactly through `f64 → f32 → f64`
(including signed zero); non-finite coordinates error. Before building a
FixedSizeList vector column, checked `dimension × full_result_row_count`
must be **≤ 16,777,216 child elements**, including placeholders for null or
absent rows; overflow/above-limit errors identify the column and counts.
Across all non-null Point/Geometry values in a spatial column, mixed SRIDs
error with column and conflicting SRIDs, including when they cross chunk
boundaries. A native spatial `Binary`/WKB column requires one consistent SRID
from its non-null values; the writer always emits `relate.srid` on its
field, and a spatial build plan without an SRID returns `ArrowError` rather
than emitting unmarked WKB. Point/Geometry coordinates encoded in
`typed-json-v1` (including values nested in lists, maps or graph properties)
must be finite (including a Point's `z`); non-finite coordinates error rather
than being serialized as null. For native 2D spatial `Binary` columns, the
same full-result preflight also rejects non-finite `x`/`y` in Points and every
Geometry coordinate, recursively through Multi* and GeometryCollection
members. `ArrowError` identifies the column, row, member/coordinate path and
axis before either writer emits a batch, even if the bad row is in a later
chunk.

A column containing **any 3D Point** (`z: Some(_)`) instead uses `Utf8` with
`relate.encoding=typed-json-v1` for every row. Its per-cell `POINT`
envelopes retain `x`, `y`, `z` and SRID, including when 3D Points mix with 2D
Points or Geometry (and across chunks). Other spatial cells retain their own
SRIDs too: native WKB's single field SRID and nested-member SRID/depth checks
do not apply to a typed-JSON column. Columns containing only 2D Points and
Geometry stay native `Binary` WKB with the field SRID and the preflight above.

Arrow WKB has **one external SRID per field**, not an SRID in each WKB geometry
or collection member. Before encoding, both batch writers preflight the full
result: every `GeometryCollection` descendant must match its immediate parent
**and** the field SRID at any nesting depth. A mismatch returns `ArrowError`
with the column, row, conflicting SRIDs and nested member index path (for
example `column 'location' row 3 collection member [1][0]`), including when
that row would be in a later chunk. Uniform-SRID nested collections round-trip
their child SRIDs and coordinates because the reader applies the field SRID to
each decoded member. This is an Arrow WKB writer restriction: model persistence
JSON can preserve different nested SRIDs.

The same full-result spatial preflight bounds **WKB header nesting** before
SRID traversal or encoding. The top-level geometry header is level 1; each
nested header adds one level, including the implicit member headers of
`MultiPoint`, `MultiLineString` and `MultiPolygon`. Level 64 is accepted and
round-trips; level 65 returns `ArrowError` with column, row, WKB depth and
nested member index path before either writer emits any batches (also for a
later chunk). The native WKB writer preflight and reader share the single
Arrow-internal `geo_encoding::MAX_GEOMETRY_DEPTH = 64` bound; this is not a
public constant or a new WKB format. See the
[`result_to_record_batch` doctest](../../../surfaces/relate-arrow/src/record_batch.rs).

There is no partial batch result on error: `result_to_record_batches` returns one
`Err` without exposing earlier batches even when a later chunk contains an
invalid Vector (`Result<Vec<RecordBatch>, ArrowError>`). The
[`record_batch_to_rows` Rust doctest](../../../surfaces/relate-arrow/src/from_arrow.rs)
is covered by CI's `cargo test` run for `relate-arrow`; it writes and reads a
marked Arrow batch, checks that invalid legacy encoding metadata returns an
error, and exercises lossless spatial/numeric fallbacks and invalid external
WKB.

`geo_encoding::wkb_to_cypher_value(&[u8], srid: i32) ->
Result<CypherValue, ArrowError>` reads one complete **2D** WKB value;
`geo_encoding::wkb_array_to_geometries(&dyn arrow_array::Array, srid: i32)
-> Result<Vec<CypherValue>, ArrowError>` accepts only `BinaryArray` and
preserves null cells. WKB carries no SRID per cell; the caller supplies it
(or the Arrow field provides `relate.srid`). These standalone WKB decoding
helpers do not validate that out-of-band SRID against an original collection's
members. The standalone raw WKB **encoding** helpers (`point_to_wkb`,
`geometry_to_wkb`, `cypher_value_to_wkb`, `geometries_to_wkb_array`) write 2D
little-endian bytes without the batch writer's full-result SRID/depth/finite
preflight: a Point's `z` is omitted, and no per-value SRID is recorded. Use a
batch writer for lossless 3D Point column selection and checked native WKB.
Standalone and Arrow-field WKB decoders return a 2D Point with `z: None`.
Readers accept byte-order marker `0` (big-endian) or `1` (little-endian) on
**every** top-level/nested header. Supported shapes: Point, LineString,
Polygon, MultiPoint, MultiLineString, MultiPolygon and GeometryCollection;
a Point child of GeometryCollection cannot be represented as a GeometryValue
and errors. Wrong array types, malformed/truncated counts or nested types,
trailing bytes, nesting deeper than 64 geometry levels, and non-finite `x`/`y`
at any depth return `ArrowError`, not Null. External WKB with NaN/Infinity
(including NaN-sentinel Point EMPTY) is rejected; array decoding adds the row
index to the coordinate/member error. See the
[runnable Arrow and WKB examples](examples/decoder_formats.md).

### Full-column Arrow type inference (`relate_arrow::schema`)

`infer_column_type(col_name: &str, rows: &[IndexMap<String, CypherValue>])
-> ColType` classifies every row of the named column, treating absent cells as
`Null`. The public `ColType` enum represents native kinds (`Bool`, `Int`,
`Float`, `Str`, `Date`, `Time`, `DateTime`, `LocalDateTime`, `Duration`,
`Spatial`, `Vector { coord, dim }`, `Node`, `Relationship`), `Null`, and the
single fallback `TypedJson`. The older `infer_type`/`infer_schema` helpers infer
from one value or sample row and cannot make full-column merge decisions;
the batch writers use full-column inference.

`ColType::TypedJson` covers homogeneous `List`, `Map`, or `Path` columns;
incompatible kinds (including vectors with different dimensions or coordinate
kinds); offset-bearing `Time` (even alone); heterogeneous fixed-offset or
named-zone `DateTime`; and `DateTime`/`LocalDateTime` or `Duration` values outside
their native nanosecond timestamp/interval ranges. A 3D Point, alone or mixed
with 2D Point/Geometry, makes the whole column `TypedJson`, preserving `z` and
per-cell SRIDs; only 2D-only spatial columns remain native WKB. An `Int`/`Float`
mix with any integer not exactly representable as `f64` (for example
`2^53 + 1 = 9_007_199_254_740_993` or `i64::MAX`) also takes the fallback,
preserving separate `INTEGER`/`FLOAT` values. A non-minute DateTime also
classifies as `TypedJson`, but errors when its cell is encoded. It is
**absorbing**: once a column reaches `TypedJson`, later values of any kind,
including `Null`, cannot restore a native kind. Otherwise `Null` is the merge
identity, a column of only null/absent cells stays `Null`, matching vector
kinds/shapes remain native, an all-`Int` column stays `Int64`, and `Int` plus `Float` promotes
to native `Float64` only if **every** integer in the column is exactly
representable as `f64` (including `i64::MIN`). Native
`DateTime`, `LocalDateTime`, and `Duration` still use the checked conversions
and limits above; only a homogeneous unnamed, minute-aligned, native-range
DateTime column stays native.

**Migration:** The public `ColType::Json` and `ColType::Mixed` variants have
been replaced by `ColType::TypedJson`. Update exhaustive matches on either
old variant to match the single new variant. This changes the inferred enum
kind, not the Arrow field format: every such fallback still writes `Utf8` with
the exact `relate.encoding=typed-json-v1` field metadata and the same
typed envelopes described above. Writer validation/errors, reader errors, and
legacy `json`/`tagged` rejection remain as specified above. The
[`infer_column_type` doctest](../../../surfaces/relate-arrow/src/schema.rs)
checks both a Map and an incompatible `Int`/`Str` column in CI.

## Errors

- `Syntax` — parser
- `Syntax("UndefinedVariable: variable '<name>' not defined")` — a variable with no binding in the evaluated row, raised for the expressions the plan-time binding walks cannot see (a `SHOW … WHERE` predicate lowered at run time, ADR-0335); an unbound variable never evaluates to `null`.
- `Execution("NumericValueOutOfRange: round precision must be at most 15")` — `round()`'s precision is bounded to `0..=15`, symmetric with its `must be non-negative` bound (ADR-0336).
- `Semantic` — unknown variable, type mismatch
- `Semantic("LOAD CSV invalid configuration")` — at database open for an invalid/missing CSV root or byte limit outside `1..=67_108_864`; does not include the path or OS error.
- `Execution("LOAD CSV file unavailable")` — local file disabled, unavailable, outside the root, non-regular, malformed source, or I/O failure; `Execution("LOAD CSV file exceeds byte limit")` — source exceeds the configured per-file byte cap. Neither error reveals the source path or OS error.
- `Execution("node ID exhausted")` / `Execution("relationship ID exhausted")` — creation cannot reserve another supported ID; direct `Graph` and query writes leave no failed-create redo.
- `Execution("write timestamp exhausted")` — a direct graph write cannot start at the timestamp sentinel; graph write state, statistics and redo are unchanged.
- `Execution("NestedEvaluationBudgetExceeded: nested evaluation exceeds 100000 passes per statement; nested subqueries compound per level (security budget).")` — a top-level statement's nested evaluation (nested `CALL` bodies, `EXISTS`/`COUNT`/`COLLECT` subqueries, taken conditional branches, `FOREACH` clause lists, NEXT-stage chains) would exceed the 100 000-pass budget ([Nested evaluation budget](#nested-evaluation-budget-adr-0395), ADR-0395).
- `Storage` — WAL redo with `commit_ts == u64::MAX` is unsupported and fails before graph mutation.
- `Storage("parquet snapshot codec is not compiled into this build (enable the \"parquet\" feature)")` — at `Database::open` / `open_with` in a build without the `parquet` feature, when the store resolves to Parquet or Parquet is requested ([Cargo features](#cargo-features), ADR-0105).
- `Storage` — a replicated or directly appended WAL record with a negative or `i64::MAX` node/relationship ID in any redo operation, relationship endpoint, or pull-identity `local_id` is rejected before writing any part of the record. WAL recovery, selected or legacy Parquet snapshot pull-map reads, and JSON `graph.json` `pull_identity` reads apply the same local ID bounds; source names, remote IDs, labels, property keys and values are not local IDs.
- `Type` — value type mismatch. The typed variants `ParameterMissing`, `DatabaseNotFound`, `Arithmetic(ArithmeticFailure)` and `Cancelled` are specified in [Typed error variants](errors.md) (ADR-0307, ADR-0308).
- `Conflict` — optimistic write-write conflict at WAL append, or a commit-time uniqueness admission conflict when a slot would introduce a `unique`/`key` constrained tuple already held by another live entity (`Display` is `write-write conflict`). Retry in the client (ADR-0017); a retried uniqueness-admission statement sees the winner and reports the precise `ConstraintValidationFailed` violation (ADR-0439).
- `Capacity { used, limit, kind }` — a growing write would exceed the live graph + index cap (ADR-0042; `kind: CapacityKind::Graph`, `Display` is the historical `capacity exceeded: used={used} limit={limit}`), or one expression-built value would exceed the per-allocation cap (ADR-0463; `kind: CapacityKind::Expression`, `Display` is the plain-language line naming the attempt, the safety intent, and `--expression-memory-limit`). Shrinks and reads still succeed. The engine does not retry.
- `Locked(String)` — nonblocking OS lock contention on a Native database root's `.cypherlite.lock` or `.cypherlite.writer`, its WAL directory's matching files (including an external `wal_dir`), or the `.rl` host file, whether the other independent opener is in this process or another. Writable opens share the identity marker and exclusively lock `.cypherlite.writer` (ADR-0136). Read-only opens take only the shared identity lock, or a shared lock on a `.rl` host whose writer still holds the exclusive lock. Reset still takes an exclusive lock on `.cypherlite.lock`. `Display` is `storage locked: {0}` (the contained path). `Database::open`/`open_with` fails before bundle initialization or WAL replay. Root is acquired before WAL; if they are the same directory, the lease is reused. Failed opens release acquired leases; stable Native `.cypherlite.lock` files remain on disk after close. A `Connection` keeps the shared database alive after its `Database` handle is dropped; `StoreTarget`/VFS handles, checkpoints, timer work, and final-drop persistence retain ownership too. The locks release after the last live owner and its final persistence complete, or on process exit/crash. `:memory:` does not lock. Permission failures, invalid paths, unsupported lock operations, corruption and unrelated I/O errors are `Io`/`Storage`, not `Locked` (ADR-0097). See the [runnable exclusive-open example](examples/storage_lock.md).
- `Storage` — WAL / snapshot VFS (ADR-0072), including malformed tagged values,
  valid but unsupported stored Geometry (with entity ID and property path), or
  unsupported JSON snapshot `value_encoding` during `Database::open`; also
  an unsupported version marker, validated before WAL recovery
  (ADR-0114): `unsupported snapshot version {v}` (Parquet `format_version`
  outside 1 to 4), `unsupported graph value encoding: {v}` (JSON
  `value_encoding` not absent, 2 or 3) and `read-only snapshot not
  supported` (JSON `read_only` true), each leaving every WAL segment
  unchanged; `invalid WAL magic: {path}` (a WAL segment whose magic is
  neither `MDBWAL01` nor `MDBWAL02`) and `unsupported .rl bundle version
  {v}` (a bundle superblock
  `VERSION` other than 1) likewise fail the open before any byte mutates;
  also, when a supported-older store proves corrupt or partial during the
  legacy-reader load, the original error keeps its variant with the version
  named in place — `{original message} (store snapshot is at manifest
  version {v})`, `{original message} (store snapshot is at value_encoding
  {v})`, or `failed to parse graph JSON: {serde error} (store snapshot is
  at value_encoding {v})` for an unparseable JSON envelope — with no
  doubled `storage error:` prefix; the old store is left intact and the
  next open re-migrates (ADR-0127); also
  unrepresentable core temporals on JSON `Database::checkpoint`, with entity ID
  and full property path before serialization (ADR-0087). A Native database or
  `.rl` bundle under an existing Native root's `.cypherlite.lock` is refused even after
  that root closes. An existing external WAL directory containing a nested
  Native `.cypherlite.lock` or `.rl` bundle cannot be attached; reset refuses such
  identities in either the root or disjoint WAL tree before staging or deleting
  anything (ADR-0097).
- `Io` — local filesystem
- `Internal` — engine-internal failure that is not the query's fault: the
  connection's query worker could not be spawned, or the worker panicked
  and the panic was returned as a typed error (ADR-0259). Always recoverable by
  the caller; never an abort. The same worker stays parked for the next
  statement.

Thread safety: `Database` is `Send + Sync`. Do not share a `Connection` across threads.

New Native database directories (including WAL, shard and CDC directories) are
created with Unix mode `0700` (also new parent components); new Native files
(including `.cypherlite.lock`, WAL, snapshot, CDC and temporary files) and `.rl` host files
use `0600` even with a permissive umask. A more restrictive umask can remove
permissions and make open fail. Existing paths retain their modes; opening does
not chmod or migrate them. The bundle contains its virtual WAL, snapshots and
catalog in **one host file**, without a `.rl.lock` or other sidecar. Relative,
absolute, `file://`, symlink and bundle hard-link aliases resolve to the same
ownership identity; a symlink in a Native `.cypherlite.lock` final component is refused.
Windows inherits host ACLs rather than promising Unix modes (ADR-0097).
Locks coordinate cooperating processes on supported local filesystems; they
are not filesystem permissions or a guarantee on a network filesystem without
enforced advisory locking. An incomplete reset of a separate WAL may retain
private tombstones and a `.reset-incomplete` root marker. Opening that root
fails with `Storage` until its old WAL is recovered; the reset error lists
recovery paths.

`relate_storage::reset_uri(uri, wal_override)` uses the same exclusive
root/WAL ownership as open. For a directory it stages separate WAL data first,
then the root, before deleting only the private tombstones. Failed staging
restores both names without replacing new occupants or returns an error naming
retained recovery paths. A `.rl` reset unlinks its locked host file without
creating a sidecar; a live independent owner yields `Locked`. A database URI
whose final component is a symlink or Windows reparse-point alias is refused
with `Storage` before taking a lease, including a dangling alias or a `.rl`
alias to an unrelated file. Reset rechecks that the requested path still names
the leased physical target before staging or unlinking it. Ordinary open still
supports aliases. Before modifying the filesystem, reset refuses root/WAL trees
that contain the root, equal it, or sit inside it except for the direct,
physically contained `<root>/wal` directory (`Storage`, not `Locked`). A default
`<root>/wal` symlink to a disjoint external directory and disjoint explicit WAL
overrides remain supported.

`relate_storage::adopt_native_default_wal(root: &Path) -> Result<()>`
is an **offline, operator-asserted** migration for a pre-ADR-0097 Native
directory whose physical direct `wal/` has no typed role. Close both possible
owners first and independently verify the WAL belongs to this root: segment
magic cannot distinguish a direct WAL from a closed independent segment-only
root opened with root == WAL. The root must have a typed root marker or a
direct regular `manifest.json`/`graph.json`; the direct non-symlink WAL must
contain a correctly named segment with a readable WAL magic (`MDBWAL01` or
`MDBWAL02`) and no root-specific
evidence. Existing `.cypherlite.lock` must be missing, empty or already correctly typed;
other nested identities, a reset-incomplete marker, or wrong role return
`Storage` without adopting. A live lease returns `Locked`. The helper
preflights both trees before creating missing `.cypherlite.lock` files; with root then WAL
leases held it rechecks physical identities, roles and nested owners, writes
the WAL then root roles in place, and syncs each role and its containing
directory on Unix. No WAL, manifest or snapshot data is moved, deleted or
replayed. It is idempotent for already adopted directories. Failed sync returns
`Io`; inspect roles before retrying. Normal open/reset **never** infer an
untyped direct WAL's ownership and fail closed before touching its contents.
See the [runnable offline adoption example](examples/adopt_legacy_wal.md)
(ADR-0088).
Persistent opens resolve physical ancestors (including symlink aliases) and
reject nested Native roots and `.rl` bundles beneath an existing Native root,
even when its lease is not held. A bundle hard link physically under the root
is nested; a symlink inside the root pointing outside is an alias to the
external file, and reset unlinks only the alias. Native reset scans the physical
root subtree and any disjoint external WAL subtree without following directory
symlinks or Windows reparse points; attaching an existing external WAL also
scans it before creating its lock and again under its lease. These scans
refuse pre-existing nested `.cypherlite.lock` markers or regular `.rl` bundles,
including independently owned live databases (`Storage`, no tombstones or
content changes). Files with bundle magic are also protected when reached via
an external `.rl` alias to a differently named host file under the root; a
`.rl` symlink within the root to an internal target is rejected. The root and
its directly contained owned `wal/.cypherlite.lock` are excluded (a markerless parent
does not presume it owns a `wal/.cypherlite.lock`). Only the disjoint WAL's own verified
`.cypherlite.lock` is exempt in that tree, never a child's marker. Ancestor checks take
O(path depth); first open of a markerless existing Native directory, external
WAL attachment, and Native reset additionally scan O(entries) in the inspected
trees. The WAL directory is also scanned on persistent Native open, before and
after taking its lease; this is startup I/O, not query-time work.

## Persistent-input and replacement boundaries (ADR-0098, ADR-0086)

Invalid WAL framing, bundle pages and Parquet metadata/pages fail open/replay
with `Storage`; value conversion can also propagate its typed decode error
(for example `Type` for malformed temporal data). Genuine VFS/OS read errors
propagate as `Io` (some snapshot object reads wrap an underlying I/O error as
`Storage`). A missing snapshot manifest means an empty snapshot, but
an unreadable or invalid *existing* manifest is an error, not an empty graph.
An incompatible legacy file is not silently repaired or discarded.

`Wal::preflight_record(lsn, commit_ts, ops) -> Result<WalPreflight>` validates
and encodes the exact candidate once without writing. The opaque proof owns
the encoded payload and records the prospective LSN and timestamp;
`Wal::append_preflighted(proof) -> Result<AppendCommitStats>` checks that the
proof still names the next record and writes its owned bytes as the frame
without re-encoding or re-checking them. `Wal::append_record` is the direct
one-call form (preflight, then append). Both append forms return the assigned
LSN and encode/write timing on success; `sync()` is required for a separate
durability barrier. A physical append, sync or rotation error poisons that
`Wal` handle: append, sync, rotate and recycle then return `Storage` with
`uncertain WAL outcome` until reopened. Deterministic preflight errors leave
it usable. See the runnable `Wal::preflight_record` doctest.

On open, only a highest segment with 1–7 bytes exactly matching a prefix of
`MDBWAL01` can have its header atomically completed, and only after all
predecessors have been fully verified. The repaired header always becomes
`MDBWAL01`: such a segment holds no frames, and ontology ops are never
appended to it (ADR-0120 §4). Empty, sole, closed, mismatching and
CRC-invalid predecessor cases fail without this repair.

| Format / operation | Enforced boundary |
| --- | --- |
| WAL append, open/replay and CDC `records_after` | A record has an 8-byte CRC/length header and **1..=64 MiB (67,108,864 bytes)** of JSON payload. `Wal::validate_redo_ids(ops) -> Result<()>` checks all persisted node/relationship IDs and endpoints and pull-identity local IDs in `0..i64::MAX`. `Wal::preflight_record(lsn, commit_ts, ops)` validates and encodes the exact serde JSON once into an owned, bounded payload; `append_preflighted` writes those bytes as the frame without re-encoding them, and `append_record` preflights **before** writing its single bounded frame. A group holds each proof's encoded payload until its append consumes it, so pre-append encoded-payload memory scales with the eligible group (each payload stays ≤ 64 MiB). Preflighting adds serialization CPU and WAL serialization time before the first frame (ADR-0094); benchmark group throughput, mutex wait, p99 and memory in the ADR-0070 baseline environment before accepting a new loadtest envelope. See the runnable Rust doc example on `Wal::validate_redo_ids`. Oversized appends fail with the capacity refusal `Error::Capacity { kind: CapacityKind::WalPayload, .. }` (ADR-0492, reclassified ADR-0494) before writing a frame or publishing its LSN; zero or oversized lengths, invalid or truncated magic, and interior CRC or decode failures fail recovery. Exact EOF at a record boundary is valid. Any segment may end with a torn tail: a short final header (1 to 7 bytes) or an in-range header whose physically incomplete payload ends at EOF. The active segment's final complete frame may also fail its CRC (a relaxed append). A complete frame whose CRC matches but whose payload does not decode fails WAL open, replay and CDC `records_after` with `Error::Storage` in every segment and position; the error names the segment and contains `may have been written by a newer Relate`, and recovery applies tolerated-tail trims (closed and active segments) only after every segment has been scanned, while the highest segment's short magic is repaired only after every earlier segment has validated (ADR-0114). Recovery replays the intact committed prefix and removes the damaged suffix via atomic replacement before later appends; a complete frame failing CRC or decode before the physical EOF, or in a closed segment, remains fatal. A real read/seek/length error or unexpected short read is never treated as EOF/torn tail. WAL open folds segment LSN, timestamp and commit counts over decoded frames; database startup applies records after the checkpoint LSN one at a time to the snapshot, including pull-identity redo in commit order. A genuine (non-tail) recovery error fails open without publishing a partial graph or recycling WAL. The public storage `Wal::replay(from_lsn) -> Result<Vec<WalRecord>>` intentionally materializes all matching records; CDC `Wal::records_after(from_lsn, limit)` materializes at most `limit` records. This is a per-record limit, not a bound on the segment, total WAL size or number of records. |
| `.rl` bundle | Two complete **4096-byte** superblock pages and a physically bounded catalog are required. The catalog is at most **16 MiB (16,777,216 bytes)**, on both read and write. Declared pages cannot exceed complete pages in the host file; catalog, live inode and freelist ranges must be in range, disjoint and non-overlapping with the two superblocks, with inode lengths fitting their extents. Checked WAL-tail ownership precedes orphan reclaim. A WAL write to a page selected by the last durable superblock copies that page to a new extent; the new data and catalog pass a contents barrier before the alternate superblock write and its publication barrier. Only after the second barrier returns can the old page be reused or commit SUCCESS be reported. Failed writes/barriers fence writes on that bundle handle until reopen. Existing v1 bundles, including older catalog WAL lengths restored by the superblock tail hint, remain readable. On macOS these barriers use `F_BARRIERFSYNC` ordering, not a stable-media `F_FULLFSYNC` flush. Existing-page read errors propagate instead of writing zero-filled replacement pages. There is no whole-bundle file-size cap. |
| Parquet manifest and pull-identity JSON; JSON `graph.json` pull identity | Each Parquet-store object is at most **16 MiB (16,777,216 bytes)** on write and before read (both VFS metadata and opened-handle length). The legacy ADR-0092 v3 `pull_identity_path` selector is removed from `Manifest`: a key still present in an old manifest is ignored on read (unknown manifest fields are ignored, so old manifests open unchanged) and dropped on the next write — current code never writes the key — and any `pull-identity/` objects or `pull-identity.json` sidecar an old store still holds are inert legacy data, never read. Every `PullIdentityEntry.local_id` must be in `0..i64::MAX` (inclusive zero, exclusive `i64::MAX`). A checkpoint with an invalid local ID returns `Error::Storage` before creating a new selected map object or publishing a new manifest; the previous snapshot and committed WAL remain available for recovery. Selected and legacy map reads return `Error::Storage` before installing any entries with an invalid local ID. Source names and remote IDs are not subject to the local-ID bound. v1/v2 monolith snapshots remain readable. The map is loaded before WAL replay; identity WAL ops replay with their graph commits. JSON stores have no separate map object: entries live in the optional `graph.json` `pull_identity` array and share its read path with no separate size envelope (ADR-0106), and the same `0..i64::MAX` local-ID rule applies — an invalid entry fails checkpoint with `Error::Storage` before writing `graph.json` or advancing its manifest, and fails load before installing any entries. |
| Parquet node/relationship shard recovery, including older monoliths | Before whole-file allocation, the VFS metadata and opened file length must each be at most **1 GiB (1,073,741,824 bytes)**; a changed length during the read fails. Loader preflight also rejects a shard over **1 GiB** on disk, footer metadata over **16 MiB**, or more than **1,000,000** declared rows across row groups. Checked file/column ranges and at most **2 GiB (2,147,483,648 bytes)** of declared uncompressed column data are required. Before Arrow page decompression, the actual page-header scan bounds headers to **64 KiB**, at most **65,536 pages** per shard, compressed *and* uncompressed payloads to **128 MiB (134,217,728 bytes)** per page, page value/row counts to **1,000,000**, and summed observed uncompressed page bytes to **2 GiB**. It also bounds Thrift nesting/container counts and checks declared/observed totals. Decode uses batches of at most **1024 rows**, counting actual rows (at most **1,000,000**) per batch and decoded column-value bytes (at most **384 MiB / 402,653,184 bytes** per row and **2 GiB** per shard) before copying/parsing each row into graph values. Limits are **per shard**, not per database or total snapshot. |

The loader copies its bounded file bytes again when building the Parquet
reader. `RELATE_SHARD_TARGET_BYTES` defaults to **256 MiB (268,435,456
bytes)** and accepts only decimal integers in **1..=1,073,741,824 bytes**;
invalid, zero, negative, overflowing and larger values return `Storage` when
checkpointing rather than falling back. A direct storage checkpoint target
has the same range. This is a *target*, not a maximum: a single row may
exceed it if its encoded shard remains within all file, page, row, decoded
byte and row-count bounds. Multi-row candidates split if they exceed the
target or any writer envelope bound; an unsplittable row fails without
publishing a manifest or recycling the committed WAL. Before building property
JSON or Arrow columns, the writer counts the exact tagged JSON and label/type
bytes across every candidate row, including rows beyond its initial 64-row
size sample. Its output sink refuses bytes beyond the 1 GiB file ceiling before
growing the encoded buffer; completed output is checked by the same
metadata/page preflight before publication. JSON
snapshot objects and manifest reads do not have these Parquet shard bounds;
these are not whole-database memory limits. Startup's WAL scan retains one
decoded record at a time; explicit `Wal::replay` retains all requested records,
so the frame cap does not bound its aggregate memory use.

New v3 Parquet parts use `nodes/{lo}-{hi}-{random128}.parquet` or
`relationships/{lo}-{hi}-{random128}.parquet`, with a fresh OS-random 128-bit
suffix for each written part (ADR-0089). The writer reserves each final name
exclusively and syncs its bytes and namespace before publishing the manifest
last. Unchanged parts retain their exact paths, including v3 fixed-name
`nodes/{lo}-{hi}.parquet` and `relationships/{lo}-{hi}.parquet` parts. v1/v2
`nodes.parquet` and `relationships.parquet` monoliths remain readable subject
to the same limits only for v1/v2 manifests with empty part lists. In v3 an
empty node or relationship part list is authoritative: retained monoliths are
neither loaded nor included in `snapshot_data_bytes()`. A failed part write
leaves the old manifest's bytes intact and keeps the WAL; a manifest
replacement can return an error after its new
contents become visible, in which case referenced new parts are retained and
the WAL is still kept. After a validated snapshot and WAL replay, unreferenced
generation-named parts can be reclaimed. On Windows, ambiguous old Native
generations are retained because a rename lacks a proven power-loss directory
barrier; Windows does not promise a new manifest's namespace durability.

No pull-identity map is written and no `pull_identity_path` manifest key is
published (ADR-0092's persisted maps are retired): a legacy key still present
in an old manifest is ignored on read and dropped on the next write, and the
`pull-identity/` objects or `pull-identity.json` sidecar an old store may
still hold are inert legacy data — never read, never rewritten, and outside
storage's sweep. The runnable `Manifest` doctest in `relate-storage`
demonstrates that a legacy `pull_identity_path` key is ignored on read; the
`storage_lock` example demonstrates persistent reopen.

`relate_storage::atomic_write` publishes a replacement using an
OS-random, exclusive virtual temporary file in the destination directory:
write, sync file, close (including on Windows), rename, then sync the Native
containing directory on Linux/macOS. New Native parent directory entries are
also synced. The destination is not removed first; a failure before rename
cleans up only the owned temp. A **post-rename directory-sync error** returns
`Io` even if the new bytes are visible; read back/recover rather than assume
which generation survived a crash. On Windows the synced file and atomic
rename do **not** guarantee that the new directory entry survives power loss.
Memory uses virtual replacement with no crash durability. Bundle publishes
virtual inode/catalog changes with its host-file barriers, without a host temp
sidecar or a per-virtual-rename host directory sync (ADR-0074, ADR-0098).
Each dirty Parquet part adds a file sync and (on Native Unix) a shard-directory
sync before the manifest's own file/directory sync. This is checkpoint I/O,
not per-query or per-commit I/O; orphan enumeration runs at validated reopen.

For the lower-level `relate_storage` VFS API, `VfsOpen::create_new()`
requests an atomic exclusive create, refusing an occupied name (including a
dangling final-component symlink on Native). `Vfs::open_exclusive` returns
`Ok(None)` on an occupied name; Native/Memory/Bundle implement the reservation
for their respective namespaces. Custom `Vfs` implementations must preserve
these semantics when used with `atomic_write`; the default fallback relies on
`exists` after `open` fails. `Vfs::create_dir_all_for_atomic` prepares parents;
`Vfs::metadata` returns the public
`VfsMeta { kind: VfsEntryKind, len: u64, mtime: Option<u64> }`, so external
VFS wrappers can delegate metadata alongside file I/O. `VfsEntryKind` is the
four-shape vocabulary — `File`, `Dir`, `Symlink` (junction/reparse included),
and `Other` (FIFO, socket, device: never admitted where a plain shape is
required) — and `VfsMeta` derives the predicates `is_dir()`, `is_symlink()`,
and `is_other()` from it, the one shape fact. `metadata` uses lstat semantics
on Native — an entry reports its own shape, never its target's — and the
Native VFS refuses symlinked files at `open` and refuses a symlinked
directory anywhere below the store root before `create_dir_all`,
`open`-with-create, or `rename` creates or writes beneath it, so a vault
walked through it can never read, write, or delete outside the store root.
`VfsMeta` offers the constructors `VfsMeta::file(len, mtime)`,
`VfsMeta::dir()`, `VfsMeta::symlink(len)`, and `VfsMeta::other(len)` — the
one spelling of each entry shape, used by every in-tree backend; `mtime`
(nanoseconds since the epoch) is `None` on virtual backends.
`Vfs::sync_dir` persists a published virtual directory entry on Native Unix.
Both defaults return `Error::Storage` if `durable()` is true, so a custom
durable VFS must override both methods with its own directory-persistence
contract; a post-rename `sync_dir` failure may leave new bytes visible but
cannot report atomic-write success. Memory uses non-durable parent creation
and a no-op directory sync;
Bundle explicitly implements virtual-directory creation and a no-op directory
sync because its host-file publication barriers persist the namespace.
A bare `NativeVfs::new`
does not acquire a database lease: ownership is acquired by persistent URI
parsing / `Database::open`, not every VFS constructor. The generic
`relate_storage::read_to_vec` has no maximum allocation of its own.

## Query input limits (ADR-0257)

Every query through `relate::Database` (`Connection::query`,
`query_with_params`, `execute`) is subject to two hard input limits,
enforced in `relate_parser::parse`:

- **Query text size: 4 MiB (4 194 304 bytes).** Checked before lexing;
  oversized input fails with a `Syntax` error naming the limit.
- **Nesting depth: 500.** Expression/primary nesting (parenthesized
  expressions, list/map literals, `NOT` chains, function arguments,
  subqueries) and statement nesting beyond 500 levels fails with a
  `Syntax` error naming the limit. The same limit is enforced again at
  the front-end lowering boundary.

Both produce typed errors — never process aborts or unbounded allocation.

## Value nesting depth (ADR-0462)

The input limits above bound static query shape; the values an expression
**builds** are capped independently at `relate::MAX_VALUE_DEPTH` (**500
container levels**). A list/map literal, comprehension, projection,
concatenation, `coll.zip`, or `reduce`/`allReduce` accumulator that would
nest past 500 levels fails with the typed execution error
`ValueDepthExceeded: value nesting exceeds 500 levels; …` instead of
building a value whose later recursive rendering, encoding, comparison or
drop could overflow the stack. Exactly 500 levels are accepted (`[]` is
level 1; `[[]]` is level 2); 501 is rejected. The cap is checked in the
`Value::checked_list`/`Value::checked_map` constructors, iteratively, and
also applies to JSON parameters at `params_from_map`. It is a fixed
constant: no option or environment override, matching the input limits'
house rule.

## Nested evaluation budget (ADR-0395)

The input limits above bound static shape; nesting multiplies dynamically.
A nested `CALL { … }` body runs once per imported row, an `EXISTS { … }` /
`COUNT { … }` / `COLLECT { … }` subquery once per outer row, and each level
multiplies the passes of the level below — a few hundred bytes of query
text can compound to an exponential number of nested engine passes that no
static limit sees. Every top-level statement therefore carries a **nested
evaluation budget of 100 000 nested clause-engine part passes** — the runtime
twin of the compile-side `MAX_CLONED_NODES` rewrite budget (ADR-0382), at
the same number.

**Counting rule:** per top-level statement, the engine counts one nested
clause-engine part pass for every nested body run — each `CALL { … }` body
once per imported row **per UNION part of the body** (every
`CALL … IN TRANSACTIONS` row included), each taken conditional
branch body once, each `EXISTS { … }` / `COUNT { … }` /
`COLLECT { … }` subquery once per outer row, each `FOREACH` clause list once
per list item per row, and each chained NEXT-stage statement once — and a
statement whose next pass would take the count past 100 000 fails instead
of running that pass. A wide body costs its part count, so the
rows × parts product lands on the budget, not just the row axis.

A flat query never ticks: the top-level statement's own clause run is not
counted. The budget is a fixed constant — no option, no environment
override. A statement whose legitimate nested fan would exceed it must
split into separate top-level statements; each gets its own budget.

Exceeding the budget fails the statement with `Error::Execution`, whose
message is:

```text
NestedEvaluationBudgetExceeded: nested evaluation exceeds 100000 passes per statement; nested subqueries compound per level (security budget).
```

## Local `LOAD CSV` capability (ADR-0081)

`OpenOptions.load_csv_dir: Option<PathBuf>` names a pre-existing CSV directory
independently of the database URI (including `:memory:` and `.rl` bundles).
At open, the engine resolves a relative directory against the process working
directory, canonicalizes it and requires a directory. `None` falls back to
`RELATE_LOAD_CSV_DIR`; with neither set, local files are denied. An explicit
option wins over the environment. Setting only `load_csv_max_bytes` never
enables access. These options are resolved when the database opens, not per
connection or per query; reopen the database to change them.

`load_csv_max_bytes: Option<u64>` defaults to **8,388,608 bytes (8 MiB)**;
`Some(n)` must be within **1..=67,108,864 bytes (64 MiB)**. Zero and larger
values fail at open, even without a CSV root. The engine reads at most
`n + 1` source bytes and rejects a file with that extra byte, regardless of
the size reported by metadata. Relative source paths stay under the canonical
root; absolute sources must use local `file:///` and stay under the root.
For example, Unix uses `file:///tmp/csv/rows.csv`; Windows uses forward-slash
drive paths such as `file:///C:/csv/rows.csv` (mapped to `C:/csv/rows.csv`).
Windows backslash, UNC/device/verbatim and drive-relative spellings are denied;
percent encoding is not decoded or accepted on either platform.
Bare absolute paths, `..`, other schemes, `file://` authorities, symlink
escapes and non-regular files are unavailable. Errors use the generic typed
messages above, with no filesystem path or OS details. Successful reads are
memoized by original source and canonical path **per statement**, including
nested execution. `relate_executor::execute` starts a fresh CSV memo on
every call, even when the same `ExecGraph` is reused; `execute_read_at`
creates a fresh read execution view for each call. A later statement reads afresh. The exact
canned CIP-202 fixture URL `https://data.neo4j.com/bands/artists.csv` works
without a root and does not fetch from the network. Canonicalization and
opening are not atomic against concurrent filesystem mutation (see ADR-0081).

See the [runnable Rust example](examples/load_csv.md).

## Query execution stack (ADR-0259)

Every query through `Connection::query` / `query_with_params` / `execute`
runs its input-proportional recursion (parse, front-end
lowering/validation, planning, execution) on that connection's worker
thread, which has a 64 MiB (67 108 864 byte) stack — unless the caller
has asserted the worker's own budget for its thread
(`assume_worker_stack_on_caller_thread`, ADR-0504), in which case
bounded-work statements run inline there and everything else takes the
worker as before. The worker is spawned
on the first query that needs it and joined when the connection is
dropped. Later statements on the same connection reuse it. A connection
whose statements all ran inline never reserves a stack.

The contract: for any query text within the ADR-0257 input limits above
— and for input that violates them — the engine returns a typed error
rather than aborting, **regardless of the calling thread's stack size**.
The calling thread publishes the query and waits (constant, shallow
frames), so callers on threads with small stacks (tokio's 2 MiB
workers, embedded threads, FFI consumer runtimes) are safe without any
action on their part. The sole assumption is that the process can spawn
one 64 MiB-stack thread per connection that has executed a query.

Two failure modes are part of the contract, both typed as
`Error::Internal`:

- **Worker spawn failure** (the OS refused the thread or the stack
  reservation): a typed internal error naming the resource failure. The
  engine never falls back to running the query on the caller's stack.
  The worker stays unset, and a later query may try to spawn again.
- **Worker panic**: caught on the worker and returned as a typed
  internal error carrying the panic message. It never unwinds into or
  aborts the calling thread. The same worker stays parked for the next
  statement.

The worker's budget is the public constant `QUERY_WORKER_STACK_BYTES`
(64 MiB, ADR-0259): the number a host asserting
`Connection::assume_worker_stack_on_caller_thread` must stack its
connection threads with (ADR-0504), exported so the engine's own Bolt
surface *derives* its connection-thread stack from it — the equality the
fast path's safety rests on is a compiler fact, not a copy. On a
caller-stack-asserted connection (ADR-0504) the whole contract holds with
the recursion running inline on that thread instead: same budget, same
typed-error containment (an inline panic is "query panicked on the
caller's thread: …", an admission error is the worker path's), and
statements that could outlive a keep-alive interval still take the worker
path above.

## EXPLAIN and PROFILE plan trees (ADR-0344/0345)

`EXPLAIN <statement>` plans the statement (parse, lower, plan, gates —
exactly the admission a plain run performs) and returns the structured
plan tree without executing anything — **no columns, no rows**; the plan
rides `QueryResult.plan` alone — `PlanArtifacts::root`, with the
statement's notifications beside it in the same artifact (ADR-0363) —
and the old single `plan` column of rendered string lines is gone
(ADR-0359). `PROFILE <statement>` executes through the normal
dispatch and returns the statement's own rows with the structured
profiled tree — per-operator metrics (`rows` produced, `time_ns`) on every
clause operator the dispatch loop executed — stamped on
`QueryResult.plan` (ADR-0344/0345 phase 2; the deprecated `_profile_*`
columns are removed).

The plan representation is the `PlanNode` tree:

- `Connection::explain_plan(cypher) -> Result<PlanNode>` — plan without
  executing: `ProduceResults` outermost, each earlier clause nested
  beneath (its input), `Union`/`UnionAll` above multi-part pipelines with
  the parts' pipelines as children, `Next` above chained `NEXT` stages.
  A planned write leaves the graph untouched, like `EXPLAIN`; a statement
  that cannot be planned fails with the planner's typed error.
- `PlanNode { operator, details, identifiers, children, estimated_rows, id, metrics }` —
  operator names follow the Neo4j vocabulary where the mechanism has an
  analog (`ProduceResults`, `Expand`, `EagerAggregation`, `Union`), this
  engine's own names otherwise (`VectorSearchMatch`, `Catalog`). Each
  operator's `details` name its subject: a `MATCH` pattern carries its
  text (variables, labels, relationship types, quantifier bounds —
  `(:Person)-[:KNOWS*1..2]->(:Person)`), a node-only pattern is a
  `NodeByLabelScan` naming its labels (`AllNodesScan` when it has none),
  an index-backed start narrates the seek instead (ADR-0486) —
  `NodeIndexSeek` for an anchored-equality point read on a range-indexed
  pair, `NodeIndexSeekByRange` for a comparison window, the details
  reading `RANGE INDEX <var>:<Label>(<prop>) WHERE <prop> <op> <bound>`
  with a multi-element pattern's text after the line and the estimate
  the seek's own count — an aggregation names its grouping keys, a
  `Create`/`Merge` carries its
  pattern text. `estimated_rows` is filled at tree construction from the
  store's cardinality hints (ADR-0218) — what they offer honestly (a
  single-label scan's label row count, a single typed relationship's
  type count), `None` where no signal covers the operator; `metrics`
  (`Option<ClauseMetrics { rows, db_hits, memory_bytes, expression_built_bytes,
  time_ns, reads, start, phases, batched, zone_skips, frontier, frontier_decline }>` — `reads` is the
  `StoreReads` kind mix behind `db_hits` (hops / neighbor walks / node
  reads), `start` the `Option<WalkStart>` naming the walk's element,
  direction and reason class, both re-exported by this crate (LABS-369,
  ADR-0375), `phases` the `PhaseTimes` seek/walk split (LABS-448),
  `batched` whether the clause ran ADR-0468's batch scan/filter/project
  chain over `Chunk` — `true` only on a scan-shaped `MATCH` the chain
  took, `false` on every other operator and every shape it declined; the
  plan table's `Details` cell marks it `[batched]` and the wire emits
  `Batched` with the metric set when true — and `zone_skips` (LABS-645)
  how many conjunct-batch evaluations that chain's filter provably
  replaced with the correct empty survivor set: the chunk's derived
  per-column min/max zone showed the conjunct can hold for no value, so
  the row walk never ran. `0` on every operator the chain declined and
  every clause that skipped nothing; the `Details` cell marks it
  `[zone-skips N]` and the wire emits `ZoneSkips` with the metric set
  when nonzero — and `frontier`
  (ADR-0478) the `FrontierCounters` run set of the frontier-NFA QPP
  kernel — `edge_visits` (the incident-edge visits its walk budget
  caps, riding the neighbor-walk read kind), `levels` (BFS levels
  completed), `endpoints` (endpoints accepted) and `cells` (the
  (node, state) pairs the runs created) — summed over the
  clause's per-seed runs, plus the NFA rung's own facts (ADR-0516):
  `rearrival_work` (the re-arrival rule's spent work, the numerator of
  `FrontierCounters::budget_fraction`, spent over the exported
  `REARRIVAL_WORK_BUDGET` constant), `memo_hits`/`memo_misses` (the
  rule's verdict-table asks answered without and with a search) and the
  admission asks by verdict — `representative_proofs`
  (representative-chain proof), `search_proven`, `search_disproven`
  and `declined_exhausted` (the budget's cut, ADR-0516): all zero on
  every clause the family declined, `levels > 0` marking the engaged
  operator, whose `Details` cell reads
  `[frontier edges=N levels=L endpoints=P cells=C]` and
  `[frontier-rule budget=F memo-hits=H memo-misses=M
  representative-proofs=R search-proven=SP search-disproven=SD
  declined-exhausted=DE]` while the wire emits the ten integers
  (`FrontierEdges`, `FrontierLevels`, `FrontierEndpoints`,
  `FrontierCells`, `FrontierMemoHits`, `FrontierMemoMisses`,
  `FrontierRepresentativeProofs`, `FrontierSearchProven`,
  `FrontierSearchDisproven`, `FrontierDeclinedExhausted`) together with
  the `FrontierBudgetFraction` float with the metric set when engaged —
  and `frontier_decline` (ADR-0516) the run-time decline's typed reason
  (`FrontierRunDecline`, re-exported by this crate beside
  `FrontierCounters` and `PhaseTimes`; `as_str` naming the decline:
  `RearrivalBudget`, the re-arrival budget's exhaustion, or
  `TableBudget`, the sparse tables' allocation ceiling, ADR-0521):
  `None` on
  every clause that never declined; the `Details` cell marks it
  `[frontier-declined rearrival-budget]` and the wire emits
  `FrontierDeclineReason` with the metric set when present) is
  `None` on every EXPLAIN node and stamped on every clause-granularity
  node the dispatch loop executed on a PROFILE run (ADR-0344). `kernel`
  (`Option<KernelRung>`, LABS-1117) names the traversal rung that served
  the clause's `MATCH` walk — `Chain { hops }`, `HubStar`, `FrontierNfa`,
  or `Walker` — `None` for every clause with no `MATCH` operator (a
  non-`MATCH` clause, a folded `MATCH`, a nested `MATCH`) and for a
  `MATCH` the batch chain answered (`batched`); a `SEARCH` `MATCH` reports
  `Walker`; the `Details` cell marks
  it `[kernel …]` and the wire emits `KernelRung` (plus `KernelHops` for
  the chain) with the metric set when present. `PlanNode::pattern_engine`
  (`Option<relate_executor::PatternEngine>`, LABS-1117) carries the
  store's static `RELATE_PATTERN_ENGINE` mode on every `MATCH` operator,
  EXPLAIN included (the wire `PatternEngine` arg) — the runtime rungs
  are operator-internal choices, so the plan can state the mode while
  `kernel` states what actually ran. The tree itself is only
  ever constructed on the
  EXPLAIN/PROFILE path, so a plain query pays nothing for it. `id` is the
  pre-order number (parent before children) the text `Id` column and the
  wire form's `args.Id` both read; the engine stamps it when it builds the
  tree, and a hand-built tree stamps its own with `PlanNode::assign_ids`
  (`PlanNode::new` is the one constructor — it starts `estimated_rows` at
  `None`, `id` at `0` and `warnings` empty). `warnings`
  (`Vec<PlanWarning>`) carries the typed warnings detected at tree
  construction: `PlanWarning::CartesianProduct` (a multi-pattern `MATCH`
  whose patterns share no join anchor), `PlanWarning::AllNodesScan { index }`
  (a label-less scan while the store carries a property index — `index` names
  the existing index, e.g. `:Person(name)`, as the suggestion), and —
  `PROFILE` only — `PlanWarning::EstimationMiss` (an operator whose
  estimate overshoots its actual rows by 10x or more, the same policy that
  flags the `Est/Actual` cell with `!`). A hand-built tree attaches its
  own with `PlanNode::with_warnings`. `PlanWarning::notification_kind()`
  names the `NotificationKind` each variant projects to — the one mapping
  the EXPLAIN/PROFILE derivation reads, and the three kinds are exactly
  the `PlanWarning` side of `NotificationKind::is_plan_warning()`.
- Plan text is rendered **at the boundary**, never in the engine
  (ADR-0359): the engine emits the tree only, and the removed
  `PlanNode::render_lines`/`render_with`/`render_lines_for_statement`/
  `render_header_for_statement` (and `PlanRenderOptions`) have no
  successors here. The one plan renderer is `relate-formatter`'s
  `plan_text_lines(tree, echo)` — the `Query:` echo header (the
  statement's one `EXPLAIN`/`PROFILE` preamble keyword stripped by the
  engine, stamped on the tree as `statement_echo`, then
  whitespace-collapsed) plus the `Plan:` marker and the boxed operator
  table (`| +` tree glyphs, per-column borders) — with the header pieces
  `plan_header_lines(echo)`. The table-mode composition — the same text
  followed by the one `Notifications:` block (ADR-0363 decision 8) — is
  the formatter's internal plan arm, not a public function: the data table
  goes to the main writer and that composition goes to the diagnostic
  writer, so one owner routes the plan. The prebuilt header means the
  renderer never re-derives it from an echo, so the bundle's facts cannot
  desync. `Formatter::format_with(data,
  diagnostics, columns, rows, meta)` is that two-writer entry point —
  every non-table mode ignores `diagnostics` and carries its plan payload
  on `data` — while `Formatter::format(w, columns, rows, meta)` is the
  one-writer form whose diagnostics are `w` itself (the CLI's machine
  modes and every embedded caller). The CLI, Bolt and HTTP call
  `plan_presentation(tree)` — the one composition (render the text, then
  decorate through `to_neo4j_value`) every plan-carrying surface shares,
  so the rendering and the decoration cannot drift; it takes the engine's
  `PlanNode` because it must call the engine's decorator, while the
  renderer itself keeps the wire-value contract. An embedded consumer
  that wants plan text depends on the formatter crate
  (`relate-formatter` is a surface crate — not available to, and not
  used by, the engine).
- `PlanNode::to_neo4j_value(plan_text)` — the Neo4j-facing plan value
  (ADR-0357), called on a result's plan root: the machine-native form
  decorated with the root args Neo4j's plan carries —
  `string-representation` (headed by the `Cypher`/`Planner`/`Runtime`
  lines tooling parses, then the caller's rendered plan text), `version`
  (the Cypher language version, `5`), the
  `planner`/`planner-impl`/`planner-version` and
  `runtime`/`runtime-impl`/`runtime-version` identity args (named honestly
  `relate`, never Neo4j's `COST`/`SLOTTED`) and `Memory` (the root's
  row-materialization bytes) — with `EstimatedRows` mirrored as Neo4j's
  float. `plan_text: Option<&str>` is the surface's rendering of the plan
  (ADR-0359: the boundary renders it through `relate-formatter`'s
  `plan_text_lines`); the engine only heads it with the identity block.
  `None` leaves `string-representation` off — what a consumer with no
  text rendering (a machine-mode consumer) asks for. It is the one
  decoration every surface calls at its boundary before converting the
  value to its own wire type, so the plan metadata cannot drift between
  Bolt, the HTTP Query API and the CLI's machine modes. `to_value` stays
  the undecorated engine form — the typed table/speedscope renderers walk
  it, so the float mirror never reaches their engine-form accessors.

- `QueryResult.plan: Option<Box<PlanArtifacts>>` — the plan artifact stamped on
  the result when the statement was `EXPLAIN` **or `PROFILE`**; its `root` is
  the structured tree (ADR-0344 phase 2 + metric fields: the PROFILE tree
  carries per-operator metrics
  — `rows` produced, `time_ns` elapsed, `db_hits` store accesses (hop
  expansions + neighbor walks + node-store reads, LABS-375),
  `memory_bytes` row-materialization bytes, `expression_built_bytes` the
  ADR-0463 construction-work telemetry (bytes the clause built, never an
  enforcement total; one over-cap allocation is refused) — on every
  clause-granularity node the dispatch loop executed), and its
  `notifications` is the list
  [Query notifications](#query-notifications-adr-0363) describes.
  `None` on every plain run — and on a `PROFILE` whose post-run estimates
  read is refused because the store was poisoned while the statement ran:
  that refusal leaves the executed rows untouched, so the result rides
  un-enriched rather than as an error that would discard it. The
  machine-native form is
  `PlanNode::to_value()` — a `Value::Map` in Neo4j's structured-plan
  field shape (`operatorType`, `args` with `Id`/`Details`/`EstimatedRows`,
  `PeakLogicalBytes` on the root when admission produced a receipt
  (ADR-0501), and — on profiled nodes — `Rows`/`TimeNs`/`DbHits`/`MemoryBytes`/
  `ExpressionBuiltBytes` emitted together,
  `identifiers`, `children`, and — on warned nodes — a `warnings` list
  of the warning names, `EstimationMiss`/`CartesianProduct`/`AllNodesScan`,
  serialized by name), numbered pre-order globally, the same ids the
  plan table's `Id` column shows — so consumers that already read
  Neo4j plans read this engine's plans unmodified. `PlanNode::is_profiled()`
  classifies the tree: `true` exactly on `PROFILE` trees — `EXPLAIN` never
  stamps metrics, `PROFILE` stamps them on every executed clause operator —
  the one classification fact every surface reads to pick its summary key
  (`plan` vs `profile` on Bolt, `queryPlan` vs `profiledQueryPlan` on the
  HTTP Query API), pinned by the engine test
  `is_profiled_classifies_explain_from_profile`
  (`relate/relate/tests/explain_profile.rs`).

See the [plan-tree example](examples/plan_tree.md).

## Query notifications (ADR-0363)

`QueryResult::notifications()` is the flat notification list derived with the
plan tree, stored on the result's `PlanArtifacts` beside that tree: `EXPLAIN`
and `PROFILE` populate it, every plain run leaves it empty because no tree is
built there (ADR-0363 decision 3). The list cannot exist without the tree,
so "notifications without a plan" is unrepresentable. The list is
deterministic (ADR-0363 decision 6): the operator-scoped entries follow the
tree's pre-order (the `Id` column's order), then the statement-scoped entries
follow by category (`UNRECOGNIZED` before `DEPRECATION`), then code, then
description, and identical `(kind, description)` pairs collapse to the first.

Each entry is a `relate::Notification` (the `relate-model` record
re-exported at the crate root; `NotificationKind`, `NotificationSeverity` and
`NotificationCategory` are re-exported beside it, so consumers never need the
`relate-model` crate for the vocabulary):

- `kind() -> NotificationKind` — the vocabulary row. The kind supplies the
  metadata, so a record never duplicates it:
  `code() -> &'static str`, `gql_status() -> &'static str`,
  `title() -> &'static str`, `severity() -> NotificationSeverity` and
  `category() -> NotificationCategory` (both with `as_str()` giving the wire
  spelling, `INFORMATION`/`WARNING` and `PERFORMANCE`/`UNRECOGNIZED`/
  `DEPRECATION`/`SCHEMA`/`HINT`), and `remedy() -> &'static str` (the kind's canonical
  remedy).
- `description() -> &str` — the instance detail (the unknown name, the index
  text, the warning message).
- `remedy() -> &str` — the effective remedy: the instance-specific action
  when the detector supplied one (`with_remedy`), else the kind's canonical
  remedy.
- `operator_id() -> Option<usize>` — the raising operator's `PlanNode.id`,
  present on operator-scoped kinds only.

The vocabulary is the one table in `relate_model::wire`
(`NotificationKind`, ADR-0363 decision 1): the operator-scoped
`CartesianProduct` (`Neo.ClientNotification.Statement.CartesianProduct`,
`03N90`), `UnboundedVariableLengthPattern` (`03N91`), `AllNodesScan`
(`Relate.Notification.Statement.AllNodesScan`, `03C90`), `MissingIndex`
(`03C91`), `EagerAggregation` (`03C92`) and `EstimationMiss` (`03C93`), plus
the statement-scoped `UnknownLabelWarning` (`01N50`),
`UnknownRelationshipTypeWarning` (`01N51`), `UnknownPropertyKeyWarning`
(`01N52`), `DeprecatedFunction`/`DeprecatedProcedure`
(`FeatureDeprecationWarning`, `01N01`) and `CallSubqueryWithoutScope`
(`01N00`). The three existing `PlanWarning` variants project to their rows by
an exhaustive match; their message text is the notification description, so
the plan table's warnings column/footer and the list cannot drift. `EstimationMiss` is
`PROFILE`-only: it needs actual rows.

### Statement-scoped notifications

The statement-scoped kinds have no operator to attach to, so they carry no
`operator_id`. Their triggers and remedies are the ADR-0363 decision 5 rows:

| kind | code | GQLSTATUS | trigger | remedy |
| --- | --- | --- | --- | --- |
| `UnknownLabelWarning` | `Neo.ClientNotification.Statement.UnknownLabelWarning` | `01N50` | A **read** node pattern names a label absent from the store catalog. | Check the spelling; the label exists after the first write that creates it. |
| `UnknownRelationshipTypeWarning` | `Neo.ClientNotification.Statement.UnknownRelationshipTypeWarning` | `01N51` | A **read** relationship pattern names a type absent from the store catalog. | Check the spelling; the type exists after the first write that creates it. |
| `UnknownPropertyKeyWarning` | `Neo.ClientNotification.Statement.UnknownPropertyKeyWarning` | `01N52` | A **read** property access or a **read** pattern property map names a key the store has never stored. | Check the spelling; the key exists after the first write that sets it. |
| `DeprecatedFunction` | `Neo.ClientNotification.Statement.FeatureDeprecationWarning` | `01N01` | A function call resolves to a function catalog entry whose `deprecated_by` is `Some` (`id()` today). | The remedy renders the replacement identifier (`id(n)` → `elementId`); the catalog's display prose is never backticked. |
| `DeprecatedProcedure` | `Neo.ClientNotification.Statement.FeatureDeprecationWarning` | `01N01` | A `CALL` resolves to a procedure registry signature whose `deprecated_by` is `Some` (e.g. `db.index.vector.queryNodes`). A registered procedure of the same name shadows the built-in, as it does for execution. | The remedy names the replacement for what it is: an identifier (`db.cdc.current`) or a clause (`SEARCH`). |
| `CallSubqueryWithoutScope` | `Neo.ClientNotification.Statement.FeatureDeprecationWarning` | `01N00` | A `CALL { … }` subquery lowers to `CallScope::None`. The scoped forms `CALL (n) { … }` and `CALL () { … }` do not fire. | Add the scope clause the body imports: `CALL (n) { … }`, or `CALL () { … }` when it imports nothing. |

The detectors read the facts the existing surfaces already expose, never a
second vocabulary (ADR-0363 decision 5):

- labels and relationship types read the statement view's `labels` /
  `rel_types` sets (the committed catalog plus an explicit transaction's
  scratch writes), the same names `db.labels` and `db.relationshipTypes`
  project and every write (and every checkpoint/WAL restore) records;
- property keys read the statement view's interner (committed plus scratch),
  a **superset** of the keys `db.propertyKeys` walks. Every stored key is interned on CREATE/SET/MERGE
  and again on checkpoint/WAL restore, so a key the store has never held is
  absent — a miss is a true miss and the warning never fires for a stored
  key. Labels, relationship types and the internal `ts` key share the
  interner and a removed key stays interned, so a hit can suppress a
  warning (a false negative) but can never invent one;
- deprecations read the function catalog's `deprecated_by` option
  (`SHOW FUNCTIONS`) and the procedure registry's
  `ProcSignature.deprecated_by` option (`SHOW PROCEDURES`) through the
  typed `builtin_function_deprecation` / `builtin_procedure_deprecation`
  lookups; `None` means not deprecated, and there is no parallel list of
  deprecated names.

**Write-clause scope (ADR-0363 decision 13).** The three `Unknown*Warning`
kinds are derived only from read-side evidence: the labels, relationship
types and pattern property keys of `MATCH` patterns (including patterns
nested in pattern predicates, comprehensions and `shortestPath`) and read
property accesses (`n.key`, `expr { .key }`). A `CREATE`/`MERGE` target
pattern contributes nothing — its names are the statement's own output — and
a `SET`/`REMOVE` property target contributes nothing for that target key,
while a read on the right-hand side (`SET n.key = n.other`) still does. A
mixed statement still reports its read half: `MATCH (n:Persn) SET n.name =
'Ada'` reports 01N50 for `Persn` and nothing for `name`, and a fresh
`CREATE (:Person {name: 'Ada'})` reports neither 01N50 nor 01N52.

`UnknownLabelWarning`, `UnknownRelationshipTypeWarning` and
`UnknownPropertyKeyWarning` are emitted at most once per unknown name per
statement. The deprecation rows are 01N01 because every deprecated catalog
entry currently names a replacement; the vocabulary has no 01N02 row yet.

### Operator-scoped non-warning notifications

The three operator-scoped kinds that are **not** plan warnings (ADR-0363
decisions 4 and 8) derive from the tree's pattern/operator/estimate facts at
tree construction, carry their operator's `PlanNode.id`, and appear only in
`QueryResult::notifications()` — the plan table's `Warnings` column and footer
keep `CartesianProduct`, `AllNodesScan` and `EstimationMiss`, so no kind is
rendered twice:

- `UnboundedVariableLengthPattern` (`Neo.ClientNotification.Statement.UnboundedVariableLengthPattern`,
  `03N91`, INFORMATION/PERFORMANCE) — a variable-length relationship
  pattern or quantified path group with no upper bound (`[:R*]`, `[*1..]`, a
  quantified group with `max: None`) that plans as an `Expand`. The
  description names the pattern text, and the canonical remedy asks for a
  bound (`[:R*1..5]`) or a selective endpoint/label predicate plus `LIMIT`.
  A bounded `[:R*1..3]` does not fire.
- `MissingIndex` (`Relate.Notification.Statement.MissingIndex`, `03C91`,
  INFORMATION/PERFORMANCE) — a label pattern with an equality property
  predicate the planner folded into the node's properties (`WHERE n.email =
  $e` or an inline `{email: $e}`) while no RANGE index serves that
  `(Label, property)` pair. Coverage is the typed executor read: a composite
  index covers every component (the executor materializes and seeks each
  one), a relationship index serves no node pair, and a property name is
  never split on a comma. The description names
  the label and property,
  and the instance remedy overrides the canonical text with the exact
  runnable statement
  `CREATE INDEX relate_<Label>__<property> FOR (n:<Label>) ON (n.<prop>)`:
  the name preserves each name's case and escapes injectively rather than
  folding, so distinct `(label, property)` pairs never share the global
  index name: `_` becomes `_5f`, every other non-ASCII-alphanumeric byte
  becomes `_` plus two lowercase hex digits, and the two escaped parts are
  joined by a double underscore (`A-B` → `relate_A_2dB__x`, `A_B` →
  `relate_A_5fB__x`, `Order Item`/`ship-date` →
  `relate_Order_20Item__ship_2ddate`, `Person`/`email` →
  `relate_Person__email`). An escaped part never contains `__` (every
  underscore starts a three-character `_hh` escape), so after the fixed
  `relate_` prefix the first `__` is the separator and the split is
  unambiguous — `(A_, x)` (`relate_A_5f__x`) and `(A, _x)`
  (`relate_A___5fx`) stay distinct, as do case-variant labels.
  Unusual labels/properties are backtick-quoted with embedded
  backticks doubled. Keyword names are quoted too (``ON (n.`order`)``),
  because the property-name position rejects a clause keyword unquoted; the
  quoting rule is the parser's single policy
  (`relate_parser::token::quote_identifier`).
  Executing the remedy verbatim creates the index and the next
  `EXPLAIN` derives no `MissingIndex`.
- `EagerAggregation` (`Relate.Notification.Statement.EagerAggregation`,
  `03C92`, INFORMATION/PERFORMANCE) — an `EagerAggregation` operator whose
  input child's `estimated_rows` reaches `EAGER_INPUT_NOTIFICATION_ROWS`
  (`100_000`). The description names the input estimate and the threshold;
  the canonical remedy asks to reduce the rows reaching the aggregation
  with a selective predicate or a `LIMIT` before it.

The formatter's composed table-mode presentation — the bundle's tree
rendered through the one plan layout, followed by the `Notifications:`
block — is what `Formatter::format_with(data, diagnostics, columns, rows,
meta)` writes: the data table goes to `data`, the plan table and its block
go to `diagnostics`, so the block follows the table it annotates. `meta`
carries the `PlanView` bundle the surface already owns (Neo4j-facing wire
value, engine-form tree, prebuilt header lines, notification list), so the
table header comes from the bundle rather than a re-derived echo. Every
other mode ignores `diagnostics`; `Formatter::format(w, columns, rows, meta)`
is the one-writer form whose diagnostics are `w` itself.

Two wire forms exist (ADR-0363 decisions 9/11/12). Their serializers are
defined in the shared `relate_model::wire` module and re-exported at the
`relate` crate root beside `Notification`, so a consumer imports one
crate:

- `relate::notification_to_wire_json(&Notification)` — the
  engine-native entry: `code`, `gql_status`, `severity`, `category`,
  `title`, `description`, `remedy`, plus `operator_id` on operator-scoped
  kinds. `relate::notifications_to_wire_json(&[Notification])` renders
  the array.
- `relate::notification_to_neo4j_json(&Notification)` — the
  Neo4j-shaped entry: exactly `code`, `title`, `description`, `severity`,
  `category`; the remedy is appended to `description` as
  ` (remedy: <remedy>)`, and no `remedy` or `position` key ever appears.
  `notification_to_neo4j_entries(&Notification)` is its ordered-pairs form
  (`Vec<(&'static str, String)>`) for callers that build a protocol record
  key by key instead of re-reading a JSON object; both read the one key
  table, so Bolt and the HTTP Query API cannot drift.

The shared success envelope (built by the one canonical
`relate_model::wire::response_envelope`, which
`execute_wire`/`run_script_wire`, the C ABI's
`query_result_to_wire`, and the MCP tool envelope use) always carries a
`notifications` array — empty on a plain run, never omitted — and the
per-phase timing split
`parse_ms` / `plan_ms` / `execute_ms` beside `elapsed_ms` (ADR-0491): the
statement's always-on PARSE / PLAN / EXECUTE window split, each phase
re-sourced from the result header through the same single Duration→ms
conversion as `elapsed_ms` (the one projection:
`relate::timing_stamp_ms(&QueryResult)` bundles the total and its
split into the `wire::TimingStampMs` the builder takes), so the three sum
to no more than the total (the inter-stage gaps stay in the total alone).

See the [notification example](examples/notifications.md).

## Batched global bulk writes (ADR-0209)

Auto-commit write statements whose plan is a *global bulk* shape — bare
`MATCH` patterns with no `WHERE`, `OPTIONAL`, or `SEARCH`, at most
pure-rename `WITH`s, and exactly one terminal write clause
(`DELETE`/`DETACH DELETE` of the matched variables, per-entity
`SET`/`REMOVE`, or `CREATE` without graph-sensitive expressions) — do not
run as one giant transaction. When the estimated touched-entity count
reaches the batching threshold, the engine snapshots the matched entity
id set and applies the write in bounded batches, each batch its own WAL
commit (the same trade-off `CALL { } IN TRANSACTIONS` makes explicit).

Consequences:

- Memory stays bounded by the batch budget plus the matched id vectors,
  and no single WAL record approaches the 64 MiB payload limit, so
  statements like `MATCH (n) DETACH DELETE n` on a dense store or a
  417×417 cross-product `CREATE` complete instead of exhausting memory.
- The success path is identical to the unbatched run: same final store
  state, same summed `QueryStats`, same (empty) result rows.
- The error path is not atomic: batches committed before a failure stay
  committed. The one exception is a non-`DETACH` node `DELETE`, whose
  `DeleteConnectedNode` check is preflighted across all targets before
  the first batch commits, so it fails with nothing written — as long as
  no concurrent writer commits a new relationship after the preflight
  snapshot; such a race surfaces `DeleteConnectedNode` mid-run with
  earlier batches committed. A non-`DETACH` `DELETE` mixing node and
  relationship targets is not eligible and keeps the unbatched path, as
  does any statement once a constraint exists (the batched run re-checks
  the catalog per batch and fails if one appears mid-statement).
- Statements on a catalog with constraints, inside an explicit
  transaction (`begin`/`commit`), or below the threshold keep the
  single-commit atomic path.

Two env vars tune the behavior (both default when unset or invalid, and
both are read once at `Database::open`, like the other engine knobs):
`RELATE_BULK_BATCH_THRESHOLD` (default `10000`, the estimated
touched-entity count at which batching engages) and
`RELATE_BULK_BATCH_BUDGET` (default `25000`, the estimated
touched-entity budget per batch). Set-but-invalid values fail open with a semantic error, like `RELATE_LOGICAL_GRAPH_SIZE_LIMIT`.

## Examples

Runnable examples live in [`relate/relate/examples/`](../../../relate/relate/examples/)
so cargo compiles and runs them in CI (`cargo run -p relate --example <name>`):

| Example | Shows |
| --- | --- |
| `create_match.rs` | open `:memory:`, create a node, match it back |
| `transaction.rs` | explicit multi-statement transaction (ADR-0028) |
| `create_index.rs` | `CREATE INDEX` / `SHOW INDEXES` / `DROP INDEX` |
| `memory_limit.rs` | `parse_memory_limit` SIZE parsing |
| `open_options.rs` | `Database::open_with` checkpoint timer + memory cap |
| `snapshot_json.rs` | JSON snapshot codec (ADR-0050) |
| `write_conflict.rs` | client-side `Error::Conflict` retry (ADR-0017) |
| `cdc_slot.rs` | CDC slot, WAL poll, apply-only replica |
| `load_csv.rs` | opt-in root and custom bounded per-file read; run `cargo run -p relate --example load_csv` |
| `storage_lock.rs` | exclusive persistent open, typed contention error and reopen after drop; run `cargo run -p relate --example storage_lock` |
| `adopt_legacy_wal.rs` | offline operator-verified default-WAL adoption, reopen and reset; run `cargo run -p relate --example adopt_legacy_wal` |
| `decoder_snapshot.rs` | checkpoint and reopen Point/Vector and valid temporals, verify a failed temporal checkpoint preserves the previous JSON checkpoint, decode a v2 escaped map, reject direct/nested Geometry on v1/v2 snapshot open with contextual `Error::Storage`, and reject a malformed point |
| `value_sharing.rs` | clones of String/List/Map/Vector results share their `Arc` payload; `.into()` / `Arc::new` parameters, `Arc::unwrap_or_clone` (ADR-0170); run `cargo run -p relate --example value_sharing` ([walkthrough](examples/value_sharing.md)) |

The [persistence writer and decoder examples](examples/decoder_formats.md) are
Rust doctests in `relate-model` and `relate-arrow`, run by
`cargo test --doc -p relate-model -p relate-arrow` in the Rust test job.
They include the public numeric-offset parser's valid and invalid inputs,
zoned Time, heterogeneous/named DateTime, subminute-offset and extreme
local-range failures, expanded-year near-boundary DateTime roundtrips, the
nested WKB SRID/depth boundary, 3D Point and inexact mixed-number fallbacks,
and external non-finite WKB rejection.
