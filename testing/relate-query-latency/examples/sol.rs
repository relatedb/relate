//! Speed-of-light harness — research-only (speed-of-light kernels):
//! compiled only under the `research` feature, not engine behavior.
//! Measures the gap between the real query engine and the hand-written
//! kernels in `relate::sol` on the graph-benchmark-ldbc SF0.1 workload, with the
//! suite's own methodology (checked warmup, then timed rounds until both
//! min-rounds and min-time, nearest-rank p50) and its own expected
//! answers as the correctness oracle.
//!
//! Run (release only — all timing is release, and the kernels are behind
//! the quarantine feature, ADR-0476):
//!
//! ```text
//! cargo run --release -p relate-query-latency --features research \
//!     --example sol [-- --store PATH]
//! ```
//!
//! The first run loads the data set into the store at `--store` (default
//! `target/query-latency/sol/graph-benchmark-ldbc.rl`) and checkpoints it; later runs open
//! it read-only.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use relate::sol::{self, SolRows};
use relate::{Connection, PlanNode, Value};
use relate_query_latency::answers::{compare, engine_rows};
use relate_query_latency::fetch::data_root;
use relate_query_latency::run::Stats;
use relate_query_latency::suite::{open_graph, GraphOptions};
use relate_query_latency::work_root;
use relate_query_latency::workload::{self, IndexProfile, ScaleFactor};

/// A query's engine timings (the answer itself is checked before timing).
struct EngineSide {
    wall: Stats,
    reported: Stats,
    rounds: usize,
}

/// One kernel variant: display name plus a boxed zero-argument call into
/// the kernels, borrowing the live graph handle for the run.
type KernelVariant<'a> = (&'static str, Box<dyn Fn() -> SolRows + 'a>);

/// One qid's kernel comparison: the variants to time, the LIMIT the
/// statement itself applies to the kernel's rows before the answer check,
/// and how the rows are printed for inspection.
struct KernelCase<'a> {
    variants: Vec<KernelVariant<'a>>,
    limit: Option<usize>,
    print: RowPrint,
}

/// The rows a variant prints for inspection: q19's grouped answer is
/// truncated to its first rows; the others print only when small.
#[derive(Clone, Copy)]
enum RowPrint {
    /// The first `n` rows, under the `kernel group` label.
    Group(usize),
    /// Every row, under the `kernel row` label, when there are at most `n`.
    AllUpTo(usize),
}

impl RowPrint {
    fn rows(&self, rows: &[Vec<Value>]) {
        match *self {
            RowPrint::Group(n) => {
                for row in rows.iter().take(n) {
                    println!("kernel group: {row:?}");
                }
            }
            RowPrint::AllUpTo(n) if rows.len() <= n => {
                for row in rows {
                    println!("kernel row: {row:?}");
                }
            }
            RowPrint::AllUpTo(_) => {}
        }
    }
}

fn main() {
    let mut store = work_root().join("sol/graph-benchmark-ldbc.rl");
    let mut min_time = Duration::from_secs(1);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--store" => store = PathBuf::from(args.next().expect("--store needs a path")),
            "--min-time" => {
                let secs: f64 = args
                    .next()
                    .expect("--min-time needs seconds")
                    .parse()
                    .expect("--min-time is a number of seconds");
                min_time = Duration::from_secs_f64(secs);
            }
            other => panic!("unknown argument {other:?} (only --store and --min-time)"),
        }
    }

    let sf = ScaleFactor::Sf0_1;
    let graph_opts = GraphOptions {
        sf,
        profile: IndexProfile::Upstream,
        store: Some(store),
        data_root: data_root(),
        download: true,
        ..GraphOptions::default()
    };
    let (db, _origin) = open_graph(&graph_opts).unwrap_or_else(|e| panic!("open graph: {e}"));
    let mut conn = db.connect().expect("connecting");
    let graph = db.sol_graph();

    context_counts(&mut conn);
    birthday_differential(&mut conn);

    let expected = workload::expected(sf).unwrap_or_else(|e| panic!("expected answers: {e}"));

    for qid in [19u32, 8, 30] {
        let q = workload::query(qid, sf).unwrap_or_else(|e| panic!("q{qid}: {e}"));
        let exp = expected.get(&qid).expect("expected answer");
        println!("\n=== q{qid} ===");
        println!("{}", q.text);

        // Engine: checked warmup, then timed rounds.
        let first = conn
            .query(&q.text)
            .unwrap_or_else(|e| panic!("q{qid}: {e}"));
        let got = engine_rows(&first.header.columns, &first.rows);
        match compare(&got, exp) {
            None => println!("engine answer: ok ({} rows)", first.rows.len()),
            Some(diff) => println!("engine answer: WRONG: {diff}"),
        }
        for row in &first.rows {
            println!("engine row: {row:?}");
        }
        drop(first);
        let engine = time_engine(&mut conn, &q.text, min_time);
        println!(
            "engine : p50 {:>10.3} ms  mean {:>10.3} ms  (reported p50 {:>10.3} ms, {} rounds)",
            engine.wall.p50_ns as f64 / 1e6,
            engine.wall.mean_ns as f64 / 1e6,
            engine.reported.p50_ns as f64 / 1e6,
            engine.rounds,
        );
        profile_plan(&mut conn, &q.text);
        // Front-end only: EXPLAIN plans without executing, isolating the
        // per-statement parse+plan cost the timed rounds re-pay.
        let explain = time_explain(&mut conn, &q.text, min_time);
        println!(
            "explain : p50 {:>10.3} ms  mean {:>10.3} ms  (parse+plan only, no execution)",
            explain.p50_ns as f64 / 1e6,
            explain.mean_ns as f64 / 1e6,
        );

        // Kernel variants: checked warmup, then timed rounds. One descriptor
        // per qid, so a new per-query difference is a field here rather than
        // another `qid` check in the loop.
        let case = match qid {
            19 => KernelCase {
                variants: vec![
                    ("kernel tag-first    ", Box::new(|| sol::q19(graph, true))),
                    ("kernel comment-first", Box::new(|| sol::q19(graph, false))),
                ],
                limit: Some(1),
                print: RowPrint::Group(3),
            },
            8 => KernelCase {
                variants: vec![
                    ("kernel forum-first  ", Box::new(|| sol::q8(graph, true))),
                    ("kernel person-first ", Box::new(|| sol::q8(graph, false))),
                ],
                limit: None,
                print: RowPrint::AllUpTo(3),
            },
            30 => KernelCase {
                variants: vec![
                    ("kernel full-count   ", Box::new(|| sol::q30(graph, false))),
                    ("kernel early-exit   ", Box::new(|| sol::q30(graph, true))),
                ],
                limit: None,
                print: RowPrint::AllUpTo(3),
            },
            _ => continue,
        };
        for (name, kernel) in case.variants {
            let warm = kernel();
            let mut got = engine_rows(&warm.columns, &warm.rows);
            // The query's LIMIT (q19) is applied here, mirroring the statement.
            if let Some(limit) = case.limit {
                got.truncate(limit);
            }
            let verdict = match compare(&got, exp) {
                None => "ok".to_string(),
                Some(diff) => format!("WRONG: {diff}"),
            };
            case.print.rows(&warm.rows);
            let stats = time_kernel(kernel.as_ref(), min_time);
            let ratio = engine.wall.p50_ns as f64 / stats.p50_ns.max(1) as f64;
            println!(
                "{name}: p50 {:>10.3} ms  mean {:>10.3} ms  ratio {:>7.1}x  [{verdict}]",
                stats.p50_ns as f64 / 1e6,
                stats.mean_ns as f64 / 1e6,
                ratio,
            );
        }
    }
}

/// The engine's own per-operator attribution: one `PROFILE <statement>`
/// run, printing the plan tree with rows, db hits and elapsed time per
/// operator.
fn profile_plan(conn: &mut Connection, text: &str) {
    let r = conn
        .query(&format!("PROFILE {text}"))
        .expect("PROFILE statement");
    let Some(artifacts) = r.plan.as_deref() else {
        println!("PROFILE: no plan stamped");
        return;
    };
    println!("PROFILE plan (rows / db hits / time per operator):");
    print_node(&artifacts.root, 0);
}

fn print_node(node: &PlanNode, depth: usize) {
    let metrics = node.metrics.map(|m| {
        format!(
            "rows={} db_hits={} time={:.3} ms start={:?}",
            m.rows,
            m.db_hits,
            m.time_ns as f64 / 1e6,
            m.start
        )
    });
    println!(
        "{}{} {}{}",
        "  ".repeat(depth),
        node.operator,
        node.details,
        metrics.map(|m| format!("  [{m}]")).unwrap_or_default()
    );
    for child in &node.children {
        print_node(child, depth + 1);
    }
}

/// Context for judging traversal choice: the labels and rel types the
/// three queries touch.
fn context_counts(conn: &mut Connection) {
    println!("context counts:");
    for label in ["Comment", "Tag", "Place", "Forum", "Person", "Post"] {
        let r = conn
            .query(&format!("MATCH (n:{label}) RETURN count(n) AS c"))
            .expect("label count");
        println!("  {label:<10} {}", r.rows[0][0]);
    }
    for rel_type in [
        "commentHasTag",
        "commentIsLocatedIn",
        "hasModerator",
        "commentHasCreator",
        "replyOfPost",
        "postHasCreator",
    ] {
        let r = conn
            .query(&format!("MATCH ()-[r:{rel_type}]->() RETURN count(r) AS c"))
            .expect("rel count");
        println!("  {rel_type:<20} {}", r.rows[0][0]);
    }
}

/// The q8 birthday assumption: lexicographic order on the data set's
/// padded ISO strings equals `date()` order.
fn birthday_differential(conn: &mut Connection) {
    let as_date = conn
        .query(
            r#"MATCH (p:Person) WHERE date(p.birthday) > date("1990-01-01") RETURN count(p) AS c"#,
        )
        .expect("date count");
    let as_str = conn
        .query(r#"MATCH (p:Person) WHERE p.birthday > "1990-01-01" RETURN count(p) AS c"#)
        .expect("string count");
    let (a, b) = (as_date.rows[0][0].clone(), as_str.rows[0][0].clone());
    println!("birthday differential: date() {a:?} vs string {b:?}");
}

/// The engine side: warmup already done by the caller; rounds until both
/// `min_time` and 5 rounds, wall time plus the engine-reported statement
/// elapsed per round.
fn time_engine(conn: &mut Connection, text: &str, min_time: Duration) -> EngineSide {
    let (mut wall, mut reported) = (Vec::new(), Vec::new());
    let mut spent = Duration::ZERO;
    while wall.len() < 100_000 && (wall.len() < 5 || spent < min_time) {
        let t = Instant::now();
        let r = conn.query(text).expect("engine round");
        let d = t.elapsed();
        reported.push(r.header.elapsed);
        wall.push(d);
        spent += d;
    }
    EngineSide {
        wall: Stats::of(&wall),
        reported: Stats::of(&reported),
        rounds: wall.len(),
    }
}

/// The statement's front-end cost: `EXPLAIN` re-parses and re-plans the
/// statement every round without executing it.
fn time_explain(conn: &mut Connection, text: &str, min_time: Duration) -> Stats {
    let explained = format!("EXPLAIN {text}");
    let mut samples = Vec::new();
    let mut spent = Duration::ZERO;
    while samples.len() < 100_000 && (samples.len() < 5 || spent < min_time) {
        let t = Instant::now();
        let r = conn.query(&explained).expect("EXPLAIN round");
        let d = t.elapsed();
        std::hint::black_box(r.rows.len());
        samples.push(d);
        spent += d;
    }
    Stats::of(&samples)
}

/// One kernel variant: warmup, then rounds with the same shape as the
/// engine loop. The returned rows are consumed (black-boxed) so the
/// optimizer cannot elide the call.
fn time_kernel(kernel: &(dyn Fn() -> SolRows), min_time: Duration) -> Stats {
    // Warmup.
    let warm = kernel();
    std::hint::black_box(&warm);
    drop(warm);

    let mut samples = Vec::new();
    let mut spent = Duration::ZERO;
    while samples.len() < 100_000 && (samples.len() < 5 || spent < min_time) {
        let t = Instant::now();
        let out = kernel();
        let d = t.elapsed();
        std::hint::black_box(out.rows.len());
        samples.push(d);
        spent += d;
    }
    Stats::of(&samples)
}
