//! Bulk-load load test: `create` / `merge-with-constraint` / `update` /
//! `delete`, one statement and one commit per batch, across a ladder of batch
//! sizes. See [`relate_datasets::bulk`] for what is measured and why the
//! `Person.id` uniqueness constraint is in place for every op except `create`.
//!
//! Runs single-threaded by design (determinism); use `fraud-loadtest
//! --workers` for the concurrent group-commit shape.

use std::path::PathBuf;

use clap::Parser;
use relate_datasets::bulk::{parse_size, run, BulkConfig, BulkOp, DEFAULT_SIZES};
use relate_datasets::loadtest::StorageArgs;
use relate_datasets::memory::format_bytes;
use relate_datasets::report::nanos_to_millis;

#[derive(Parser, Debug)]
#[command(
    name = "bulk-loadtest",
    about = "Bulk create / merge / update / delete load test at 5k..100k rows per statement"
)]
struct Args {
    /// Operations to measure, comma-separated (`create`, `merge-with-constraint`, `update`, `delete`)
    #[arg(long, value_name = "LIST", value_delimiter = ',', value_parser = BulkOp::parse, default_values_t = BulkOp::all())]
    ops: Vec<BulkOp>,
    /// Batch sizes, comma-separated (`5000`, `50_000`, `50k`)
    #[arg(long, value_name = "LIST", value_delimiter = ',', value_parser = parse_size, default_values_t = DEFAULT_SIZES)]
    sizes: Vec<usize>,
    /// Measured repetitions per (operation, size); each rep re-creates its base data, untimed
    #[arg(long, default_value_t = 1)]
    reps: usize,
    /// Unmeasured repetitions run first (each still gets its untimed setup)
    #[arg(long, default_value_t = 0)]
    warmup: usize,
    /// Directory for the per-measurement databases, or `:memory:` for a fresh in-memory database per measurement
    #[arg(long, default_value = "target/bulk-loadtest-data")]
    dir: String,
    /// Open-time storage flags ([`StorageArgs`])
    #[command(flatten)]
    storage: StorageArgs,
    /// Report JSON path
    #[arg(long, default_value = "target/bulk-loadtest.json")]
    out: PathBuf,
    /// Record the engine's commit-timing report (parse+plan, overlay execute, wal_lock wait, encode+write, sync_all, apply_redo) into each report entry
    #[arg(long)]
    commit_timing: bool,
}

fn main() {
    let args = Args::parse();
    let cfg = BulkConfig {
        dir: args.dir.clone(),
        sizes: args.sizes,
        ops: args.ops,
        reps: args.reps,
        warmup: args.warmup,
        out: Some(args.out.clone()),
        logical_graph_size_limit: args.storage.logical_graph_size_limit,
        durability_mode: args.storage.durability_mode,
        commit_timing: args.commit_timing,
    };
    println!(
        "bulk-loadtest: {} ops x {} sizes x {} rep(s) into {}",
        cfg.ops.len(),
        cfg.sizes.len(),
        cfg.reps,
        cfg.dir
    );
    match run(&cfg) {
        // `run()` writes `--out` itself (via `cfg.out`).
        Ok(report) => {
            println!(
                "\n{} measured rows in {} ms",
                report.total_rows(),
                report.duration_ms
            );
            println!(
                "{:<21} {:>3} {:>8} {:>8} {:>12} {:>9} {:>9} {:>9}",
                "op", "con", "rows", "ms", "rows/s", "p50", "p95", "p99"
            );
            for e in &report.results {
                let (p50, p95, p99) = match &e.latency_ns {
                    Some(l) => (
                        format!("{:.1}ms", nanos_to_millis(l.p50)),
                        format!("{:.1}ms", nanos_to_millis(l.p95)),
                        format!("{:.1}ms", nanos_to_millis(l.p99)),
                    ),
                    None => ("-".into(), "-".into(), "-".into()),
                };
                println!(
                    "{:<21} {:>3} {:>8} {:>8} {:>12.1} {:>9} {:>9} {:>9}{}",
                    e.op.id(),
                    if e.constraint { "yes" } else { "no" },
                    e.size,
                    e.duration_ms,
                    e.rows_per_sec,
                    p50,
                    p95,
                    p99,
                    match (&e.error, e.errors) {
                        (Some(msg), n) => format!("  errors={n}: {msg}"),
                        (None, 0) => String::new(),
                        (None, n) => format!("  errors={n}"),
                    }
                );
            }
            println!("(con = Person.id uniqueness constraint present during the op)");
            println!(
                "wrote {}  (peak RSS {})",
                args.out.display(),
                format_bytes(
                    report
                        .results
                        .iter()
                        .map(|e| e.rss_bytes)
                        .max()
                        .unwrap_or(0)
                )
            );
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list flags are clap's: omitted means the documented defaults,
    /// given means split on commas with each element parsed (or refused)
    /// by the shared element parsers.
    #[test]
    fn list_flags_default_and_split() {
        let args = Args::try_parse_from(["bulk-loadtest"]).unwrap();
        assert_eq!(args.ops, BulkOp::all());
        assert_eq!(args.sizes, DEFAULT_SIZES.to_vec());

        let args = Args::try_parse_from([
            "bulk-loadtest",
            "--ops",
            "create, merge",
            "--sizes",
            "1,10_000,50k",
        ])
        .unwrap();
        assert_eq!(args.ops, vec![BulkOp::Create, BulkOp::MergeWithConstraint]);
        assert_eq!(args.sizes, vec![1, 10_000, 50_000]);

        // Both list flags share one whitespace policy: the chunks clap
        // hands the element parsers are trimmed.
        let args = Args::try_parse_from([
            "bulk-loadtest",
            "--ops",
            "create, merge",
            "--sizes",
            "1, 5000",
        ])
        .unwrap();
        assert_eq!(args.sizes, vec![1, 5000]);

        assert!(Args::try_parse_from(["bulk-loadtest", "--ops", "bogus"]).is_err());
        assert!(Args::try_parse_from(["bulk-loadtest", "--sizes", "18446744073709552k"]).is_err());
        assert!(Args::try_parse_from(["bulk-loadtest", "--sizes", "0"]).is_err());
        assert!(Args::try_parse_from(["bulk-loadtest", "--sizes", "abc"]).is_err());
    }
}
