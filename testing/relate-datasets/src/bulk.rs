//! Bulk-load harness: one statement, one commit, at large batch sizes.
//!
//! Where the mixed-workload harness in [`crate::loadtest`] measures many small
//! ops against a generated fraud/IAM graph, this one measures whole-graph
//! bulk operations — the shape a data-loading job actually has:
//!
//! - **create** — `UNWIND range(1,$n) AS id CREATE (p:Person {name: 'p'+id, id: id, age: id%100})`
//! - **merge-with-constraint** — the same `UNWIND`, but `MERGE (p:Person {id: id})`,
//!   with `CREATE CONSTRAINT bulk_person_id FOR (p:Person) REQUIRE p.id IS UNIQUE`
//!   in place so the merge looks the id up through the owned index rather than
//!   scanning.
//! - **update** — base data first (untimed), then
//!   `UNWIND range(1,$n) AS id MATCH (p:Person {id: id}) SET p.age = p.age + 1`
//! - **delete** — base data first (untimed), then
//!   `UNWIND range(1,$n) AS id MATCH (p:Person {id: id}) DETACH DELETE p`
//!
//! Each operation is one Cypher statement and therefore one commit. Sizes
//! default to 5k / 10k / 50k / 100k; a size that is slow is the measurement,
//! not a bug to be capped away.
//!
//! # Design notes
//!
//! - **The uniqueness constraint is created for every op except `create`.**
//!   `merge-with-constraint` needs it by definition; `update` and `delete` need
//!   the same indexed `MATCH (p:Person {id: id})` to stay within a sane time
//!   budget (`Person` has 100k nodes at the top size, so an unindexed
//!   per-row label scan would be quadratic). Each entry records
//!   `constraint: true|false` so the number can never be read as unindexed.
//! - **Single-threaded.** Each batch is one statement, hence one commit, so
//!   worker concurrency would only measure the group-commit path that
//!   `crate::loadtest`'s `--workers` already covers. Determinism wins here.
//! - **Every measurement gets a fresh database** so batches never inherit the
//!   previous batch's graph, and the untimed base-data setup is timed
//!   separately (`setup_ms`) rather than folded into the reported latency.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use relate::{Database, DurabilityMode, Error, OpenOptions, Result, Value};
use relate_test_support::sampling::process_rss_bytes;
use serde::{Deserialize, Serialize};

use crate::loadtest::{scalar, unix_ms};
use crate::report::{LatencyNs, Rank};
use relate_test_support::profiler::{sample, DEFAULT_FREQUENCY};

/// Schema version of [`BulkReport`]. Bump when the JSON shape changes.
/// 2 adds `config.durability_mode` and per-entry `commit_timing`; 3 adds
/// `config.sample_dir`.
pub const BULK_REPORT_SCHEMA_VERSION: u32 = 3;

/// `create`: one statement, one commit.
pub const CREATE_CYPHER: &str =
    "UNWIND range(1,$n) AS id CREATE (p:Person {name: 'p'+id, id: id, age: id%100})";

/// `merge-with-constraint`: same `UNWIND`, merge on the constrained key.
pub const MERGE_CYPHER: &str = "UNWIND range(1,$n) AS id MERGE (p:Person {id: id})";

/// `update`: bump `age` on every node created by the untimed setup.
pub const UPDATE_CYPHER: &str =
    "UNWIND range(1,$n) AS id MATCH (p:Person {id: id}) SET p.age = p.age + 1";

/// `delete`: `DETACH DELETE` every node created by the untimed setup.
pub const DELETE_CYPHER: &str =
    "UNWIND range(1,$n) AS id MATCH (p:Person {id: id}) DETACH DELETE p";

/// The uniqueness constraint behind `merge-with-constraint`'s indexed lookup.
pub const CONSTRAINT_CYPHER: &str =
    "CREATE CONSTRAINT bulk_person_id FOR (p:Person) REQUIRE p.id IS UNIQUE";

/// Delete every node (untimed setup between measured repetitions).
pub const CLEAR_CYPHER: &str = "MATCH (n) DETACH DELETE n";

/// Batch sizes measured by default (5k / 10k / 50k / 100k).
pub const DEFAULT_SIZES: [usize; 4] = [5_000, 10_000, 50_000, 100_000];

/// One bulk operation type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BulkOp {
    Create,
    MergeWithConstraint,
    Update,
    Delete,
}

impl BulkOp {
    /// Stable id, identical to the serde name (`create` /
    /// `merge-with-constraint` / `update` / `delete`).
    pub fn id(self) -> &'static str {
        match self {
            BulkOp::Create => "create",
            BulkOp::MergeWithConstraint => "merge-with-constraint",
            BulkOp::Update => "update",
            BulkOp::Delete => "delete",
        }
    }

    /// The timed statement (`$n` = batch size).
    pub fn cypher(self) -> &'static str {
        match self {
            BulkOp::Create => CREATE_CYPHER,
            BulkOp::MergeWithConstraint => MERGE_CYPHER,
            BulkOp::Update => UPDATE_CYPHER,
            BulkOp::Delete => DELETE_CYPHER,
        }
    }

    /// `update` / `delete` / `merge-with-constraint` run against pre-created
    /// base data; `create` starts from an empty graph.
    pub fn needs_base(self) -> bool {
        !matches!(self, BulkOp::Create)
    }

    /// Whether the `Person.id` uniqueness constraint is in place for the op.
    /// `create` measures a bare append; the other three need the lookup.
    pub fn needs_constraint(self) -> bool {
        self.needs_base()
    }

    /// Every op, in report order.
    pub fn all() -> Vec<BulkOp> {
        vec![
            BulkOp::Create,
            BulkOp::MergeWithConstraint,
            BulkOp::Update,
            BulkOp::Delete,
        ]
    }

    /// Parse a CLI `--ops` element (`merge` is accepted for
    /// `merge-with-constraint`).
    pub fn parse(s: &str) -> std::result::Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "create" => Ok(BulkOp::Create),
            "merge" | "merge-with-constraint" | "merge_with_constraint" => {
                Ok(BulkOp::MergeWithConstraint)
            }
            "update" | "set" => Ok(BulkOp::Update),
            "delete" => Ok(BulkOp::Delete),
            other => Err(format!(
                "unknown op {other:?} (expected create | merge-with-constraint | update | delete)"
            )),
        }
    }
}

/// The report/CLI spelling — the same string [`BulkOp::id`] and serde use,
/// so clap can render a default list.
impl std::fmt::Display for BulkOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

/// One `--sizes` element: `50000`, `50_000`, or `50k` → 50000. Trimmed like
/// [`BulkOp::parse`], since clap's comma splitter hands the chunks through
/// raw; `0` and an out-of-range value are rejected here so clap's own error
/// carries them.
pub fn parse_size(part: &str) -> std::result::Result<usize, String> {
    let part = part.trim();
    let cleaned = part.replace('_', "");
    let (digits, mult) = match cleaned.strip_suffix(['k', 'K']) {
        Some(d) => (d, 1_000usize),
        None => (cleaned.as_str(), 1),
    };
    let n = digits
        .parse::<usize>()
        .map_err(|_| format!("invalid size {part:?}"))?
        .checked_mul(mult)
        .ok_or_else(|| format!("invalid size {part:?}"))?;
    if n == 0 {
        Err("--sizes entries must be >= 1".into())
    } else {
        Ok(n)
    }
}

/// What [`run`] measures.
#[derive(Debug, Clone)]
pub struct BulkConfig {
    /// Base directory for the per-measurement databases, or `:memory:` for a
    /// fresh in-memory database per measurement.
    pub dir: String,
    /// Batch sizes (rows per statement, hence per commit).
    pub sizes: Vec<usize>,
    /// Operation types to measure.
    pub ops: Vec<BulkOp>,
    /// Measured repetitions per (op, size). `1` makes the percentiles
    /// degenerate; more reps cost proportionally more wall clock.
    pub reps: usize,
    /// Unmeasured repetitions run first (each still gets its untimed setup).
    pub warmup: usize,
    /// Report JSON path.
    pub out: Option<PathBuf>,
    /// Live graph + index cap in bytes. `None` uses `RELATE_LOGICAL_GRAPH_SIZE_LIMIT`.
    pub logical_graph_size_limit: Option<u64>,
    /// WAL durability for persistent databases. Ignored for `:memory:`.
    pub durability_mode: DurabilityMode,
    /// Record per-commit step histograms (the `--commit-timing` flag).
    pub commit_timing: bool,
    /// Run every measurement's repetition loop under the in-process sampler
    /// and write `top.txt` / `folded.txt` / `flame.svg` per (op, size) under
    /// this directory (the `--sample` flag).
    pub sample_dir: Option<PathBuf>,
}

impl Default for BulkConfig {
    fn default() -> Self {
        Self {
            dir: "target/bulk-loadtest-data".into(),
            sizes: DEFAULT_SIZES.to_vec(),
            ops: BulkOp::all(),
            reps: 1,
            warmup: 0,
            out: None,
            logical_graph_size_limit: None,
            durability_mode: DurabilityMode::Full,
            commit_timing: false,
            sample_dir: None,
        }
    }
}

impl BulkConfig {
    /// One rep per size — the shape the CI job runs.
    pub fn ci() -> Self {
        Self {
            sizes: vec![1_000, 5_000],
            ..Self::default()
        }
    }

    fn validate(&self) -> Result<()> {
        if self.ops.is_empty() {
            return Err(Error::semantic("bulk config: no operations selected"));
        }
        if self.sizes.is_empty() {
            return Err(Error::semantic("bulk config: no batch sizes selected"));
        }
        if self.sizes.contains(&0) {
            return Err(Error::semantic("bulk config: batch sizes must be >= 1"));
        }
        if self.reps == 0 {
            return Err(Error::semantic("bulk config: --reps must be >= 1"));
        }
        if let Some(dir) = &self.sample_dir {
            // The report records the path as a string like every other
            // recorded path; a non-UTF-8 path is refused here rather than
            // silently mangled by a lossy conversion later.
            if dir.to_str().is_none() {
                return Err(Error::semantic(
                    "bulk config: --sample must be a UTF-8 path (the report records it as text)",
                ));
            }
        }
        if self.sample_dir.is_some() {
            relate_test_support::profiler::require_available().map_err(Error::semantic)?;
        }
        Ok(())
    }
}

/// One (operation, batch size) measurement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkEntry {
    /// Operation id (`create`, `merge-with-constraint`, `update`, `delete`).
    pub op: BulkOp,
    pub size: usize,
    /// Measured repetitions contributing to `latency_ns`.
    pub reps: usize,
    /// Unmeasured repetitions run first.
    pub warmup: usize,
    /// `Person.id` uniqueness constraint present during the op.
    pub constraint: bool,
    /// Statements per rep — always 1 (one statement, one commit).
    pub statements: u32,
    /// Reps whose statement returned an error.
    pub errors: usize,
    pub error: Option<String>,
    /// `size * reps` rows processed by the measured reps.
    pub rows: usize,
    /// Untimed base-data setup (clear + create) summed over all reps.
    pub setup_ms: u64,
    /// Wall clock of the measured reps only.
    pub duration_ms: u64,
    /// Measured reps per second.
    pub ops_per_sec: f64,
    /// Rows per second (`rows / duration`).
    pub rows_per_sec: f64,
    /// Per-rep batch latency. `None` when every measured rep errored.
    pub latency_ns: Option<LatencyNs>,
    /// Nodes in the graph when the timed reps started.
    pub nodes_before: i64,
    /// Nodes in the graph after the measured reps.
    pub nodes_after: i64,
    pub rss_bytes: u64,
    pub logical_used_bytes: u64,
    /// Database path used for this measurement (`:memory:` for in-memory).
    pub uri: String,
    /// The engine's rendered commit-timing report — aggregate per-step
    /// percentiles (parse+plan, overlay execute, wal_lock wait,
    /// encode+write, `sync_all`, apply_redo) over every commit in the
    /// measurement, each rep's untimed setup statements included. `None`
    /// when `--commit-timing` is off, and for an entry whose measurement
    /// failed before running ([`Self::failed`]); presence is owned by the
    /// flag, never inferred from what the engine returned. This is a rendered
    /// diagnostics dump for reading, not a schema-modeled structure: the row
    /// vocabulary is pinned by
    /// [`tests::commit_timing_flag_is_recorded_only_when_asked_for`], so an
    /// engine-side rename fails that test instead of silently reshaping
    /// reports. An in-memory database still records (its `sync_all` is ~0),
    /// which isolates engine cost from the barrier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_timing: Option<String>,
}

impl BulkEntry {
    /// An entry for a measurement that could not even be set up (open failed,
    /// base data failed, …). Keeps the report one-entry-per-(op, size).
    fn failed(cfg: &BulkConfig, op: BulkOp, size: usize, uri: String, error: String) -> Self {
        Self {
            op,
            size,
            reps: 0,
            warmup: cfg.warmup,
            constraint: op.needs_constraint(),
            statements: 1,
            errors: 1,
            error: Some(error),
            rows: 0,
            setup_ms: 0,
            duration_ms: 0,
            ops_per_sec: 0.0,
            rows_per_sec: 0.0,
            latency_ns: None,
            nodes_before: 0,
            nodes_after: 0,
            rss_bytes: 0,
            logical_used_bytes: 0,
            uri,
            commit_timing: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkReportConfig {
    pub dir: String,
    pub ops: Vec<BulkOp>,
    pub sizes: Vec<usize>,
    pub reps: usize,
    pub warmup: usize,
    #[serde(default)]
    pub logical_graph_size_limit: Option<u64>,
    /// WAL durability mode. Omitted in older reports (treat as `full`).
    #[serde(default)]
    pub durability_mode: DurabilityMode,
    /// Whether the run captured commit-timing diagnostics
    /// (`--commit-timing`). Omitted in older reports (treat as `false`).
    #[serde(default)]
    pub commit_timing: bool,
    /// Where the run wrote per-(op, size) stack profiles (`--sample`), as a
    /// string like every other recorded path. Omitted in older reports
    /// (treat as off).
    #[serde(default)]
    pub sample_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkReport {
    pub schema_version: u32,
    pub name: String,
    pub started_at: String,
    pub finished_at: String,
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: u64,
    /// Whole `run()` wall (all measurements, setup included).
    pub duration_ms: u64,
    pub engine: String,
    pub engine_version: String,
    pub config: BulkReportConfig,
    /// One entry per (operation, batch size), in `config.ops` × `config.sizes` order.
    pub results: Vec<BulkEntry>,
}

impl BulkReport {
    /// Entry for one (op, size), if the run produced it.
    pub fn entry(&self, op: BulkOp, size: usize) -> Option<&BulkEntry> {
        self.results.iter().find(|e| e.op == op && e.size == size)
    }

    /// Total measured rows across every entry.
    pub fn total_rows(&self) -> usize {
        self.results.iter().map(|e| e.rows).sum()
    }
}

/// Run every configured (operation, batch size) measurement.
pub fn run(cfg: &BulkConfig) -> Result<BulkReport> {
    crate::memory::require_sampling_available()?;
    cfg.validate()?;
    let started = Utc::now();
    let started_ms = unix_ms();
    let wall = Instant::now();

    // A bad `--sample` path must fail the run before any measurement, not
    // discard measurements as failed entries with an exit code of 0: the
    // root and every per-(op, size) profile directory are created up front.
    // Nothing is stored — the measurement loop recomputes the same
    // `measurement_key` path, so the flag and the directory can never
    // disagree.
    if let Some(root) = &cfg.sample_dir {
        fs::create_dir_all(root)?;
        for &op in &cfg.ops {
            for &size in &cfg.sizes {
                fs::create_dir_all(root.join(measurement_key(op, size)))?;
            }
        }
    }
    let mut results = Vec::with_capacity(cfg.ops.len() * cfg.sizes.len());
    for &op in &cfg.ops {
        for &size in &cfg.sizes {
            let uri = measurement_uri(&cfg.dir, op, size);
            let profile_dir = cfg
                .sample_dir
                .as_ref()
                .map(|root| root.join(measurement_key(op, size)));
            let (entry, outputs) =
                match measure(cfg, op, size, &uri) {
                    Ok((e, outputs)) => {
                        println!(
                        "  {:<21} {:>7} rows  {:>8.1} ms  {:>10.1} rows/s  p50={:.1}ms  errors={}",
                        op.id(),
                        size,
                        entry_ms(&e),
                        e.rows_per_sec,
                        e.latency_ns.as_ref().map(|l| l.p50 as f64 / 1e6).unwrap_or(0.0),
                        e.errors
                    );
                        (e, outputs)
                    }
                    Err((msg, outputs)) => {
                        eprintln!("  {:<21} {:>7} rows  FAILED: {msg}", op.id(), size);
                        (BulkEntry::failed(cfg, op, size, uri, msg), outputs)
                    }
                };
            // Profile emission is the run's artifact, like `--out`: one
            // guard for both outcomes (the fault carries whatever profile
            // the run already recorded — the failure case is the one the
            // stacks are most useful for), and a write failure fails the
            // run, not the entry.
            if let (Some(dir), Some(sampled)) = (&profile_dir, &outputs) {
                emit_profile(dir, cfg, op, size, sampled)?;
            }
            results.push(entry);
        }
    }

    let finished = Utc::now();
    let report = BulkReport {
        schema_version: BULK_REPORT_SCHEMA_VERSION,
        name: "bulk-loadtest".into(),
        started_at: started.to_rfc3339_opts(SecondsFormat::Millis, true),
        finished_at: finished.to_rfc3339_opts(SecondsFormat::Millis, true),
        started_at_unix_ms: started_ms,
        finished_at_unix_ms: unix_ms(),
        duration_ms: wall.elapsed().as_millis() as u64,
        engine: "relate".into(),
        engine_version: env!("CARGO_PKG_VERSION").into(),
        config: BulkReportConfig {
            dir: cfg.dir.clone(),
            ops: cfg.ops.clone(),
            sizes: cfg.sizes.clone(),
            reps: cfg.reps,
            warmup: cfg.warmup,
            logical_graph_size_limit: cfg.logical_graph_size_limit,
            durability_mode: cfg.durability_mode,
            commit_timing: cfg.commit_timing,
            sample_dir: cfg
                .sample_dir
                .as_deref()
                .and_then(Path::to_str)
                .map(String::from),
        },
        results,
    };
    if let Some(path) = &cfg.out {
        write_report(path, &report)?;
    }
    Ok(report)
}

fn entry_ms(e: &BulkEntry) -> f64 {
    e.duration_ms as f64
}

/// The per-(op, size) key every per-measurement path is built from — the
/// database subdirectory and the `--sample` profile directory stay in step.
fn measurement_key(op: BulkOp, size: usize) -> String {
    format!("{}-{size}", op.id())
}

/// Per-measurement database URI: a fresh subdirectory per (op, size) so no
/// measurement inherits another's graph. `:memory:` stays in memory.
pub fn measurement_uri(base: &str, op: BulkOp, size: usize) -> String {
    let base = base.trim_end_matches('/');
    if base.is_empty() || base == ":memory:" {
        ":memory:".into()
    } else {
        format!("{base}/{}", measurement_key(op, size))
    }
}

/// What one measurement's repetition loop accumulated. Exists so the loop
/// can run inside the sampler's closure (or straight, when `--sample` is
/// off) without threading six locals through it.
struct Reps {
    setup_ms: u64,
    timed_ns: u64,
    latencies: Vec<u64>,
    errors: usize,
    first_error: Option<String>,
    nodes_before: i64,
}

/// The repetition loop: per rep, the untimed setup (clear + base data)
/// then the one timed statement. Note that under `--sample` the sampler
/// sees the setup statements too — the folded stacks show both shapes.
fn rep_loop(
    conn: &mut relate::Connection,
    cfg: &BulkConfig,
    op: BulkOp,
    params: &BTreeMap<String, Value>,
) -> Result<Reps> {
    let mut reps = Reps {
        setup_ms: 0,
        timed_ns: 0,
        latencies: Vec::new(),
        errors: 0,
        first_error: None,
        nodes_before: 0,
    };
    for rep in 0..(cfg.warmup + cfg.reps) {
        // Untimed: every repetition starts from the same starting graph.
        let t_setup = Instant::now();
        conn.query(CLEAR_CYPHER)?;
        if op.needs_base() {
            conn.query_with_params(CREATE_CYPHER, params.clone())?;
        }
        reps.setup_ms += t_setup.elapsed().as_millis() as u64;
        reps.nodes_before = scalar(conn, "MATCH (n) RETURN count(n) AS n")?;

        let t0 = Instant::now();
        let outcome = conn.query_with_params(op.cypher(), params.clone());
        let dt = t0.elapsed().as_nanos() as u64;
        if rep >= cfg.warmup {
            reps.timed_ns += dt;
            match outcome {
                Ok(_) => reps.latencies.push(dt),
                Err(e) => {
                    reps.errors += 1;
                    if reps.first_error.is_none() {
                        reps.first_error = Some(e.to_string());
                    }
                }
            }
        }
    }
    Ok(reps)
}

/// Emit one measurement's stack profile under `dir`. Emission is the
/// run's, like `--out`'s report: a write failure fails the run instead of
/// becoming a failed entry.
fn emit_profile(
    dir: &Path,
    cfg: &BulkConfig,
    op: BulkOp,
    size: usize,
    sampled: &relate_test_support::profiler::Sampled<()>,
) -> Result<()> {
    let header = vec![
        format!(
            "bulk-loadtest {} size={} reps={} warmup={} durability={}",
            op.id(),
            size,
            cfg.reps,
            cfg.warmup,
            cfg.durability_mode
        ),
        format!("{} samples", sampled.stacks.total()),
    ];
    relate_test_support::profiler::write_stack_outputs(
        dir,
        &sampled.stacks,
        &sampled.flamegraph_svg,
        &header,
    )
    .map_err(Error::internal)
}

/// A measurement's failure: the message, and the profile the run already
/// recorded before failing (the rounds ran under the profiler, so a
/// post-loop failure keeps its stacks — the case they are most useful for).
type MeasureFault = (String, Option<relate_test_support::profiler::Sampled<()>>);

/// Open the measurement's fresh store: reset the URI, open with the
/// measurement's options (checkpoint timer off), and connect. The one
/// place the setup-fault adapter (`None` — no profile exists yet) is
/// written.
fn open_measurement(
    cfg: &BulkConfig,
    uri: &str,
) -> std::result::Result<(Database, relate::Connection), String> {
    if uri != ":memory:" {
        if let Some(parent) = Path::new(uri).parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
        }
        relate_storage::reset_uri(uri, None).map_err(|e| e.to_string())?;
    }
    let db = Database::open_with(
        uri,
        OpenOptions {
            // The checkpoint timer would otherwise fire mid-measurement.
            checkpoint_interval: Some(Duration::ZERO),
            logical_graph_size_limit: cfg.logical_graph_size_limit,
            durability_mode: Some(cfg.durability_mode),
            ..OpenOptions::default()
        },
    )
    .map_err(|e| e.to_string())?;
    let conn = db.connect().map_err(|e| e.to_string())?;
    Ok((db, conn))
}

/// Run one (op, size) measurement. The entry and, when `--sample` is on,
/// the profile it produced (`Sampled`'s stacks and flame graph, the shared
/// kit's own name for the pair) — emission is [`run`]'s, so a profile
/// write failure fails the run instead of becoming a failed entry. A
/// failed measurement still returns its profile alongside a failed entry:
/// the rounds already ran under the profiler.
fn measure(
    cfg: &BulkConfig,
    op: BulkOp,
    size: usize,
    uri: &str,
) -> std::result::Result<
    (
        BulkEntry,
        Option<relate_test_support::profiler::Sampled<()>>,
    ),
    MeasureFault,
> {
    let (db, mut conn) = open_measurement(cfg, uri).map_err(|e| (e, None))?;
    if cfg.commit_timing {
        db.enable_commit_timing();
    }

    if op.needs_constraint() {
        conn.query(CONSTRAINT_CYPHER)
            .map_err(|e| (e.to_string(), None))?;
    }
    let params: BTreeMap<String, Value> =
        BTreeMap::from([("n".to_string(), Value::Int(size as i64))]);

    // The repetition loop, straight or under the in-process profiler. A
    // failed loop still yields its profile — the rounds already ran under
    // the profiler, and the failure case is the one the stacks are most
    // useful for — so the profile is taken out before the error propagates.
    let (reps, outputs) = match &cfg.sample_dir {
        Some(_) => {
            let sampled = sample(DEFAULT_FREQUENCY, || rep_loop(&mut conn, cfg, op, &params))
                .map_err(|e| (e, None))?;
            let profile = Some(relate_test_support::profiler::Sampled {
                value: (),
                stacks: sampled.stacks,
                flamegraph_svg: sampled.flamegraph_svg,
            });
            match sampled.value {
                Ok(reps) => (reps, profile),
                Err(msg) => return Err((msg.to_string(), profile)),
            }
        }
        None => (
            rep_loop(&mut conn, cfg, op, &params).map_err(|e| (e.to_string(), None))?,
            None,
        ),
    };

    // The fallible post-loop reads keep the profile on failure: it is
    // moved into the fault only on that arm, so the success path still
    // owns it for the return.
    let post = (|| -> Result<(i64, u64)> {
        let nodes_after = scalar(&mut conn, "MATCH (n) RETURN count(n) AS n")?;
        let (logical_used_bytes, _) = db.memory_usage()?;
        Ok((nodes_after, logical_used_bytes))
    })();
    let (nodes_after, logical_used_bytes) = match post {
        Ok(v) => v,
        Err(e) => return Err((e.to_string(), outputs)),
    };
    let secs = reps.timed_ns as f64 / 1e9;
    let ops = reps.latencies.len();
    let rows = ops * size;
    // Presence is owned by the flag, never inferred from what the engine
    // returned: the env-seeded engine switch must not leak a `Some` into a
    // flag-off run, and an engine that produced nothing despite the flag
    // stays visible as an empty dump rather than reading as "off".
    let timing = cfg.commit_timing.then(|| db.commit_timing_report());

    Ok((
        BulkEntry {
            op,
            size,
            reps: cfg.reps,
            warmup: cfg.warmup,
            constraint: op.needs_constraint(),
            statements: 1,
            errors: reps.errors,
            error: reps.first_error,
            rows,
            setup_ms: reps.setup_ms,
            duration_ms: reps.timed_ns / 1_000_000,
            ops_per_sec: if secs > 0.0 { ops as f64 / secs } else { 0.0 },
            rows_per_sec: if secs > 0.0 { rows as f64 / secs } else { 0.0 },
            latency_ns: LatencyNs::from_samples(&reps.latencies, Rank::Canonical),
            nodes_before: reps.nodes_before,
            nodes_after,
            rss_bytes: process_rss_bytes()
                .expect("RSS sampling availability is checked at run start"),
            logical_used_bytes,
            uri: uri.to_string(),
            commit_timing: timing,
        },
        outputs,
    ))
}

pub fn write_report(path: &Path, report: &BulkReport) -> Result<()> {
    crate::report::write_report(path, report).map_err(relate::Error::storage)
}

/// Convenience runner used by the binary and the integration test.
pub fn run_bulk(cfg: &BulkConfig) -> Result<BulkReport> {
    run(cfg)
}

#[cfg(test)]
mod tests;
