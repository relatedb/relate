//! Statement-handoff micro-harness (REL8-1424): isolates the per-statement
//! cost of the query-worker handoff an unasserted connection pays
//! (ADR-0259's publish-and-wait), on the current engine, without Python in
//! the loop.
//!
//! The recipe is the ticket's E2 verification: loop
//! `Connection::query_with_params` over the sqlite-gate PR workload's
//! 8-statement brief mix (verbatim, `pr_mix` in the private gate's
//! `workloads.py`, plus a `RETURN 1` floor statement as the harness
//! control) in two modes, interleaved round by round:
//!
//! - **(a) default** — an unasserted connection on the process's main
//!   thread, exactly what the Python wheel, the FFI surface and the CLI
//!   drive: a cache hit the ADR-0663 containment rule admits runs inline,
//!   everything else takes the worker handoff.
//! - **(b) asserted** — a connection on a thread spawned with the engine's
//!   exported `QUERY_WORKER_STACK_BYTES` budget after
//!   `assume_worker_stack_on_caller_thread()` (ADR-0504): bounded work runs
//!   inline, everything else escalates with the admission in hand.
//!
//! `(a) - (b)` per statement is the handoff cost on this machine — the
//! statement classes split visibly: statements both modes serve inline
//! (warm shallow-bounded cache hits) show ~0, statements only (b) serves
//! inline (cold bounded reads, hits the containment rule declines) show the
//! full handoff, and statements both modes route to the worker (the
//! var-length `neighbourhood` statement, every write) show ~0 **because
//! both pay the handoff** — a cost this diff cannot see and the report
//! names explicitly. The gap is a signed number: near-zero noise on either
//! side of zero is the class evidence, so it is never clamped.
//!
//! Two phases, both interleaved so machine drift cancels within a round:
//!
//! - **cold** — one mix pass on each of `--cold-stores` fresh stores per
//!   mode (the plan cache is per-store, so every pass is first-execution):
//!   the pure pre-ADR-0663 handoff reading, still what a cold statement
//!   pays today.
//! - **warm** — `--rounds` mix passes per mode after `--warmup` untimed
//!   passes: what a warm mix still pays.
//!
//! The graph is an evidence-shaped store sized to the gate's smallest band
//! (`le40`) — the kinds, properties and rel types the mix reads:
//! Prompt/Plan/Edit/Hunk/Commit/Claim/Run/File with `id` properties,
//! PLANNED/PRODUCED/HAS_HUNK/IN_COMMIT/ABOUT/TOUCHES/VERIFIES/REVISES. The
//! handoff term is data-independent, so the shape matters only for the
//! statements' plans, not their row counts. A statement that answers zero
//! rows fails the harness loudly: the mix's statements are all non-empty
//! on the gate's graphs.
//!
//! ```text
//! cargo build --release -p relate-query-latency --bin handoff_bench
//! target/release/handoff_bench --rounds 1500 --cold-stores 40
//! ```
//!
//! A dev tool (not CI-gated), like `plan_bench`: time it in a quiet window
//! (the repo's measurement protocol) — the figures are wall-clock medians
//! at microsecond scale and CPU contention lands in them directly.

use std::collections::BTreeMap;
use std::hint::black_box;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use relate::{Connection, Database, Value, QUERY_WORKER_STACK_BYTES};
// The canonical percentile (the one every other report computes) — a
// benchmark with its own rank rule measures a different statistic.
use relate_test_support::percentile;

/// The string parameter map's one spelling — `&str` pairs straight into
/// `Arc<str>` payloads, no call site re-derives the conversion (the
/// `examples/update_bench.rs::params` convention).
fn string_params(pairs: &[(&str, &str)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), Value::String((*v).into())))
        .collect()
}

/// One statement of the brief mix: its harness name, its Cypher spelling
/// (verbatim from the gate's `pr_mix`), its explicit parameters, and its
/// route class — the single source the report's class column prints, so
/// the per-statement reading is derived from `MIX`, never re-listed in
/// prose at the use site.
struct MixStmt {
    name: &'static str,
    cypher: &'static str,
    params: &'static [(&'static str, &'static str)],
    class: &'static str,
}

/// The PR workload's brief mix, verbatim, with the floor statement first.
/// Parameterized statements bind the same values every call (`$c` the
/// commit `M1`, `$h` the hunk `H1`) — the values the graph below gives
/// every conjunct of every statement a non-empty answer.
const MIX: &[MixStmt] = &[
    MixStmt {
        name: "floor",
        cypher: "RETURN 1 AS one",
        params: &[],
        class: "declined: auto-param hit",
    },
    MixStmt {
        name: "kinds",
        cypher: "MATCH (n) RETURN labels(n)[0] AS kind, count(*) AS c ORDER BY kind",
        params: &[],
        class: "declined: auto-param hit",
    },
    MixStmt {
        name: "provenance",
        cypher: "MATCH (p:Prompt)-[:PLANNED]->(:Plan)-[:PRODUCED]->(:Edit)-[:HAS_HUNK]->(h:Hunk)-[:IN_COMMIT]->(c:Commit {id: $c}) \
                 RETURN DISTINCT p.id AS prompt ORDER BY prompt",
        params: &[("c", "M1")],
        class: "worker-both: 5 fixed hops",
    },
    MixStmt {
        name: "claims",
        cypher: "MATCH (cl:Claim)-[:ABOUT]->(h:Hunk)-[:IN_COMMIT]->(c:Commit) \
                 RETURN c.id AS commit_id, count(DISTINCT cl) AS claims ORDER BY claims DESC, commit_id LIMIT 5",
        params: &[],
        class: "worker-both: 2 hops",
    },
    MixStmt {
        name: "owners",
        cypher: "MATCH (e:Edit)-[:TOUCHES]->(f:File) \
                 RETURN f.id AS file, count(*) AS edits ORDER BY edits DESC, file LIMIT 5",
        params: &[],
        class: "declined: auto-param hit",
    },
    MixStmt {
        name: "runs",
        cypher: "MATCH (r:Run)-[:VERIFIES]->(c:Commit {id: $c}) RETURN r.id AS run ORDER BY run",
        params: &[("c", "M1")],
        class: "inline-both: raw-keyed 1 hop",
    },
    MixStmt {
        name: "neighbourhood",
        cypher: "MATCH (h:Hunk {id: $h})-[*1..2]-(n) WHERE n <> h WITH DISTINCT n RETURN count(*) AS n",
        params: &[("h", "H1")],
        class: "worker-both: var-length",
    },
    MixStmt {
        name: "lineage",
        cypher: "MATCH (e:Edit)-[:REVISES]->(:Edit) RETURN count(DISTINCT e) AS revised",
        params: &[],
        class: "inline-both: raw-keyed 1 hop",
    },
    MixStmt {
        name: "files",
        cypher: "MATCH (c:Commit {id: $c})-[:TOUCHES]->(f:File) RETURN f.id AS file ORDER BY file",
        params: &[("c", "M1")],
        class: "inline-both: param-keyed 1 hop",
    },
];

/// `(label, id)` pairs for the evidence-shaped store: 2 Prompt, 3 Plan,
/// 5 Edit, 7 Hunk, 4 Commit, 3 Claim, 3 Run, 5 File — the gate's smallest
/// band, every kind the mix reads.
const NODES: &[(&str, &str)] = &[
    ("Prompt", "S1"),
    ("Prompt", "S2"),
    ("Plan", "P1"),
    ("Plan", "P2"),
    ("Plan", "P3"),
    ("Edit", "E1"),
    ("Edit", "E2"),
    ("Edit", "E3"),
    ("Edit", "E4"),
    ("Edit", "E5"),
    ("Hunk", "H1"),
    ("Hunk", "H2"),
    ("Hunk", "H3"),
    ("Hunk", "H4"),
    ("Hunk", "H5"),
    ("Hunk", "H6"),
    ("Hunk", "H7"),
    ("Commit", "M1"),
    ("Commit", "M2"),
    ("Commit", "M3"),
    ("Commit", "M4"),
    ("Claim", "C1"),
    ("Claim", "C2"),
    ("Claim", "C3"),
    ("Run", "R1"),
    ("Run", "R2"),
    ("Run", "R3"),
    ("File", "F1"),
    ("File", "F2"),
    ("File", "F3"),
    ("File", "F4"),
    ("File", "F5"),
];

/// `(source id, rel type, target id)` triples: one pattern per CREATE, so
/// no statement depends on the multi-pattern CREATE shape, and every mix
/// conjunct non-empty — every Commit has hunks, verification runs and
/// touched files; every hunk lineage and claims; the provenance chain
/// reaches `M1`; the neighbourhood reaches both a 1-hop and a 2-hop
/// neighbour.
const EDGES: &[(&str, &str, &str)] = &[
    ("S1", "PLANNED", "P1"),
    ("S2", "PLANNED", "P2"),
    ("S2", "PLANNED", "P3"),
    ("P1", "PRODUCED", "E1"),
    ("P1", "PRODUCED", "E2"),
    ("P2", "PRODUCED", "E3"),
    ("P3", "PRODUCED", "E4"),
    ("P3", "PRODUCED", "E5"),
    ("E1", "HAS_HUNK", "H1"),
    ("E1", "HAS_HUNK", "H2"),
    ("E2", "HAS_HUNK", "H3"),
    ("E3", "HAS_HUNK", "H4"),
    ("E4", "HAS_HUNK", "H5"),
    ("E5", "HAS_HUNK", "H6"),
    ("E5", "HAS_HUNK", "H7"),
    ("H1", "IN_COMMIT", "M1"),
    ("H2", "IN_COMMIT", "M1"),
    ("H3", "IN_COMMIT", "M1"),
    ("H4", "IN_COMMIT", "M2"),
    ("H5", "IN_COMMIT", "M2"),
    ("H6", "IN_COMMIT", "M3"),
    ("H7", "IN_COMMIT", "M4"),
    ("C1", "ABOUT", "H1"),
    ("C2", "ABOUT", "H2"),
    ("C3", "ABOUT", "H4"),
    ("E1", "TOUCHES", "F1"),
    ("E1", "TOUCHES", "F2"),
    ("E2", "TOUCHES", "F2"),
    ("E3", "TOUCHES", "F3"),
    ("E4", "TOUCHES", "F4"),
    ("E5", "TOUCHES", "F5"),
    ("R1", "VERIFIES", "M1"),
    ("R2", "VERIFIES", "M2"),
    ("R3", "VERIFIES", "M3"),
    ("E2", "REVISES", "E1"),
    ("E4", "REVISES", "E3"),
    ("M1", "TOUCHES", "F1"),
    ("M2", "TOUCHES", "F3"),
    ("M3", "TOUCHES", "F5"),
];

/// The worker primer: one cheap statement that routes to the query worker
/// in BOTH modes (`UNWIND` is unbounded row potential, so even an asserted
/// connection escalates it), run untimed before a cold pass. A fresh
/// connection spawns its 64 MiB worker on the first worker-routed
/// statement — a ~0.4-0.5 ms one-time cost that would otherwise land on
/// the first mix statement the mode routes there (the floor statement in
/// the default mode, the first escalation in the asserted one) and swamp
/// the ~µs handoff term the cold phase exists to isolate. The primer is
/// not part of the mix, so its cache entry cannot serve any mix statement.
const WORKER_PRIMER: &str = "UNWIND [0] AS x RETURN x";

/// One loading statement, the one failure spelling: run it and name the
/// context in the panic, so the node and edge loops cannot drift apart.
fn run_load(conn: &mut Connection, stmt: String, params: BTreeMap<String, Value>, ctx: &str) {
    conn.query_with_params(&stmt, params)
        .unwrap_or_else(|e| panic!("{ctx}: {e}"));
}

/// Build one fresh evidence-shaped in-memory store (loading pays whatever
/// route the loading connection takes; it is never timed).
fn build_store() -> Database {
    let db = Database::open(":memory:").expect("open scratch store");
    let mut conn = db.connect().expect("connect loading session");
    for (label, id) in NODES {
        run_load(
            &mut conn,
            format!("CREATE (:{label} {{id: $id}})"),
            string_params(&[("id", id)]),
            &format!("create {label} {id}"),
        );
    }
    for (a, ty, b) in EDGES {
        run_load(
            &mut conn,
            format!("MATCH (a {{id: $a}}), (b {{id: $b}}) CREATE (a)-[:{ty}]->(b)"),
            string_params(&[("a", a), ("b", b)]),
            &format!("create edge {a}-{ty}->{b}"),
        );
    }
    db
}

/// Validate the store: every mix statement answers at least one row.
fn validate_mix(conn: &mut Connection) {
    for stmt in MIX {
        let result = conn
            .query_with_params(stmt.cypher, string_params(stmt.params))
            .unwrap_or_else(|e| panic!("mix statement {} failed: {e}", stmt.name));
        assert!(
            !result.rows.is_empty(),
            "mix statement {} answered zero rows: the store shape broke",
            stmt.name
        );
    }
}

/// Run the mix once, returning one wall-clock duration per statement.
/// The result is black-boxed inside the window so nothing is optimized
/// away, and the `expect` sits outside it.
fn time_mix(conn: &mut Connection) -> Vec<Duration> {
    MIX.iter()
        .map(|stmt| {
            let params = string_params(stmt.params);
            let start = Instant::now();
            let result = conn.query_with_params(stmt.cypher, params);
            let elapsed = start.elapsed();
            let result = result.unwrap_or_else(|e| panic!("{} failed: {e}", stmt.name));
            black_box(&result.rows);
            elapsed
        })
        .collect()
}

/// `passes` untimed mix runs on `conn` — the warm-up loop both modes run
/// before their timed phase, one spelling so the two sides cannot drift.
fn warmup_passes(conn: &mut Connection, passes: usize) {
    for _ in 0..passes {
        black_box(time_mix(conn));
    }
}

/// The harness's two modes, the axis everything else threads through:
/// **(a) Default** — an unasserted connection on the thread the host gave
/// it (the main thread here; what the Python wheel, the FFI surface and
/// the CLI drive) — and **(b) Asserted** — a connection on a thread
/// spawned with the engine's exported `QUERY_WORKER_STACK_BYTES` budget
/// (the one `thread::Builder` site in `main`) after
/// `assume_worker_stack_on_caller_thread()` (ADR-0504), the shape a
/// serving host that CAN assert spawns. The mode owns the axis's one
/// connection-level spelling — the exhaustive match below, so a third
/// variant would fail to compile rather than silently become a no-op —
/// never a repeated conditional or a naked boolean.
enum Mode {
    Default,
    Asserted,
}

impl Mode {
    /// Apply the mode's connection-level precondition: the asserted mode
    /// asserts the query worker's own stack budget (ADR-0504's fast path);
    /// the default mode asserts nothing — that is the entire thing the
    /// harness measures. Exhaustive, not `matches!`: a new variant must
    /// carry its own decision here.
    fn assume_stack(&self, conn: &mut Connection) {
        match self {
            Mode::Asserted => conn.assume_worker_stack_on_caller_thread(),
            Mode::Default => {}
        }
    }
}

/// One mode's warm bring-up, the single spelling both sides call: build
/// the store, connect, apply the mode's connection recipe, validate the
/// mix, run the untimed warm-up passes. A step added here lands on both
/// modes together — the drift guard `warmup_passes` alone could not give,
/// with the bring-up hand-spelled per side.
fn warm_connection(warmup: usize, mode: Mode) -> Connection {
    let mut conn = build_store().connect().expect("warm connect");
    mode.assume_stack(&mut conn);
    validate_mix(&mut conn);
    warmup_passes(&mut conn, warmup);
    conn
}

/// One mode's cold connection on its own fresh store: connect, apply the
/// mode's recipe, and prime the per-connection worker spawn away — the
/// ~0.5 ms one-time cost would otherwise land on the first mix statement
/// the mode routes to the worker (the primer statement is not part of the
/// mix, so its cache entry cannot serve any mix statement).
fn cold_connection(mode: Mode) -> Connection {
    let mut conn = build_store().connect().expect("cold connect");
    mode.assume_stack(&mut conn);
    black_box(conn.query(WORKER_PRIMER).expect("worker primer"));
    conn
}

/// One statement's samples across a phase's rounds, both modes side by
/// side — one row per mix statement, so the two modes' slots for a
/// statement cannot drift apart in any traversal.
#[derive(Default)]
struct Sample {
    a: Vec<Duration>,
    b: Vec<Duration>,
}

/// One phase's samples: one row per mix statement, filed by
/// `Sample::record` once per interleaved round.
struct Samples {
    rows: Vec<Sample>,
}

impl Samples {
    /// Empty rows, one per mix statement.
    fn new() -> Self {
        Self {
            rows: MIX.iter().map(|_| Sample::default()).collect(),
        }
    }

    /// File one round's timings: mode (a) from the main thread, mode (b)
    /// from the asserted thread's reply. One timing per mix statement is
    /// the invariant the zip relies on, so it is asserted here — the one
    /// place it could break — instead of letting a short side silently
    /// truncate its row.
    fn record(&mut self, a: Vec<Duration>, b: Vec<Duration>) {
        assert_eq!(a.len(), MIX.len(), "one timing per mix statement (a)");
        assert_eq!(b.len(), MIX.len(), "one timing per mix statement (b)");
        for (row, (a_dt, b_dt)) in self.rows.iter_mut().zip(a.into_iter().zip(b)) {
            row.a.push(a_dt);
            row.b.push(b_dt);
        }
    }
}

/// One interleaved round, the shape both phases share: time the mix on
/// mode (a)'s `conn`, hand `cmd` to the asserted thread, wait for its
/// timings, file both. The mode-(b) connection (warm or fresh-cold, per
/// the command) is the thread's own business, inside `run_asserted_mode`.
fn interleaved_round(
    cmd: AssertedCmd,
    conn: &mut Connection,
    cmd_tx: &Sender<AssertedCmd>,
    reply_rx: &Receiver<Vec<Duration>>,
    samples: &mut Samples,
) {
    let timings = time_mix(conn);
    cmd_tx.send(cmd).expect("asserted thread alive");
    let reply = reply_rx.recv().expect("asserted reply");
    samples.record(timings, reply);
}

/// The asserted mode's commands (the mode runs on its own 64 MiB thread
/// for the whole run; one channel round trip per interleaved round).
enum AssertedCmd {
    /// One mix pass on the warm connection.
    WarmRound,
    /// One mix pass on a fresh store and a fresh asserted connection.
    ColdRound,
}

/// The asserted mode's whole run, on its own 64 MiB thread: a warm
/// asserted connection for the warm phase, and a fresh asserted cold
/// connection for every cold round — both through the shared factories,
/// so a bring-up change lands on both modes together. The two arms differ
/// only in which connection they time against; the reply is sent once,
/// after the match, so the arms cannot drift apart. `ready` signals once
/// the warm bring-up is done: main blocks on it before the first timed
/// round, so neither side's untimed warm-up can overlap the other's timed
/// sampling.
fn run_asserted_mode(
    warmup: usize,
    cmd_rx: Receiver<AssertedCmd>,
    reply_tx: Sender<Vec<Duration>>,
    ready_tx: Sender<()>,
) {
    let mut warm_conn = warm_connection(warmup, Mode::Asserted);
    ready_tx
        .send(())
        .expect("main thread waiting for readiness");
    for cmd in cmd_rx {
        let timings = match cmd {
            AssertedCmd::WarmRound => time_mix(&mut warm_conn),
            AssertedCmd::ColdRound => time_mix(&mut cold_connection(Mode::Asserted)),
        };
        reply_tx.send(timings).expect("main thread still receiving");
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "handoff_bench",
    about = "statement-handoff micro-harness: the sqlite-gate PR brief mix, \
             (a) default-thread unasserted vs (b) asserted 64 MiB thread, \
             interleaved (REL8-1424's E2 recipe; dev tool, not CI-gated)"
)]
struct Args {
    /// Timed mix passes per mode in the warm phase (at least one — the
    /// report's percentile asserts non-empty samples).
    #[arg(
        long,
        default_value_t = 1500,
        value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..)
    )]
    rounds: usize,
    /// Untimed mix passes per mode before the warm phase (zero allowed).
    #[arg(long, default_value_t = 200)]
    warmup: usize,
    /// Fresh stores per mode in the cold phase, one mix pass each (at
    /// least one — the report's percentile asserts non-empty samples).
    #[arg(
        long,
        default_value_t = 40,
        value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..)
    )]
    cold_stores: usize,
}

fn main() {
    let args = Args::parse();
    eprintln!(
        "handoff_bench: {} statements, {}-node store, rounds {} (warmup {}), cold stores {}",
        MIX.len(),
        NODES.len(),
        args.rounds,
        args.warmup,
        args.cold_stores
    );

    // The asserted mode's thread, on the worker's own exported stack
    // budget — the precondition `Mode::Asserted::assume_stack` promises on
    // the connection, made real for the thread it runs on (the shape a
    // serving host that CAN assert spawns, the Bolt surface). One command
    // per interleaved round.
    let (cmd_tx, cmd_rx) = mpsc::channel::<AssertedCmd>();
    let (reply_tx, reply_rx) = mpsc::channel::<Vec<Duration>>();
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let asserted = thread::Builder::new()
        .stack_size(QUERY_WORKER_STACK_BYTES)
        .spawn(move || run_asserted_mode(args.warmup, cmd_rx, reply_tx, ready_tx))
        .expect("spawn asserted-mode thread");

    // Mode (a): the process's own main thread, unasserted — what the
    // Python wheel, the FFI surface and the CLI drive.
    let mut warm_conn = warm_connection(args.warmup, Mode::Default);
    // Both warm-ups done before any timed round: the asserted thread's
    // untimed bring-up must not overlap this side's timed sampling.
    ready_rx.recv().expect("asserted mode reports readiness");

    // Cold phase, interleaved per store: every pass is first-execution on
    // its own store (worker primed), so both modes pay their own cold
    // route.
    let mut cold = Samples::new();
    for _ in 0..args.cold_stores {
        let mut conn = cold_connection(Mode::Default);
        interleaved_round(
            AssertedCmd::ColdRound,
            &mut conn,
            &cmd_tx,
            &reply_rx,
            &mut cold,
        );
    }

    // Warm phase, interleaved per round.
    let mut warm = Samples::new();
    for _ in 0..args.rounds {
        interleaved_round(
            AssertedCmd::WarmRound,
            &mut warm_conn,
            &cmd_tx,
            &reply_rx,
            &mut warm,
        );
    }
    drop(cmd_tx);
    asserted.join().expect("asserted thread joined cleanly");

    report_phase("cold — first execution on fresh stores", &cold);
    report_phase("warm — steady state", &warm);
    println!(
        "\nreading: the class column (from MIX, its single source) names each statement's \
         route; a ~0 gap on a worker-both class means the handoff is paid by BOTH modes \
         and is invisible to this diff — only the declined class's gap is the unasserted \
         mode's own handoff cost."
    );
}

/// `Duration` in microseconds — the one scale conversion the report reads,
/// so no row can drift from its neighbors.
fn us(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e6
}

/// One phase's report: per statement, the (a) and (b) medians, their
/// signed gap, and the statement's route class (from `MIX`, its single
/// source). The gap — the handoff cost the unasserted mode still pays on
/// the statements only the asserted mode serves inline — keeps its sign:
/// near-zero noise on either side is the class evidence (both-inline vs
/// both-worker), and a clamped gap would bias the mix sum upward. Signed
/// in f64 because `Duration` subtraction panics on underflow, and a
/// negative gap (b slower than a) is exactly the noise the class reading
/// needs to see.
fn report_phase(phase: &str, samples: &Samples) {
    println!("\n{phase} (p50 µs per statement):");
    println!(
        "  {:<16} {:>8} {:>8} {:>10}  class",
        "statement", "a", "b", "gap a-b"
    );
    let mut mix_gap_us = 0.0f64;
    for (stmt, row) in MIX.iter().zip(samples.rows.iter()) {
        let (a_us, b_us) = (us(percentile(&row.a, 0.50)), us(percentile(&row.b, 0.50)));
        let gap_us = a_us - b_us;
        mix_gap_us += gap_us;
        println!(
            "  {:<16} {:>8.2} {:>8.2} {:>10.2}  {}",
            stmt.name, a_us, b_us, gap_us, stmt.class
        );
    }
    println!(
        "  {:<16} {:>8} {:>8} {:>10.2}",
        "mix (sum of p50 gaps)", "", "", mix_gap_us
    );
}
