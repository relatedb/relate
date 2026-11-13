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
//!   `expansion_3`, `expansion_3_unrolled`, `scan_expand_2hop_filter`, and
//!   the shortest-path groups `shortest_pair`, `shortest_path_fn`,
//!   `shortest_expr`, `all_shortest_paths`, `shortest_postfix`,
//!   `shortest_fan`, `shortest_all_pairs`).
//! * `--sample DIR` runs the selected groups under the in-process profiler
//!   and writes `DIR/<group>/top.txt`, `folded.txt` and `flame.svg`; no
//!   samply needed.
//! * A timed run also prints one `<group>: plan ...` line per group, the
//!   PROFILE plan evidence (operator, `kernel` rung, read counts) of the
//!   group's query at start id 1, recorded before its timed rounds.
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
use relate::{Connection, PlanNode, QueryResult, Result, Value};
use relate_test_support::pokec::{open, PokecShape};
use relate_test_support::profiler::{sample, DEFAULT_FREQUENCY};

/// What a group's query binds: the single shape rides here, never a sniff
/// of the query text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Binds {
    /// No parameter: one unparametrized run per round.
    None,
    /// `$id` over the start ids `1..=N`.
    Start(u64),
    /// `$id` over the start ids `1..=N` and `$to`, the start id plus
    /// [`PAIR_OFFSET`]: the two endpoints of a shortest-path pair group.
    Pair(u64),
}

/// One measured group: its query, what it binds, and how many rounds each
/// start id runs.
struct Group {
    name: &'static str,
    query: &'static str,
    rounds: u64,
    binds: Binds,
}

/// The distance, in ids, from a pair group's start to its `$to` end: a
/// mid-range user on both fixture shapes (both hold at least 10,000
/// users), so each pair is a hub-head start against an arbitrary user.
const PAIR_OFFSET: u64 = 5000;

impl Group {
    /// The start ids one round runs: `Some(1..=N)` for a parameterised
    /// group, the single unparametrized `None` otherwise — the one place
    /// that knows the split.
    fn schedule(&self) -> impl Iterator<Item = Option<u64>> {
        let (last, parameterised) = match self.binds {
            Binds::None => (1, false),
            Binds::Start(n) | Binds::Pair(n) => (n, true),
        };
        (1..=last).map(move |s| parameterised.then_some(s))
    }

    /// The start id of the warm-up and `PROFILE` runs.
    fn first_start(&self) -> Option<u64> {
        self.schedule().next().expect("a schedule is never empty")
    }

    /// The parameters of one run at `start` (an item of [`Self::schedule`]):
    /// `$id`, and `$to` for a pair group — the one place that knows which
    /// groups bind a second end.
    fn params(&self, start: Option<u64>) -> BTreeMap<String, Value> {
        let int = |n: u64| Value::Int(i64::try_from(n).expect("start ids are small"));
        let mut params = BTreeMap::new();
        if let Some(s) = start {
            params.insert("id".to_owned(), int(s));
            if matches!(self.binds, Binds::Pair(_)) {
                params.insert("to".to_owned(), int(s + PAIR_OFFSET));
            }
        }
        params
    }
}

/// Start ids 1.. are the fixture's hub-heavy head (i^-0.4 weights), the
/// shape multi-hop expansion cost hangs on.
const GROUPS: [Group; 12] = [
    Group {
        name: "count",
        query: "MATCH (n:User) RETURN count(*)",
        rounds: 20,
        binds: Binds::None,
    },
    Group {
        name: "scan_agg_global",
        query: "MATCH (n:User) RETURN n.age, count(*) ORDER BY n.age",
        rounds: 20,
        binds: Binds::None,
    },
    Group {
        name: "expansion_3",
        query: "MATCH (a:User {id:$id})-[:Friend*3]->(b:User) RETURN count(b)",
        rounds: 2,
        binds: Binds::Start(64),
    },
    Group {
        name: "expansion_3_unrolled",
        query: "MATCH (a:User {id:$id})-[:Friend]->()-[:Friend]->()-[:Friend]->(b:User) RETURN count(b)",
        rounds: 2,
        binds: Binds::Start(64),
    },
    Group {
        name: "scan_expand_2hop_filter",
        query: "MATCH (a:User {id:$id})-[:Friend*2]->(b:User) WHERE b.age > 25 RETURN count(b)",
        rounds: 2,
        binds: Binds::Start(64),
    },
    Group {
        name: "shortest_pair",
        query: "MATCH p = SHORTEST 1 (a:User {id:$id})-[:Friend*..6]-(b:User {id:$to}) \
                RETURN count(p)",
        rounds: 2,
        binds: Binds::Pair(64),
    },
    Group {
        name: "shortest_path_fn",
        query: "MATCH p = shortestPath((a:User {id:$id})-[:Friend*..6]-(b:User {id:$to})) \
                RETURN count(p)",
        rounds: 2,
        binds: Binds::Pair(64),
    },
    Group {
        name: "shortest_expr",
        query: "MATCH (a:User {id:$id}), (b:User {id:$to}) \
                RETURN count(shortestPath((a)-[:Friend*..6]-(b)))",
        rounds: 2,
        binds: Binds::Pair(64),
    },
    Group {
        name: "all_shortest_paths",
        query: "MATCH p = allShortestPaths((a:User {id:$id})-[:Friend*..6]-(b:User {id:$to})) \
                RETURN count(p)",
        rounds: 2,
        binds: Binds::Pair(64),
    },
    Group {
        name: "shortest_postfix",
        query: "MATCH p = SHORTEST 1 (a:User {id:$id})-[:Friend]-{1,6}(b:User {id:$to}) \
                RETURN count(p)",
        rounds: 2,
        binds: Binds::Pair(64),
    },
    Group {
        name: "shortest_fan",
        query: "MATCH p = SHORTEST 1 (a:User {id:$id})-[:Friend*..3]-(b:User) RETURN count(p)",
        rounds: 2,
        binds: Binds::Start(16),
    },
    Group {
        name: "shortest_all_pairs",
        query: "MATCH (a:User), (b:User) \
                WHERE a.id IN range($id, $id + 7) AND b.id IN range($id + 100, $id + 107) \
                MATCH p = SHORTEST 1 (a)-[:Friend*..6]-(b) RETURN count(p)",
        rounds: 2,
        binds: Binds::Start(16),
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
    /// `expansion_3_unrolled`, `scan_expand_2hop_filter`, `shortest_pair`,
    /// `shortest_path_fn`, `shortest_expr`, `all_shortest_paths`,
    /// `shortest_postfix`, `shortest_fan`, `shortest_all_pairs`); every group
    /// when omitted.
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
    let out = profile_query(conn, g)?;
    println!("{}: {out:#?}", g.name);
    Ok(())
}

/// `PROFILE g.query` at the group's first start id.
fn profile_query(conn: &mut Connection, g: &Group) -> Result<QueryResult> {
    let params = g.params(g.first_start());
    conn.query_with_params(&format!("PROFILE {}", g.query), params)
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
/// in-process sampler, writing into `sample_dir`, when it is given.
fn run_timed(conn: &mut Connection, g: &Group, sample_dir: Option<&Path>) -> Result<()> {
    // One warm round of the group's own query — primes its plans and
    // caches so the measured rounds are steady-state. Propagated like
    // a measured round: a warm-up failure is a harness failure.
    warm(conn, g)?;
    // The plan evidence is recorded before the timed rounds, from the
    // warmed plan: a group that regressed to enumeration is visible here
    // even when its rounds never finish a sample.
    println!(
        "{}: plan {}",
        g.name,
        plan_evidence(&profile_query(conn, g)?)
    );
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
    let out = conn.query_with_params(g.query, g.params(start))?;
    match out.rows.first().and_then(|row| row.first()) {
        Some(Value::Int(n)) => Ok(*n),
        other => panic!(
            "pokec_profile: {:#?} did not answer one integer (got {other:?})",
            g.query
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest store both ends of every pair group exist in: users
    /// `1..=START + PAIR_OFFSET` need ids past [`PAIR_OFFSET`].
    const SHAPE: PokecShape = PokecShape {
        nodes: 5_200,
        rels: 26_000,
        seed: 0x706f_6b65_6301,
    };

    #[test]
    fn only_pair_groups_bind_the_second_end() {
        for g in &GROUPS {
            let params = g.params(g.first_start());
            assert_eq!(
                params.contains_key("to"),
                matches!(g.binds, Binds::Pair(_)),
                "{}",
                g.name
            );
            assert_eq!(
                params.contains_key("id"),
                g.binds != Binds::None,
                "{}",
                g.name
            );
        }
        let pair = GROUPS
            .iter()
            .find(|g| g.name == "shortest_pair")
            .expect("the pair group");
        let offset = i64::try_from(PAIR_OFFSET).expect("PAIR_OFFSET is small");
        assert_eq!(
            pair.params(Some(3)).get("to"),
            Some(&Value::Int(3 + offset))
        );
    }

    #[test]
    fn every_group_answers_one_integer_and_profiles_a_plan() {
        let db = open(SHAPE).expect("open the fixture");
        let mut conn = db.connect().expect("connect");
        for g in &GROUPS {
            run(&mut conn, g, g.first_start()).unwrap_or_else(|e| panic!("{}: {e}", g.name));
            let evidence = plan_evidence(&profile_query(&mut conn, g).expect("PROFILE"));
            assert!(
                evidence.contains("kernel="),
                "{}: plan evidence names a kernel slot: {evidence}",
                g.name
            );
        }
    }

    /// The shortest-path groups agree where they ask one question: the
    /// selector, the function, the postfix quantifier and the expression
    /// form on one pair find the same number of paths (one, or none when
    /// the pair is unconnected).
    #[test]
    fn the_pair_spellings_agree() {
        let db = open(SHAPE).expect("open the fixture");
        let mut conn = db.connect().expect("connect");
        let by_name = |name: &str| GROUPS.iter().find(|g| g.name == name).expect(name);
        let answers: Vec<i64> = ["shortest_pair", "shortest_path_fn", "shortest_postfix"]
            .iter()
            .map(|n| {
                let g = by_name(n);
                run(&mut conn, g, Some(1)).expect(n)
            })
            .collect();
        assert!(answers.iter().all(|a| *a == answers[0]), "{answers:?}");
        let expr = run(&mut conn, by_name("shortest_expr"), Some(1)).expect("expr");
        assert_eq!(expr, answers[0], "the expression form finds the same pair");
    }
}
