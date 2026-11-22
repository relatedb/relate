//! The update-shape measurement harness behind the delta-tier decisions
//! (ADR-0655, ADR-0658): how the delta tier and the record store behave
//! under the update shapes that stress them, before and after a fold. A
//! research-gated dev tool (`required-features = ["research"]`), never a
//! documented surface, never CI-gated.
//!
//! ```text
//! update_bench build <store>                          # shared committed store
//! update_bench big-tx <store> [concentrated|spread]   # one tx, 500k rels
//! update_bench small-tx <store> <threads> <stmts>     # concurrent small writes
//! update_bench mixed <store> <stmts> [hot]            # read+write per stmt
//! update_bench reads <store> <reps>                   # pure reads, committed
//! update_bench pinned <store>                         # one held reader + hot writes
//! update_bench reopen <store>                         # read-only open
//! ```
//!
//! Every number prints as `metric <name> <value>`; a load stamp heads each
//! run. `/usr/bin/time -l` around the process is the authoritative peak RSS.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use relate::{Connection, Database, DurabilityMode, Error, OpenMode, OpenOptions, Value};
use relate_datasets::report::{mib, nanos_to_millis};
use relate_query_latency::report::started_unix;
use relate_query_latency::run::Stats;
use relate_query_latency::{measured, open_store as open, rows_param};
use relate_test_support::rng::Rng;
use relate_test_support::sampling::{process_peak_rss_bytes, process_rss_bytes};

const NODES: i64 = 100_000;
const BATCH: usize = 10_000;

/// The one relationship-row statement every scenario seeds through — the row
/// contract is `s`/`e`/`k`.
const CREATE_KNOWS_ROWS: &str = "UNWIND $rows AS r MATCH (a) WHERE elementId(a) = r.s \
                                 MATCH (b) WHERE elementId(b) = r.e \
                                 CREATE (a)-[:KNOWS {k: r.k}]->(b) RETURN count(*) AS c";

/// Flush one batch of `CREATE_KNOWS_ROWS` rows, handing the buffer over —
/// the one BATCH-boundary spelling `seed_knows` and `big_tx` (and both
/// tails) share.
fn flush_rows(conn: &mut Connection, rows: &mut Vec<Value>) -> BenchResult<()> {
    conn.query_with_params(CREATE_KNOWS_ROWS, rows_param(std::mem::take(rows)))?;
    Ok(())
}

/// The hot-update statement `mixed` and `pinned` both time — a named
/// constant like its siblings, so the two scenarios cannot drift apart.
const SET_HOT_VALUE: &str =
    "MATCH (n:Person {i: $i}) SET n.v = coalesce(n.v, 0) + 1 RETURN n.v AS v";

/// The harness's one error: an engine failure or a harness refusal (a bad
/// argument, an unavailable sampler). `main` prints it as one line. The
/// crate's `Result<T, String>` would force a `map_err` on every engine
/// call; one `From` impl keeps every scenario's `?` as it reads.
enum BenchError {
    Engine(relate::Error),
    Message(String),
}

impl From<relate::Error> for BenchError {
    fn from(e: relate::Error) -> Self {
        Self::Engine(e)
    }
}

impl From<String> for BenchError {
    fn from(m: String) -> Self {
        Self::Message(m)
    }
}

impl fmt::Display for BenchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Engine(e) => e.fmt(f),
            Self::Message(m) => f.write_str(m),
        }
    }
}

type BenchResult<T> = Result<T, BenchError>;

/// Refuse a bad argv: the usage line, then exit 2 — the one policy for every
/// argument the harness cannot type.
fn usage() -> ! {
    eprintln!(
        "usage: update_bench build <store> | big-tx <store> [concentrated|spread] | \
         small-tx <store> <threads> <stmts> | mixed <store> <stmts> [hot] | \
         reads <store> <reps> | pinned <store> | reopen <store>"
    );
    std::process::exit(2);
}

fn num<T: FromStr>(arg: &str) -> T {
    arg.parse().unwrap_or_else(|_| usage())
}

fn stamp(scenario: &str) {
    println!(
        "stamp {scenario} epoch {} host {}",
        started_unix(),
        std::env::var("HOST").unwrap_or_else(|_| "unknown".into())
    );
}

fn opts_writable() -> OpenOptions {
    OpenOptions {
        durability_mode: Some(DurabilityMode::Snapshot),
        checkpoint_interval: Some(Duration::ZERO), // folds only on our explicit checkpoint
        ..OpenOptions::default()
    }
}

fn opts_readonly() -> OpenOptions {
    OpenOptions {
        mode: OpenMode::ReadOnly,
        ..OpenOptions::default()
    }
}

/// One statement's parameters from a pair slice — the parameter map's one
/// spelling, so no call site re-derives the map construction.
fn params(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

/// One row value from a pair slice — [`params`]' conversion, wrapped as
/// the row the batch statements read.
fn map_row(pairs: &[(&str, Value)]) -> Value {
    Value::Map(params(pairs).into())
}

/// Create the node set (`NODES` Person nodes) in batches, returning their
/// element ids. The count is the harness's own constant, not a parameter:
/// no caller can pass a subset under a batch shape sized for the whole set.
fn create_nodes(conn: &mut Connection) -> BenchResult<Vec<Arc<str>>> {
    conn.execute("CREATE INDEX FOR (p:Person) ON (p.i)")?;
    let stmt = "UNWIND $rows AS r CREATE (p:Person {i: r.i}) RETURN elementId(p) AS e";
    let mut ids = Vec::with_capacity(NODES as usize);
    for start in (0..NODES).step_by(BATCH) {
        let rows: Vec<Value> = (start..(start + BATCH as i64).min(NODES))
            .map(|i| map_row(&[("i", Value::Int(i))]))
            .collect();
        let r = conn.query_with_params(stmt, rows_param(rows))?;
        for row in &r.rows {
            ids.push(Arc::from(row[0].as_string().expect("elementId column")));
        }
    }
    Ok(ids)
}

/// Seed `rounds` deterministic KNOWS permutations over `ids`, batched: the
/// base adjacency `build` and `small_tx` both start from, `perm` spelling the
/// scenario's destination arithmetic.
fn seed_knows(
    conn: &mut Connection,
    ids: &[Arc<str>],
    rounds: i64,
    perm: impl Fn(i64, i64) -> i64,
) -> BenchResult<()> {
    let n = ids.len() as i64;
    for k in 0..rounds {
        // Each batch is built in place and handed over — the loader's
        // `rows_param(std::mem::take(&mut rows))` shape, never a
        // materialised round deep-cloned per chunk.
        let mut rows = Vec::with_capacity(BATCH);
        for i in 0..n {
            rows.push(map_row(&[
                ("s", Value::String(ids[i as usize].clone())),
                ("e", Value::String(ids[(perm(i, k) % n) as usize].clone())),
                ("k", Value::Int(k)),
            ]));
            if rows.len() == BATCH {
                flush_rows(conn, &mut rows)?;
            }
        }
        if !rows.is_empty() {
            flush_rows(conn, &mut rows)?;
        }
    }
    Ok(())
}

/// The read set: two degree probes and a label count. Each probe carries its
/// statement — no name match, so a new probe cannot silently run another
/// probe's query.
const PROBES: [(&str, &str); 3] = [
    (
        "degree",
        "MATCH (n:Person {i: $i})-[:KNOWS]->(m) RETURN count(m) AS c",
    ),
    (
        "reach2",
        "MATCH (n:Person {i: $i}) MATCH (n)-[:KNOWS*2]->(m) RETURN count(DISTINCT m) AS c",
    ),
    ("count_label", "MATCH (n:Person) RETURN count(n) AS c"),
];

/// Time the probes per statement, reported through the suite's own
/// [`Stats`] so these p50s are comparable with the published ones.
fn timed_reads(conn: &mut Connection, reps: usize, label: &str) -> BenchResult<()> {
    let mut rng = Rng(0xDEADBEEF);
    for (name, stmt) in PROBES {
        let mut times = Vec::with_capacity(reps);
        for _ in 0..reps {
            let i = rng.below(NODES as u64) as i64;
            // An unreferenced parameter is ignored (the engine has no
            // unused-parameter error), so one map serves every probe.
            let p = params(&[("i", Value::Int(i))]);
            let t = Instant::now();
            conn.query_with_params(stmt, p)?;
            times.push(t.elapsed());
        }
        let stats = Stats::of(&times);
        println!(
            "metric read_{label}_{name}_ms_mean {:.3}",
            nanos_to_millis(stats.mean_ns)
        );
        println!(
            "metric read_{label}_{name}_ms_p50 {:.3}",
            nanos_to_millis(stats.p50_ns)
        );
    }
    Ok(())
}

/// Print the adjacency + record-store lines of the census that matter here.
fn census_lines(tag: &str, c: &relate::HeapCensus) {
    println!("metric {tag}_used_bytes {}", c.used_bytes);
    for (name, side) in [("out", &c.out), ("in", &c.r#in)] {
        println!(
            "metric {tag}_{name}_delta_bytes {} nodes {} hops {} sealed_hops {} chunks {}",
            side.delta_bytes, side.nodes, side.hops, side.sealed_hops, side.chunks
        );
        println!(
            "metric {tag}_{name}_sealed_arena_bytes {} dead_bytes {}",
            side.arena_bytes, side.dead_bytes
        );
    }
    println!(
        "metric {tag}_node_chain_bytes {} inline {} spilled_slots {}",
        c.node_chain_bytes, c.node_chains.inline, c.node_chains.spilled_slots
    );
    println!(
        "metric {tag}_rel_chain_bytes {} inline {} spilled_slots {}",
        c.rel_chain_bytes, c.rel_chains.inline, c.rel_chains.spilled_slots
    );
    println!(
        "metric {tag}_node_record_bytes {} rel_record_bytes {}",
        c.node_record_bytes, c.rel_record_bytes
    );
}

fn rss(tag: &str) -> BenchResult<()> {
    println!(
        "metric {tag}_rss_mb {:.1}",
        mib(measured(process_rss_bytes(), tag)?)
    );
    Ok(())
}

// ------------------------------------------------------------------ build

fn build(store: &Path) -> BenchResult<()> {
    stamp("build");
    let db = open(store, opts_writable(), "creating")?;
    let mut conn = db.connect()?;
    let t = Instant::now();
    let ids = create_nodes(&mut conn)?;
    println!("metric build_nodes_s {:.2}", t.elapsed().as_secs_f64());

    // 300k KNOWS rels, spread: three deterministic permutations.
    let t = Instant::now();
    seed_knows(&mut conn, &ids, 3, |i, k| i * (7 + 6 * k) + 3 + 2 * k)?;
    println!("metric build_rels_s {:.2}", t.elapsed().as_secs_f64());
    let t = Instant::now();
    db.checkpoint()?;
    println!("metric build_checkpoint_s {:.2}", t.elapsed().as_secs_f64());
    census_lines("built", &db.sol_graph().heap_census());
    rss("built")?;
    Ok(())
}

// ------------------------------------------------------------------ big-tx

/// The two big-tx shapes: `concentrated` lands 5000 hops on each of 100 hub
/// sources (the delta tier's unbounded-`Vec` case); `spread` lands 5 on each
/// of 100k, the control that stays inside the ladder band.
#[derive(Clone, Copy, PartialEq)]
enum BigTxMode {
    Concentrated,
    Spread,
}

/// Every mode, the word it is named by, and its source-node count — the one
/// table `parse`, `tag` and `hubs` all read, so a mode cannot half-exist.
const MODES: [(&str, BigTxMode, i64); 2] = [
    ("concentrated", BigTxMode::Concentrated, 100),
    ("spread", BigTxMode::Spread, NODES),
];

impl BigTxMode {
    fn parse(word: &str) -> Option<Self> {
        MODES
            .iter()
            .find(|(tag, _, _)| *tag == word)
            .map(|&(_, mode, _)| mode)
    }

    /// The mode's own row — the one table read `tag` and `hubs` share, so
    /// the table is scanned in one spelling and the table-row invariant is
    /// stated once.
    fn row(self) -> (&'static str, i64) {
        MODES
            .iter()
            .find(|(_, mode, _)| *mode == self)
            .map(|&(tag, _, hubs)| (tag, hubs))
            .expect("every mode is a table row")
    }

    fn tag(self) -> &'static str {
        self.row().0
    }

    fn hubs(self) -> i64 {
        self.row().1
    }
}

fn big_tx(store: &Path, mode: BigTxMode) -> BenchResult<()> {
    stamp(&format!("big_tx_{}", mode.tag()));
    let db = open(store, opts_writable(), "creating")?;
    let mut conn = db.connect()?;
    let ids = create_nodes(&mut conn)?;
    rss("big_tx_after_nodes")?;

    // 500k rels in ONE transaction, in 10k-row batches inside begin..commit.
    let hubs = mode.hubs();
    let per_hub: i64 = 500_000 / hubs;
    conn.begin()?;
    let t = Instant::now();
    let mut batch_no = 0u64;
    let mut rows: Vec<Value> = Vec::with_capacity(BATCH);
    for h in 0..hubs {
        for j in 0..per_hub {
            let s = ids[h as usize].clone();
            let e = ids[((h.wrapping_mul(2654435761) + j * 40503) % NODES) as usize].clone();
            rows.push(map_row(&[
                ("s", Value::String(s)),
                ("e", Value::String(e)),
                ("k", Value::Int(j)),
            ]));
            if rows.len() == BATCH {
                let t2 = Instant::now();
                flush_rows(&mut conn, &mut rows)?;
                println!(
                    "metric big_tx_batch_{batch_no}_ms {}",
                    t2.elapsed().as_millis()
                );
                batch_no += 1;
            }
        }
    }
    if !rows.is_empty() {
        flush_rows(&mut conn, &mut rows)?;
    }
    println!("metric big_tx_write_s {:.2}", t.elapsed().as_secs_f64());
    rss("big_tx_pre_commit")?;
    conn.commit()?;
    rss("big_tx_post_commit")?;
    census_lines("bigtx_unfolded", &db.sol_graph().heap_census());
    // reads while the delta is live
    let mut rc = db.connect()?;
    timed_reads(&mut rc, 20, "prefold")?;
    let t = Instant::now();
    db.checkpoint()?;
    println!("metric big_tx_fold_s {:.2}", t.elapsed().as_secs_f64());
    census_lines("bigtx_folded", &db.sol_graph().heap_census());
    timed_reads(&mut rc, 20, "postfold")?;
    // Settle: further fold passes with no writes between — the stability
    // arm's two-pass hold is what seals a chunk that stopped growing.
    for pass in 2..=3u32 {
        let t = Instant::now();
        db.checkpoint()?;
        println!(
            "metric big_tx_fold{pass}_s {:.2}",
            t.elapsed().as_secs_f64()
        );
        census_lines(
            &format!("bigtx_settled{pass}"),
            &db.sol_graph().heap_census(),
        );
    }
    rss("big_tx_end")?;
    println!(
        "metric big_tx_peak_rss_mb {:.1}",
        mib(measured(process_peak_rss_bytes(), "big_tx: peak")?)
    );
    Ok(())
}

// ---------------------------------------------------------------- small-tx

/// The concurrent writers' shared counters.
#[derive(Clone, Default)]
struct Tally {
    done: Arc<AtomicU64>,
    retries: Arc<AtomicU64>,
    exhausted: Arc<AtomicU64>,
    errors: Arc<AtomicU64>,
}

/// One writer: from its own source span, `per_thread` CREATEs into a shared
/// destination range, retrying `Conflict`s (the engine's optimistic
/// admission). The counters are the shared report.
fn small_tx_writer(
    db: &Database,
    barrier: &Barrier,
    tally: &Tally,
    t: usize,
    threads: usize,
    per_thread: usize,
) -> BenchResult<()> {
    let mut conn = db.connect()?;
    let stmt = "MATCH (a:Person {i: $i}) MATCH (b:Person {i: $j}) \
                CREATE (a)-[:KNOWS {k: $k}]->(b)";
    let mut rng = Rng(0xC0FFEE + t as u64 * 7919);
    barrier.wait();
    let span = NODES as u64 / threads as u64;
    for n in 0..per_thread as u64 {
        // thread t writes FROM its own node span: disjoint sources,
        // shared destinations — the concurrent-append stress.
        let i = t as u64 * span + rng.below(span);
        let j = rng.below(NODES as u64);
        let p = params(&[
            ("i", Value::Int(i as i64)),
            ("j", Value::Int(j as i64)),
            ("k", Value::Int(1000 + n as i64)),
        ]);
        let mut attempt = 0u32;
        loop {
            match conn.query_with_params(stmt, p.clone()) {
                Ok(_) => {
                    tally.done.fetch_add(1, Ordering::Relaxed);
                    break;
                }
                Err(Error::Conflict) if attempt < 200 => {
                    tally.retries.fetch_add(1, Ordering::Relaxed);
                    attempt += 1;
                }
                Err(Error::Conflict) => {
                    // The retry budget spent is its own outcome, not the
                    // catch-all failure: the statement conflicted, it did
                    // not fail.
                    tally.exhausted.fetch_add(1, Ordering::Relaxed);
                    eprintln!("small_tx exhausted: 200 conflicts on one statement");
                    break;
                }
                Err(e) => {
                    tally.errors.fetch_add(1, Ordering::Relaxed);
                    eprintln!("small_tx err: {e}");
                    break;
                }
            }
        }
    }
    Ok(())
}

fn small_tx(store: &Path, threads: usize, per_thread: usize) -> BenchResult<()> {
    stamp("small_tx");
    let db = Arc::new(open(store, opts_writable(), "creating")?);
    {
        let mut conn = db.connect()?;
        let ids = create_nodes(&mut conn)?;
        // a base degree of 2 so small updates append to existing chunks
        seed_knows(&mut conn, &ids, 2, |i, k| i * (11 + 4 * k) + 7)?;
        db.checkpoint()?; // fold the base: small updates then land on sealed chunks
        census_lines("smalltx_base", &db.sol_graph().heap_census());
    }

    // main is the barrier's last participant: it releases the writers and
    // starts the clock in the same instant, so the window covers exactly the
    // write phase (not the tail of the spawn loop).
    let barrier = Arc::new(Barrier::new(threads + 1));
    let tally = Tally::default();
    let mut handles = Vec::new();
    for t in 0..threads {
        let db = db.clone();
        let barrier = barrier.clone();
        let tally = tally.clone();
        handles.push(std::thread::spawn(move || {
            small_tx_writer(&db, &barrier, &tally, t, threads, per_thread)
        }));
    }
    let t = Instant::now();
    barrier.wait();
    for h in handles {
        h.join().expect("writer thread")?;
    }
    let wall = t.elapsed().as_secs_f64();
    let ok = tally.done.load(Ordering::Relaxed);
    println!("metric small_tx_stmts {ok}");
    println!(
        "metric small_tx_retries {}",
        tally.retries.load(Ordering::Relaxed)
    );
    println!(
        "metric small_tx_exhausted {}",
        tally.exhausted.load(Ordering::Relaxed)
    );
    println!(
        "metric small_tx_errors {}",
        tally.errors.load(Ordering::Relaxed)
    );
    println!("metric small_tx_wall_s {wall:.2}");
    println!("metric small_tx_stmts_per_s {:.0}", ok as f64 / wall);
    rss("small_tx_pre_fold")?;
    census_lines("smalltx_unfolded", &db.sol_graph().heap_census());
    let mut rc = db.connect()?;
    timed_reads(&mut rc, 20, "prefold")?;
    let t = Instant::now();
    db.checkpoint()?;
    println!("metric small_tx_fold_s {:.2}", t.elapsed().as_secs_f64());
    census_lines("smalltx_folded", &db.sol_graph().heap_census());
    timed_reads(&mut rc, 20, "postfold")?;
    println!(
        "metric small_tx_peak_rss_mb {:.1}",
        mib(measured(process_peak_rss_bytes(), "small_tx: peak")?)
    );
    Ok(())
}

// ------------------------------------------------------------------- mixed

/// `range` is the update target count: the full node set, or the hot subset
/// for repeated updates of the same entities.
fn mixed(store: &Path, stmts: usize, range: u64) -> BenchResult<()> {
    stamp("mixed");
    let db = open(store, opts_writable(), "creating")?;
    let mut conn = db.connect()?;
    create_nodes(&mut conn)?;
    let mut times = Vec::with_capacity(stmts);
    let mut rng = Rng(0xABCDEF);
    for _ in 0..stmts {
        let p = params(&[("i", Value::Int(rng.below(range) as i64))]);
        let t = Instant::now();
        conn.query_with_params(SET_HOT_VALUE, p)?;
        times.push(t.elapsed());
    }
    let stats = Stats::of(&times);
    println!("metric mixed_p50_ms {:.3}", nanos_to_millis(stats.p50_ns));
    println!("metric mixed_p99_ms {:.3}", nanos_to_millis(stats.p99_ns));
    println!("metric mixed_mean_ms {:.3}", nanos_to_millis(stats.mean_ns));
    census_lines("mixed_unfolded", &db.sol_graph().heap_census());
    db.checkpoint()?;
    census_lines("mixed_folded", &db.sol_graph().heap_census());
    println!(
        "metric mixed_peak_rss_mb {:.1}",
        mib(measured(process_peak_rss_bytes(), "mixed: peak")?)
    );
    Ok(())
}

// ----------------------------------------------------------------- pinned

/// The pinned-reader boundary case: one reader holds a pre-update
/// snapshot open while 100k hot updates land on 1000 nodes — the version
/// retention the GC horizon cannot prune. Retained as a boundary
/// measurement only: a long-pinned reader is not a supported pattern for
/// an embedded local database, so no tier decision may cite it.
fn pinned(store: &Path) -> BenchResult<()> {
    stamp("pinned");
    let db = open(store, opts_writable(), "creating")?;
    let mut setup = db.connect()?;
    create_nodes(&mut setup)?;

    // The reader: begin + one read pins the snapshot; held open through
    // the whole write phase.
    let mut reader = db.connect()?;
    reader.begin()?;
    reader.query("MATCH (n:Person) RETURN count(n) AS c")?;

    // The writer: 100k SETs on the hot 1000.
    let mut writer = db.connect()?;
    let mut rng = Rng(0x1234);
    let t = Instant::now();
    for _ in 0..100_000 {
        let p = params(&[("i", Value::Int(rng.below(1000) as i64))]);
        writer.query_with_params(SET_HOT_VALUE, p)?;
    }
    println!("metric pinned_write_s {:.2}", t.elapsed().as_secs_f64());
    println!(
        "metric pinned_reader_count {}",
        reader
            .query("MATCH (n:Person) WHERE n.v IS NOT NULL RETURN count(n) AS c")?
            .rows[0][0]
            .as_int()
            .expect("count(*) returns an integer")
    );
    census_lines("pinned_held", &db.sol_graph().heap_census());
    rss("pinned_held")?;
    // Release the pin: the GC can prune, the checkpoint folds chains back.
    reader.commit()?;
    db.checkpoint()?;
    census_lines("pinned_released", &db.sol_graph().heap_census());
    println!(
        "metric pinned_peak_rss_mb {:.1}",
        mib(measured(process_peak_rss_bytes(), "pinned: peak")?)
    );
    Ok(())
}

// ------------------------------------------------------------------ reads

fn reads(store: &Path, reps: usize) -> BenchResult<()> {
    stamp("reads");
    let db = open(store, opts_readonly(), "opening read-only")?;
    let mut conn = db.connect()?;
    timed_reads(&mut conn, reps, "committed")?;
    census_lines("reads", &db.sol_graph().heap_census());
    Ok(())
}

// ----------------------------------------------------------------- reopen

fn reopen(store: &Path) -> BenchResult<()> {
    stamp("reopen");
    let base = measured(process_rss_bytes(), "reopen: base")?;
    let t = Instant::now();
    let db = open(store, opts_readonly(), "opening read-only")?;
    let open_s = t.elapsed().as_secs_f64();
    println!("metric reopen_s {open_s:.3}");
    rss("reopen")?;
    println!(
        "metric reopen_peak_rss_mb {:.1}",
        mib(measured(process_peak_rss_bytes(), "reopen: peak")?)
    );
    println!("metric reopen_base_rss_mb {:.1}", mib(base));
    census_lines("reopened", &db.sol_graph().heap_census());
    Ok(())
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    let res: BenchResult<()> = match args.as_slice() {
        ["build", a] => build(Path::new(a)),
        ["big-tx", a] => big_tx(Path::new(a), BigTxMode::Concentrated),
        ["big-tx", a, m] => {
            let Some(mode) = BigTxMode::parse(m) else {
                usage()
            };
            big_tx(Path::new(a), mode)
        }
        ["small-tx", a, t, n] => small_tx(Path::new(a), num(t), num(n)),
        ["mixed", a, n] => mixed(Path::new(a), num(n), NODES as u64),
        ["mixed", a, n, h] => mixed(Path::new(a), num(n), num(h)),
        ["reads", a, r] => reads(Path::new(a), num(r)),
        ["pinned", a] => pinned(Path::new(a)),
        ["reopen", a] => reopen(Path::new(a)),
        _ => usage(),
    };
    if let Err(e) = res {
        eprintln!("update_bench: {e}");
        std::process::exit(1);
    }
}
