//! The bulk harness's end-to-end and schema tests, split out of
//! `bulk.rs` to keep that module off the file-size ceiling (AGENTS.md:
//! no 1k-line creep).

use super::*;
use crate::memory::format_bytes;

#[test]
fn op_ids_match_serde_names() {
    for op in BulkOp::all() {
        let json = serde_json::to_value(op).unwrap();
        assert_eq!(json, serde_json::json!(op.id()));
        assert_eq!(BulkOp::parse(op.id()).unwrap(), op);
    }
    assert_eq!(BulkOp::parse("merge").unwrap(), BulkOp::MergeWithConstraint);
}

#[test]
fn op_statements_are_the_documented_batch_cypher() {
    assert!(CREATE_CYPHER.contains("CREATE (p:Person"));
    assert!(MERGE_CYPHER.contains("MERGE (p:Person {id: id})"));
    assert!(UPDATE_CYPHER.contains("SET p.age = p.age + 1"));
    assert!(DELETE_CYPHER.contains("DETACH DELETE p"));
    assert!(CONSTRAINT_CYPHER.contains("REQUIRE p.id IS UNIQUE"));
    for op in BulkOp::all() {
        assert!(op.cypher().starts_with("UNWIND range(1,$n) AS id"));
    }
    assert!(!BulkOp::Create.needs_base());
    assert!(!BulkOp::Create.needs_constraint());
    assert!(BulkOp::Update.needs_constraint());
}

#[test]
fn measurement_uri_is_per_op_and_size() {
    assert_eq!(
        measurement_uri(":memory:", BulkOp::Create, 5000),
        ":memory:"
    );
    assert_eq!(measurement_uri("", BulkOp::Create, 5000), ":memory:");
    assert_eq!(
        measurement_uri("/tmp/bulk/", BulkOp::Delete, 50_000),
        "/tmp/bulk/delete-50000"
    );
    assert_eq!(
        measurement_uri("target/d", BulkOp::MergeWithConstraint, 10_000),
        "target/d/merge-with-constraint-10000"
    );
}

#[test]
fn default_sizes_match_the_documented_batch_ladder() {
    assert_eq!(DEFAULT_SIZES, [5_000, 10_000, 50_000, 100_000]);
    assert_eq!(BulkConfig::default().ops.len(), 4);
    assert_eq!(BulkConfig::default().reps, 1);
    assert_eq!(BulkConfig::default().durability_mode, DurabilityMode::Full);
    assert!(!BulkConfig::default().commit_timing);
}

/// The six diagnostic rows the engine renders, in order. Pinning them
/// here makes an engine-side rename a failing test, not a silent
/// reshape of schema-2 reports.
const COMMIT_TIMING_STEPS: [&str; 6] = [
    "parse+plan",
    "overlay execute",
    "wal_lock wait",
    "encode+write",
    "sync_all",
    "apply_redo",
];

/// Assert every pinned step appears as a *row* of the rendered dump. The
/// trailing space matters: the report header also names `sync_all`
/// ("N durable commits, M sync_all):"), so a bare `contains` would pass
/// without the row being there.
fn assert_step_rows(timing: &str) {
    for step in COMMIT_TIMING_STEPS {
        assert!(
            timing.contains(&format!("{step} ")),
            "{step} row missing: {timing}"
        );
    }
}

/// `--commit-timing` records the commit-step diagnostics into the entry;
/// off it is `None`. On, both persistent and in-memory databases record
/// (in-memory with `sync_all` ~0, which isolates engine cost from the
/// barrier). The two flag values must produce distinguishable entries.
#[test]
fn commit_timing_flag_is_recorded_only_when_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let base = BulkConfig {
        dir: dir.path().to_string_lossy().into(),
        ops: vec![BulkOp::Create],
        sizes: vec![1],
        reps: 2,
        warmup: 0,
        out: None,
        ..BulkConfig::default()
    };
    let on = run(&BulkConfig {
        commit_timing: true,
        ..base.clone()
    })
    .unwrap();
    let entry = on.entry(BulkOp::Create, 1).unwrap();
    assert_eq!(entry.errors, 0, "{:?}", entry.error);
    assert_step_rows(entry.commit_timing.as_ref().expect("histograms recorded"));
    assert!(on.config.commit_timing);

    let off = run(&base).unwrap();
    assert!(!off.config.commit_timing);
    assert!(off
        .entry(BulkOp::Create, 1)
        .unwrap()
        .commit_timing
        .is_none());

    let memory = run(&BulkConfig {
        dir: ":memory:".into(),
        commit_timing: true,
        ..base.clone()
    })
    .unwrap();
    let memory_entry = memory.entry(BulkOp::Create, 1).unwrap();
    assert_eq!(memory_entry.errors, 0, "{:?}", memory_entry.error);
    assert_step_rows(
        memory_entry
            .commit_timing
            .as_ref()
            .expect("in-memory commits are sampled"),
    );
}

/// Schema-1 reports (no `durability_mode`, no `commit_timing`) still
/// deserialize, defaulting to `full`.
#[test]
fn schema_one_reports_deserialize_with_defaults() {
    let json = serde_json::json!({
        "schema_version": 1,
        "name": "bulk-loadtest",
        "started_at": "2026-11-11T00:00:00.000Z",
        "finished_at": "2026-11-11T00:00:01.000Z",
        "started_at_unix_ms": 0,
        "finished_at_unix_ms": 1000,
        "duration_ms": 1000,
        "engine": "relate",
        "engine_version": "0.0.0",
        "config": {
            "dir": "target/bulk-loadtest-data",
            "ops": ["create"],
            "sizes": [5000],
            "reps": 1,
            "warmup": 0
        },
        "results": [{
            "op": "create", "size": 5000, "reps": 1, "warmup": 0,
            "constraint": false, "statements": 1, "errors": 0,
            "error": null, "latency_ns": null,
            "rows": 5000, "setup_ms": 0, "duration_ms": 10,
            "ops_per_sec": 100.0, "rows_per_sec": 500000.0,
            "nodes_before": 0, "nodes_after": 5000,
            "rss_bytes": 0, "logical_used_bytes": 0,
            "uri": "target/bulk-loadtest-data/create-5000"
        }]
    });
    let report: BulkReport = serde_json::from_value(json).unwrap();
    assert_eq!(report.config.durability_mode, DurabilityMode::Full);
    assert!(report.results[0].commit_timing.is_none());
}

#[test]
fn config_validation_rejects_empty_inputs() {
    let bad = BulkConfig {
        ops: Vec::new(),
        ..BulkConfig::default()
    };
    assert!(bad.validate().is_err());
    let bad = BulkConfig {
        sizes: Vec::new(),
        ..BulkConfig::default()
    };
    assert!(bad.validate().is_err());
    let bad = BulkConfig {
        reps: 0,
        ..BulkConfig::default()
    };
    assert!(bad.validate().is_err());
    assert!(BulkConfig::default().validate().is_ok());
}

/// End-to-end on a small size (in-memory): every op runs, the node counts
/// move the right way, and the report round-trips through JSON.
#[test]
fn small_bulk_run_covers_every_op() {
    let cfg = BulkConfig {
        dir: ":memory:".into(),
        sizes: vec![200],
        ..BulkConfig::default()
    };
    let report = run(&cfg).unwrap();
    assert_eq!(report.results.len(), 4);
    assert_eq!(report.total_rows(), 800);

    let create = report.entry(BulkOp::Create, 200).unwrap();
    assert_eq!((create.nodes_before, create.nodes_after), (0, 200));
    assert!(!create.constraint);
    assert_eq!(create.errors, 0, "{:?}", create.error);
    assert_eq!(create.reps, 1);
    assert!(create.latency_ns.is_some());

    let merge = report.entry(BulkOp::MergeWithConstraint, 200).unwrap();
    assert_eq!((merge.nodes_before, merge.nodes_after), (200, 200));
    assert!(merge.constraint);
    assert_eq!(merge.errors, 0, "{:?}", merge.error);

    let update = report.entry(BulkOp::Update, 200).unwrap();
    assert_eq!((update.nodes_before, update.nodes_after), (200, 200));
    assert_eq!(update.errors, 0, "{:?}", update.error);

    let delete = report.entry(BulkOp::Delete, 200).unwrap();
    assert_eq!((delete.nodes_before, delete.nodes_after), (200, 0));
    assert_eq!(delete.errors, 0, "{:?}", delete.error);

    let json = serde_json::to_string(&report).unwrap();
    let back: BulkReport = serde_json::from_str(&json).unwrap();
    assert_eq!(back.schema_version, BULK_REPORT_SCHEMA_VERSION);
    assert_eq!(back.results.len(), 4);
    assert_eq!(back.config.sizes, vec![200]);
    assert!(
        format_bytes(back.results[0].logical_used_bytes).ends_with('B')
            || !back.results[0].uri.is_empty()
    );
}

/// A failed measurement is reported as an entry, not an aborted run.
#[test]
fn failed_measurement_keeps_one_entry_per_op_and_size() {
    // A filesystem path that cannot be created (a file where a dir is
    // needed) makes the open fail.
    let tmp = tempfile::tempdir().unwrap();
    let blocker = tmp.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let cfg = BulkConfig {
        dir: blocker.to_string_lossy().into_owned(),
        sizes: vec![10],
        ops: vec![BulkOp::Create],
        ..BulkConfig::default()
    };
    let report = run(&cfg).unwrap();
    assert_eq!(report.results.len(), 1);
    let e = &report.results[0];
    assert_eq!(e.errors, 1);
    assert!(e.error.is_some());
    assert_eq!(e.rows, 0);
    assert!(e.latency_ns.is_none());
}

/// `--sample` writes the three stack outputs per (op, size) and the report
/// records where. The availability guard is asserted on every platform
/// (its outcome must equal the support constant); only the file outputs
/// need a sampler build, and their loop is long enough that the CPU-time
/// timer is guaranteed samples.
#[test]
fn sample_dir_writes_stack_outputs() {
    use relate_test_support::profiler::IN_PROCESS_SAMPLER;
    let dir = tempfile::tempdir().unwrap();
    let sample_dir = dir.path().join("profiles");
    // The guard's outcome is pinned cross-platform: ok exactly when a
    // sampler is built.
    assert_eq!(
        BulkConfig {
            sample_dir: Some(sample_dir.clone()),
            ..BulkConfig::default()
        }
        .validate()
        .is_ok(),
        IN_PROCESS_SAMPLER
    );
    if !IN_PROCESS_SAMPLER {
        return;
    }
    let report = run(&BulkConfig {
        dir: ":memory:".into(),
        ops: vec![BulkOp::Create],
        sizes: vec![100],
        reps: 100,
        commit_timing: false,
        sample_dir: Some(sample_dir.clone()),
        ..BulkConfig::default()
    })
    .unwrap();
    let recorded = sample_dir.to_string_lossy().into_owned();
    assert_eq!(report.config.sample_dir.as_deref(), Some(recorded.as_str()));
    let out = sample_dir.join("create-100");
    let top = std::fs::read_to_string(out.join("top.txt")).unwrap();
    assert!(
        top.contains("bulk-loadtest create size=100"),
        "header: {top}"
    );
    assert!(std::fs::read_to_string(out.join("folded.txt"))
        .unwrap()
        .contains(';'));
    assert!(std::fs::metadata(out.join("flame.svg")).unwrap().len() > 0);
}
