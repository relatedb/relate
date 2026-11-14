use std::path::PathBuf;

use clap::Parser;
use relate_datasets::bench::{run as run_benchmark, BenchConfig};
use relate_datasets::harness::{parse_store, resolve_stores, shape_logical_bytes, StoreDataset};
use relate_datasets::report::{print_report, report_json, repro_command};
use relate_datasets::shapes::GraphShape;

#[derive(Parser, Debug)]
#[command(
    name = "benchmark-harness",
    about = "Baseline topology logical bytes and expansion throughput per dataset"
)]
struct Args {
    /// Existing store to benchmark, as NAME=PATH. Opened read-only.
    /// Repeatable.
    #[arg(long, value_parser = parse_store)]
    db: Vec<StoreDataset>,
    /// Directory of pulled graph-examples stores: each subdirectory holding a
    /// manifest.json is benchmarked under its directory name. Repeatable.
    #[arg(long)]
    graph_examples_dir: Vec<PathBuf>,
    /// Synthetic memory shape to build and benchmark: ring, fanout, flat,
    /// dense-ring, or mid-degree. Repeatable. Default when absent: ring and
    /// fanout.
    #[arg(long, default_values_t = [GraphShape::Ring, GraphShape::Fanout])]
    shape: Vec<GraphShape>,
    /// Logical graph memory for each synthetic shape, in MB (1024×1024 bytes).
    #[arg(long, default_value_t = 8)]
    shape_logical_mb: u64,
    /// Integer properties on every synthetic node. The first is `id`.
    #[arg(long, default_value_t = 1)]
    attributes: u32,
    /// Timed rounds per traversal after one warm-up; the best round is
    /// reported.
    #[arg(long, default_value_t = 3)]
    rounds: u32,
    /// Report JSON path. The table always prints to stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = Args::parse();
    let stores = resolve_stores(&args.db, &args.graph_examples_dir)?;
    let cfg = BenchConfig {
        stores,
        shapes: args.shape.clone(),
        shape_logical_bytes: shape_logical_bytes(args.shape_logical_mb)?,
        attributes: args.attributes,
        rounds: args.rounds,
        out: args.out.clone(),
        repro_command: repro_command(),
    };
    let report = run_benchmark(&cfg)?;
    let json = report_json(&report)?;
    print_report(&json, args.out.as_deref())
}
