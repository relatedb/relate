use std::path::PathBuf;

use clap::Parser;
use serde::Serialize;

use relate_datasets::codec::{
    extract_chunks, measure_dataset, BaselineTable, CodecDatasetReport, Scheme,
};
use relate_datasets::harness::{
    build_shape, hardware_note, parse_store, resolve_stores, shape_logical_bytes, StoreDataset,
};
use relate_datasets::report::{print_report, report_json, repro_command};
use relate_datasets::shapes::GraphShape;

#[derive(Parser, Debug)]
#[command(
    name = "codec-prototype",
    about = "Scalar compressed-adjacency codec prototype: encoded bytes and decode throughput per dataset"
)]
struct Args {
    /// Existing store to measure, as NAME=PATH. Opened read-only.
    /// Repeatable.
    #[arg(long, value_parser = parse_store)]
    db: Vec<StoreDataset>,
    /// Directory of pulled graph-examples stores: each subdirectory holding a
    /// manifest.json is measured under its directory name. Repeatable.
    #[arg(long)]
    graph_examples_dir: Vec<PathBuf>,
    /// Synthetic memory shape to build and measure: ring, fanout, flat,
    /// dense-ring, or mid-degree. Repeatable. Default when absent: ring,
    /// fanout, and dense-ring (the baseline's shape set).
    #[arg(long, default_values_t = [GraphShape::Ring, GraphShape::Fanout, GraphShape::DenseRing])]
    shape: Vec<GraphShape>,
    /// Logical graph memory for each synthetic shape, in MB (1024×1024 bytes).
    #[arg(long, default_value_t = 8)]
    shape_logical_mb: u64,
    /// Integer properties on every synthetic node. The first is `id`.
    #[arg(long, default_value_t = 1)]
    attributes: u32,
    /// Timed decode rounds per scheme after one warm-up; the best round is
    /// reported.
    #[arg(long, default_value_t = 5)]
    rounds: u32,
    /// Also measure the SIMD decode variant (NEON on aarch64; the scalar
    /// fallback elsewhere). Pass `--simd false` for scalar-only numbers.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    simd: bool,
    /// Baseline lookup JSON: a benchmark-harness `--out` report supplying
    /// (topology_logical_bytes, 1-hop hps) per dataset name. Default when
    /// absent: the compiled-in table recorded 2026-10-11.
    #[arg(long)]
    baseline: Option<PathBuf>,
    /// Report JSON path. The table always prints to stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Serialize)]
struct CodecReport {
    hardware: String,
    repro_command: String,
    /// How the measured hop streams were obtained.
    provenance: String,
    datasets: Vec<CodecDatasetReport>,
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
    let shape_logical_bytes = shape_logical_bytes(args.shape_logical_mb)?;
    let baseline = match &args.baseline {
        Some(path) => BaselineTable::load(path)?,
        None => BaselineTable::built_in(),
    };

    let mut datasets = Vec::new();
    let mut run_one = |name: String, db: &relate::Database| -> Result<(), String> {
        let report = extract_chunks(db)
            .and_then(|chunks| measure_dataset(&name, &chunks, args.rounds, args.simd, &baseline))
            .map_err(|err| format!("{name}: {err}"))?;
        eprintln!(
            "{name}: {} hops, for128 {} B, hybrid {} B",
            report.hops,
            report.scheme(Scheme::For128).bytes,
            report.scheme(Scheme::Hybrid).bytes,
        );
        datasets.push(report);
        Ok(())
    };
    for store in &stores {
        let db = relate::Database::open_read_only(&store.path.display().to_string())
            .map_err(|err| format!("{}: open read-only: {err}", store.name))?;
        run_one(store.name.clone(), &db)?;
    }
    for shape in &args.shape {
        let name = format!("shape-{shape}");
        let db = build_shape(*shape, shape_logical_bytes, args.attributes)
            .map_err(|err| format!("{name}: {err}"))?;
        run_one(name, &db)?;
    }

    let report = CodecReport {
        hardware: hardware_note(),
        repro_command: repro_command(),
        provenance: "hop streams reconstructed from a relationship scan \
                     (id(r), endpoints, type, integer r.ts), grouped per \
                     (node, type, direction) and sorted with the store's \
                     cmp_ts_desc order; TypedAdj itself is crate-private"
            .into(),
        datasets,
    };
    let json = report_json(&report)?;
    print_report(&json, args.out.as_deref())
}
