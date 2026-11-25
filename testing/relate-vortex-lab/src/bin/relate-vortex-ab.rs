//! Engine A/B driver (ADR-0665): the query-latency suite, the pokec groups, or
//! a stress driver (the graph-examples sandbox differential, the datasets
//! loadtests) with the Vortex property store attached, so a run can be
//! compared against the default store. The store attach is injected through
//! each harness's hook; the measured arguments are the harness's own.
//!
//! Usage:
//! `relate-vortex-ab run [--qids 1,8,12] [--min-time 1] [--store PATH] [--data DIR] [--out FILE]`
//! `relate-vortex-ab pokec <small|medium> [--group NAME] [--sample DIR | --profile]`
//! `relate-vortex-ab sandbox [--default] [NAMES…]`
//! `relate-vortex-ab loadtest <fraud|identity> [--default] [--scale S] [--ops N] [--warmup N] [--workers N]`

use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use relate_datasets::loadtest::{self, CommonArgs, LoadtestConfig};
use relate_query_latency::run::RunConfig;
use relate_query_latency::suite::{run_suite, GraphOptions, SuiteOptions};
use relate_query_latency::workload::{IndexProfile, ScaleFactor, QIDS};
use relate_vortex_lab::engine;

// The measuring binary's one counting allocator (the A/B reports read its
// counters through `bench`): a bin chooses, the library never does — both
// arms of this binary pay it alike.
#[global_allocator]
static ALLOC: relate_vortex_lab::bench::Counting = relate_vortex_lab::bench::Counting;

fn main() -> std::process::ExitCode {
    match run() {
        Ok(code) => std::process::ExitCode::from(code),
        Err(error) => {
            eprintln!("relate-vortex-ab: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u8, String> {
    match Cli::parse().verb {
        Verb::Pokec(args) => {
            relate_query_latency::pokec::run_groups(
                Some(engine::attach),
                args.pokec.into_options(),
            )
            .map_err(|e| e.to_string())?;
            Ok(0)
        }
        Verb::Run(args) => run_suite_with_store(args),
        Verb::Sandbox(args) => run_sandbox(args),
        Verb::Loadtest(args) => run_loadtest(args),
    }
}

/// A harness driven with the Vortex property store attached (ADR-0665).
#[derive(Debug, Parser)]
#[command(name = "relate-vortex-ab")]
struct Cli {
    // One root parser and one subcommand enum own every argument — no argv
    // string matching, slicing or hand-written usage errors.
    #[command(subcommand)]
    verb: Verb,
}

#[derive(Debug, Subcommand)]
enum Verb {
    /// The pokec groups with the Vortex store attached.
    Pokec(PokecArgs),
    /// The query-latency suite with the Vortex store attached.
    Run(RunArgs),
    /// The neo4j-graph-examples differential suite with the Vortex store attached.
    Sandbox(SandboxArgs),
    /// A datasets loadtest, read-only, with the Vortex store attached
    /// (`--write-fraction` is accepted from the shared surface but forced to 0.0).
    Loadtest(LoadtestArgs),
}

/// `pokec`'s flags — the shared pokec harness flag set.
#[derive(Debug, Args)]
struct PokecArgs {
    #[command(flatten)]
    pokec: relate_query_latency::cli::PokecArgs,
}

/// `sandbox`'s arguments — the neo4j-graph-examples differential suite with
/// the Vortex store attached to every pulled copy. `--default` runs the
/// identical suite without the attach; the per-statement outcome lines
/// (`OUT <id> <kind>`) of the two runs diff to the store-caused differences.
/// Requires docker and `RELATE_BIN`. With no names, every non-opt-in dataset
/// runs; an unknown name is an error.
#[derive(Debug, Args)]
struct SandboxArgs {
    /// Run the identical suite without the Vortex attach.
    #[arg(long = "default")]
    default_store: bool,
    /// Datasets to run (manifest names); every non-opt-in dataset when empty.
    names: Vec<String>,
}

fn run_sandbox(args: SandboxArgs) -> Result<u8, String> {
    use relate_graph_examples::sandbox;

    // `sandbox::run` owns the dataset selection (`config::parse_dataset_filter`)
    // and the whole orchestration (dumps, reference boot, per-dataset runs).
    let names: Vec<&str> = args.names.iter().map(String::as_str).collect();
    let attach: Option<relate_test_support::AttachHook> = if args.default_store {
        None
    } else {
        Some(engine::attach)
    };
    let summary = sandbox::run(&names, attach)?;
    // One stable line per statement, so the attached and default runs diff.
    for r in &summary.records {
        println!(
            "OUT {} {} {}",
            r.id(),
            r.outcome.kind(),
            // One line per statement: a payload that carries newlines
            // collapses them, or the run-to-run diff loses its shape.
            r.outcome.payload().unwrap_or_default().replace('\n', " ")
        );
    }
    println!("SUMMARY {}", summary.summary.line());
    Ok(u8::from(summary.infra_failed()))
}

/// The loadtest dataset — resolved once at parse time to one
/// `(knobs, run function)` row, so the argv name is never discriminated twice.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum LoadtestDataset {
    /// The fraud graph (payments + AML).
    Fraud,
    /// The synthetic IAM identity graph.
    Identity,
}

/// `loadtest`'s arguments — the datasets loadtest read-only
/// (`write_fraction: 0.0`, the one mode an attached store stays valid in)
/// with the Vortex store attached to the loaded graph. `--default` runs the
/// identical workload without the attach; the report's per-usecase error
/// counts and answers diff between the two runs. The shared `CommonArgs`
/// flags (`--scale`, `--ops`, `--warmup`, `--workers`, …) are the loadtest
/// bins' canonical surface, so the parser policy is theirs.
#[derive(Debug, Args)]
struct LoadtestArgs {
    #[arg(value_enum)]
    dataset: LoadtestDataset,
    /// Run the identical workload without the Vortex attach.
    #[arg(long = "default")]
    default_store: bool,
    #[command(flatten)]
    common: CommonArgs,
}

fn run_loadtest(args: LoadtestArgs) -> Result<u8, String> {
    let name = args
        .dataset
        .to_possible_value()
        .expect("every ValueEnum variant carries a possible value")
        .get_name()
        .to_string();
    // One row per dataset: its generator knobs are its loadtest bin's
    // defaults (fraud fanout_k 12 / cycle_len 4; identity nest_depth 4 /
    // group_k 12).
    let (knobs, run): (
        loadtest::Knobs,
        fn(&LoadtestConfig) -> relate::Result<loadtest::LoadtestReport>,
    ) = match args.dataset {
        LoadtestDataset::Fraud => (loadtest::Knobs::default(), loadtest::run_fraud),
        LoadtestDataset::Identity => (
            loadtest::Knobs::Identity {
                nest_depth: 4,
                group_k: 12,
            },
            loadtest::run_identity,
        ),
    };
    let mut cfg = args.common.config(knobs, PathBuf::new());
    // This driver prints the report; it writes no JSON side file.
    cfg.out = None;
    // Read-only: a write after the attach makes the store decline every
    // read — the loadtest's mixed mode cannot exercise the store.
    cfg.write_fraction = 0.0;
    cfg.attach = if args.default_store {
        None
    } else {
        Some(engine::attach)
    };
    let report = run(&cfg).map_err(|e| format!("loadtest {name}: {e}"))?;
    for u in &report.usecases {
        println!("UC {} ops={} errors={}", u.id, u.ops, u.errors);
    }
    println!(
        "SUMMARY ops={} reads/s={:.1} errors={}",
        report.config.ops,
        report.throughput.reads_per_sec,
        report.usecases.iter().map(|u| u.errors).sum::<usize>()
    );
    Ok(0)
}

/// `run`'s flags — the suite bin's flag set the A/B needs.
#[derive(Debug, Args)]
struct RunArgs {
    /// Comma-separated qids; absent runs the whole suite.
    #[arg(long, value_delimiter = ',')]
    qids: Option<Vec<u32>>,
    /// Minimum time per query, in seconds.
    #[arg(long = "min-time", default_value_t = 1.0)]
    min_time: f64,
    /// The Vortex store path (`None` loads into `:memory:`).
    #[arg(long)]
    store: Option<PathBuf>,
    /// The data root (see `relate_query_latency::fetch::data_root`).
    #[arg(long = "data")]
    data_dir: Option<PathBuf>,
    /// Write the JSON report to this path.
    #[arg(long)]
    out: Option<PathBuf>,
}

/// `run`: the query-latency suite over the loaded graph with the Vortex store
/// attached. Mirrors the suite bin's flags the A/B needs.
fn run_suite_with_store(args: RunArgs) -> Result<u8, String> {
    let sf = ScaleFactor::Sf0_1;
    let graph = GraphOptions {
        sf,
        profile: IndexProfile::Upstream,
        store: args.store,
        data_root: args
            .data_dir
            .unwrap_or_else(relate_query_latency::fetch::data_root),
        download: true,
        attach: Some(engine::attach),
    };
    let opts = SuiteOptions {
        graph,
        qids: args.qids.unwrap_or_else(|| QIDS.collect()),
        run: RunConfig {
            min_time: Duration::from_secs_f64(args.min_time),
            ..RunConfig::new(sf.default_timeout())
        },
    };
    let report = run_suite(&opts)?;
    println!("{}", report.table());
    if let Some(out) = args.out {
        report.write_json(&out)?;
    }
    Ok(u8::from(!report.failures().is_empty()))
}
