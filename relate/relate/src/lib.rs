//! Embedded Relate: OpenCypher on an in-memory property graph.

mod binding;
mod checkpoint;
mod clone;
mod commit;
mod commit_timing;
mod connection;
mod engines;
mod explain;
mod follow;
mod gate;
mod notifications;
mod ontology;
mod open;
mod open_config;
mod phase_timing;
mod plan_bytes;
mod plan_cache;
mod plan_node;
mod procedures;
mod profile;
mod publish;
mod query_worker;
mod resize;
mod result;
mod shared;
mod statement_notifications;
mod stats;
#[doc(hidden)]
pub mod test_opts;
#[cfg(test)]
mod tests;
mod token_budget;

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

pub use plan_node::{ClauseMetrics, PlanArtifacts, PlanNode, PlanWarning};
pub use relate_core::{
    node_element_id, parse_node_element_id, parse_rel_element_id, rel_element_id,
};
pub use relate_storage::{
    cypher_to_value, value_to_cypher, MarkdownBodyProperties, SnapshotCodecKind, WalRecord,
};

pub use binding::{
    binding_error_metadata, execute_wire, params_from_json, params_from_map, params_from_value,
    plan_wire_json, query_result_to_cypher_rows, query_result_to_wire, query_stats_to_wire,
    run_script_wire, timing_stamp_ms,
};
pub use clone::CloneOptions;
pub use connection::{Connection, Executed, ExecutionFailure};
/// A cooperative cancellation flag for a running query (ADR-0111).
///
/// A transport (or any caller) holds a handle and calls [`CancelToken::cancel`]
/// while the query runs on another thread; the executor checks it at
/// row-production points and in long loops. A cancelled query returns
/// `Error::Cancelled`, whose message is `"query cancelled"`, and the
/// connection it ran on remains usable. Cloning shares the same flag.
///
/// ```
/// use relate::CancelToken;
///
/// let token = CancelToken::new();
/// assert!(!token.is_cancelled());
/// let transport = token.clone();
/// transport.cancel();
/// assert!(token.is_cancelled());
/// ```
pub use relate_core::CancelToken;
pub use relate_core::Namespace;
pub use relate_core::{
    next_depth, parse_expression_memory_limit, parse_memory_limit, parse_positive_memory_limit,
    require_positive_memory_limit, uuid_hyphenated, uuid_parse, value_depth_exceeded,
    ArithmeticFailure, CapacityKind, Error, Node, Path, PropertyMap, Relationship, Result,
    Temporal, Value, MAX_VALUE_DEPTH,
};
pub use relate_core::{
    require_traversal_depth, traversal_depth_exceeded_message, MAX_TRAVERSAL_DEPTH,
    MAX_TRAVERSAL_DEPTH_CEILING,
};
pub use relate_core::{ExecutionReceipt, TokenEstimate, TokenOutcome};
/// The engine's one error classification (ADR-0502 §2): the Neo4j status
/// code, the GQLSTATUS, the compile/runtime/internal stage (ADR-0319) and
/// the drivers' transient test for a query-engine [`Error`] — the single
/// source Bolt's FAILURE rendering, HTTP's error envelope, the CLI's
/// exit-code classifier, Python and the C ABI derive from, never a second
/// table.
pub use relate_error::{
    client_message, error_info, log_internal_original, ErrorInfo, ErrorStage, INTERNAL_ERROR_INFO,
    INTERNAL_MESSAGE,
};
pub use relate_executor::{
    FrontierCounters, FrontierDecline, FrontierDeclineReason, FrontierRunDecline, KernelRung,
    PatternEngine, PhaseTimes, Procedure, PullKind, QueryStats, StartReason, StatementEffect,
    StoreReads, WalkStart,
};
// Research-only (speed-of-light kernels), quarantined behind the
// non-default `research` cargo feature (ADR-0476): this re-export and the
// `sol_graph` method on `Database` below compile only under it.
#[cfg(feature = "research")]
#[doc(hidden)]
pub use relate_executor::sol;
// The research-only heap-census harness's result type (same quarantine):
// named by the `heap_census` example through this crate.
#[cfg(feature = "research")]
#[doc(hidden)]
pub use relate_executor::HeapCensus;
// The ADR-0463 default per-allocation expression cap, for the surfaces that
// render or calibrate against it (`serve --help`, the budget fixtures).
pub use open_config::{
    durability_mode_from_setting, expression_limit_from_setting, reject_cypherlite_env,
    workspace_limit_from_setting,
};
/// The inline fast path's panic guard, visible to the crate's own
/// containment proof (`tests/stack.rs`) so the test drives the function
/// production drives (ADR-0504) — test-only, never part of the public
/// surface.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use query_worker::catch_inline_panic;
pub use query_worker::Heartbeat;
#[cfg(not(target_arch = "wasm32"))]
pub use query_worker::QUERY_WORKER_STACK_BYTES;
#[doc(hidden)]
pub use relate_executor::EXPRESSION_BUDGET_BYTES;
// The ADR-0395 re-arrival work budget constant, for the PROFILE fixtures
// that calibrate the frontier budget fraction against it (the fraction's
// single-sourced denominator, never re-spelled by a consumer).
#[doc(hidden)]
pub use relate_executor::REARRIVAL_WORK_BUDGET;
/// The query-notification record and vocabulary (ADR-0363), with the three
/// wire serializers re-exported beside it so a consumer imports one crate:
/// [`notification_to_wire_json`], [`notifications_to_wire_json`] and
/// [`notification_to_neo4j_json`]. They remain defined in
/// `relate_model::wire`, the one wire-encoding module every surface
/// shares.
pub use relate_model::wire::{
    notification_to_neo4j_json, notification_to_wire_json, notifications_to_wire_json,
    Notification, NotificationCategory, NotificationKind, NotificationSeverity,
};
pub use relate_planner::QueryType;
// LABS-1120: the plan-cache outcome rides the result header — the one
// public path for the type (its home module is crate-private).
pub use plan_cache::PlanCacheOutcome;
pub use result::{QueryResult, ResultHeader};
#[cfg(test)]
pub(crate) use shared::ReadPause;
pub(crate) use shared::{check_loaded_graph, ensure_writable, report_trimmed_tails, Shared};
pub use token_budget::TokenPoolSnapshot;

use checkpoint::checkpoint_now;
use commit::{format_commit_timing, CommitTimingBuf};
/// Open handle to a Relate database.
pub struct Database {
    inner: Arc<Shared>,
}

impl Drop for Database {
    fn drop(&mut self) {
        // Join before this `Arc` drops. A one-shot checkpoint keeps its own
        // `Arc` until it exits, and a timer that upgraded its `Weak` still
        // owns the store lock until then. Windows enforces that lock per
        // handle, so a reopen in this process has to wait here.
        self.inner.stop_background();
    }
}

/// Stop flag shared by the checkpoint, durability, and follow threads.
pub(crate) struct ThreadStop {
    pub(crate) stop: Mutex<bool>,
    pub(crate) cv: Condvar,
}

/// Park until `interval` elapses or `ctl` is signaled, then run `tick`.
/// wasm has no background threads, so this returns `Ok(None)` there.
pub(crate) fn spawn_interval_worker(
    name: &'static str,
    weak: Weak<Shared>,
    ctl: Arc<ThreadStop>,
    interval: Duration,
    tick: impl Fn(&Shared) + Send + 'static,
) -> Result<Option<JoinHandle<()>>> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (name, weak, ctl, interval, tick);
        Ok(None)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || loop {
                let Ok(stop) = ctl.stop.lock() else {
                    break;
                };
                if *stop {
                    break;
                }
                let (guard, _) = match ctl.cv.wait_timeout(stop, interval) {
                    Ok(waited) => waited,
                    Err(err) => err.into_inner(),
                };
                if *guard {
                    break;
                }
                drop(guard);
                let Some(shared) = weak.upgrade() else {
                    break;
                };
                tick(&shared);
            })
            .map(Some)
            .map_err(|err| Error::internal(format!("failed to spawn {name}: {err}")))
    }
}

impl ThreadStop {
    pub(crate) fn new() -> Self {
        Self {
            stop: Mutex::new(false),
            cv: Condvar::new(),
        }
    }

    pub(crate) fn signal(&self) {
        if let Ok(mut stop) = self.stop.lock() {
            *stop = true;
            self.cv.notify_all();
        }
    }
}

/// Seconds between timed checkpoints when the WAL has work (ADR-0041).
pub const DEFAULT_CHECKPOINT_INTERVAL_SECS: u64 = 300;

/// When an acknowledged write becomes durable on persistent storage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DurabilityMode {
    /// Barrier the WAL before acknowledging each group commit.
    #[default]
    Full,
    /// Acknowledge after apply and barrier dirty WAL data about once per second.
    EverySecond,
    /// Acknowledge after apply; only completed checkpoints are durable.
    Snapshot,
}

impl std::fmt::Display for DurabilityMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Full => "full",
            Self::EverySecond => "everysecond",
            Self::Snapshot => "snapshot",
        })
    }
}

impl FromStr for DurabilityMode {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "full" => Ok(Self::Full),
            "everysecond" => Ok(Self::EverySecond),
            "snapshot" => Ok(Self::Snapshot),
            other => Err(Error::semantic(format!(
                "invalid durability mode {other:?}; expected full, everysecond, or snapshot"
            ))),
        }
    }
}

/// Options for [`Database::open_with`].
#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    /// Local WAL directory. Ignored for `:memory:`. Filesystem URIs default
    /// to `<dir>/wal`.
    pub wal_dir: Option<PathBuf>,
    /// Timer period. `None` uses `RELATE_CHECKPOINT_INTERVAL_SECS` (default
    /// 300). `Some(Duration::ZERO)` disables the timer.
    pub checkpoint_interval: Option<Duration>,
    /// Commit durability policy (ADR-0077, ADR-0470). `Some(mode)` wins;
    /// `None` uses `RELATE_DURABILITY_MODE`
    /// (`full`/`everysecond`/`snapshot`), then [`DurabilityMode::default`]
    /// (`Full`). A set variable value that does not parse fails open with
    /// the same semantic error as a bad `--durability-mode` value.
    pub durability_mode: Option<DurabilityMode>,
    /// Live graph + index cap in bytes (ADR-0042). `Some(0)` is unlimited.
    /// `None` uses `RELATE_LOGICAL_GRAPH_SIZE_LIMIT` (`SIZE`: bytes or `mb`/`mib`/`gb`/`gib`)
    /// or unlimited when that variable is unset. A set value that does not
    /// parse fails open with the same semantic error as a bad `--logical-graph-size-limit`.
    pub logical_graph_size_limit: Option<u64>,
    /// Concurrent logical-workspace pool in bytes (ADR-0452). A positive
    /// value turns admission on at that capacity and does not read the
    /// environment. `None` reads `RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT`
    /// (`SIZE`: bytes or `mb`/`mib`/`gb`/`gib`); unset or blank leaves
    /// admission off. `Some(0)`, and a zero or unparseable environment
    /// value, fail open. A set `RELATE_TOKEN_ADMISSION` fails open:
    /// the limit is the switch.
    pub concurrent_logical_workspace_limit: Option<u64>,
    /// The largest explicit traversal hop bound a statement may ask for
    /// (LABS-610, ADR-0466): `*1..N`, `*..N` and a CIP-28 quantifier
    /// `{1,N}` above this fail at admission with `TraversalDepthExceeded`,
    /// before the walker runs. Valid values are 1 through
    /// [`MAX_TRAVERSAL_DEPTH_CEILING`] (2000); 0 and larger values fail
    /// open. `None` uses `RELATE_MAX_TRAVERSAL_DEPTH` (default
    /// [`MAX_TRAVERSAL_DEPTH`], 500). Raising it is a deliberate act: the
    /// recursive walker risks a stack overflow on small-stack platforms
    /// above the ceiling.
    pub max_traversal_depth: Option<u32>,

    /// Per-allocation expression cap in bytes (ADR-0463). One value an
    /// expression builds may not exceed it. `Some(0)` disables the cap
    /// (the explicit `none` opt-out for known bulk pipelines). `None` uses
    /// `RELATE_EXPRESSION_MEMORY_LIMIT` (`SIZE`: bytes or
    /// `mb`/`mib`/`gb`/`gib`, or `none`) and otherwise the engine default
    /// ([`relate_executor::EXPRESSION_BUDGET_BYTES`]). A set value that
    /// does not parse fails open with a semantic error, like the other
    /// memory-limit knobs.
    pub expression_memory_limit: Option<u64>,
    /// Opt-in safe mode (LABS-680, ADR-0477): refuse agent-typical
    /// dangerous statement shapes — a new named node without a label, and a
    /// `MATCH` read without `LIMIT` or a reducing aggregate (the exemption is
    /// per item: any `collect()` item refuses even beside a reducing
    /// aggregate). Default `false`; never auto-detected. A policy, not a
    /// database limit.
    pub safe_mode: bool,
    /// Snapshot codec for an empty URI (ADR-0050). `None` uses
    /// `RELATE_SNAPSHOT_FORMAT` or the build's default,
    /// [`SnapshotCodecKind::default`] (Parquet with the `parquet` feature,
    /// JSON otherwise; ADR-0105). Existing directories use the envelope; a
    /// mismatch is an error.
    pub snapshot_codec: Option<relate_storage::SnapshotCodecKind>,
    /// The markdown vault's body-property configuration (ADR-0460):
    /// which properties render as the node file's body — the list's
    /// first entry bare, each subsequent one as a `## <property>`
    /// section. `None` — the default — uses
    /// `RELATE_MARKDOWN_BODY_PROPERTIES` or the sidecar manifest's
    /// recorded configuration; the unconfigured form is ADR-0406's
    /// single `body` property. A set configuration on a store whose
    /// codec is not markdown fails the open instead of being ignored.
    pub markdown_body_properties: Option<relate_storage::MarkdownBodyProperties>,
    /// Existing directory allowed for local LOAD CSV (ADR-0081). `None` uses
    /// `RELATE_LOAD_CSV_DIR` or denies local files.
    pub load_csv_dir: Option<PathBuf>,
    /// Maximum bytes read per CSV source. `None` defaults to 8 MiB; valid
    /// overrides are 1 through 64 MiB. Does not enable file access by itself.
    pub load_csv_max_bytes: Option<u64>,
    /// Writable, frozen read-only, a read-only WAL follower, or the
    /// ephemeral turn-into (ADR-0136, ADR-0137, ADR-0500).
    /// [`OpenMode::Follow`] and [`OpenMode::Ephemeral`] are read-only.
    /// Follow is refused for `:memory:` and `.rl`.
    pub mode: OpenMode,
    /// What a follower does when a checkpoint or trim removes WAL records
    /// it has not applied (ADR-0441). [`FollowGapPolicy::Reload`] loads a
    /// replacement graph beside the live one. [`FollowGapPolicy::Yield`]
    /// latches [`Error::FOLLOW_GAP`] and leaves the new graph to the host.
    pub follow_gap: FollowGapPolicy,
}

/// How a WAL follower recovers when it cannot keep applying (ADR-0441).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FollowGapPolicy {
    /// Load the snapshot and surviving WAL beside the installed graph, then
    /// swap (ADR-0137).
    #[default]
    Reload,
    /// Latch a retryable `follow-gap` failure and stop the follow loop.
    /// The loaded graph stays; [`Database::needs_reopen`] reports the latch.
    Yield,
}

/// How [`Database::open_with`] attaches to a store.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OpenMode {
    /// The writable owner. Checkpoints, appends, and takes the writer lease.
    #[default]
    Writable,
    /// Load once and refuse every mutation (ADR-0136). A native directory
    /// can be open beside the writable owner. A `.rl` bundle can be open
    /// only while no writer holds it. Does not create a missing store.
    ReadOnly,
    /// Read-only, then tail the native WAL. A gap reloads or yields according
    /// to [`OpenOptions::follow_gap`] (ADR-0137, ADR-0441).
    Follow,
    /// **Turn the store into the in-memory store** (LABS-903, ADR-0500): a
    /// read-only open that loads the snapshot and replays the WAL as usual,
    /// then releases the source — nothing is held open or locked, the
    /// source can be deleted or opened writable underneath, and the loaded
    /// copy is frozen snapshot freshness. [`Database::open_ephemeral`] is
    /// the convenience constructor. On `:memory:` the release is a no-op
    /// (the store is already the in-memory store).
    Ephemeral,
}

impl OpenMode {
    /// Frozen reader, follower, or the ephemeral turn-into.
    pub fn is_read_only(self) -> bool {
        !matches!(self, Self::Writable)
    }

    /// WAL follower.
    pub fn follows(self) -> bool {
        matches!(self, Self::Follow)
    }
}

impl OpenOptions {
    /// Default options with `mode` set. [`OpenMode::Writable`] is [`Default`].
    pub fn with_mode(mode: OpenMode) -> Self {
        Self {
            mode,
            ..Default::default()
        }
    }
}

impl Database {
    pub fn connect(&self) -> Result<Connection> {
        self.connect_typed(Namespace::Neo4j)
    }

    /// Connect with a typed session namespace (ADR-0117 §2, ADR-0150): no
    /// name parsing — a typed [`Namespace`] always exists, so this cannot
    /// fail with `DatabaseNotFound`.
    ///
    /// ```
    /// # use relate::{Database, Namespace};
    /// let db = Database::open(":memory:").unwrap();
    /// let conn = db.connect_typed(Namespace::SystemOntologies).unwrap();
    /// assert_eq!(conn.namespace(), Namespace::SystemOntologies);
    /// ```
    pub fn connect_typed(&self, namespace: Namespace) -> Result<Connection> {
        self.inner.check_healthy()?;
        Ok(connection::fresh(Arc::clone(&self.inner), namespace))
    }

    /// Connect with a session namespace (ADR-0117 §2): `neo4j`, `system` or
    /// `system_ontologies` (ADR-0116 §1). Names match case-insensitively;
    /// any other name fails with `DatabaseNotFound` and CE's live text
    /// `Graph not found: <name>` (ADR-0146).
    ///
    /// ```
    /// # use relate::{Database, Namespace};
    /// let db = Database::open(":memory:").unwrap();
    /// let conn = db.connect_namespace("SYSTEM").unwrap();
    /// assert_eq!(conn.namespace(), Namespace::System);
    /// assert!(db.connect_namespace("movies").is_err());
    /// ```
    pub fn connect_namespace(&self, name: &str) -> Result<Connection> {
        let namespace = connection::parse_session_namespace(name)?;
        self.connect_typed(namespace)
    }

    pub fn checkpoint(&self) -> Result<()> {
        ensure_writable(&self.inner)?;
        checkpoint_now(&self.inner)
    }

    /// Research-only (speed-of-light kernels), quarantined behind the
    /// non-default `research` cargo feature (ADR-0476): the `neo4j`
    /// namespace's live graph, for the hand-written kernels in
    /// [`sol`](crate::sol). The handle must be quiescent (no concurrent
    /// writes) while the caller holds the reference. Present only when
    /// this crate is built with `--features research`.
    #[cfg(feature = "research")]
    #[doc(hidden)]
    pub fn sol_graph(&self) -> &relate_executor::Graph {
        &self.inner.graph
    }

    /// Checkpoint when this handle can write. A read-only or follow handle
    /// succeeds without writing (ADR-0136). [`Self::checkpoint`] still
    /// returns the read-only error.
    pub fn checkpoint_if_writable(&self) -> Result<()> {
        if self.is_read_only() {
            Ok(())
        } else {
            self.checkpoint()
        }
    }

    /// Bytes of the latest Parquet snapshot (`manifest.json` + parts). WAL excluded.
    pub fn snapshot_data_bytes(&self) -> Result<u64> {
        let _read = self.inner.read_guard()?;
        relate_storage::snapshot_data_bytes(&self.inner.store)
    }

    /// Live graph + index logical bytes and the configured cap (ADR-0042).
    /// `None` limit means unlimited.
    ///
    /// A poisoned/fenced handle is a typed error rather than an apparent
    /// empty, unlimited graph (ADR-0099).
    pub fn memory_usage(&self) -> Result<(u64, Option<u64>)> {
        let _read = self.inner.read_guard()?;
        let used = self.inner.total_used_bytes();
        let limit = self.inner.resolved.logical_graph_size_limit;
        Ok((used, if limit == 0 { None } else { Some(limit) }))
    }

    /// Tokens reserved in the process pool. `None` when admission is off.
    /// This does not take the publication guard: the counter is atomic, and
    /// a sampler must be able to read it while the handle is busy.
    pub fn token_pool(&self) -> Option<TokenPoolSnapshot> {
        self.inner.resolved.budget.snapshot()
    }

    /// Invoke `hook` on the query thread when a reservation crosses from the
    /// admission ceiling into the tail. One call per crossing. Admission off
    /// leaves the hook unset.
    pub fn on_token_ceiling(&self, hook: impl Fn(TokenPoolSnapshot) + Send + Sync + 'static) {
        self.inner.resolved.budget.on_ceiling(hook);
    }

    /// Register a TCK / user procedure visible to subsequent queries.
    pub fn register_procedure(&self, proc: Procedure) -> Result<()> {
        let _read = self.inner.read_guard()?;
        self.inner.graph.register_procedure(proc);
        Ok(())
    }

    #[doc(hidden)]
    pub fn enable_commit_timing(&self) {
        self.inner.timing_on.store(true, Ordering::Relaxed);
    }

    /// Drop samples so graph load is not mixed with a later workload.
    /// No-op unless `RELATE_COMMIT_TIMING=1`.
    #[doc(hidden)]
    pub fn clear_commit_timing(&self) {
        if let Ok(mut buf) = self.inner.timing.lock() {
            *buf = CommitTimingBuf::default();
        }
    }

    /// Histogram of commit steps (encode+write, `sync_all`, `apply_redo`, lock waits).
    /// Empty unless `RELATE_COMMIT_TIMING=1`.
    #[doc(hidden)]
    pub fn commit_timing_report(&self) -> String {
        self.inner.timing_dumped.store(true, Ordering::Relaxed);
        match self.inner.timing.lock() {
            Ok(buf) => format_commit_timing(&buf),
            Err(_) => String::new(),
        }
    }

    /// Whether this handle is read-only right now. A `ReadOnly` open
    /// always is. A `Follow` or `Writable` open is, after [`Self::demote`],
    /// until [`Self::promote`] takes the writer lease again (ADR-0440).
    pub fn is_read_only(&self) -> bool {
        !self.inner.writable.load(Ordering::Acquire)
    }

    /// Whether this handle was opened [`OpenMode::Follow`] (ADR-0440).
    /// A `Writable` open is already the writer and `promote` on it is a
    /// no-op; after `demote` that same handle can `promote` again. A
    /// `ReadOnly` open can never become writable.
    pub fn is_follower(&self) -> bool {
        self.inner.resolved.opened_as_follow
    }

    /// Whether this handle was opened in safe mode (LABS-680, ADR-0477):
    /// the opt-in policy that refuses agent-typical dangerous statement
    /// shapes. Fixed for the life of the handle, like the open mode. Never
    /// auto-detected; `false` unless `OpenOptions::safe_mode` (or the CLI's
    /// `--safe` / `RELATE_SAFE_MODE=1`) asked for it.
    pub fn is_safe_mode(&self) -> bool {
        self.inner.resolved.safe_mode
    }

    /// Whether a [`FollowGapPolicy::Yield`] follower has hit a gap or a torn
    /// apply and is waiting for the host to drop this engine and open
    /// another (ADR-0441). New queries fail with a retryable
    /// [`Error::FOLLOW_GAP`] until that happens. [`FollowGapPolicy::Reload`]
    /// never sets this: it installs the replacement graph itself.
    pub fn needs_reopen(&self) -> bool {
        self.inner.needs_reopen()
    }

    /// Drop without checkpointing (simulates a crash after WAL fsync).
    pub fn leak_without_checkpoint(self) {
        self.inner.persist_on_drop.store(false, Ordering::Release);
        // Keep the WAL until the last Connection is gone, but never leak the
        // ownership lease indefinitely in this crash-simulation hook.
        drop(self);
    }

    /// Replica write path: validate redo IDs and complete named label-existence
    /// enforcement metadata, append to this WAL, then `apply_redo`. Assigns a
    /// new LSN; keeps the source `commit_ts` and graph IDs. Created IDs and
    /// relationship endpoints outside `0..i64::MAX` return `Error::Storage`
    /// without appending.
    pub fn apply_replicated(&self, record: &WalRecord) -> Result<u64> {
        commit::commit_replicated(&self.inner, record)
    }
}
