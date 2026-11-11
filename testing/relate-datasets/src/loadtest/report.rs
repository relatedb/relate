//! The mixed-workload loadtest report schema: the JSON shape the fraud and
//! identity loadtests publish (the bulk and memory-shape harnesses carry
//! their own), split out of `loadtest.rs` to keep that module off the
//! file-size ceiling (AGENTS.md: no 1k-line creep).

use std::collections::BTreeMap;

use relate::{DurabilityMode, Error};
use serde::{Deserialize, Serialize};

use crate::loadtest::{Catalog, ExecOutcome, LoadtestConfig, LoadtestDataset, MemoryReport};
use crate::report::LatencyNs;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    Read,
    Write,
}

/// Whether an op ran before or after the measurement window.
///
/// Serializes as `warmup` / `measured`, the report JSON's published spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Warmup,
    Measured,
}

/// Whole-graph counts plus per-dataset scalars (fraud: `accounts`,
/// `customers`, `transfers`; identity: `users`, `groups`, `roles`,
/// `memberships`). Scalars flatten into the same JSON object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphSnapshot {
    pub nodes: i64,
    pub relationships: i64,
    #[serde(flatten)]
    pub scalars: BTreeMap<String, i64>,
}

impl GraphSnapshot {
    /// One dataset-specific count (`"accounts"`, `"users"`, …). 0 when absent.
    pub fn scalar(&self, key: &str) -> i64 {
        self.scalars.get(key).copied().unwrap_or(0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsecaseSummary {
    pub id: String,
    pub kind: OpKind,
    /// Tied to the workload's planted theme (AML/fraud or IAM).
    pub related: bool,
    pub description: String,
    pub ops: usize,
    pub errors: usize,
    pub statements: usize,
    pub latency_ns: Option<LatencyNs>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpRecord {
    pub seq: u64,
    pub phase: Phase,
    pub usecase: String,
    pub kind: OpKind,
    /// Tied to the workload's planted theme (AML/fraud or IAM).
    pub related: bool,
    pub statements: u32,
    pub ok: bool,
    pub error: Option<String>,
    pub latency_ns: u64,
    pub rows: usize,
    /// Worker index that ran this op (`0` when `--workers 1`).
    #[serde(default)]
    pub worker: u32,
}

impl OpRecord {
    /// A record for an op that ran to completion: the catalog row's metadata
    /// (`id`, `kind`, `related`) plus the outcome's statement count and
    /// summed read rows. Generic over the catalog's picker/planned types and
    /// the outcome's update type so both harnesses (the in-crate runner and
    /// the Aura reference) build records through the one constructor.
    pub fn succeeded<W, P, U>(
        uc: &Catalog<W, P>,
        seq: u64,
        phase: Phase,
        worker: u32,
        done: &ExecOutcome<U>,
        latency_ns: u64,
    ) -> Self {
        Self {
            seq,
            phase,
            usecase: uc.id.into(),
            kind: uc.kind,
            related: uc.related,
            statements: done.statements,
            ok: true,
            error: None,
            latency_ns,
            rows: done.rows,
            worker,
        }
    }

    /// A record for an op that failed: the catalog row's metadata plus the
    /// error text. No statement completed, so `statements` and `rows` are 0.
    pub fn failed<W, P>(
        uc: &Catalog<W, P>,
        seq: u64,
        phase: Phase,
        worker: u32,
        error: &Error,
        latency_ns: u64,
    ) -> Self {
        Self {
            seq,
            phase,
            usecase: uc.id.into(),
            kind: uc.kind,
            related: uc.related,
            statements: 0,
            ok: false,
            error: Some(error.to_string()),
            latency_ns,
            rows: 0,
            worker,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportConfig {
    pub uri: String,
    pub scale: f64,
    pub graph_seed: u64,
    pub workload_seed: u64,
    pub ops: usize,
    pub warmup: usize,
    /// Concurrent connections. Omitted in older reports (treat as 1).
    #[serde(default = "default_workers")]
    pub workers: usize,
    /// Fraction of ops that are writes. Omitted in older reports (treat as 0.5).
    #[serde(default = "default_write_fraction")]
    pub write_fraction: f64,
    /// WAL durability mode. Omitted in older reports (treat as `full`).
    #[serde(default)]
    pub durability_mode: DurabilityMode,
}

fn default_workers() -> usize {
    1
}

fn default_write_fraction() -> f64 {
    0.5
}

/// Workload-specific report fields. One nested object keeps the rest of the
/// report schema identical across workloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadSection {
    /// Workload id (`fraud` / `identity`).
    pub kind: String,
    /// Memory ratio denominator: the snapshot scalar RSS/graph bytes are
    /// divided by (`accounts` / `users`).
    pub unit: String,
    /// Generator knobs (`fanout_k`/`cycle_len` or `nest_depth`/`group_k`).
    pub config: serde_json::Value,
    /// Planted pattern ids from the generator.
    pub planted: serde_json::Value,
    pub dataset: DatasetSizes,
}

/// Assemble the nested per-workload report object. Shared by the in-crate
/// harness and the reference runner (Aura).
pub fn workload_section<D: LoadtestDataset>(
    dataset: D,
    cfg: &LoadtestConfig,
    graph: &D::Graph,
    start: GraphSnapshot,
    finish: GraphSnapshot,
) -> WorkloadSection {
    WorkloadSection {
        kind: dataset.kind().into(),
        unit: dataset.unit().into(),
        config: serde_json::to_value(cfg.knobs)
            .expect("knobs serialize infallibly: both variants are plain usize-field structs"),
        planted: dataset.planted_value(graph),
        dataset: DatasetSizes { start, finish },
    }
}

/// Measured use-case ops per **worker** wall-clock second (warmup excluded
/// from the count; load/generate is not in the denominator).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Throughput {
    pub ops_per_sec: f64,
    pub reads_per_sec: f64,
    pub writes_per_sec: f64,
    pub read_ops: usize,
    pub write_ops: usize,
}

pub fn throughput_from(records: &[OpRecord], duration_ms: u64) -> Throughput {
    let secs = (duration_ms.max(1) as f64) / 1000.0;
    let mut read_ops = 0usize;
    let mut write_ops = 0usize;
    for o in records.iter().filter(|o| o.phase == Phase::Measured) {
        match o.kind {
            OpKind::Read => read_ops += 1,
            OpKind::Write => write_ops += 1,
        }
    }
    let ops = read_ops + write_ops;
    Throughput {
        ops_per_sec: ops as f64 / secs,
        reads_per_sec: read_ops as f64 / secs,
        writes_per_sec: write_ops as f64 / secs,
        read_ops,
        write_ops,
    }
}

/// Schema version of [`LoadtestReport`]. Bump when the JSON shape changes.
///
/// 6: one honest schema for every workload. Workload-specific config knobs,
/// planted patterns, and dataset counts moved into the nested `workload`
/// object; `fraud_related` / `iam_related` unified to `related`;
/// `rss_per_account` / `rss_per_user` unified to `rss_per_unit` (the unit is
/// named by `workload.unit`).
pub const REPORT_SCHEMA_VERSION: u32 = 6;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadtestReport {
    pub schema_version: u32,
    pub name: String,
    pub started_at: String,
    pub finished_at: String,
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: u64,
    /// Whole `run()` wall (generate + load + workers + finish snapshot).
    pub duration_ms: u64,
    /// Generate, reset, open, and graph load (schema 4).
    #[serde(default)]
    pub load_duration_ms: u64,
    /// Warmup + measured worker ops (schema 4). Throughput uses this.
    #[serde(default)]
    pub worker_duration_ms: u64,
    pub engine: String,
    pub engine_version: String,
    pub config: ReportConfig,
    /// Per-workload fields: kind, unit, knobs, planted patterns, dataset counts.
    pub workload: WorkloadSection,
    /// Measured ops / `worker_duration_ms` (warmup and load excluded).
    #[serde(default)]
    pub throughput: Throughput,
    /// Process RSS and per-entity cost (schema 3).
    #[serde(default)]
    pub memory: MemoryReport,
    pub usecases: Vec<UsecaseSummary>,
    pub ops: Vec<OpRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetSizes {
    pub start: GraphSnapshot,
    pub finish: GraphSnapshot,
}
