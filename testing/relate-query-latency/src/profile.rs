//! `query-latency profile` (ADR-0244): loop one query under an in-process
//! sampling profiler and write where its time goes.
//!
//! The graph is loaded (or a store reused) and the query run once untimed
//! with its answer checked; only then does sampling start, so the load and
//! the warmup are not in the profile. The query then loops for `--seconds`
//! (at least one iteration) under `pprof-rs`, which samples every thread on
//! a CPU-time timer, including the connection's query worker.
//!
//! Outputs, under `target/query-latency/profile/<workload>-sf<sf>-q<NN>/`:
//!
//! - `top.txt`: the file to read ([`relate_test_support::stacks::top_text`]);
//! - `folded.txt`: collapsed stacks (root first, `;`-separated, then the
//!   sample count), for diffing two runs or re-rendering;
//! - `flame.svg`: a flame graph;
//! - `summary.json`: the run's facts.
//!
//! `--sampler none` runs the same loop without the in-process sampler (and
//! writes only `summary.json`), so `samply` or `perf` can record the
//! process instead.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use relate::Connection;
use serde::Serialize;

use crate::answers;
use crate::report::{git_state, write_pretty_json, Git, StoreOrigin};
use crate::run::{execute, Stats, Watchdog};
use crate::suite::{connect, open_graph, GraphOptions};
use crate::workload::{expected, query, IndexProfile, Query};
use crate::{work_root, Result};
use relate_test_support::profiler::sample;

/// Bumped when a `summary.json` field changes meaning or disappears.
const SUMMARY_SCHEMA: u32 = 1;

/// Which sampler records the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sampler {
    /// `pprof-rs`, in-process (the `sampling` feature, unix only).
    Pprof,
    /// No in-process sampler: an external one (`samply`, `perf`) records
    /// the process.
    None,
}

impl Sampler {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pprof => "pprof",
            Self::None => "none",
        }
    }
}

impl std::str::FromStr for Sampler {
    type Err = String;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "pprof" => Ok(Self::Pprof),
            "none" => Ok(Self::None),
            other => Err(format!("sampler is pprof or none, got {other:?}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProfileOptions {
    pub graph: GraphOptions,
    pub qid: u32,
    /// How long to loop the query (at least one iteration runs).
    pub seconds: Duration,
    /// Samples per second of CPU time.
    pub frequency: i32,
    pub sampler: Sampler,
    /// Per-execution timeout.
    pub timeout: Duration,
    /// Output directory (default:
    /// `target/query-latency/profile/graph-benchmark-ldbc-sf<sf>-q<NN>[-text]`).
    pub out_dir: Option<PathBuf>,
}

/// `<work root>/profile/graph-benchmark-ldbc-sf<sf>-q<NN>`, with `-text` for the text index
/// profile.
fn default_out_dir(graph: &GraphOptions, qid: u32) -> PathBuf {
    let suffix = match graph.profile {
        IndexProfile::Upstream => "",
        IndexProfile::Text => "-text",
    };
    work_root().join("profile").join(format!(
        "{}-sf{}-q{qid:02}{suffix}",
        crate::workload::NAME,
        graph.sf
    ))
}

/// `summary.json`.
#[derive(Debug, Serialize)]
pub struct ProfileSummary {
    pub schema: u32,
    pub workload: &'static str,
    pub sf: String,
    pub index_profile: String,
    pub qid: u32,
    pub query_sha256: String,
    pub sampler: &'static str,
    /// What the in-process sampler recorded; null with `--sampler none`.
    pub sampling: Option<SamplingSummary>,
    pub iterations: usize,
    /// Wall seconds the loop ran.
    pub loop_seconds: f64,
    /// Engine-reported elapsed (ADR-0221) per iteration.
    pub engine: Stats,
    pub git: Git,
    pub graph: StoreOrigin,
    pub out_dir: String,
}

/// What the in-process sampler recorded.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SamplingSummary {
    /// Samples per second of CPU time.
    pub frequency_hz: i32,
    /// Stack samples taken.
    pub samples: u64,
}

/// What the query loop measured.
#[derive(Debug, Clone)]
pub struct LoopStats {
    pub iterations: usize,
    pub wall: Duration,
    pub engine: Vec<Duration>,
}

/// Run `q` until `seconds` have passed (at least once), each execution
/// under `timeout`.
pub fn query_loop(
    conn: &mut Connection,
    q: &Query,
    seconds: Duration,
    timeout: Duration,
    watchdog: &Watchdog,
) -> Result<LoopStats> {
    let t = Instant::now();
    let mut engine = Vec::new();
    while engine.is_empty() || t.elapsed() < seconds {
        let (r, _) = execute(conn, &q.text, timeout, watchdog).map_err(|(status, msg)| {
            format!("q{} iteration {}: {status}: {msg}", q.qid, engine.len() + 1)
        })?;
        engine.push(r.header.elapsed);
    }
    Ok(LoopStats {
        iterations: engine.len(),
        wall: t.elapsed(),
        engine,
    })
}

/// `top.txt`'s two header lines: the query and its timing, then the
/// sampling and the commit.
fn top_header(
    opts: &ProfileOptions,
    q: &Query,
    stats: &LoopStats,
    engine: &Stats,
    samples: u64,
    git: &Git,
) -> [String; 2] {
    let commit = git
        .commit
        .as_deref()
        .map(|c| &c[..c.len().min(12)])
        .unwrap_or("unknown");
    let dirty = if git.dirty == Some(true) {
        " (dirty)"
    } else {
        ""
    };
    [
        format!(
            "q{} at SF{}, {} indexes: {} iterations in {:.2} s; engine elapsed p50 {:.3} ms, \
             mean {:.3} ms",
            q.qid,
            opts.graph.sf,
            opts.graph.profile.as_str(),
            stats.iterations,
            stats.wall.as_secs_f64(),
            engine.p50_ns as f64 / 1e6,
            engine.mean_ns as f64 / 1e6
        ),
        format!(
            "{samples} samples at {} Hz (pprof, in-process); commit {commit}{dirty}",
            opts.frequency
        ),
    ]
}

/// Profile one query and write the outputs.
pub fn profile(opts: &ProfileOptions) -> Result<ProfileSummary> {
    let q = query(opts.qid, opts.graph.sf)?;
    let expected = expected(opts.graph.sf)?;
    let out_dir = opts
        .out_dir
        .clone()
        .unwrap_or_else(|| default_out_dir(&opts.graph, opts.qid));
    let (db, graph) = open_graph(&opts.graph)?;
    let mut conn = connect("the profile loop's store", &db)?;
    let watchdog = Watchdog::new();

    // The checked warmup: a query that answers wrongly is not worth
    // profiling.
    let (first, _) = execute(&mut conn, &q.text, opts.timeout, &watchdog)
        .map_err(|(status, msg)| format!("q{} warmup: {status}: {msg}", q.qid))?;
    if let Some(exp) = expected.get(&q.qid) {
        if let Some(diff) = answers::diff(&first, exp) {
            return Err(format!(
                "q{} answers wrongly, not profiling it: {diff}",
                q.qid
            ));
        }
    }
    drop(first);

    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("creating {}: {e}", out_dir.display()))?;
    eprintln!(
        "query-latency: looping q{} for {:.1} s under sampler {} (pid {})",
        q.qid,
        opts.seconds.as_secs_f64(),
        opts.sampler.as_str(),
        std::process::id()
    );
    if opts.sampler == Sampler::Pprof {
        // Refuse before opening a graph: the kit's one authored refusal.
        relate_test_support::profiler::require_available()?;
    }
    let run_loop =
        |conn: &mut Connection| query_loop(conn, &q, opts.seconds, opts.timeout, &watchdog);
    // The loop's outcome is held (not propagated) until the artifacts are
    // written: the rounds already ran under the profiler, so a failed loop
    // keeps its stacks — the failure case is the one they are most useful
    // for. The failure header names the failure instead of the timings.
    let (stats, recorded) = match opts.sampler {
        Sampler::Pprof => {
            let s = sample(opts.frequency, || run_loop(&mut conn))?;
            (s.value, Some((s.stacks, s.flamegraph_svg)))
        }
        Sampler::None => (run_loop(&mut conn), None),
    };
    drop(conn);
    drop(db);

    let git = git_state();
    let sampling = match &recorded {
        Some((stacks, svg)) => {
            let header = match &stats {
                Ok(stats) => top_header(
                    opts,
                    &q,
                    stats,
                    &Stats::of(&stats.engine),
                    stacks.total(),
                    &git,
                ),
                Err(_) => [
                    format!("query-latency profile q{} (loop failed)", q.qid),
                    format!("{} samples", stacks.total()),
                ],
            };
            relate_test_support::profiler::write_stack_outputs(&out_dir, stacks, svg, &header)?;
            Some(SamplingSummary {
                frequency_hz: opts.frequency,
                samples: stacks.total(),
            })
        }
        None => None,
    };
    let stats = stats?;
    let engine = Stats::of(&stats.engine);
    let summary = ProfileSummary {
        schema: SUMMARY_SCHEMA,
        workload: crate::workload::NAME,
        sf: opts.graph.sf.as_str().into(),
        index_profile: opts.graph.profile.as_str().into(),
        qid: q.qid,
        query_sha256: q.sha256.clone(),
        sampler: opts.sampler.as_str(),
        sampling,
        iterations: stats.iterations,
        loop_seconds: stats.wall.as_secs_f64(),
        engine,
        git,
        graph,
        out_dir: out_dir.display().to_string(),
    };
    write_pretty_json(&summary, &out_dir.join("summary.json"))?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampler_parses() {
        assert_eq!("pprof".parse::<Sampler>().unwrap(), Sampler::Pprof);
        assert_eq!("none".parse::<Sampler>().unwrap(), Sampler::None);
        assert!("perf".parse::<Sampler>().is_err());
    }

    #[test]
    fn sampling_summary_nests_frequency_and_samples() {
        let some = serde_json::to_value(Some(SamplingSummary {
            frequency_hz: 997,
            samples: 12,
        }))
        .unwrap();
        assert_eq!(
            some,
            serde_json::json!({"frequency_hz": 997, "samples": 12})
        );
        let none: Option<SamplingSummary> = None;
        assert_eq!(serde_json::to_value(none).unwrap(), serde_json::Value::Null);
    }
}
