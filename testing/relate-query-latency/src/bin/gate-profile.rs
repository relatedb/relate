//! One Cypher statement under the in-process sampler, against one store —
//! the helper the sqlite-gate's profiling mode calls per case (LABS-1637).
//!
//! The gate is a Python harness in a separate repository: it measures
//! latency and parity, but it cannot say where the time goes, because the
//! sampling primitive lives in Rust (`relate-test-support`'s profiler) and
//! Python cannot call it. This binary is the bridge — the gate shells out
//! to it exactly as it already shells out for timing, and reads back the
//! same three artifacts every profiling harness here produces
//! (`top.txt`, `folded.txt`, `flame.svg`).
//!
//! Deliberately one query, one store, one profile: no corpus, no rounds,
//! no comparison. Everything the gate needs that this does not do, the
//! gate already does in Python. The profile wraps the query's first
//! execution only — a timed run needs many, and mixing the two would
//! change both.
//!
//! Exit codes, so the caller can tell refusal from failure:
//!   0  profiled; the three artifacts are in `--out-dir`
//!   2  usage (clap)
//!   3  this build has no in-process profiler (`UNAVAILABLE`) — a skip,
//!      not a failure; the gate lane must not go red on it
//!   1  anything else (open, query, write)

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;
use relate::Database;
use relate_test_support::profiler::{
    require_available, sample, write_stack_outputs, DEFAULT_FREQUENCY,
};

/// Profile one statement. All flags are required except `--frequency`.
#[derive(Parser)]
#[command(
    name = "gate-profile",
    about = "One Cypher statement under the in-process sampler, for the sqlite-gate's profiling mode",
    version
)]
struct Args {
    /// The store to open: a directory path, or `:memory:` for an
    /// in-memory store the caller has no way to pre-load — so in practice
    /// a path, because the gate builds its graphs on disk.
    #[arg(long)]
    db: String,

    /// The Cypher statement to profile. It runs once, unwrapped from any
    /// driver protocol — the profile is of the engine, not of a session.
    #[arg(long)]
    query: String,

    /// Sampling frequency, samples per CPU-second (pprof-rs). Defaults to
    /// the shared `DEFAULT_FREQUENCY`, the one value every profiling
    /// harness here uses. The range rides the declaration, not a check in
    /// `main`: pprof divides by the frequency when it builds its timer, so
    /// zero panics inside the library rather than returning an error, and
    /// clap is this crate's one spelling of a flag constraint.
    #[arg(
        long,
        default_value_t = DEFAULT_FREQUENCY,
        value_parser = clap::value_parser!(i32).range(1..)
    )]
    frequency: i32,

    /// Directory the three profile outputs land in. Created if absent;
    /// `top.txt` carries the header lines below, `folded.txt` and
    /// `flame.svg` the stacks.
    #[arg(long)]
    out_dir: PathBuf,
}

fn main() -> ExitCode {
    let args = Args::parse();

    // The clean skip: a build without the profiler feature refuses here,
    // with the one authored message (its text names the remedy — run
    // under an external sampler instead). The gate reads exit 3 as "this
    // build cannot profile", never as a lane failure.
    if let Err(e) = require_available() {
        eprintln!("gate-profile: {e}");
        return ExitCode::from(3);
    }

    match run(&args) {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("gate-profile: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<String, String> {
    let db = Database::open(&args.db).map_err(|e| format!("opening {}: {e}", args.db))?;
    let mut conn = db.connect().map_err(|e| format!("connecting: {e}"))?;

    std::fs::create_dir_all(&args.out_dir)
        .map_err(|e| format!("creating {}: {e}", args.out_dir.display()))?;

    // Timed inside the closure: `sample` also builds the profiler, tears
    // it down and renders the flamegraph, and folding that into the
    // reported time would overstate the statement — which is the number
    // the gate compares against SQLite.
    let sampled = sample(args.frequency, || {
        let started = Instant::now();
        let answer = conn.query(&args.query);
        (answer, started.elapsed())
    })?;
    let (answer, elapsed) = sampled.value;

    // The header `top.txt` carries: what ran, against what, and the two
    // facts a reader needs before looking at a single frame — how long
    // the statement took and how many samples the profiler got. A query
    // fast enough to yield no samples produces an empty profile, and the
    // header is what makes that legible rather than silently blank.
    //
    // Written before the answer is propagated: a statement that fails
    // under the sampler is exactly when a profile is worth having, so the
    // stacks and flamegraph are kept rather than dropped with the error.
    // The failure header names the failure in place of the timings — the
    // shape `profile.rs` uses for a failed loop.
    let header = match &answer {
        Ok(answer) => vec![
            format!("gate-profile: one statement against {}", args.db),
            format!("elapsed: {:.3} ms", elapsed.as_secs_f64() * 1000.0),
            format!("samples: {}", sampled.stacks.total()),
            format!(
                "rows: {} — the answer, for the gate's parity check",
                answer.rows.len()
            ),
        ],
        Err(e) => vec![
            format!("gate-profile: one statement against {} (failed)", args.db),
            format!("failure: {e}"),
            format!("samples: {}", sampled.stacks.total()),
        ],
    };
    write_stack_outputs(
        &args.out_dir,
        &sampled.stacks,
        &sampled.flamegraph_svg,
        &header,
    )?;

    let answer = answer.map_err(|e| format!("the query failed under the sampler: {e}"))?;

    Ok(format!(
        "profiled {} samples in {:.3} ms; {} rows; artifacts in {}",
        sampled.stacks.total(),
        elapsed.as_secs_f64() * 1000.0,
        answer.rows.len(),
        args.out_dir.display()
    ))
}
