//! The open-time storage flags every loadtest binary in this crate shares
//! ([`StorageArgs`]) and the mixed-workload arg set built on them
//! ([`CommonArgs`]). Split out of `loadtest.rs` to keep that module off the
//! file-size ceiling (AGENTS.md: no 1k-line creep).

use std::path::PathBuf;

use relate::DurabilityMode;

use crate::loadtest::{Knobs, LoadtestConfig};

/// The open-time storage flags every loadtest binary in this crate shares —
/// the mixed-workload `CommonArgs` and the bulk harness flatten this one
/// declaration rather than restating the flags.
#[derive(clap::Args, Debug, Clone)]
pub struct StorageArgs {
    /// WAL durability: full, everysecond, or snapshot (ignored for `:memory:`; default full)
    #[arg(long, default_value_t = DurabilityMode::Full)]
    pub durability_mode: DurabilityMode,
    /// Live graph + index cap: integer bytes, or a size with mb/mib/gb/gib (RELATE_LOGICAL_GRAPH_SIZE_LIMIT; 0 or omit is unlimited)
    #[arg(long, value_name = "SIZE", value_parser = parse_cli_size)]
    pub logical_graph_size_limit: Option<u64>,
}

/// CLI flags shared by every `*-loadtest` binary in this crate. Bins flatten
/// this plus their dataset's knob struct (`FraudKnobsArgs` /
/// `IdentityKnobsArgs`) and a per-binary `--out` default.
#[derive(clap::Args, Debug, Clone)]
pub struct CommonArgs {
    /// Database URI (`:memory:`, path, or `file://`)
    #[arg(long, default_value = ":memory:")]
    pub uri: String,
    /// Graph size vs full generator (`1.0` = 10,000 accounts or users)
    #[arg(long, default_value_t = 0.05)]
    pub scale: f64,
    /// RNG for the synthetic graph
    #[arg(long, default_value_t = 42)]
    pub graph_seed: u64,
    /// RNG for use-case mix and entity picks
    #[arg(long, default_value_t = 7)]
    pub workload_seed: u64,
    /// Measured operations (after warmup)
    #[arg(long, default_value_t = 80)]
    pub ops: usize,
    /// Unmeasured operations run first (omitted from latency summaries)
    #[arg(long, default_value_t = 8)]
    pub warmup: usize,
    /// Local WAL directory when `--uri` is persistent
    #[arg(long)]
    pub wal_dir: Option<PathBuf>,
    /// Delete the existing database at `--uri` (and `--wal-dir` if set) before load. No-op for `:memory:`
    #[arg(long)]
    pub reset: bool,
    /// Concurrent connections issuing ops (`1` = serial). Independent waiting writes share one WAL barrier in `full` mode
    #[arg(long, default_value_t = 1)]
    pub workers: usize,
    /// Fraction of ops that are writes (`0.0` = reads only, `1.0` = writes only)
    #[arg(long, default_value_t = 0.5)]
    pub write_fraction: f64,
    /// Open-time storage flags ([`StorageArgs`])
    #[command(flatten)]
    pub storage: StorageArgs,
}

impl CommonArgs {
    /// Assemble the library config from the shared flags, the dataset knobs
    /// (flattened into the bin), and the bin's resolved `--out` path.
    pub fn config(self, knobs: Knobs, out: PathBuf) -> LoadtestConfig {
        LoadtestConfig {
            uri: self.uri,
            scale: self.scale,
            graph_seed: self.graph_seed,
            workload_seed: self.workload_seed,
            knobs,
            ops: self.ops,
            warmup: self.warmup,
            out: Some(out),
            wal_dir: self.wal_dir,
            reset: self.reset,
            workers: self.workers,
            write_fraction: self.write_fraction,
            logical_graph_size_limit: self.storage.logical_graph_size_limit,
            durability_mode: self.storage.durability_mode,
        }
    }
}

/// clap value parser for `--logical-graph-size-limit` (integer bytes or mb/mib/gb/gib).
pub fn parse_cli_size(s: &str) -> std::result::Result<u64, String> {
    relate::parse_memory_limit(s).map_err(|e| e.to_string())
}
