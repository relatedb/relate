//! Pokec-shaped expansion profiling harness (LABS-1116).
//!
//! A dev tool (not CI-gated), the `plan_bench` precedent: runs the
//! traverse-bench pokec query groups against the synthetic pokec fixture
//! (`relate_test_support::pokec`) as one self-terminating process a
//! sampling profiler (samply) can record. Per group it prints wall time,
//! the total row count, and a checksum — the fixture is deterministic,
//! so identical checksums before and after a change are the correctness
//! cross-check for a perf edit.
//!
//! [`run_groups`] is the typed entry: the caller resolves its flags (the
//! `pokec_profile` bin and the Vortex A/B driver's `pokec` subcommand both
//! parse [`crate::cli::PokecArgs`]) into [`Options`]. The usage text lives
//! with the binary; the flag table is in `docs/datasets.md`.

mod groups;
#[cfg(test)]
mod tests;

use std::borrow::Cow;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use relate::{Connection, PlanNode, QueryResult, Result, Value};
use relate_test_support::pokec::{open, PokecShape};
use relate_test_support::profiler::{sample, DEFAULT_FREQUENCY};

use self::groups::{Group, Run, GROUPS};
use crate::alloc;

/// The process's peak resident set size so far, in bytes. Monotonic, so a
/// group's own peak is only readable in a fresh process (`--group NAME`).
fn peak_rss_bytes() -> u64 {
    relate_test_support::sampling::process_peak_rss_bytes()
        .expect("peak RSS is readable on this platform")
}

/// What the invocation does: exactly one of the (CLI-exclusive) flags' jobs,
/// resolved once, so no flag is read again after this.
pub enum Mode {
    /// Print each group's `PROFILE` at start id 1 (`--profile`).
    Profile,
    /// Time each group's rounds (see [`Timed`]).
    Timed(Timed),
}

impl Mode {
    /// Whether the job runs under the in-process sampler (`--sample DIR`).
    fn samples(&self) -> bool {
        matches!(
            self,
            Mode::Timed(Timed {
                sample_dir: Some(_),
                ..
            })
        )
    }
}

/// The resolved timed job: `profiled` times `PROFILE <query>` (the memo
/// counters of ADR-0654), and `sample_dir` (`--sample DIR`) names the root
/// the in-process sampler writes each group's stacks under.
pub struct Timed {
    pub profiled: bool,
    pub sample_dir: Option<PathBuf>,
}

/// A resolved pokec run: the fixture shape, the one group to run (`None` runs
/// every group), and what the invocation does.
pub struct Options {
    pub shape: PokecShape,
    pub group: Option<String>,
    pub mode: Mode,
}

/// Refuse `--sample` before the fixture opens: the kit's one authored
/// refusal, mapped into this binary's error type.
fn require_sampler(mode: &Mode) -> Result<()> {
    if mode.samples() {
        relate_test_support::profiler::require_available().map_err(relate::Error::internal)?;
    }
    Ok(())
}

/// Resolve a `--group` name against the table before the fixture opens; the
/// resolved value is the group itself.
fn group_by_name(name: &str) -> std::result::Result<&'static Group, String> {
    GROUPS.iter().find(|g| g.name == name).ok_or_else(|| {
        let known: Vec<&str> = GROUPS.iter().map(|g| g.name).collect();
        format!("no group named {name:?} (one of {})", known.join(", "))
    })
}

/// The group names in catalogue order — the one list the CLI's `--group`
/// possible values and its help derive from, so neither drifts from
/// [`GROUPS`].
pub(crate) fn group_names() -> impl Iterator<Item = &'static str> {
    GROUPS.iter().map(|g| g.name)
}

/// Run the pokec groups. `attach` is the experimental property-store hook
/// (ADR-0665), run once after the fixture opens.
pub fn run_groups(attach: Option<crate::AttachHook>, opts: Options) -> Result<()> {
    require_sampler(&opts.mode)?;
    let selected: Vec<&Group> = match opts.group.as_deref() {
        Some(name) => vec![group_by_name(name).map_err(relate::Error::internal)?],
        None => GROUPS.iter().collect(),
    };
    let db = open(opts.shape)?;
    if let Some(attach) = attach {
        attach(&db).map_err(relate::Error::internal)?;
    }
    let mut conn = db.connect()?;
    for g in selected {
        match &opts.mode {
            Mode::Profile => run_profile(&mut conn, g)?,
            Mode::Timed(timed) => run_timed(&mut conn, g, timed)?,
        }
    }
    Ok(())
}

/// Print `PROFILE <query>` for `g` at its first run (its parameter, when it
/// takes one, rides the same binding as a measured round).
fn run_profile(conn: &mut Connection, g: &Group) -> Result<()> {
    let out = profile_query(conn, g)?;
    println!("{}: {out:#?}", g.name);
    Ok(())
}

/// `PROFILE g.query` at the group's first run.
fn profile_query(conn: &mut Connection, g: &Group) -> Result<QueryResult> {
    conn.query_with_params(&format!("PROFILE {}", g.query), g.first_run().params)
}

/// The plan evidence of a PROFILEd group: one pre-order `operator
/// [kernel rows db_hits hops walks node_reads]` entry per plan node
/// (`[no metrics]` for a folded operator), so a regression of a
/// shortest-path group from the shortest route to enumerate-then-select
/// shows as a changed operator, a changed rung (`kernel`) or a blown-up
/// read count beside a timing that did not finish.
/// The engine's PROFILE has no shortest-route rung name (the route runs
/// inside the walker's `MATCH` operator, `kernel=Walker`), so the read mix
/// is the evidence the route leaves.
fn plan_evidence(out: &QueryResult) -> String {
    fn walk(node: &PlanNode, parts: &mut Vec<String>) {
        // An operator the engine folded into its child (a count answered by
        // the MATCH beneath it) carries no metrics of its own.
        parts.push(match node.metrics.as_ref() {
            Some(m) => format!(
                "{} [kernel={} rows={} db_hits={} hops={} walks={} node_reads={}]",
                node.operator,
                m.kernel.map_or("-", |k| k.as_str()),
                m.rows,
                m.db_hits,
                m.reads.hops,
                m.reads.neighbor_walks,
                m.reads.node_reads
            ),
            None => format!("{} [no metrics]", node.operator),
        });
        for child in &node.children {
            walk(child, parts);
        }
    }
    let mut parts = Vec::new();
    walk(
        out.plan_root().expect("PROFILE stamps the plan"),
        &mut parts,
    );
    parts.join(" > ")
}

/// Time `g`'s measured rounds and print rows and checksum; under the
/// in-process sampler, writing into the group's directory under the
/// job's `sample_dir`.
fn run_timed(conn: &mut Connection, g: &Group, timed: &Timed) -> Result<()> {
    // One warm round of the group's own query — primes its plans and
    // caches so the measured rounds are steady-state. Propagated like
    // a measured round: a warm-up failure is a harness failure.
    // The statement text is resolved once per group, outside the timed
    // rounds: `--profiled` prefixes `PROFILE`, never a per-round format.
    let statement = if timed.profiled {
        Cow::Owned(format!("PROFILE {}", g.query))
    } else {
        Cow::Borrowed(g.query)
    };
    warm(conn, g, &statement)?;
    // The plan evidence is recorded before the timed rounds, from the
    // warmed plan: a group that regressed to enumeration is visible here
    // even when its rounds never finish a sample.
    println!(
        "{}: plan {}",
        g.name,
        plan_evidence(&profile_query(conn, g)?)
    );
    let m = match &timed.sample_dir {
        Some(dir) => run_sampled(conn, g, &dir.join(g.name), &statement)?,
        None => measure(conn, g, &statement)?,
    };
    println!(
        "{}: {} ms rows={} checksum={} alloc_bytes={} live_peak={} live_end={} \
         rss_base={} rss_peak={}",
        g.name,
        m.elapsed.as_millis(),
        m.rows,
        m.checksum,
        m.alloc_bytes,
        m.live_peak,
        m.live_end,
        m.rss_base,
        m.rss_peak
    );
    Ok(())
}

/// [`measure`] under the in-process profiler, writing the shared
/// `top.txt` / `folded.txt` / `flame.svg` trio into `dir` (one directory
/// per group).
fn run_sampled(
    conn: &mut Connection,
    g: &Group,
    dir: &std::path::Path,
    statement: &str,
) -> Result<Measured> {
    // Create (and so validate) the directory before the sampled run, so an
    // unwritable `--sample DIR` fails fast, not after the whole measurement.
    std::fs::create_dir_all(dir)?;
    let sampled = sample(DEFAULT_FREQUENCY, || measure(conn, g, statement))
        .map_err(relate::Error::internal)?;
    // The rounds have already run: write the profile before propagating a
    // failed round's error, so the failure case keeps its stacks — the
    // case they are most useful for.
    let header = vec![format!("pokec_profile {} ({} rounds)", g.name, g.rounds)];
    let written = relate_test_support::profiler::write_stack_outputs(
        dir,
        &sampled.stacks,
        &sampled.flamegraph_svg,
        &header,
    );
    let measured = sampled.value?;
    written.map_err(relate::Error::internal)?;
    Ok(measured)
}

/// What the measured rounds produced: the total row count, the checksum,
/// and the time of the rounds themselves.
struct Measured {
    rows: i64,
    checksum: i64,
    elapsed: Duration,
    alloc_bytes: u64,
    /// High-water live heap bytes over the rounds, minus the live bytes
    /// before them.
    live_peak: i64,
    /// Live heap bytes after the rounds, minus the live bytes before them.
    live_end: i64,
    /// The process's peak RSS before the rounds and after them.
    rss_base: u64,
    rss_peak: u64,
}

/// Run `g`'s measured rounds.
fn measure(conn: &mut Connection, g: &Group, statement: &str) -> Result<Measured> {
    let live_before = alloc::live();
    alloc::reset_peak();
    let rss_base = peak_rss_bytes();
    let t = Instant::now();
    let bytes_before = alloc::alloc_bytes();
    let mut rows = 0i64;
    let mut checksum = 0i64;
    for r in 0..g.rounds {
        for step in g.runs() {
            let out = run(conn, g, statement, &step)?;
            rows += out;
            // The fold mixes the round and (as 0 when absent) the start id,
            // so recorded checksums stay comparable across revisions of
            // this harness; it fingerprints the whole invocation.
            checksum = checksum
                .wrapping_add(out.wrapping_mul(0x9e37_79b9))
                .wrapping_add(i64::try_from(r).expect("round count is small"))
                .wrapping_add(
                    step.start
                        .map_or(0, |s| i64::try_from(s).expect("start ids are small")),
                );
        }
    }
    Ok(Measured {
        rows,
        checksum,
        elapsed: t.elapsed(),
        alloc_bytes: alloc::alloc_bytes() - bytes_before,
        live_peak: alloc::peak_live() - live_before,
        live_end: alloc::live() - live_before,
        rss_base,
        rss_peak: peak_rss_bytes(),
    })
}

/// One warm round of `g`'s own query — its first run. Not counted in the
/// measurements.
fn warm(conn: &mut Connection, g: &Group, statement: &str) -> Result<()> {
    run(conn, g, statement, &g.first_run()).map(|_| ())
}

/// Run `statement` — `g`'s query, `PROFILE`d under `--profiled` — once for
/// `step` and return its one integer answer.
fn run(conn: &mut Connection, g: &Group, statement: &str, step: &Run) -> Result<i64> {
    let out = conn.query_with_params(statement, step.params.clone())?;
    match out.rows.first().and_then(|row| row.first()) {
        Some(Value::Int(n)) => Ok(*n),
        other => panic!(
            "pokec_profile: {:#?} did not answer one integer (got {other:?})",
            g.query
        ),
    }
}
