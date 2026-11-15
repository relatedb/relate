//! Codec lab: measure every compiled snapshot codec on one graph —
//! checkpoint wall time, load wall time, and the store's bytes on disk.
//!
//! Usage:
//!   cargo run --release -p relate-codec-lab -- <source> [repeats]
//!
//! `source` is one of:
//!   `fraud:<scale>`     relate-datasets fraud generator (default `fraud:2.0`;
//!                       a bare number is the same thing)
//!   `pokec:<shape>`     the synthetic pokec fixture (`tiny`, `small`,
//!                       `medium`) — property-light, degree-skewed
//!   `ldbc:<dataset-dir>`  an extracted LDBC social_network CSV directory
//!                       (e.g. .../social_network-sf0.1-CsvBasic-
//!                       StringDateFormatter), loaded through the
//!                       query-latency suite's loader — property-heavy
//!
//! The graph is built once and every codec checkpoints and reloads the
//! same rows, so the numbers are comparable; the reload's exact content
//! signature must match the source graph's or the run fails (a codec
//! that loses data must not report a fast time).

use std::path::Path;
use std::time::{Duration, Instant};

use relate::Database;
use relate_datasets::{generate_fraud, load_fraud, FraudConfig};
use relate_executor::Graph;
use relate_storage::{
    checkpoint, load, parse_uri_with_options, snapshot_data_bytes, store_info, SnapshotCodecKind,
    StoreSnapshot,
};
use relate_test_support::graph::signature;

/// The fraud benchmark's store: the generator's graph loaded through the
/// datasets crate's own loader (`load_fraud`), so the lab measures the
/// graph the fraud benchmark runs on — schema, planted-account encoding
/// and relationship directions included.
fn build_fraud(scale: f64) -> Database {
    let graph = generate_fraud(&FraudConfig {
        scale,
        ..FraudConfig::default()
    });
    let db = Database::open(":memory:").expect("memory store");
    let mut conn = db.connect().expect("connection");
    load_fraud(&mut conn, &graph).expect("fraud load");
    db
}

/// The pokec fixture's store, as the lab's one graph: the fixture
/// populates through the public API (indexes included, exactly as the
/// benchmark's import does), and the lab checkpoints the live graph.
fn build_pokec(shape: &str) -> Database {
    use relate_test_support::pokec::{self, PokecShape};
    let shape = match shape {
        "tiny" => PokecShape::TINY,
        "small" => PokecShape::SMALL,
        "medium" => PokecShape::MEDIUM,
        other => panic!("unknown pokec shape '{other}' (tiny, small, medium)"),
    };
    pokec::open(shape).expect("pokec fixture open")
}

/// The LDBC social-network CSV directory, loaded through the
/// query-latency suite's own loader (upstream index profile, published
/// counts asserted) — the shape the latency suite measures.
fn build_ldbc(dataset_dir: &Path) -> Database {
    use relate_query_latency::workload::{IndexProfile, ScaleFactor};
    let db = Database::open(":memory:").expect("memory store");
    let mut conn = db.connect().expect("connection");
    let report = relate_query_latency::load::load(
        &mut conn,
        dataset_dir,
        IndexProfile::Upstream,
        Some(ScaleFactor::Sf0_1.published_counts()),
    )
    .expect("ldbc load");
    println!(
        "ldbc load: {:.1} s, {} nodes / {} relationships",
        report.seconds, report.nodes, report.relationships
    );
    db
}

struct Measurement {
    codec: SnapshotCodecKind,
    checkpoint: Duration,
    load: Duration,
    engine_bytes: u64,
    dir_bytes: u64,
    /// Manifest part files of the checkpointed store (`None` for
    /// envelope-only codecs) — the file-count term of the write cost,
    /// printed so a part-multiplication change shows as a number, not an
    /// inference from dir bytes.
    parts: Option<usize>,
}

fn measure(codec: SnapshotCodecKind, graph: &Graph, repeats: u32, root: &Path) -> Measurement {
    let mut best_checkpoint = Duration::MAX;
    let mut best_load = Duration::MAX;
    let mut engine_bytes = 0u64;
    let mut on_disk = 0u64;
    let mut parts = None;
    let expected = signature(graph);
    for round in 0..repeats {
        let dir = root.join(format!("{}-{round}", codec.as_str()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let target = parse_uri_with_options(dir.to_str().expect("utf-8 path"), None, Some(codec))
            .expect("store target");
        let started = Instant::now();
        checkpoint(&target, graph, None).expect("checkpoint");
        best_checkpoint = best_checkpoint.min(started.elapsed());
        engine_bytes = snapshot_data_bytes(&target).expect("snapshot bytes");
        // The public cold read's total: the same walk the engine's own
        // info path takes, bundle stores included. The same read parses
        // the manifest, so the part count is one field on it.
        let info = store_info(&dir).expect("store info");
        on_disk = info.total_bytes;
        parts = match info.snapshot {
            StoreSnapshot::Parquet(manifest) => {
                Some(manifest.node_parts.len() + manifest.rel_parts.len())
            }
            // Envelope-only codecs carry no manifest parts to count.
            StoreSnapshot::Envelope { .. } => None,
            // This round checkpointed the store above; no snapshot means
            // the cold read is broken, and a broken harness must not
            // print a quiet `-`.
            StoreSnapshot::None => panic!("no snapshot after this round's checkpoint"),
        };
        let started = Instant::now();
        let reloaded = load(&target).expect("load");
        best_load = best_load.min(started.elapsed());
        assert_eq!(
            signature(&reloaded),
            expected,
            "{}: reloaded content differs",
            codec.as_str()
        );
        drop(target);
    }
    Measurement {
        codec,
        checkpoint: best_checkpoint,
        load: best_load,
        engine_bytes,
        dir_bytes: on_disk,
        parts,
    }
}

/// The one source parser: `fraud:<scale>` (a bare number is the same
/// thing), `pokec:<shape>`, `ldbc:<dataset-dir>`.
fn parse_source(arg: &str) -> (String, Database) {
    // `fraud:<scale>` and a bare number are the same source: strip the
    // prefix once, then one parse arm builds it.
    let bare = arg.strip_prefix("fraud:").unwrap_or(arg);
    if let Ok(scale) = bare.parse::<f64>() {
        (format!("fraud scale {scale}"), build_fraud(scale))
    } else if let Some(shape) = arg.strip_prefix("pokec:") {
        (format!("pokec {shape}"), build_pokec(shape))
    } else if let Some(dir) = arg.strip_prefix("ldbc:") {
        (
            format!(
                "ldbc {}",
                Path::new(dir)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ),
            build_ldbc(Path::new(dir)),
        )
    } else {
        panic!("unknown source '{arg}' (fraud:<scale>, pokec:<shape>, ldbc:<dir>)");
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let source = args.next().unwrap_or_else(|| "fraud:2.0".into());
    let repeats: u32 = args
        .next()
        .map(|a| a.parse().expect("repeats must be a number"))
        .unwrap_or(3);
    // A zero-repeat run would report `Duration::MAX` timings and skip the
    // content gate; extra positionals are a typo'd flag, not a quiet no-op.
    assert!(
        repeats >= 1,
        "repeats must be at least 1 (usage: <source> [repeats])"
    );
    assert!(
        args.next().is_none(),
        "unexpected extra argument (usage: <source> [repeats])"
    );

    let (label, db) = parse_source(&source);
    let graph = db.sol_graph();
    let ts = graph.commit_ts();
    let nodes = graph.all_node_ids_at(ts).len();
    let rels = graph.all_rel_ids_at(ts).len();
    println!("{label}: {nodes} nodes, {rels} relationships");

    let root = tempfile::tempdir().expect("scratch root");
    let mut rows = Vec::new();
    for codec in SnapshotCodecKind::compiled_snapshot_containers() {
        // The typed container set: a vault writer (markdown) is not a
        // snapshot container and is not measured here.
        rows.push(measure(codec, graph, repeats, root.path()));
    }

    print_report(&rows);
}

/// The measurement table and the parquet-relative size ratios.
fn print_report(rows: &[Measurement]) {
    println!(
        "\n{:<8} {:>12} {:>12} {:>14} {:>14} {:>6}",
        "codec", "checkpoint", "load", "snapshot bytes", "dir bytes", "parts"
    );
    for row in rows {
        let parts = row
            .parts
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".into());
        println!(
            "{:<8} {:>10.1}ms {:>10.1}ms {:>14} {:>14} {:>6}",
            row.codec.as_str(),
            row.checkpoint.as_secs_f64() * 1000.0,
            row.load.as_secs_f64() * 1000.0,
            row.engine_bytes,
            row.dir_bytes,
            parts
        );
    }
    // The lab's own manifest compiles parquet and measures every
    // root-layout codec, so the baseline row is invariant-bound: a miss is
    // a harness bug, not a reportable state.
    let baseline = rows
        .iter()
        .find(|row| row.codec == SnapshotCodecKind::Parquet)
        .expect("the lab always measures the parquet baseline")
        .dir_bytes;
    println!("\nrelative to parquet dir bytes:");
    for row in rows {
        println!(
            "{:<8} {:>7.2}x",
            row.codec.as_str(),
            row.dir_bytes as f64 / baseline as f64
        );
    }
}
