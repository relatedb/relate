//! Shared load-test harness: one runner parameterized over a small dataset
//! hook ([`LoadtestDataset`]: generate / load / snapshot scalars / catalog /
//! plan / exec). Fraud and IAM identity supply only their catalog table and
//! exec arms in [`fraud`] and [`identity`]; each catalog row carries the
//! planning function for its use case, so there is no id-keyed dispatch.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use chrono::{SecondsFormat, Utc};
use rand::rngs::StdRng;
use rand::seq::IndexedRandom;
use rand::Rng;
use relate::{
    Connection, Database, DurabilityMode, Error, OpenOptions, QueryResult, Result, Value,
};
use serde::{Deserialize, Serialize};

use crate::harness::scalar;
use crate::report::{LatencyNs, Rank};

mod args;
pub mod fraud;
pub mod identity;
mod memory;
mod report;

pub use args::{parse_cli_size, CommonArgs, StorageArgs};
pub use fraud::{FraudKnobsArgs, FraudLoadtest};
pub use identity::{IdentityKnobsArgs, IdentityLoadtest};
pub use memory::{
    build_memory_report, sample_rss, GraphMemory, MemoryPoint, MemoryReport, MemoryWorkloadDelta,
};
pub use report::{
    throughput_from, workload_section, DatasetSizes, GraphSnapshot, LoadtestReport, OpKind,
    OpRecord, Phase, ReportConfig, Throughput, UsecaseSummary, WorkloadSection,
    REPORT_SCHEMA_VERSION,
};

/// `--write-fraction` must be in `[0.0, 1.0]`.
pub fn validate_write_fraction(write_fraction: f64) -> Result<f64> {
    if !(0.0..=1.0).contains(&write_fraction) {
        return Err(Error::semantic(format!(
            "--write-fraction must be between 0.0 and 1.0 (got {write_fraction})"
        )));
    }
    Ok(write_fraction)
}

/// Dataset-specific generator knobs carried by [`LoadtestConfig`].
///
/// Serializes untagged: `{"fanout_k":…,"cycle_len":…}` or
/// `{"nest_depth":…,"group_k":…}`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Knobs {
    Fraud { fanout_k: usize, cycle_len: usize },
    Identity { nest_depth: usize, group_k: usize },
}

impl Default for Knobs {
    fn default() -> Self {
        Knobs::Fraud {
            fanout_k: 12,
            cycle_len: 4,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadtestConfig {
    pub uri: String,
    pub scale: f64,
    pub graph_seed: u64,
    pub workload_seed: u64,
    /// Workload-specific generator knobs (fraud fanout/cycle, identity nest/group).
    pub knobs: Knobs,
    /// Measured operations after warmup.
    pub ops: usize,
    pub warmup: usize,
    pub out: Option<PathBuf>,
    pub wal_dir: Option<PathBuf>,
    /// Delete the database at `uri` (and `wal_dir` if set) before opening.
    pub reset: bool,
    /// Concurrent connections issuing ops (`1` = serial).
    pub workers: usize,
    /// Fraction of ops that are writes (`0.0` = reads only, `1.0` = writes only).
    pub write_fraction: f64,
    /// Live graph + index cap in bytes. `None` uses `RELATE_LOGICAL_GRAPH_SIZE_LIMIT` or unlimited.
    pub logical_graph_size_limit: Option<u64>,
    /// WAL durability policy. Ignored for `:memory:`.
    pub durability_mode: DurabilityMode,
}

impl Default for LoadtestConfig {
    fn default() -> Self {
        Self {
            uri: ":memory:".into(),
            scale: 0.05,
            graph_seed: 42,
            workload_seed: 7,
            knobs: Knobs::default(),
            ops: 80,
            warmup: 8,
            out: None,
            wal_dir: None,
            reset: false,
            workers: 1,
            write_fraction: 0.5,
            logical_graph_size_limit: None,
            durability_mode: DurabilityMode::Full,
        }
    }
}

impl LoadtestConfig {
    pub fn ci() -> Self {
        Self {
            ops: 40,
            warmup: 4,
            ..Self::default()
        }
    }
}

/// One weighted use case: report metadata plus the function that plans it.
///
/// `W` is the owning dataset's workload picker and `P` its planned op, so a
/// use case is exactly one catalog row. Adding one means adding a row with
/// its `plan` function — there is no id-keyed dispatch to keep in step.
pub struct Catalog<W, P> {
    pub id: &'static str,
    pub kind: OpKind,
    /// Tied to the workload's planted theme (fraud: AML; identity: IAM).
    pub related: bool,
    pub description: &'static str,
    pub weight: u32,
    pub plan: fn(&mut W) -> P,
}

/// A weighted catalog pick paired with the op that its row planned.
pub type PlannedSelection<W, P> = (&'static Catalog<W, P>, P);

/// The dataset hook the harness is parameterized over. Implementations are
/// unit structs (`FraudLoadtest`, `IdentityLoadtest`); all state lives in the
/// generated graph and the `Work` picker.
pub trait LoadtestDataset: Copy + Send + Sync + 'static {
    /// Generated graph (kept on the main thread).
    type Graph: Send;
    /// Workload picker state, shared by workers under a mutex.
    type Work: Send;
    /// A planned use-case op, executed outside the picker lock.
    type Planned: Send;
    /// Post-op picker bookkeeping (e.g. a provisioned entity id).
    type Update: Send;

    /// Report `workload.kind` (`fraud` / `identity`).
    fn kind(self) -> &'static str;
    /// Report `name` (`fraud-mixed-payments` / `identity-mixed-iam`).
    fn name(self) -> &'static str;
    /// Memory ratio denominator: snapshot scalar key (`accounts` / `users`).
    fn unit(self) -> &'static str;
    /// Weighted use-case catalog.
    fn catalog(self) -> &'static [Catalog<Self::Work, Self::Planned>];
    /// Generate the graph from `cfg` (seeds, scale, knobs).
    fn generate(self, cfg: &LoadtestConfig) -> Result<Self::Graph>;
    /// Load the generated graph into a freshly opened database.
    fn load(self, conn: &mut Connection, graph: &Self::Graph) -> Result<()>;
    /// Dataset-specific snapshot scalars (`accounts`, `users`, …).
    fn snapshot_scalars(self, conn: &mut Connection) -> Result<BTreeMap<String, i64>>;
    /// Build the workload picker from the generated graph.
    fn workload(self, graph: &Self::Graph, seed: u64, write_fraction: f64) -> Self::Work;
    /// Choose and plan the next op. Called under the picker lock.
    fn next_op(self, work: &mut Self::Work) -> PlannedSelection<Self::Work, Self::Planned>;
    /// Execute one planned op. Called outside the picker lock.
    fn exec(
        self,
        conn: &mut Connection,
        planned: Self::Planned,
    ) -> Result<ExecOutcome<Self::Update>>;
    /// Apply post-op bookkeeping to the picker (under the lock).
    fn record(self, work: &mut Self::Work, update: Self::Update);
    /// `workload.planted` JSON for the report.
    fn planted_value(self, graph: &Self::Graph) -> serde_json::Value;
}

/// Outcome of executing one planned op: statement count, returned rows, and
/// post-op picker bookkeeping applied under the workload lock.
#[derive(Debug)]
pub struct ExecOutcome<U> {
    pub statements: u32,
    pub rows: usize,
    pub update: U,
}

/// Engine-neutral statement parameter. Reference runners convert to their
/// client's value type (`neo4rs::BoltType`).
#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    Str(String),
    Int(i64),
    Float(f64),
}

impl From<Param> for Value {
    fn from(p: Param) -> Value {
        match p {
            Param::Str(s) => Value::String(s.into()),
            Param::Int(n) => Value::Int(n),
            Param::Float(f) => Value::Float(f),
        }
    }
}

impl From<&str> for Param {
    fn from(v: &str) -> Param {
        Param::Str(v.into())
    }
}

/// One statement produced by a `planned_cypher` table.
#[derive(Debug, Clone)]
pub struct PlannedStatement {
    pub cypher: String,
    pub params: Vec<(&'static str, Param)>,
    /// Writes run with conflict retry where the engine has one; reads
    /// contribute their row count to the op's `rows`.
    pub kind: OpKind,
}

impl PlannedStatement {
    pub fn read(cypher: impl Into<String>) -> Self {
        Self {
            cypher: cypher.into(),
            params: Vec::new(),
            kind: OpKind::Read,
        }
    }

    pub fn write(cypher: impl Into<String>) -> Self {
        Self {
            cypher: cypher.into(),
            params: Vec::new(),
            kind: OpKind::Write,
        }
    }

    pub fn param(mut self, key: &'static str, value: Param) -> Self {
        self.params.push((key, value));
        self
    }
}

/// Run the full load test: generate, open, load, workers, finish snapshot.
pub fn run<D: LoadtestDataset>(dataset: D, cfg: &LoadtestConfig) -> Result<LoadtestReport> {
    crate::memory::require_sampling_available()?;
    let write_fraction = validate_write_fraction(cfg.write_fraction)?;
    let started = Utc::now();
    let started_ms = unix_ms();
    let wall = Instant::now();
    let workers = cfg.workers.max(1);

    let load_wall = Instant::now();
    let graph = dataset.generate(cfg)?;
    if cfg.reset {
        relate_storage::reset_uri(&cfg.uri, cfg.wal_dir.as_deref())?;
    }
    let db = Database::open_with(
        &cfg.uri,
        OpenOptions {
            wal_dir: cfg.wal_dir.clone(),
            checkpoint_interval: None,
            durability_mode: Some(cfg.durability_mode),
            logical_graph_size_limit: cfg.logical_graph_size_limit,
            ..OpenOptions::default()
        },
    )?;
    let mut conn = db.connect()?;
    let after_open_mem = sample_rss(None, dataset.unit());
    dataset.load(&mut conn, &graph)?;

    let start_size = snapshot(&mut conn, dataset)?;
    let start_mem = sample_rss(Some(&start_size), dataset.unit());
    let (logical_used_start, logical_limit) = db.memory_usage()?;
    drop(conn);
    db.clear_commit_timing();
    let load_duration_ms = load_wall.elapsed().as_millis() as u64;

    let db = Arc::new(db);
    let work = dataset.workload(&graph, cfg.workload_seed, write_fraction);
    let worker_wall = Instant::now();
    let records = run_workers(dataset, &db, work, cfg.warmup, cfg.ops, workers)?;
    let worker_duration_ms = worker_wall.elapsed().as_millis() as u64;

    let mut conn = db.connect()?;
    let finish_size = snapshot(&mut conn, dataset)?;
    let finish_mem = sample_rss(Some(&finish_size), dataset.unit());
    let (logical_used, _) = db.memory_usage()?;
    let memory = build_memory_report(
        after_open_mem,
        start_mem,
        finish_mem,
        &start_size,
        &finish_size,
        dataset.unit(),
    )
    .with_logical(logical_used_start, logical_used, logical_limit);
    let finished = Utc::now();
    let finished_ms = unix_ms();
    let duration_ms = wall.elapsed().as_millis() as u64;
    let throughput = throughput_from(&records, worker_duration_ms);

    let report = LoadtestReport {
        schema_version: REPORT_SCHEMA_VERSION,
        name: dataset.name().into(),
        started_at: started.to_rfc3339_opts(SecondsFormat::Millis, true),
        finished_at: finished.to_rfc3339_opts(SecondsFormat::Millis, true),
        started_at_unix_ms: started_ms,
        finished_at_unix_ms: finished_ms,
        duration_ms,
        load_duration_ms,
        worker_duration_ms,
        engine: "relate".into(),
        engine_version: env!("CARGO_PKG_VERSION").into(),
        config: ReportConfig {
            uri: cfg.uri.clone(),
            scale: cfg.scale,
            graph_seed: cfg.graph_seed,
            workload_seed: cfg.workload_seed,
            ops: cfg.ops,
            warmup: cfg.warmup,
            workers,
            write_fraction,
            durability_mode: cfg.durability_mode,
        },
        workload: workload_section(dataset, cfg, &graph, start_size, finish_size),
        throughput,
        memory,
        usecases: summarize(&records, dataset.catalog()),
        ops: records,
    };

    if let Some(path) = &cfg.out {
        write_report(path, &report)?;
    }
    let timing = db.commit_timing_report();
    if !timing.is_empty() {
        eprint!("{timing}");
    }
    Ok(report)
}

/// Fraud graph, mixed payments + AML (`fraud-mixed-payments`).
pub fn run_fraud(cfg: &LoadtestConfig) -> Result<LoadtestReport> {
    run(FraudLoadtest, cfg)
}

/// Identity graph, directory + IAM (`identity-mixed-iam`).
pub fn run_identity(cfg: &LoadtestConfig) -> Result<LoadtestReport> {
    run(IdentityLoadtest, cfg)
}

/// Claim the next op for a worker: one unique `fetch_add` off the shared
/// counter. `None` stops the worker (`total` ops are already claimed); the
/// first `warmup` claims are [`Phase::Warmup`], the rest [`Phase::Measured`].
/// One claim/stop/phase policy shared by every `run_workers` loop (the
/// in-crate harness and the Aura reference).
pub fn claim_next_op(next: &AtomicU64, total: u64, warmup: u64) -> Option<(u64, Phase)> {
    let seq = next.fetch_add(1, Ordering::Relaxed);
    if seq > total {
        return None;
    }
    let phase = if seq <= warmup {
        Phase::Warmup
    } else {
        Phase::Measured
    };
    Some((seq, phase))
}

fn run_workers<D: LoadtestDataset>(
    dataset: D,
    db: &Arc<Database>,
    work: D::Work,
    warmup: usize,
    ops: usize,
    workers: usize,
) -> Result<Vec<OpRecord>> {
    let total = warmup as u64 + ops as u64;
    if total == 0 {
        return Ok(Vec::new());
    }
    let work = Arc::new(Mutex::new(work));
    let next = Arc::new(AtomicU64::new(1));
    let mut handles = Vec::with_capacity(workers);
    for wid in 0..workers {
        let db = Arc::clone(db);
        let work = Arc::clone(&work);
        let next = Arc::clone(&next);
        handles.push(std::thread::spawn(move || -> Result<Vec<OpRecord>> {
            let mut conn = db.connect()?;
            let mut records = Vec::new();
            loop {
                let Some((seq, phase)) = claim_next_op(&next, total, warmup as u64) else {
                    break;
                };
                records.push(exec_one(dataset, &mut conn, &work, seq, phase, wid as u32)?);
            }
            Ok(records)
        }));
    }
    let mut records = Vec::with_capacity(total as usize);
    for h in handles {
        let batch = h
            .join()
            .map_err(|_| relate::Error::semantic("loadtest worker panicked"))??;
        records.extend(batch);
    }
    records.sort_by_key(|r| r.seq);
    Ok(records)
}

fn exec_one<D: LoadtestDataset>(
    dataset: D,
    conn: &mut Connection,
    work: &Mutex<D::Work>,
    seq: u64,
    phase: Phase,
    worker: u32,
) -> Result<OpRecord> {
    let (uc, planned) = {
        let mut w = work
            .lock()
            .map_err(|e| relate::Error::semantic(e.to_string()))?;
        dataset.next_op(&mut w)
    };
    let t0 = Instant::now();
    let outcome = dataset.exec(conn, planned);
    let latency_ns = t0.elapsed().as_nanos() as u64;
    match outcome {
        Ok(done) => {
            let record = OpRecord::succeeded(uc, seq, phase, worker, &done, latency_ns);
            {
                let mut w = work
                    .lock()
                    .map_err(|e| relate::Error::semantic(e.to_string()))?;
                dataset.record(&mut w, done.update);
            }
            Ok(record)
        }
        Err(e) => Ok(OpRecord::failed(uc, seq, phase, worker, &e, latency_ns)),
    }
}

/// Weighted read/write pick shared by every workload picker.
///
/// The pool of rows matching the drawn kind is non-empty by construction:
/// every catalog table carries at least one row per kind (fraud: 10 reads
/// and 7 writes; identity: 10 of each), and every row's weight is at least
/// 1, so the pool's total weight is too. A catalog that breaks either
/// invariant trips the asserts below instead of selecting a fabricated
/// fallback row.
pub(crate) fn choose_usecase<W, P>(
    rng: &mut StdRng,
    catalog: &'static [Catalog<W, P>],
    write_fraction: f64,
) -> &'static Catalog<W, P> {
    let want_write = rng.random::<f64>() < write_fraction;
    let wanted = |c: &&Catalog<W, P>| matches!(c.kind, OpKind::Write) == want_write;
    let matching: Vec<&'static Catalog<W, P>> = catalog.iter().filter(wanted).collect();
    assert!(
        !matching.is_empty(),
        "catalog has no {} rows: every catalog table has at least one row per kind \
         (fraud: 10 reads and 7 writes; identity: 10 of each)",
        if want_write { "write" } else { "read" }
    );
    let total: u32 = matching.iter().map(|c| c.weight).sum();
    assert!(total > 0, "every catalog row weight is at least 1");
    let mut x = rng.random_range(0..total);
    for c in matching {
        if x < c.weight {
            return c;
        }
        x -= c.weight;
    }
    unreachable!("the matching rows' weights sum to {total}, so the walk always selects one")
}

/// Uniformly pick (and clone) one element from a workload pool. Every
/// picker routes through this single implementation; the pools
/// (`ordinary`, `customers`, `GRANTABLE_ROLE_IDS`, …) are non-empty by construction.
pub(crate) fn pick_one<T: Clone>(rng: &mut StdRng, pool: &[T]) -> T {
    pool.choose(rng)
        .expect("workload picker pool is non-empty by construction")
        .clone()
}

/// Execute one `planned_cypher` statement against a Relate connection:
/// writes go through conflict-retry, reads count rows.
fn exec_statement(conn: &mut Connection, st: &PlannedStatement) -> Result<usize> {
    let params: BTreeMap<String, Value> = st
        .params
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone().into()))
        .collect();
    match st.kind {
        OpKind::Write => {
            write_query(conn, &st.cypher, params)?;
            Ok(0)
        }
        OpKind::Read => Ok(conn.query_with_params(&st.cypher, params)?.rows.len()),
    }
}

/// Execute every statement of one planned op through [`exec_statement`]:
/// the statement count and summed read rows an [`ExecOutcome`] carries.
/// Dataset `exec` implementations delegate here and supply only their
/// post-op update value.
fn exec_statements(conn: &mut Connection, spec: &[PlannedStatement]) -> Result<(u32, usize)> {
    let mut rows = 0usize;
    for st in spec {
        rows += exec_statement(conn, st)?;
    }
    Ok((spec.len() as u32, rows))
}

/// Write with optimistic-conflict retry (ADR-0015).
pub fn write_query(
    conn: &mut Connection,
    cypher: &str,
    params: BTreeMap<String, Value>,
) -> Result<QueryResult> {
    for _ in 0..8 {
        match conn.query_with_params(cypher, params.clone()) {
            Ok(r) => return Ok(r),
            Err(Error::Conflict) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(Error::conflict())
}

pub fn write_report(path: &Path, report: &LoadtestReport) -> Result<()> {
    crate::report::write_report(path, report).map_err(relate::Error::storage)
}

pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

fn snapshot<D: LoadtestDataset>(conn: &mut Connection, dataset: D) -> Result<GraphSnapshot> {
    Ok(GraphSnapshot {
        nodes: scalar(conn, "MATCH (n) RETURN count(n) AS n")?,
        relationships: scalar(conn, "MATCH ()-[r]->() RETURN count(r) AS n")?,
        scalars: dataset.snapshot_scalars(conn)?,
    })
}

pub fn summarize<W, P>(ops: &[OpRecord], catalog: &[Catalog<W, P>]) -> Vec<UsecaseSummary> {
    catalog
        .iter()
        .map(|c| {
            let measured: Vec<&OpRecord> = ops
                .iter()
                .filter(|o| o.phase == Phase::Measured && o.usecase == c.id)
                .collect();
            let errors = measured.iter().filter(|o| !o.ok).count();
            let lats: Vec<u64> = measured
                .iter()
                .filter(|o| o.ok)
                .map(|o| o.latency_ns)
                .collect();
            let statements = measured.iter().map(|o| o.statements as usize).sum();
            UsecaseSummary {
                id: c.id.into(),
                kind: c.kind,
                related: c.related,
                description: c.description.into(),
                ops: measured.len(),
                errors,
                statements,
                latency_ns: LatencyNs::from_samples(&lats, Rank::FrozenEnvelope),
            }
        })
        .filter(|u| u.ops > 0)
        .collect()
}

#[cfg(test)]
mod tests;
