//! LABS-263 / ADR-0209: global bulk writes run in bounded batches on the
//! auto-commit path. These tests force batching on small graphs through
//! `RELATE_BULK_BATCH_THRESHOLD` / `RELATE_BULK_BATCH_BUDGET` and
//! assert both the semantics (final state, write stats, atomicity of the
//! preflighted error) and the batching itself (one WAL record per batch,
//! counted as the WAL LSN delta).

mod common;

use common::{run, scalar_one, BatchEnv};

/// WAL records committed so far. Each commit advances the LSN by one.
fn committed_records(db: &relate::Database) -> u64 {
    relate::test_opts::wal_lsn(db).unwrap()
}

#[test]
fn detach_delete_dense_store_batches_and_completes() {
    let _env = BatchEnv::forced();
    let db = common::env_seam::open(":memory:", relate::OpenOptions::default());
    let mut conn = db.connect().unwrap();
    // 60 nodes, all pairs connected: 3600 rels — the ticket's dense store.
    run(&mut conn, "UNWIND range(1, 60) AS i CREATE (:N {id: i})");
    run(
        &mut conn,
        "MATCH (n) MATCH (m) WITH n AS a, m AS b CREATE (a)-[:R]->(b)",
    );
    assert_eq!(
        scalar_one(&mut conn, "MATCH ()-[r]->() RETURN count(r)"),
        relate::Value::Int(3600)
    );
    let before = committed_records(&db);
    let result = run(&mut conn, "MATCH (n) DETACH DELETE n");
    let batches = committed_records(&db) - before;
    assert!(
        batches > 1,
        "detach delete of ~3660 entities must batch, got {batches} record(s)"
    );
    assert_eq!(result.stats.nodes_deleted, 60);
    assert_eq!(result.stats.relationships_deleted, 3600);
    assert_eq!(
        scalar_one(&mut conn, "MATCH (n) RETURN count(n)"),
        relate::Value::Int(0)
    );
}

#[test]
fn cross_product_create_batches_and_completes() {
    let _env = BatchEnv::forced();
    let db = common::env_seam::open(":memory:", relate::OpenOptions::default());
    let mut conn = db.connect().unwrap();
    run(&mut conn, "UNWIND range(1, 40) AS i CREATE (:N {id: i})");
    let before = committed_records(&db);
    let result = run(
        &mut conn,
        "MATCH (n) MATCH (m) WITH n AS a, m AS b CREATE (a)-[:TAGGED]->(b)",
    );
    let batches = committed_records(&db) - before;
    // 1600 combinations at a budget of 100 per batch: 16 batches.
    assert_eq!(batches, 16, "expected 16 batch commits, got {batches}");
    assert_eq!(result.stats.relationships_created, 1600);
    assert_eq!(
        scalar_one(&mut conn, "MATCH ()-[r:TAGGED]->() RETURN count(r)"),
        relate::Value::Int(1600)
    );
}

#[test]
fn set_remove_and_rel_delete_batch_with_correct_stats() {
    let _env = BatchEnv::forced();
    let db = common::env_seam::open(":memory:", relate::OpenOptions::default());
    let mut conn = db.connect().unwrap();
    run(
        &mut conn,
        "UNWIND range(1, 250) AS i CREATE (:N {id: i, x: 1})-[:R {k: i}]->(:M {id: i})",
    );
    let result = run(&mut conn, "MATCH (n:N) SET n.x = n.x + 1, n:Tagged");
    assert_eq!(result.stats.properties_set, 250);
    assert_eq!(
        scalar_one(&mut conn, "MATCH (n:Tagged) RETURN sum(n.x)"),
        relate::Value::Int(500)
    );
    let result = run(&mut conn, "MATCH (n:Tagged) REMOVE n:Tagged");
    assert_eq!(result.stats.labels_removed, 1);
    assert_eq!(
        scalar_one(&mut conn, "MATCH (n:Tagged) RETURN count(n)"),
        relate::Value::Int(0)
    );
    let result = run(&mut conn, "MATCH ()-[r]->() DELETE r");
    assert_eq!(result.stats.relationships_deleted, 250);
    assert_eq!(
        scalar_one(&mut conn, "MATCH (n) RETURN count(n)"),
        relate::Value::Int(500)
    );
}

#[test]
fn non_detach_delete_error_is_preflighted_atomically() {
    let _env = BatchEnv::forced();
    let db = common::env_seam::open(":memory:", relate::OpenOptions::default());
    let mut conn = db.connect().unwrap();
    run(
        &mut conn,
        "UNWIND range(1, 120) AS i CREATE (:N {id: i})-[:R]->(:M {id: i})",
    );
    let error = common::err_text(conn.query("MATCH (n:N) DELETE n"));
    assert!(
        error.contains("DeleteConnectedNode"),
        "unexpected error: {error}"
    );
    // ADR-0565: live Neo4j reports the ConstraintVerificationFailed class.
    assert_eq!(
        common::error_class_literal(&mut conn, "MATCH (n:N) DELETE n"),
        "ConstraintVerificationFailed"
    );
    // The preflight raised before any batch committed.
    assert_eq!(
        scalar_one(&mut conn, "MATCH (n) RETURN count(n)"),
        relate::Value::Int(240)
    );
}

#[test]
fn constrained_catalog_keeps_the_atomic_unbatched_path() {
    let _env = BatchEnv::forced();
    let db = common::env_seam::open(":memory:", relate::OpenOptions::default());
    let mut conn = db.connect().unwrap();
    run(
        &mut conn,
        "CREATE CONSTRAINT c FOR (n:N) REQUIRE n.id IS UNIQUE",
    );
    run(&mut conn, "UNWIND range(1, 120) AS i CREATE (:N {id: i})");
    let before = committed_records(&db);
    let error = common::err_text(conn.query("MATCH (n:N) SET n.id = 1"));
    assert!(
        error.contains("ConstraintValidationFailed"),
        "unexpected error: {error}"
    );
    // Unbatched: the failed statement committed nothing at all.
    assert_eq!(committed_records(&db), before);
    assert_eq!(
        scalar_one(&mut conn, "MATCH (n:N) WHERE n.id = 1 RETURN count(n)"),
        relate::Value::Int(1)
    );
}

#[test]
fn below_threshold_writes_keep_a_single_commit() {
    let _env = BatchEnv::disabled();
    let db = common::env_seam::open(":memory:", relate::OpenOptions::default());
    let mut conn = db.connect().unwrap();
    run(&mut conn, "UNWIND range(1, 40) AS i CREATE (:N {id: i})");
    let before = committed_records(&db);
    run(&mut conn, "MATCH (n) DETACH DELETE n");
    assert_eq!(committed_records(&db) - before, 1);
}
