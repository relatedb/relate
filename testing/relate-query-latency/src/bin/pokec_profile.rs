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
//! Usage: `cargo run --release -p relate-query-latency --bin
//! pokec_profile -- <small|medium> [--group NAME] [--sample DIR |
//! --profile]` (`--help` lists every flag; the flag table is in
//! `docs/datasets.md`).
//!
//! * `small` — 10,000 users / 121,716 friendships, opens in seconds.
//! * `medium` — 100,000 users / 1,768,164 friendships; the open cost is
//!   the population cost.
//! * `--group NAME` runs one group (`count`, `scan_agg_global`,
//!   `expansion_3`, `expansion_3_unrolled`, `scan_expand_2hop_filter`).
//! * `--sample DIR` runs the selected groups under the in-process profiler
//!   and writes `DIR/<group>/top.txt`, `folded.txt` and `flame.svg`; no
//!   samply needed.
//! * `--profile` (needs `--group`) prints the `PROFILE` result of the
//!   group's query at start id 1 instead of timing it.
//! * No env vars beyond cargo's own.
//!
//! Record a profile: `samply record -s -n -o prof.json.gz -- <binary> small`.
//! Build with frame pointers for trustworthy stacks
//! (`RUSTFLAGS="-C force-frame-pointers=yes"`).
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use relate::{Connection, Result, Value};
use relate_test_support::pokec::{open, PokecShape};
use relate_test_support::profiler::{sample, DEFAULT_FREQUENCY};

/// One measured group: its query, how many start ids (`1..=starts`) feed
/// the `$id` parameter, and how many rounds each start id runs. `None`
/// marks the queries that take no parameter — the param shape rides
/// here, never a sniff of the query text.
struct Group {
    name: &'static str,
    query: &'static str,
    rounds: u64,
    starts: Option<u64>,
}

impl Group {
    /// The start ids one round runs: `Some(1..=starts)` for a parameterised
    /// group, the single unparametrized `None` otherwise — the one place
    /// that knows the split.
    fn schedule(&self) -> impl Iterator<Item = Option<u64>> {
        let last = self.starts.unwrap_or(1);
        let parameterised = self.starts.is_some();
        (1..=last).map(move |s| parameterised.then_some(s))
    }

    /// The start id of the warm-up and `PROFILE` runs.
    fn first_start(&self) -> Option<u64> {
        self.schedule().next().expect("a schedule is never empty")
    }
}

/// Start ids 1.. are the fixture's hub-heavy head (i^-0.4 weights), the
/// shape multi-hop expansion cost hangs on.
const GROUPS: [Group; 5] = [
    Group {
        name: "count",
        query: "MATCH (n:User) RETURN count(*)",
        rounds: 20,
        starts: None,
    },
    Group {
        name: "scan_agg_global",
        query: "MATCH (n:User) RETURN n.age, count(*) ORDER BY n.age",
        rounds: 20,
        starts: None,
    },
    Group {
        name: "expansion_3",
        query: "MATCH (a:User {id:$id})-[:Friend*3]->(b:User) RETURN count(b)",
        rounds: 2,
        starts: Some(64),
    },
    Group {
        name: "expansion_3_unrolled",
        query: "MATCH (a:User {id:$id})-[:Friend]->()-[:Friend]->()-[:Friend]->(b:User) RETURN count(b)",
        rounds: 2,
        starts: Some(64),
    },
    Group {
        name: "scan_expand_2hop_filter",
        query: "MATCH (a:User {id:$id})-[:Friend*2]->(b:User) WHERE b.age > 25 RETURN count(b)",
        rounds: 2,
        starts: Some(64),
    },
];

/// The fixture shape to open.
#[derive(Clone, Copy, ValueEnum)]
enum Shape {
    /// 10,000 users / 121,716 friendships.
    Small,
    /// 100,000 users / 1,768,164 friendships.
    Medium,
}

impl Shape {
    fn fixture(self) -> PokecShape {
        match self {
            Shape::Small => PokecShape::SMALL,
            Shape::Medium => PokecShape::MEDIUM,
        }
    }
}

#[derive(Parser)]
#[command(
    name = "pokec_profile",
    about = "Pokec expansion profiling harness (dev tool, not CI-gated)"
)]
struct Args {
    /// Which pokec shape to open.
    #[arg(value_enum)]
    shape: Shape,
    /// Run only this group (`count`, `scan_agg_global`, `expansion_3`,
    /// `expansion_3_unrolled`, `scan_expand_2hop_filter`); every group when omitted.
    #[arg(long, value_parser = group_by_name)]
    group: Option<&'static Group>,
    /// Run the selected groups under the in-process sampler and write
    /// `<DIR>/<group>/top.txt`, `folded.txt` and `flame.svg`.
    #[arg(long, value_name = "DIR", conflicts_with = "profile")]
    sample: Option<PathBuf>,
    /// Print the `PROFILE` result of the group's query at start id 1
    /// instead of timing it.
    #[arg(long, requires = "group")]
    profile: bool,
}

/// What the invocation does: exactly one of three jobs, resolved once from
/// the (clap-exclusive) flags.
enum Mode {
    /// Print each group's `PROFILE` at start id 1 (`--profile`).
    Profile,
    /// Time each group under the in-process sampler and write its stacks
    /// under the directory (`--sample DIR`).
    Sampled(PathBuf),
    /// Time each group and print rows and checksum.
    Timed,
}

impl Args {
    fn mode(&self) -> Mode {
        match (self.profile, &self.sample) {
            (true, _) => Mode::Profile,
            (false, Some(dir)) => Mode::Sampled(dir.clone()),
            (false, None) => Mode::Timed,
        }
    }

    /// Refuse `--sample` before the fixture opens: the kit's one authored
    /// refusal, mapped into this binary's error type.
    fn require_sampler(&self) -> Result<()> {
        if self.sample.is_some() {
            relate_test_support::profiler::require_available().map_err(relate::Error::internal)?;
        }
        Ok(())
    }
}

/// Resolve `--group` against the table at parse time, before the fixture
/// opens; the parsed value is the group itself.
fn group_by_name(name: &str) -> std::result::Result<&'static Group, String> {
    GROUPS.iter().find(|g| g.name == name).ok_or_else(|| {
        let known: Vec<&str> = GROUPS.iter().map(|g| g.name).collect();
        format!("no group named {name:?} (one of {})", known.join(", "))
    })
}

fn main() -> Result<()> {
    let args = Args::parse();
    args.require_sampler()?;
    let selected: Vec<&Group> = match args.group {
        Some(g) => vec![g],
        None => GROUPS.iter().collect(),
    };
    let db = open(args.shape.fixture())?;
    let mut conn = db.connect()?;
    let mode = args.mode();
    for g in selected {
        match &mode {
            Mode::Profile => run_profile(&mut conn, g)?,
            Mode::Sampled(dir) => run_timed(&mut conn, g, Some(&dir.join(g.name)))?,
            Mode::Timed => run_timed(&mut conn, g, None)?,
        }
    }
    Ok(())
}

/// Print `PROFILE <query>` for `g` at start id 1 (its parameter, when it
/// takes one, rides the same binding as a measured round).
fn run_profile(conn: &mut Connection, g: &Group) -> Result<()> {
    let params = params_for(g.first_start());
    let out = conn.query_with_params(&format!("PROFILE {}", g.query), params)?;
    println!("{}: {out:#?}", g.name);
    Ok(())
}

/// Time `g`'s measured rounds and print rows and checksum; under the
/// in-process sampler, writing into `sample_dir`, when it is given.
fn run_timed(conn: &mut Connection, g: &Group, sample_dir: Option<&Path>) -> Result<()> {
    // One warm round of the group's own query — primes its plans and
    // caches so the measured rounds are steady-state. Propagated like
    // a measured round: a warm-up failure is a harness failure.
    warm(conn, g)?;
    let m = match sample_dir {
        Some(dir) => run_sampled(conn, g, dir)?,
        None => measure(conn, g)?,
    };
    println!(
        "{}: {} ms rows={} checksum={}",
        g.name,
        m.elapsed.as_millis(),
        m.rows,
        m.checksum
    );
    Ok(())
}

/// [`measure`] under the in-process profiler, writing the shared
/// `top.txt` / `folded.txt` / `flame.svg` trio into `dir` (one directory
/// per group).
fn run_sampled(conn: &mut Connection, g: &Group, dir: &Path) -> Result<Measured> {
    // Create (and so validate) the directory before the sampled run, so an
    // unwritable `--sample DIR` fails fast, not after the whole measurement.
    std::fs::create_dir_all(dir)?;
    let sampled =
        sample(DEFAULT_FREQUENCY, || measure(conn, g)).map_err(relate::Error::internal)?;
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
}

/// `g`'s query parameters at start id `start`: `$id` for a parameterised
/// group (`Some`), none for one that takes no parameter (`None`).
fn params_for(start: Option<u64>) -> BTreeMap<String, Value> {
    let mut params = BTreeMap::new();
    if let Some(s) = start {
        let id = i64::try_from(s).expect("start ids are small");
        params.insert("id".to_owned(), Value::Int(id));
    }
    params
}

/// Run `g`'s measured rounds.
fn measure(conn: &mut Connection, g: &Group) -> Result<Measured> {
    let t = Instant::now();
    let mut rows = 0i64;
    let mut checksum = 0i64;
    for r in 0..g.rounds {
        for start in g.schedule() {
            let out = run(conn, g, start)?;
            rows += out;
            // The fold mixes the round and (as 0 when absent) the start id,
            // so recorded checksums stay comparable across revisions of
            // this harness; it fingerprints the whole invocation.
            checksum = checksum
                .wrapping_add(out.wrapping_mul(0x9e37_79b9))
                .wrapping_add(i64::try_from(r).expect("round count is small"))
                .wrapping_add(start.map_or(0, |s| i64::try_from(s).expect("start ids are small")));
        }
    }
    Ok(Measured {
        rows,
        checksum,
        elapsed: t.elapsed(),
    })
}

/// One warm round of `g`'s own query — its first start id, or the single
/// unparametrized run. Not counted in the measurements.
fn warm(conn: &mut Connection, g: &Group) -> Result<()> {
    run(conn, g, g.first_start()).map(|_| ())
}

/// Run `g`'s query once at `start` and return its one integer answer.
fn run(conn: &mut Connection, g: &Group, start: Option<u64>) -> Result<i64> {
    let out = conn.query_with_params(g.query, params_for(start))?;
    match out.rows.first().and_then(|row| row.first()) {
        Some(Value::Int(n)) => Ok(*n),
        other => panic!(
            "pokec_profile: {:#?} did not answer one integer (got {other:?})",
            g.query
        ),
    }
}
