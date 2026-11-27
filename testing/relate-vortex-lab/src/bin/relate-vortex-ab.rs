//! Engine A/B driver (ADR-0665): the query-latency suite or the pokec groups
//! with the Vortex property store attached, so a run can be compared against
//! the default store. The store attach is injected through the suite's hook;
//! the measured arguments are the suite's own (`--store`, `--qids`,
//! `--min-time`, `--out`; the pokec shape and its flags).
//!
//! Usage:
//! `relate-vortex-ab run [--qids 1,8,12] [--min-time 1] [--store PATH] [--data DIR] [--out FILE]`
//! `relate-vortex-ab pokec <small|medium> [--group NAME] [--sample DIR | --profile]`

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
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
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match argv.first().map(String::as_str) {
        Some("pokec") => {
            let args = match PokecArgs::try_parse_from(
                std::iter::once("relate-vortex-ab pokec".to_string())
                    .chain(argv[1..].iter().cloned()),
            ) {
                Ok(args) => args,
                Err(e) => e.exit(),
            };
            relate_query_latency::pokec::run_groups(
                Some(engine::attach),
                args.pokec.into_options(),
            )
            .map_err(|e| e.to_string())?;
            Ok(0)
        }
        Some("run") => run_suite_with_store(&argv[1..]),
        other => Err(format!(
            "unknown command {other:?}: expected `run` or `pokec`"
        )),
    }
}

/// `pokec`'s flags — the shared pokec harness flag set.
#[derive(Debug, Parser)]
#[command(name = "relate-vortex-ab pokec")]
struct PokecArgs {
    #[command(flatten)]
    pokec: relate_query_latency::cli::PokecArgs,
}

/// `run`'s flags — the suite bin's flag set the A/B needs.
#[derive(Debug, Parser)]
#[command(name = "relate-vortex-ab run")]
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
fn run_suite_with_store(argv: &[String]) -> Result<u8, String> {
    // clap owns the flag errors: unknown flag / missing value / --help all
    // exit with clap's own text and status.
    let args = match RunArgs::try_parse_from(
        std::iter::once("relate-vortex-ab run".to_string()).chain(argv.iter().cloned()),
    ) {
        Ok(args) => args,
        Err(e) => e.exit(),
    };

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
