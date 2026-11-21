//! Adjacency-payload A/B lab: measure the opt-in checkpoint payload
//! (ADR-0551) against the row re-fold it replaces — per store size, the
//! checkpoint publication, the store open (wall plus its adjacency-tail
//! share), the store close, and a fixed read and write workload, with the
//! runs paired and interleaved (payload on, payload off, payload on, payload
//! off) and each run stamped with the host's 1/5/15-minute load averages
//! (the quiet-box protocol).
//!
//! Usage:
//!   cargo run --release -p relate-adjacency-payload-lab -- [--rounds N] \
//!       [--out PATH] [--only NAME] [--workdir DIR] [--smoke]
//!
//! The three stores are `relate-datasets` shapes tuned by closed-form census
//! to the target relationship count: dense-ring and mid-degree at ~2M rels,
//! and mid-degree at >=10M rels. The payload threshold
//! (`RELATE_ADJACENCY_PAYLOAD_MIN_RELS`) is set to [`LAB_MIN_RELS`] so both
//! arms publish at every size; the arm toggles only `RELATE_ADJACENCY_PAYLOAD`.
//! The harness writes the recorded results as JSON on stdout (and to `--out`).
//!
//! The store open is the snapshot load through `relate_storage::load_timed`:
//! its wall plus the adjacency tail (the payload install, or the rel-row
//! re-fold) that the payload exists to cut. The read/write/close phases run
//! on a `Database` opened over that same store, and the close is the drop
//! checkpoint — the recurring publication cost an enabled payload charges
//! every checkpoint.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use relate::{Database, OpenOptions};
use relate_datasets::shapes::{census, statements, GraphShape, IndexCounts, ShapeScale};
use relate_executor::Graph;
use relate_storage::{
    checkpoint, load_timed, parse_uri_with_options, snapshot_data_bytes, SnapshotCodecKind,
    StoreTarget, TimedLoad, ADJACENCY_PAYLOAD_ENV, ADJACENCY_PAYLOAD_MIN_RELS_ENV,
};
use relate_test_support::copy_dir;
use serde::Serialize;

/// The lab's threshold: every store publishes a payload on the on arm,
/// whatever its size.
const LAB_MIN_RELS: u64 = 0;
/// The size search's upper wall: one node's logical charge exceeds 64 bytes
/// for every shape and attribute set this lab builds, so a target of
/// `u32::MAX * 64` logical bytes still carries a `u32` node count and the
/// bisect's overshoot never reaches census's overflow error.
const MAX_LOGICAL_BYTES: u64 = u32::MAX as u64 * 64;
/// Every shape carries the integer `id` property (its first attribute) and
/// the harness's read/write probes address nodes by it.
const ATTRIBUTES: u32 = 1;

/// The two arms — the only difference between a paired run is the payload
/// opt-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arm {
    On,
    Off,
}

/// The arms in measurement order — the one order/name fact: the label is
/// the entry's and [`Arm::all`] yields its entries.
const ARM_TABLE: [(Arm, &str); 2] = [(Arm::On, "on"), (Arm::Off, "off")];

impl Serialize for Arm {
    /// The arm's [`ARM_TABLE`] label, never a second spelling beside it.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
    }
}

impl Arm {
    /// The arms in measurement order — from [`ARM_TABLE`] alone.
    fn all() -> impl Iterator<Item = Arm> {
        ARM_TABLE.iter().map(|&(arm, _)| arm)
    }

    /// The arm's report label — its [`ARM_TABLE`] spelling.
    fn label(self) -> &'static str {
        ARM_TABLE
            .iter()
            .find(|(arm, _)| *arm == self)
            .expect("every arm is tabled")
            .1
    }

    /// Point the process environment at this arm.
    fn set_env(self) {
        std::env::set_var(ADJACENCY_PAYLOAD_MIN_RELS_ENV, LAB_MIN_RELS.to_string());
        match self {
            Arm::On => std::env::set_var(ADJACENCY_PAYLOAD_ENV, "1"),
            Arm::Off => std::env::remove_var(ADJACENCY_PAYLOAD_ENV),
        }
    }
}

struct StoreSpec {
    name: &'static str,
    shape: GraphShape,
    target_rels: u64,
}

/// The three stores the acceptance names: two shapes at ~2M rels and one at
/// >=10M rels.
const STORES: [StoreSpec; 3] = [
    StoreSpec {
        name: "dense-ring-2m",
        shape: GraphShape::DenseRing,
        target_rels: 2_000_000,
    },
    StoreSpec {
        name: "mid-degree-2m",
        shape: GraphShape::MidDegree,
        target_rels: 2_000_000,
    },
    StoreSpec {
        name: "mid-degree-10m",
        shape: GraphShape::MidDegree,
        target_rels: 10_000_000,
    },
];

/// Every shape in the table has a `id` property to index, so the link
/// statements' `MATCH (… {id})` is an index seek, not a label scan.
fn indexes() -> IndexCounts {
    IndexCounts {
        text: 0,
        point: 0,
        range: 1,
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// The host's 1/5/15-minute load averages — the quiet-box stamp. `NaN` when
/// the platform cannot report them.
fn load_stamp() -> [f64; 3] {
    #[cfg(unix)]
    {
        let mut averages = [f64::NAN; 3];
        // SAFETY: `getloadavg` writes at most `nelem` doubles into the
        // buffer, and the buffer is exactly that long.
        let written = unsafe { libc::getloadavg(averages.as_mut_ptr(), 3) };
        if written == 3 {
            averages
        } else {
            [f64::NAN; 3]
        }
    }
    #[cfg(not(unix))]
    {
        [f64::NAN; 3]
    }
}

/// The logical-byte scale whose closed-form census first reaches `target`
/// relationships — the smallest at or above it, so a `>=10M` store is never a
/// few rows short.
fn tune(shape: GraphShape, target: u64) -> (ShapeScale, u64, u64) {
    let indexes = indexes();
    // The search never probes a logical size the shape's `u32` node count
    // cannot carry, so a census error is a broken lab configuration and
    // propagates instead of reading as "above the target".
    let rels = |logical_bytes: u64| {
        census(
            shape,
            ShapeScale::Large { logical_bytes },
            indexes,
            ATTRIBUTES,
        )
        .expect("the capped size range is representable")
        .relationships
    };
    let mut lo = 1u64;
    let mut hi = MAX_LOGICAL_BYTES;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if rels(mid) >= target {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    let shape_scale = ShapeScale::Large { logical_bytes: lo };
    let totals = census(shape, shape_scale, indexes, ATTRIBUTES).expect("census");
    assert!(
        totals.relationships >= target,
        "{shape}: no logical size under the census wall reaches {target} relationships"
    );
    (shape_scale, totals.nodes, totals.relationships)
}

/// Build the store's master graph in memory from the shape's own statement
/// generator — the one graph both arms checkpoint.
fn build(shape: GraphShape, scale: ShapeScale, label: &str) -> Database {
    let db = Database::open(":memory:").expect("memory store");
    let statements = statements(shape, scale, indexes(), ATTRIBUTES).expect("statements");
    let mut conn = db.connect().expect("connection");
    let total = statements.len();
    for (nth, statement) in statements.iter().enumerate() {
        if nth % 1000 == 0 {
            eprintln!("  {label}: statement {nth}/{total}");
        }
        conn.query(statement).unwrap_or_else(|error| {
            panic!("{label}: statement {nth} failed: {error}\n{statement}")
        });
    }
    drop(conn);
    db
}

/// Checkpoint the master graph to `dir` with the arm's payload setting; the
/// wall is the commit/checkpoint publication cost the payload adds.
fn publish(graph: &Graph, dir: &Path, arm: Arm) -> Publish {
    std::fs::create_dir_all(dir).expect("store dir");
    let target = store_target(dir);
    arm.set_env();
    let load = load_stamp();
    let started = Instant::now();
    checkpoint(&target, graph, None).expect("checkpoint");
    let wall = started.elapsed();
    let store_bytes = snapshot_data_bytes(&target).expect("snapshot bytes");
    Publish {
        wall_ms: ms(wall),
        store_bytes,
        load,
    }
}

fn store_target(dir: &Path) -> StoreTarget {
    parse_uri_with_options(
        dir.to_str().expect("utf-8 path"),
        None,
        Some(SnapshotCodecKind::Parquet),
    )
    .expect("store target")
}

/// Remove a lab scratch tree: a missing path is fine, anything else is a
/// failure.
fn remove_dir(path: &Path) {
    match std::fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("remove {}: {error}", path.display()),
    }
}

/// The fixed read workload: out-degree, in-degree and a two-hop expansion
/// around three nodes, plus the graph's total relationship count. Addresses
/// nodes by the shape's `id` property, so one query set serves every shape.
fn read_queries(node_ids: &[u64]) -> Vec<String> {
    let mut queries = Vec::new();
    for &id in node_ids {
        queries.push(format!(
            "MATCH (a)-[r]->(b) WHERE a.id = {id} RETURN count(b) AS n"
        ));
        queries.push(format!(
            "MATCH (a)<-[r]-(b) WHERE a.id = {id} RETURN count(b) AS n"
        ));
        queries.push(format!(
            "MATCH (a)-[]->(b)-[]->(c) WHERE a.id = {id} RETURN count(c) AS n"
        ));
    }
    queries.push("MATCH ()-[r]->() RETURN count(r) AS n".into());
    queries
}

/// The fixed write batch: a fresh label's nodes plus one relationship each
/// from node `id = 0`, so the close checkpoint has dirty rows above and
/// below the fold's seal floor to publish.
fn write_queries() -> Vec<String> {
    vec![
        "UNWIND range(0, 999) AS k CREATE (:LabWrite {id: 1000000000 + k})".into(),
        "UNWIND range(0, 99) AS k MATCH (a) WHERE a.id = 0 \
         MATCH (b:LabWrite) WHERE b.id = 1000000000 + k CREATE (a)-[:LINK]->(b)"
            .into(),
    ]
}

#[derive(Serialize)]
struct Publish {
    wall_ms: f64,
    store_bytes: u64,
    load: [f64; 3],
}

#[derive(Serialize)]
struct ArmPublish {
    on: Publish,
    off: Publish,
}

#[derive(Serialize)]
struct Run {
    arm: Arm,
    round: u32,
    open_ms: f64,
    fold_ms: f64,
    read_ms: f64,
    write_ms: f64,
    close_ms: f64,
    load: [f64; 3],
}

#[derive(Serialize)]
struct StoreResult {
    name: &'static str,
    shape: String,
    target_rels: u64,
    nodes: u64,
    relationships: u64,
    publish: ArmPublish,
    runs: Vec<Run>,
}

/// One arm's open/read/write/close over a pristine copy of the store.
fn measure_run(dir: &Path, arm: Arm, round: u32, node_ids: &[u64]) -> Run {
    arm.set_env();
    let load = load_stamp();
    // Store open: the snapshot load, wall plus the adjacency tail. A
    // degraded open would mislabel the on arm as the payload arm, so it
    // refuses the measurement instead of recording a fold under it.
    let target = store_target(dir);
    let started = Instant::now();
    let TimedLoad {
        graph,
        adjacency,
        warnings,
    } = load_timed::<Graph>(&target).expect("load");
    let open = started.elapsed();
    assert!(
        warnings.is_empty(),
        "{} arm degraded: {warnings:?}",
        arm.label()
    );
    let fold = adjacency.expect("the parquet open times its adjacency tail");
    drop(graph);
    // The target's writer lease must go before the Database opens the same
    // directory.
    drop(target);

    // Read/write/close on a Database opened over the same store.
    let db = Database::open_with(
        dir.to_str().expect("utf-8 path"),
        OpenOptions {
            checkpoint_interval: Some(Duration::ZERO),
            ..OpenOptions::default()
        },
    )
    .expect("database open");
    let mut conn = db.connect().expect("connection");

    let started = Instant::now();
    for query in read_queries(node_ids) {
        conn.query(&query).expect("read query");
    }
    let read = started.elapsed();

    let started = Instant::now();
    for query in write_queries() {
        conn.query(&query).expect("write query");
    }
    let write = started.elapsed();

    drop(conn);
    let started = Instant::now();
    drop(db);
    let close = started.elapsed();

    Run {
        arm,
        round,
        open_ms: ms(open),
        fold_ms: ms(fold),
        read_ms: ms(read),
        write_ms: ms(write),
        close_ms: ms(close),
        load,
    }
}

/// The arm's published store directory under `workdir`.
fn arm_base(workdir: &Path, store: &str, arm: Arm) -> PathBuf {
    workdir.join(format!("{store}-{}", arm.label()))
}

/// One arm's mean of `field` over `runs`; a missing arm is a refused
/// measurement, never a zero that reads as an instant one.
fn arm_mean(runs: &[Run], arm: Arm, field: fn(&Run) -> f64) -> f64 {
    let mut sum = 0.0;
    let mut count = 0;
    for run in runs.iter().filter(|run| run.arm == arm) {
        sum += field(run);
        count += 1;
    }
    assert!(count > 0, "no measured {} runs", arm.label());
    sum / count as f64
}

/// One arm's per-phase means, named like the phases.
struct Means {
    open: f64,
    fold: f64,
    close: f64,
    read: f64,
    write: f64,
}

impl Means {
    fn of(runs: &[Run], arm: Arm) -> Self {
        Self {
            open: arm_mean(runs, arm, |run| run.open_ms),
            fold: arm_mean(runs, arm, |run| run.fold_ms),
            close: arm_mean(runs, arm, |run| run.close_ms),
            read: arm_mean(runs, arm, |run| run.read_ms),
            write: arm_mean(runs, arm, |run| run.write_ms),
        }
    }
}

/// One measured phase: its report label and the [`Means`] accessor it
/// renders — the one phase order/name table.
type Phase = (&'static str, fn(&Means) -> f64);

/// The measured phases, in report order — the one list the rendered line
/// derives from. The decision rule reads the named fields (`means.open`,
/// `means.close`) instead.
const PHASES: [Phase; 5] = [
    ("open", |means| means.open),
    ("adjacency tail", |means| means.fold),
    ("close", |means| means.close),
    ("read", |means| means.read),
    ("write", |means| means.write),
];

/// The per-size numbers and the threshold recommendation, one paragraph.
///
/// The decision rule: the payload is worth enabling at a size when its open
/// win (re-fold wall minus install wall) is positive **and** larger than the
/// checkpoint penalty it adds (the publish and close deltas) — one open
/// amortizes one checkpoint. The recommendation lowers the 10,000,000
/// default only to the smallest measured size that passes.
fn verdict(stores: &[StoreResult]) -> String {
    let mut lines = Vec::new();
    let mut paying: Option<(&StoreResult, f64, f64)> = None;
    for store in stores {
        let off = Means::of(&store.runs, Arm::Off);
        let on = Means::of(&store.runs, Arm::On);
        let win = off.open - on.open;
        let win_pct = if off.open > 0.0 {
            win / off.open * 100.0
        } else {
            0.0
        };
        let penalty =
            (store.publish.on.wall_ms - store.publish.off.wall_ms) + (on.close - off.close);
        if win > 0.0 && win > penalty && paying.is_none() {
            paying = Some((store, win, penalty));
        }
        let phases: Vec<String> = PHASES
            .iter()
            .map(|(label, mean)| format!("{label} {:.0} -> {:.0} ms", mean(&off), mean(&on)))
            .collect();
        lines.push(format!(
            "{} ({} nodes / {} rels): {} ({} {:.0} ms, {:.0}%); publish {:.0} -> {:.0} ms; \
             per-checkpoint penalty {:.0} ms",
            store.name,
            store.nodes,
            store.relationships,
            phases.join("; "),
            if win >= 0.0 { "win" } else { "loss" },
            win.abs(),
            win_pct,
            store.publish.off.wall_ms,
            store.publish.on.wall_ms,
            penalty,
        ));
    }
    let recommendation = match paying {
        Some((store, win, penalty)) => format!(
            "Lower the 10,000,000 default to about {} relationships: at {} nodes / {} rels the open \
             saves {:.0} ms against a {:.0} ms per-checkpoint penalty.",
            store.target_rels, store.nodes, store.relationships, win, penalty
        ),
        None => "Keep the 10,000,000 default: no measured size showed an open win that covers the \
                 per-checkpoint penalty it adds."
            .to_string(),
    };
    format!("{}. {}", lines.join("; "), recommendation)
}

#[derive(Serialize)]
struct Results {
    started_unix: u64,
    finished_unix: u64,
    rounds: u32,
    min_rels_env: u64,
    stores: Vec<StoreResult>,
    verdict: String,
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before the unix epoch")
        .as_secs()
}

fn default_workdir() -> PathBuf {
    // The quiet-box protocol keeps fresh build trees out of Spotlight's
    // indexed volumes; `/private/tmp` is the macOS spelling of `/tmp`.
    let base = if Path::new("/private/tmp").is_dir() {
        PathBuf::from("/private/tmp")
    } else {
        std::env::temp_dir()
    };
    base.join(format!(
        "relate-adjacency-payload-lab-{}",
        std::process::id()
    ))
}

const USAGE: &str = "usage: relate-adjacency-payload-lab [--rounds N] [--out PATH] \
                     [--only NAME] [--workdir DIR] [--smoke]";

/// Exit with `message` and the documented usage line — bad arguments are
/// the caller's input, not an invariant.
fn usage(message: &str) -> ! {
    eprintln!("{message}\n{USAGE}");
    std::process::exit(2)
}

/// The next argument value, refusing a missing one.
fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next()
        .unwrap_or_else(|| usage(&format!("{flag} needs a value")))
}

struct Args {
    rounds: u32,
    out: Option<PathBuf>,
    only: Option<String>,
    workdir: PathBuf,
    smoke: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        rounds: 2,
        out: None,
        only: None,
        workdir: default_workdir(),
        smoke: false,
    };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--rounds" => {
                let raw = value(&mut argv, "--rounds");
                args.rounds = raw
                    .parse()
                    .unwrap_or_else(|_| usage(&format!("--rounds is not a number: {raw:?}")));
            }
            "--out" => args.out = Some(PathBuf::from(value(&mut argv, "--out"))),
            "--only" => args.only = Some(value(&mut argv, "--only")),
            "--workdir" => args.workdir = PathBuf::from(value(&mut argv, "--workdir")),
            "--smoke" => args.smoke = true,
            other => usage(&format!("unknown argument {other:?}")),
        }
    }
    if args.rounds == 0 {
        usage("--rounds must be at least 1");
    }
    validate_workdir(&args.workdir);
    args
}

/// Refuse a `--workdir` whose recursive cleanup would be a foot-gun: the
/// path must be absolute, and must be neither the filesystem root nor an
/// ancestor (itself included) of the current or the home directory — the
/// lab deletes the whole tree when it finishes.
fn validate_workdir(workdir: &Path) {
    if !workdir.is_absolute() {
        usage(&format!(
            "--workdir must be an absolute path: {}",
            workdir.display()
        ));
    }
    if workdir.parent().is_none() {
        usage("--workdir must not be the filesystem root");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let current = std::env::current_dir().ok();
    for (what, anchor) in [
        ("the home directory", home),
        ("the current directory", current),
    ] {
        if let Some(anchor) = anchor {
            if anchor.starts_with(workdir) {
                usage(&format!(
                    "--workdir must not be {what} or contain it: {}",
                    workdir.display()
                ));
            }
        }
    }
}

/// Measure one store: tune to `target` relationships, build the master
/// graph, publish both arms, then run the interleaved rounds over pristine
/// copies.
fn measure_store(spec: &StoreSpec, target: u64, rounds: u32, workdir: &Path) -> StoreResult {
    let (scale, nodes, relationships) = tune(spec.shape, target);
    eprintln!(
        "{}: shape {} target {} rels -> census {} nodes / {} rels",
        spec.name, spec.shape, target, nodes, relationships
    );
    let db = build(spec.shape, scale, spec.name);
    let graph = db.sol_graph();
    let on_base = arm_base(workdir, spec.name, Arm::On);
    let off_base = arm_base(workdir, spec.name, Arm::Off);
    let on = publish(graph, &on_base, Arm::On);
    let off = publish(graph, &off_base, Arm::Off);
    eprintln!(
        "{}: published payload-on {} ms / {} bytes, payload-off {} ms / {} bytes",
        spec.name, on.wall_ms, on.store_bytes, off.wall_ms, off.store_bytes
    );
    drop(db);

    let node_ids = [0u64, nodes / 2, nodes.saturating_sub(1)];
    let mut runs = Vec::new();
    for round in 0..rounds {
        for arm in Arm::all() {
            let published = arm_base(workdir, spec.name, arm);
            let scratch = workdir.join(format!("{}-{}-r{round}", spec.name, arm.label()));
            copy_dir(&published, &scratch);
            let run = measure_run(&scratch, arm, round, &node_ids);
            eprintln!(
                "{}: round {} arm {} open {:.0} ms (tail {:.0} ms) read {:.0} ms write {:.0} ms close {:.0} ms load {:?}",
                spec.name, round, run.arm.label(), run.open_ms, run.fold_ms, run.read_ms, run.write_ms, run.close_ms, run.load
            );
            remove_dir(&scratch);
            runs.push(run);
        }
    }
    remove_dir(&on_base);
    remove_dir(&off_base);
    StoreResult {
        name: spec.name,
        shape: spec.shape.to_string(),
        target_rels: target,
        nodes,
        relationships,
        publish: ArmPublish { on, off },
        runs,
    }
}

fn run(args: &Args) -> Results {
    // The self-check sizes: the same three stores, 100x smaller.
    let divisor: u64 = if args.smoke { 100 } else { 1 };

    let started_unix = now_unix();
    let mut stores = Vec::new();
    for spec in &STORES {
        if let Some(name) = &args.only {
            if name != spec.name {
                continue;
            }
        }
        let target = spec.target_rels / divisor;
        stores.push(measure_store(spec, target, args.rounds, &args.workdir));
    }
    remove_dir(&args.workdir);

    let verdict = verdict(&stores);
    Results {
        started_unix,
        finished_unix: now_unix(),
        rounds: args.rounds,
        min_rels_env: LAB_MIN_RELS,
        stores,
        verdict,
    }
}

fn main() {
    let args = parse_args();
    let json = serde_json::to_string(&run(&args)).expect("results json");
    if let Some(path) = &args.out {
        std::fs::write(path, &json).expect("write results");
    }
    println!("{json}");
}
