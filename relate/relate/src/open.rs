//! The open path of [`Database`] (ADR-0371): `open`, `open_read_only`,
//! `open_with_wal_dir`, `open_with`, `open_vfs`, and `open_store` with its
//! resolve_* wiring — split out of lib.rs so that file stays under the
//! 1k-line budget (LABS-392). Inherent methods on `Database`; the public
//! paths are unchanged.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use relate_core::{
    with_load_scope, Error, LoadThreads, OpenTransientTracker, PreviewFeatures, Result,
};
use relate_executor::{CsvPolicy, Graph, PatternEngine};
use relate_storage::{
    open_store_notifying, open_store_readonly_with_ontology, parse_uri_opening_with_config,
    CodecOpenConfig, SnapshotCodecKind, StoreTarget, Vfs, Wal,
};

use super::commit_timing::commit_timing_env;
use super::ontology;
use super::open_config::{
    reject_cypherlite_env, resolve_bulk_batch_limits, resolve_checkpoint_interval,
    resolve_codec_config, resolve_durability_mode, resolve_expression_memo_budget,
    resolve_expression_memory_limit, resolve_load_threads, resolve_max_traversal_depth,
    resolve_memory_limit, resolve_pattern_engine, resolve_preview_features, resolve_snapshot_codec,
    resolve_token_limits, resolve_wal_payload_limit,
};
use super::shared::{OpenResolved, SharedOpenState};
use super::token_budget;
use super::{
    check_loaded_graph, report_trimmed_tail, Database, DurabilityMode, OpenMode, OpenOptions,
    Shared,
};

/// What the loader selection hands the open: the storage family's
/// read-only carrier — graph, WAL, checkpointed ontology document,
/// and committed schema (ADR-0649 — `None` for every store whose
/// format defines no committed schema files) — under its facade name,
/// marking that it passed the screen below. One shape across the
/// boundary, never a second re-declaration of the same fields.
pub(crate) type ScreenedOpen = relate_storage::ReadonlyStore<Graph>;

/// The one loader selection and screen for a store open — the open
/// path and the Follow-mode reload both load through this, so both
/// policies have one home. Every read-only-mode open (`ReadOnly`,
/// `Follow`, or `Ephemeral`) of a **durable** store loads through the
/// non-trimming read-only loader (ADR-0136 — it opens no WAL for
/// append and repairs nothing); a writable open, and any open of a
/// `:memory:`-shaped store (which has no existing WAL for the
/// read-only loader to open), loads through the notifying writable
/// loader, which reports each trimmed torn tail the moment it is
/// trimmed (a trim is durable, so a failure anywhere later in the open
/// must not swallow the durability event — the same reporting the
/// promote path uses). The typed [`OpenMode`] — the same one the
/// `StoreTarget` was constructed from — decides, so the loader choice
/// cannot disagree with the construction. The screen every consumer of
/// a loaded ontology document owes it runs once on the returned
/// carrier, whichever loader ran: a stored document must parse before
/// it is trusted (a corrupt or hand-crafted record surfaces here
/// rather than at the first procedure that touches it).
pub(crate) fn open_store_screened(
    target: &StoreTarget,
    mode: OpenMode,
    wal_payload_limit: u64,
) -> Result<ScreenedOpen> {
    let opened = if mode.is_read_only() && target.vfs().durable() {
        open_store_readonly_with_ontology(target, wal_payload_limit)?
    } else {
        // The notify sink already reported the open's trims, so the
        // report is not carried past the screen.
        open_store_notifying(target, wal_payload_limit, &mut report_trimmed_tail)?.store
    };
    ontology::validate_stored_document(opened.ontology.as_deref())?;
    Ok(opened)
}

/// [`open_store_screened`] inside a load scope of `threads`: the load's
/// transient allocations are charged to one fresh tracker, and its peak is
/// returned beside the screened store. The open and the Follow gap reload
/// both load through this.
pub(crate) fn open_store_scoped(
    target: &StoreTarget,
    mode: OpenMode,
    wal_payload_limit: u64,
    threads: LoadThreads,
) -> Result<(ScreenedOpen, u64)> {
    let tracker = Arc::new(OpenTransientTracker::new());
    let opened = with_load_scope(threads, Arc::clone(&tracker), || {
        open_store_screened(target, mode, wal_payload_limit)
    })?;
    Ok((opened, tracker.peak_bytes()))
}

/// The preflight-resolved open knobs, in one named carrier: the validated
/// LOAD CSV policy, the memory limit, the traversal-depth admission limit
/// (LABS-610), the commit durability policy (ADR-0470), the store's
/// resolved preview set (ADR-0371), the diagnostic pattern-engine switch
/// (ADR-0519), the requested snapshot codec, the
/// markdown vault's body-property configuration (ADR-0460), the
/// checkpoint interval, the bulk-batch limits, the token limits and the
/// per-record WAL payload cap (ADR-0650), and the load thread count
/// (ADR-0664).
/// [`Database::open_preflight`] resolves every knob before a store
/// directory is leased, so every environment-driven failure precedes the
/// store load. [`Database::open_store`] installs the rest; the snapshot
/// codec and the body properties are spent earlier, when `open_with` /
/// `open_vfs` build the `StoreTarget`.
struct OpenKnobs {
    csv_policy: CsvPolicy,
    logical_graph_size_limit: u64,
    max_traversal_depth: u32,

    expression_memory_limit: Option<u64>,
    /// Whole MiB (ADR-0657).
    expression_memo_budget: u64,
    durability_mode: DurabilityMode,
    preview: PreviewFeatures,
    pattern_engine: PatternEngine,
    snapshot_codec: Option<SnapshotCodecKind>,
    /// The resolved codec-specific open-time configuration (ADR-0460) —
    /// the spent form; the option→config conversion happens once, in the
    /// preflight below.
    codec_config: Option<CodecOpenConfig>,
    checkpoint_interval: std::time::Duration,
    /// `(threshold, budget)` of the bulk-batch commit path.
    bulk_batch_limits: (u64, u64),
    wal_payload_limit: u64,
    token_limits: token_budget::TokenLimits,
    load_threads: LoadThreads,
}

impl Database {
    /// Open `:memory:`, a filesystem path, or `file://…`.
    /// Paths ending in `.rl` — or the legacy v0.16 `.cl` — are a single-file
    /// bundle (ADR-0074, ADR-0517 §5); other paths are a Native directory.
    /// Always loads the snapshot then replays the WAL (`MemoryVfs` for
    /// `:memory:`).
    pub fn open(uri: &str) -> Result<Self> {
        Self::open_with(uri, OpenOptions::default())
    }

    /// Load an existing store once and refuse writes and checkpoints (ADR-0136).
    /// A missing store is not created.
    pub fn open_read_only(uri: &str) -> Result<Self> {
        Self::open_with(uri, OpenOptions::with_mode(OpenMode::ReadOnly))
    }

    /// Load an existing store once **into memory** and release the source
    /// (LABS-903): the snapshot is loaded and the WAL replayed as a
    /// read-only open does, then the `.rl` host file or store directory is
    /// closed and unlocked — the running store is `:memory:`-shaped, and
    /// the source can be deleted or opened writable by someone else
    /// immediately. Writes and checkpoints are refused (there is nothing to
    /// write back to). A missing store is not created.
    pub fn open_ephemeral(uri: &str) -> Result<Self> {
        Self::open_with(uri, OpenOptions::with_mode(OpenMode::Ephemeral))
    }

    /// The peak transient memory, in bytes, of the load that built this
    /// handle's graph: the most the snapshot decode and the adjacency
    /// rebuild held at once beyond the graph they produced. `0` for an
    /// empty `:memory:` store, which loads nothing. A `Follow` handle
    /// replaces it with the peak of each gap reload.
    ///
    /// ```
    /// use relate::Database;
    ///
    /// let db = Database::open(":memory:")?;
    /// assert_eq!(db.open_peak_transient_bytes(), 0);
    /// # Ok::<(), relate::Error>(())
    /// ```
    pub fn open_peak_transient_bytes(&self) -> u64 {
        self.inner.open_peak_transient_bytes.load(Ordering::Relaxed)
    }

    /// Test hook: the shared engine state, for `commit.rs` group-commit tests
    /// that build a `flush_group` batch by hand. Not part of the public API.
    #[cfg(test)]
    pub(crate) fn shared(&self) -> Arc<Shared> {
        Arc::clone(&self.inner)
    }

    /// `wal_dir` overrides the local WAL path for filesystem URIs (default
    /// `<dir>/wal`). Ignored for `:memory:`. `s3://` is rejected (ADR-0044).
    pub fn open_with_wal_dir(uri: &str, wal_dir: Option<PathBuf>) -> Result<Self> {
        Self::open_with(
            uri,
            OpenOptions {
                wal_dir,
                ..Default::default()
            },
        )
    }

    /// Open with WAL directory, checkpoint-timer, and memory-limit overrides
    /// (ADR-0041, ADR-0042).
    pub fn open_with(uri: &str, opts: OpenOptions) -> Result<Self> {
        // Reject an invalid LOAD CSV policy, memory limit, preview
        // feature list, and markdown body-property spec before
        // parse_uri_opening leases a directory. A failed open must not
        // leave that directory behind.
        let knobs = Self::open_preflight(&opts)?;
        let read_only = opts.mode.is_read_only();
        // The codec-specific configuration rides the parse: the storage
        // layer installs — or refuses — it before the native lease, so
        // a failed open on a fresh path leaves nothing behind.
        let store = parse_uri_opening_with_config(
            uri,
            opts.wal_dir.clone(),
            knobs.snapshot_codec,
            read_only,
            knobs.codec_config.clone(),
        )?;
        Self::open_store(store, opts, knobs)
    }

    /// Open on a caller-supplied [`Vfs`] (ADR-0145).
    ///
    /// Snapshot and WAL share `vfs` (prefix `wal`). `opts.wal_dir` is ignored.
    /// Follow is rejected because the store has no native directory.
    /// `snapshot_codec: None` uses [`SnapshotCodecKind::default`].
    ///
    /// ADR-0623 runs mode-aware in the construction
    /// ([`StoreTarget::from_vfs_with_codec_opening`]): a writable open
    /// adopts a legacy `.cypherlite/` engine directory on the VFS
    /// through one [`Vfs::rename`](relate_storage::Vfs::rename) before
    /// the store is built; a read-only open never adopts — it refuses a
    /// legacy markdown sidecar closed, naming the one writable pass that
    /// adopts it, and a legacy `.cypherlite/` without the manifest (a
    /// classic store's schema mirror alone) is inert, opened unchanged.
    pub fn open_vfs(vfs: Arc<dyn Vfs>, opts: OpenOptions) -> Result<Self> {
        let knobs = Self::open_preflight(&opts)?;
        // No lease to protect here (the VFS is caller-supplied): the
        // construction itself gates the codec configuration before its
        // pre-read step, so a config-refused open never reaches the
        // writable leg's adoption rename — the native leg's
        // fail-before-mutation order, shared by both legs.
        // The open's mode threads once into the storage construction
        // (the typed twin of the `read_only` bool `open_with` derives
        // for its parse): every read-only shape — frozen, follow,
        // ephemeral — is a read-only construction; follow itself is
        // refused below for a store with no native directory.
        let mode = match opts.mode {
            OpenMode::Writable => relate_storage::OpenMode::OpenOrCreate,
            OpenMode::ReadOnly | OpenMode::Follow | OpenMode::Ephemeral => {
                relate_storage::OpenMode::ReadOnly
            }
        };
        let codec = knobs.snapshot_codec.unwrap_or_default();
        let store =
            StoreTarget::from_vfs_with_codec_opening(vfs, codec, mode, knobs.codec_config.clone())?;
        Self::open_store(store, opts, knobs)
    }

    /// LOAD CSV root and byte limit from `opts`, with `RELATE_LOAD_CSV_DIR`
    /// as the root fallback. Resolved before any filesystem open so a bad
    /// policy does not create the database directory — the fail-before-lease
    /// contract the preflight below upholds.
    fn load_csv_policy(opts: &OpenOptions) -> Result<CsvPolicy> {
        let root = opts
            .load_csv_dir
            .clone()
            .or_else(|| std::env::var_os("RELATE_LOAD_CSV_DIR").map(PathBuf::from));
        CsvPolicy::new(root, opts.load_csv_max_bytes)
    }

    /// The one fail-before-lease preflight both public entry points run:
    /// the caller-resolved knobs whose invalid values must reject the open
    /// before a store directory is leased or written (the failed-open
    /// invariant the CSV and memory-limit knobs established).
    fn open_preflight(opts: &OpenOptions) -> Result<OpenKnobs> {
        // The product-rename guard (ADR-0517): a still-set CYPHERLITE_* name
        // fails closed before any knob resolves it as unset.
        reject_cypherlite_env()?;
        Ok(OpenKnobs {
            csv_policy: Self::load_csv_policy(opts)?,
            logical_graph_size_limit: resolve_memory_limit(opts)?,
            max_traversal_depth: resolve_max_traversal_depth(opts)?,

            expression_memory_limit: resolve_expression_memory_limit(opts)?,
            expression_memo_budget: resolve_expression_memo_budget(opts)?,
            durability_mode: resolve_durability_mode(opts)?,
            preview: resolve_preview_features()?,
            pattern_engine: resolve_pattern_engine()?,
            snapshot_codec: resolve_snapshot_codec(opts)?,
            codec_config: resolve_codec_config(opts)?,
            checkpoint_interval: resolve_checkpoint_interval(opts),
            bulk_batch_limits: resolve_bulk_batch_limits()?,
            wal_payload_limit: resolve_wal_payload_limit(opts)?,
            token_limits: resolve_token_limits(opts)?,
            load_threads: resolve_load_threads(opts)?,
        })
    }

    fn open_store(store: StoreTarget, opts: OpenOptions, knobs: OpenKnobs) -> Result<Self> {
        let read_only = opts.mode.is_read_only();
        let follow = opts.mode.follows();
        // LABS-903: an ephemeral open is `OpenMode::Ephemeral` — read-only
        // by construction and never a follower, so the illegal combinations
        // (a writable or following turn-into) are unrepresentable rather
        // than validated here.
        // One writer lease (ADR-0136 / ADR-0440). A writable native open
        // moves it from the VFS onto `Shared` before loading, so `demote`
        // can drop it later. A follow or read-only open has none yet.
        #[cfg(not(target_arch = "wasm32"))]
        let writer_lease = if read_only {
            None
        } else {
            store.vfs().take_writer_lease()
        };
        if follow && store.filesystem_dir().is_none() {
            return Err(Error::semantic(
                "live freshness (WAL follow) is only supported for native directory databases",
            ));
        }
        // Every field is named: a new `OpenKnobs` field must be handled here
        // (or explicitly discarded) instead of riding an unnoticed `..`.
        // `snapshot_codec` and `codec_config` were spent before the store
        // existed — they selected the store's codec and installed its
        // open-time configuration in `open_with`/`open_vfs`.
        let OpenKnobs {
            csv_policy,
            logical_graph_size_limit,
            max_traversal_depth,

            expression_memory_limit,
            expression_memo_budget,
            durability_mode,
            preview,
            pattern_engine,
            snapshot_codec: _,
            codec_config: _,
            checkpoint_interval,
            bulk_batch_limits: (bulk_batch_threshold, bulk_batch_budget),
            wal_payload_limit,
            token_limits,
            load_threads,
        } = knobs;
        let (
            ScreenedOpen {
                mut graph,
                mut wal,
                ontology,
                committed,
            },
            open_peak_transient_bytes,
        ) = open_store_scoped(&store, opts.mode, wal_payload_limit, load_threads)?;
        check_loaded_graph(&graph)?;
        graph.configure_load_csv(csv_policy);
        // ADR-0463: the per-allocation expression cap rides the same
        // pre-share store-level bundle as the CSV policy and preview set.
        graph.configure_expression_memory_limit(expression_memory_limit);
        // ADR-0657: the shared memo budget rides the same bundle.
        graph.configure_expression_memo_budget(expression_memo_budget);
        // ADR-0378: the store's resolved preview set lives on the same
        // store-level `StoreServices` bundle as the CSV policy, so every
        // namespace and session of the store gates (and lists) alike.
        graph.configure_preview(preview);
        // ADR-0519: the diagnostic pattern-engine switch rides the same
        // pre-share store-level bundle, so every namespace and session of
        // the store routes alike and a follower keeps it across a reload.
        graph.configure_pattern_engine(pattern_engine);
        // ADR-0120 §2/§4: the loaded document is the store's ontology
        // state. It drives the WAL segment magic explicitly (over the
        // scanned active segment's default); no document keeps today's
        // behavior exactly: MDBWAL01. The document stays the sole stored
        // source of truth — no `system_ontologies` graph is derived from
        // it (ADR-0229 §3) — but it still must parse: open surfaces a
        // corrupt or hand-crafted record here instead of deferring the
        // failure to the first procedure that touches it.
        wal.set_ontology_held(ontology.is_some() || committed.is_some());
        let budget = token_budget::TokenBudget::new(token_limits);
        // The knob cluster, stated once: the open path resolves every knob
        // and hands `Shared::new` the whole `OpenResolved` value.
        let resolved = OpenResolved {
            read_only,
            opened_as_follow: follow,
            durability_mode,
            logical_graph_size_limit,
            max_traversal_depth,
            safe_mode: opts.safe_mode,
            budget,
            bulk_batch_threshold,
            bulk_batch_budget,
            wal_payload_limit,
            checkpoint_interval,
            follow_gap: opts.follow_gap,
            load_threads,
        };
        // LABS-903's turn-into, at the seam the memory-first load created:
        // everything above materialized the graph and the replayed WAL into
        // memory from `store` — so an ephemeral open has nothing left to
        // read from it. One named operation releases the source; a
        // `:memory:` open is its own stand-in already (the release is the
        // no-op it never takes).
        let (store, wal) = if opts.mode == OpenMode::Ephemeral && store.vfs().durable() {
            release_durable_source(store, wal)?
        } else {
            (store, wal)
        };
        let commit_timing = commit_timing_env();
        let shared = Arc::new(Shared::new(
            graph,
            store,
            wal,
            SharedOpenState {
                resolved,
                ontology,
                commit_timing,
                open_peak_transient_bytes,
            },
        )?);
        #[cfg(not(target_arch = "wasm32"))]
        let lease: InstalledLease = writer_lease;
        #[cfg(target_arch = "wasm32")]
        let lease: InstalledLease = ();
        finish_open(&shared, committed, lease)?;
        Ok(Self { inner: shared })
    }
}

/// The writer lease a writable open installs on `Shared`: the platform's
/// lease where the filesystem layer keeps one, the unit carrier where it has
/// none (wasm). Kept a named type so [`finish_open`] has one signature on
/// both targets.
#[cfg(not(target_arch = "wasm32"))]
type InstalledLease = Option<relate_storage::WriterLease>;
#[cfg(target_arch = "wasm32")]
type InstalledLease = ();

/// The open tail, everything after [`Shared::new`]: the committed schema's
/// apply, the token table's baseline drain, the writer-lease install and the
/// three background-thread starts. One named step, so `open_store` reads
/// resolve → open → build → finish and the constructor stops growing per
/// feature. The lease carrier is [`InstalledLease`] — empty on wasm, where
/// there is no filesystem lease to install.
fn finish_open(
    shared: &Arc<Shared>,
    committed: Option<relate_storage::CommittedSchema>,
    _writer_lease: InstalledLease,
) -> Result<()> {
    // The committed schema's open tail (ADR-0649): applied after the schema
    // mirror verification (inside the load above) and after the WAL replay,
    // through the replay path's own redo machinery — never inside the load,
    // where the mirror's checkpointed-state comparison would read a file
    // edit as drift. Idempotent: every open re-applies the committed facts,
    // so a machine whose sidecar predates a file edit converges without a
    // checkpoint.
    if let Some(committed) = committed {
        crate::committed::apply(shared, committed)?;
    }
    // ADR-0687: the state this open installed — the WAL replay and the
    // committed schema above — is the token table's baseline, not a change:
    // no plan can predate the open, so the mutation sites' reports are
    // drained and discarded rather than read as fresh DDL by the first
    // commit's publish.
    drop(shared.graph.take_applied_token_changes());
    // Each start_* helper is the same gate `Database::promote` / `demote`
    // later reuse (ADR-0440): a writable open starts the timers, a follow
    // open starts the follow thread, and a `ReadOnly` open — writable nor
    // following — starts nothing.
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(lease) = _writer_lease {
        *shared.writer_lease.lock()? = Some(lease);
    }
    crate::resize::start_checkpoint_timer(shared)?;
    crate::resize::start_durability_timer(shared)?;
    crate::resize::start_follow_thread(shared)?;
    Ok(())
}

/// LABS-903's turn-into, one named operation: release a loaded durable
/// source and hand the store and WAL a single shared `:memory:`-shaped
/// stand-in — the same one-VFS shape `:memory:` opens with, so every
/// holder reads the same in-process store afterwards. The source (the
/// `.rl` host file or the store directory) is closed and unlocked the
/// moment it drops: nothing holds it open, and another process can open
/// it writable immediately.
fn release_durable_source(store: StoreTarget, mut wal: Wal) -> Result<(StoreTarget, Wal)> {
    let released = relate_storage::memory_store_with_codec(store.snapshot_codec())?;
    // The store and the WAL share the released target's one vfs — exactly
    // the `:memory:` shape.
    wal.release_source(released.vfs())?;
    Ok((released, wal))
}
