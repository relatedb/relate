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
use relate_core::Value;
use relate_datasets::{generate_fraud, FraudConfig, FraudGraph};
use relate_executor::Graph;
use relate_storage::{
    checkpoint, load, parse_uri_with_options, snapshot_data_bytes, store_info, SnapshotCodecKind,
};
use relate_test_support::graph::{props, signature};

/// One generated-row id lookup — the invariant (every generated row names
/// generated endpoints) in one spelling: a miss is a generator bug and
/// fails the run, never a silently dropped row.
fn row_id(map: &std::collections::BTreeMap<String, i64>, key: &str, what: &str) -> i64 {
    *map.get(key)
        .unwrap_or_else(|| panic!("fraud generator: unknown {what} {key}"))
}

/// The fraud graph as checkpoint rows: Customer/Account nodes, OWNS and
/// TRANSFER relationships with their scalar properties.
fn build_fraud(scale: f64) -> Graph {
    let FraudGraph {
        customers,
        accounts,
        ownership,
        transfers,
        ..
    } = generate_fraud(&FraudConfig {
        scale,
        ..FraudConfig::default()
    });
    let graph = Graph::default();
    let mut next_id = 1i64;
    let mut customer_ids = std::collections::BTreeMap::new();
    for c in &customers {
        graph
            .load_checkpoint_node(relate_core::Node {
                id: next_id,
                labels: std::sync::Arc::new(vec!["Customer".into()]),
                properties: props(vec![
                    ("id", Value::String(c.id.as_str().into())),
                    ("name", Value::String(c.name.as_str().into())),
                ]),
            })
            .expect("checkpoint node");
        customer_ids.insert(c.id.clone(), next_id);
        next_id += 1;
    }
    let mut account_ids = std::collections::BTreeMap::new();
    for a in &accounts {
        let labels = match &a.planted {
            Some(kind) if !kind.is_empty() => vec!["Account".to_owned(), kind.clone()],
            _ => vec!["Account".to_owned()],
        };
        graph
            .load_checkpoint_node(relate_core::Node {
                id: next_id,
                labels: std::sync::Arc::new(labels),
                properties: props(vec![("id", Value::String(a.id.as_str().into()))]),
            })
            .expect("checkpoint node");
        account_ids.insert(a.id.clone(), next_id);
        next_id += 1;
    }
    let mut rel_id = 1i64;
    // The generator's ownership rows are `(account id, customer id)` — the
    // order its own `ownership.push((aid, cid))` writes. A dangling
    // endpoint is a generator bug, not a row to drop silently (the
    // checksum gate cannot see a truncated graph).
    for (account, customer) in &ownership {
        let start = row_id(&customer_ids, customer, "customer");
        let end = row_id(&account_ids, account, "account");
        graph
            .load_checkpoint_rel(relate_core::Relationship {
                id: rel_id,
                start,
                end,
                rel_type: "OWNS".into(),
                properties: props(Vec::new()),
            })
            .expect("checkpoint rel");
        rel_id += 1;
    }
    for t in &transfers {
        let start = row_id(&account_ids, &t.from, "account");
        let end = row_id(&account_ids, &t.to, "account");
        graph
            .load_checkpoint_rel(relate_core::Relationship {
                id: rel_id,
                start,
                end,
                rel_type: "TRANSFER".into(),
                properties: props(vec![
                    ("id", Value::String(t.id.as_str().into())),
                    ("amount", Value::Float(t.amount)),
                    ("ts", Value::Int(t.ts)),
                ]),
            })
            .expect("checkpoint rel");
        rel_id += 1;
    }
    graph.set_next_node_id(next_id);
    graph.set_next_rel_id(rel_id);
    graph.set_commit_ts(1);
    graph
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

/// The one graph every codec measures, whichever side holds it: the
/// fraud rows directly, the fixture/loaded stores through the facade's
/// research-only live-graph handle.
enum Source {
    Rows(Box<Graph>),
    Store(Database),
}

impl Source {
    fn graph(&self) -> &Graph {
        match self {
            Source::Rows(graph) => graph,
            Source::Store(db) => db.sol_graph(),
        }
    }
}

struct Measurement {
    codec: SnapshotCodecKind,
    checkpoint: Duration,
    load: Duration,
    engine_bytes: u64,
    dir_bytes: u64,
}

fn measure(codec: SnapshotCodecKind, graph: &Graph, repeats: u32, root: &Path) -> Measurement {
    let mut best_checkpoint = Duration::MAX;
    let mut best_load = Duration::MAX;
    let mut engine_bytes = 0u64;
    let mut on_disk = 0u64;
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
        // info path takes, bundle stores included.
        on_disk = store_info(&dir).expect("store info").total_bytes;
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
    }
}

/// The one source parser: `fraud:<scale>` (a bare number is the same
/// thing), `pokec:<shape>`, `ldbc:<dataset-dir>`.
fn parse_source(arg: &str) -> (String, Source) {
    // `fraud:<scale>` and a bare number are the same source: strip the
    // prefix once, then one parse arm builds it.
    let bare = arg.strip_prefix("fraud:").unwrap_or(arg);
    if let Ok(scale) = bare.parse::<f64>() {
        (
            format!("fraud scale {scale}"),
            Source::Rows(Box::new(build_fraud(scale))),
        )
    } else if let Some(shape) = arg.strip_prefix("pokec:") {
        (format!("pokec {shape}"), Source::Store(build_pokec(shape)))
    } else if let Some(dir) = arg.strip_prefix("ldbc:") {
        (
            format!(
                "ldbc {}",
                Path::new(dir)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ),
            Source::Store(build_ldbc(Path::new(dir))),
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

    let (label, source) = parse_source(&source);
    let graph = source.graph();
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
        "\n{:<8} {:>12} {:>12} {:>14} {:>14}",
        "codec", "checkpoint", "load", "snapshot bytes", "dir bytes"
    );
    for row in rows {
        println!(
            "{:<8} {:>10.1}ms {:>10.1}ms {:>14} {:>14}",
            row.codec.as_str(),
            row.checkpoint.as_secs_f64() * 1000.0,
            row.load.as_secs_f64() * 1000.0,
            row.engine_bytes,
            row.dir_bytes
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
