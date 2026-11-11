//! Scratch measurement harness: where a restored store's memory sits, per
//! data structure, and how the two Parquet restore decodes compare.
//!
//! Worktree-only (branch `research/heap-census`); never a documented
//! surface, never CI-gated.
//!
//! ```text
//! heap_census build <ldbc-csv-dir> <store-dir>     # CSV -> parquet store
//! heap_census checkpoint <store-dir>               # open + checkpoint (a
//!                                                   # legacy store upgrades
//!                                                   # wide here, ADR-0553)
//! heap_census restore <store-dir>                  # read-only open + census
//! heap_census rebuild-indexes <store-dir>          # time one extra rebuild
//! ```
//!
//! Peak RSS inside `restore` is the process high-water mark (`getrusage`,
//! the workspace's shared sampler); the independent `/usr/bin/time -l`
//! figure is the authoritative peak.

use std::path::Path;
use std::time::{Duration, Instant};

use relate::{Database, DurabilityMode, HeapCensus, OpenMode, OpenOptions, SnapshotCodecKind};
use relate_query_latency::load::{self, LoadReport};
use relate_query_latency::workload::{IndexProfile, ScaleFactor};
use relate_test_support::sampling::{process_peak_rss_bytes, process_rss_bytes};

/// A sampler's reading, or the harness's one explicit error: a measurement
/// the process failed to take is never printed as a zero.
fn measured(sample: Option<u64>, what: &str) -> Result<u64, String> {
    sample.ok_or_else(|| format!("{what}: process RSS sampling unavailable"))
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Open `store` with `opts`; `what` names the operation in the error.
fn open(store: &Path, opts: OpenOptions, what: &str) -> Result<Database, String> {
    let path = store.to_str().ok_or("store path is not UTF-8")?;
    Database::open_with(path, opts).map_err(|e| format!("{what} {}: {e}", store.display()))
}

fn build(csv_dir: &Path, store: &Path) -> Result<(), String> {
    let db = open(
        store,
        OpenOptions {
            durability_mode: Some(DurabilityMode::Snapshot),
            checkpoint_interval: Some(Duration::ZERO),
            snapshot_codec: Some(SnapshotCodecKind::Parquet),
            ..OpenOptions::default()
        },
        "creating",
    )?;
    println!(
        "build: opened store at {} (rss {:.1} MB)",
        store.display(),
        mb(measured(process_rss_bytes(), "build: opened")?)
    );

    let mut conn = db.connect().map_err(|e| format!("connect: {e}"))?;
    let LoadReport {
        seconds,
        nodes,
        relationships,
        ..
    } = load::load(
        &mut conn,
        csv_dir,
        IndexProfile::Upstream,
        Some(ScaleFactor::Sf0_1.published_counts()),
    )?;
    println!(
        "build: loaded {nodes} nodes / {relationships} rels in {seconds:.1} s \
         (rss {:.1} MB)",
        mb(measured(process_rss_bytes(), "build: loaded")?)
    );

    let t = Instant::now();
    db.checkpoint().map_err(|e| format!("checkpoint: {e}"))?;
    println!(
        "build: checkpoint in {:.2} s (rss {:.1} MB)",
        t.elapsed().as_secs_f64(),
        mb(measured(process_rss_bytes(), "build: checkpoint")?)
    );
    let g = db.sol_graph();
    println!(
        "build: used_bytes {:.1} MB, accounted_bytes {:.1} MB",
        mb(g.used_bytes()),
        mb(g.accounted_bytes())
    );
    Ok(())
}

fn checkpoint(store: &Path) -> Result<(), String> {
    // A plain checkpoint re-encodes wide: wide parts are the Parquet
    // codec's only write encoding (ADR-0553), so on a legacy-encoded
    // store this open + checkpoint pair is the one-time upgrade.
    let t = Instant::now();
    let db = open(store, OpenOptions::default(), "opening")?;
    let open = t.elapsed().as_secs_f64();
    let t = Instant::now();
    db.checkpoint().map_err(|e| format!("checkpoint: {e}"))?;
    println!(
        "checkpoint: open {open:.2} s, checkpoint {:.2} s",
        t.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Time one extra `rebuild_indexes` on a restored graph — the redundant
/// second pass `open.rs::replay_after_ontology` always runs after
/// `finish_restore` already rebuilt the indexes.
fn rebuild_indexes(store: &Path) -> Result<(), String> {
    let db = open(
        store,
        OpenOptions {
            mode: OpenMode::ReadOnly,
            ..OpenOptions::default()
        },
        "opening read-only",
    )?;
    let g = db.sol_graph();
    let before = g.used_bytes();
    let t = Instant::now();
    g.rebuild_indexes();
    println!(
        "rebuild-indexes: {:.3} s on the restored graph (used_bytes {:.1} -> {:.1} MB)",
        t.elapsed().as_secs_f64(),
        mb(before),
        mb(g.used_bytes())
    );
    Ok(())
}

fn restore(store: &Path) -> Result<(), String> {
    let baseline = measured(process_rss_bytes(), "restore: baseline")?;
    let t = Instant::now();
    let db = open(
        store,
        OpenOptions {
            mode: OpenMode::ReadOnly,
            ..OpenOptions::default()
        },
        "opening read-only",
    )?;
    let open_secs = t.elapsed().as_secs_f64();
    let peak = measured(process_peak_rss_bytes(), "restore: peak")?;
    let steady = measured(process_rss_bytes(), "restore: steady")?;

    println!("restore: {}", store.display());
    println!(
        "restore: open {open_secs:.2} s; rss baseline {:.1} MB, peak {:.1} MB, \
         steady {:.1} MB (transient {:.1} MB)",
        mb(baseline),
        mb(peak),
        mb(steady),
        mb(peak.saturating_sub(steady))
    );

    let g = db.sol_graph();
    print_census(&g.heap_census());
    Ok(())
}

fn print_census(c: &HeapCensus) {
    println!();
    println!("=== census (read-only graph) ===");
    println!(
        "engine totals: used_bytes {:.1} MB | accounted_bytes {:.1} MB | \
         structure_bytes {:.1} MB",
        mb(c.used_bytes),
        mb(c.accounted_bytes),
        mb(c.structure_bytes)
    );
    println!();

    let line = |label: &str, bytes: u64, count: String| {
        println!("{label:<30} {:>12} {:>9.1} MB  {count}", bytes, mb(bytes));
    };

    line(
        "node slot array",
        c.node_slot_bytes,
        format!(
            "{} ids, span {}, {} side",
            c.node_ids, c.node_dense_span, c.node_side_entries
        ),
    );
    line(
        "node version chains",
        c.node_chain_bytes,
        format!(
            "{} inline, {} spilled slots / {} versions",
            c.node_chains.inline, c.node_chains.spilled_slots, c.node_chains.spilled_versions
        ),
    );
    line(
        "node records (logical)",
        c.node_record_bytes,
        format!(
            "{} props, {} label refs, {} string bytes",
            c.node_props, c.node_label_refs, c.node_string_bytes
        ),
    );
    line(
        "rel slot array",
        c.rel_slot_bytes,
        format!(
            "{} ids, span {}, {} side",
            c.rel_ids, c.rel_dense_span, c.rel_side_entries
        ),
    );
    line(
        "rel version chains",
        c.rel_chain_bytes,
        format!(
            "{} inline, {} spilled slots / {} versions",
            c.rel_chains.inline, c.rel_chains.spilled_slots, c.rel_chains.spilled_versions
        ),
    );
    line(
        "rel records (logical)",
        c.rel_record_bytes,
        format!("{} props, {} string bytes", c.rel_props, c.rel_string_bytes),
    );
    line(
        "out adjacency (slots+heap)",
        c.out.slot_bytes + c.out.heap_bytes,
        format!(
            "{} nodes, {} hops, {} chunks ({} sealed)",
            c.out.nodes, c.out.hops, c.out.chunks, c.out.sealed_chunks
        ),
    );
    line(
        "in adjacency (slots+heap)",
        c.r#in.slot_bytes + c.r#in.heap_bytes,
        format!(
            "{} nodes, {} hops, {} chunks ({} sealed)",
            c.r#in.nodes, c.r#in.hops, c.r#in.chunks, c.r#in.sealed_chunks
        ),
    );
    line(
        "label index (est.)",
        c.label_index_bytes,
        format!(
            "{} buckets, {} entries",
            c.label_index_buckets, c.label_index_entries
        ),
    );
    line(
        "declared RANGE index (est.)",
        c.declared.range_bytes,
        format!(
            "{} pairs, {} order keys, {} posted ids",
            c.declared.range_pairs, c.declared.range_order_keys, c.declared.range_posted_ids
        ),
    );
    line(
        "declared TEXT index",
        c.declared.text_bytes,
        format!(
            "{} pairs, {} grams (no byte walk)",
            c.declared.text_pairs, c.declared.text_grams
        ),
    );
    line(
        "declared POINT index",
        c.declared.point_bytes,
        format!(
            "{} pairs, {} entries (est.)",
            c.declared.point_pairs, c.declared.point_entries
        ),
    );
    line(
        "count state",
        c.count_stats_bytes,
        format!(
            "{} rel types, {} pairs, {} labels, {} nodes",
            c.count_rel_types, c.count_pair_entries, c.count_label_entries, c.count_node_total
        ),
    );
    line(
        "degree histograms",
        c.degree_hist_bytes,
        format!("{} (label,type) histograms", c.degree_hist_entries),
    );
    line("constraint keys", c.constraint_key_bytes, String::new());
    line(
        "interner",
        c.interner_bytes,
        format!("{} distinct strings", c.interner_keys),
    );
    println!();
    line("PHYSICAL TOTAL (sum)", c.physical_total(), String::new());
    println!(
        "strides: node version {} B, rel version {} B",
        c.node_version_stride, c.rel_version_stride
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let res = match args.as_slice() {
        [cmd, a, b] if cmd == "build" => build(Path::new(a), Path::new(b)),
        [cmd, a] if cmd == "checkpoint" => checkpoint(Path::new(a)),
        [cmd, a] if cmd == "restore" => restore(Path::new(a)),
        [cmd, a] if cmd == "rebuild-indexes" => rebuild_indexes(Path::new(a)),
        _ => Err(
            "usage: heap_census build <csv-dir> <store> | checkpoint <store> | \
             restore <store> | rebuild-indexes <store>"
                .to_string(),
        ),
    };
    if let Err(e) = res {
        eprintln!("heap_census: {e}");
        std::process::exit(2);
    }
}
