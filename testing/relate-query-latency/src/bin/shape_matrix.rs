//! Shape-matrix harness (REL8-2242 step 3): one variable-length / QPP
//! query timed over many graph and query shapes, with the median, the
//! noise spread and the heap peak per cell, so the QPP memo is judged on
//! more than the social-network Q3/Q4. Dev tool, not CI-gated; time it in
//! a quiet window. Flags: `docs/datasets.md#shape-matrix-rel8-2242`.
//!
//! ```text
//! cargo run --release -p relate-query-latency --bin shape_matrix -- --label baseline
//! cargo run --release -p relate-query-latency --bin shape_matrix -- \
//!     --compare target/query-latency/shape-matrix/baseline.json \
//!               target/query-latency/shape-matrix/memo.json
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use relate_query_latency::shape::{self, run::Options};

// The per-cell heap peak reads the counting allocator.
#[global_allocator]
static A: relate_query_latency::alloc::Counting = relate_query_latency::alloc::Counting;

/// Time one query over the shape matrix, or compare two reports.
#[derive(Parser)]
#[command(name = "shape_matrix")]
struct Args {
    /// Name of this run; the report is `<OUT>/<LABEL>.json` and `.md`
    #[arg(long, default_value = "baseline")]
    label: String,
    /// Report directory
    #[arg(long, default_value = "target/query-latency/shape-matrix")]
    out: PathBuf,
    /// Most driver rows (`P` nodes) a statement fans out from
    #[arg(long, default_value_t = 2000)]
    persons: usize,
    /// Interleaved timed rounds per cell (at least 5 for a spread)
    #[arg(long, default_value_t = 9, value_parser = clap::value_parser!(u32).range(5..))]
    rounds: u32,
    /// Path visits one statement may cost: a cell's driver rows are cut
    /// to budget / visits per start, and a cell left under 4 rows is
    /// reported as skipped
    #[arg(long, default_value_t = 600_000.0)]
    budget: f64,
    /// Run only cells whose id contains this text
    #[arg(long)]
    filter: Option<String>,
    /// Print the cell ids and their statements, run nothing
    #[arg(long)]
    list: bool,
    /// Compare two report JSONs (before, after) instead of running
    #[arg(long, num_args = 2, value_names = ["BEFORE", "AFTER"])]
    compare: Option<Vec<PathBuf>>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    if let Some(paths) = &args.compare {
        return match shape::run::compare(&paths[0], &paths[1]) {
            Ok(table) => {
                print!("{table}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("shape_matrix: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let cells: Vec<_> = shape::matrix()
        .into_iter()
        .filter(|c| args.filter.as_ref().is_none_or(|f| c.id().contains(f)))
        .collect();
    if args.list {
        for cell in &cells {
            println!("{}\t{}", cell.id(), cell.query());
        }
        return ExitCode::SUCCESS;
    }
    let options = Options {
        persons: args.persons,
        rounds: args.rounds as usize,
        budget: args.budget,
        label: args.label.clone(),
    };
    let report = shape::run::run(&cells, &options);
    let markdown = report.to_markdown();
    print!("{markdown}");
    let written = std::fs::create_dir_all(&args.out)
        .and_then(|()| {
            std::fs::write(
                args.out.join(format!("{}.json", args.label)),
                serde_json::to_string_pretty(&report.to_json()).expect("report serializes"),
            )
        })
        .and_then(|()| std::fs::write(args.out.join(format!("{}.md", args.label)), markdown));
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("shape_matrix: writing the report: {e}");
            ExitCode::FAILURE
        }
    }
}
