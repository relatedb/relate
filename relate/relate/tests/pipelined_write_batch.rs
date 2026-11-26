//! ADR-0642: a pipelined write batch coalesces the eligible CREATE-only
//! auto-commit statements of one framed Bolt message batch into one WAL
//! commit. The commit count is the WAL LSN delta, the same counter the
//! ADR-0209 bulk tests read.

mod common;

use common::{err_text, run, scalar_one};

/// WAL records committed so far. Each commit advances the LSN by one.
fn committed_records(db: &relate::Database) -> u64 {
    relate::test_opts::wal_lsn(db).unwrap()
}

#[test]
fn a_create_only_batch_commits_once_and_applies_every_statement() {
    let db = relate::Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let before = committed_records(&db);
    conn.begin_write_batch();
    for id in 1..=5 {
        run(
            &mut conn,
            &format!("UNWIND [{id}] AS id CREATE (:N {{id: id}})"),
        );
    }
    let bookmark = conn
        .commit_write_batch()
        .unwrap()
        .expect("the batch created nodes");
    assert!(bookmark.starts_with("relate:"), "{bookmark}");
    assert_eq!(
        committed_records(&db) - before,
        1,
        "the batch is one WAL record"
    );
    assert_eq!(
        scalar_one(&mut conn, "MATCH (n:N) RETURN count(n)"),
        relate::Value::Int(5)
    );
}

#[test]
fn an_empty_batch_commits_nothing() {
    let db = relate::Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let before = committed_records(&db);
    conn.begin_write_batch();
    run(&mut conn, "MATCH (n) RETURN count(n)");
    assert!(conn.commit_write_batch().unwrap().is_none());
    assert_eq!(committed_records(&db), before);
}

#[test]
fn a_statement_outside_the_create_shape_ends_the_batch_first() {
    let db = relate::Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let before = committed_records(&db);
    conn.begin_write_batch();
    run(&mut conn, "CREATE (:N {id: 1})");
    // Not a CREATE-only statement: it must run on the committed state, so
    // the batch commits first and the write keeps its own commit.
    run(&mut conn, "MERGE (:N {id: 2})");
    assert_eq!(committed_records(&db) - before, 2);
    // The batch ended with that statement; a later CREATE commits alone.
    run(&mut conn, "CREATE (:N {id: 3})");
    assert_eq!(committed_records(&db) - before, 3);
    conn.commit_write_batch().unwrap();
    assert_eq!(
        scalar_one(&mut conn, "MATCH (n:N) RETURN count(n)"),
        relate::Value::Int(3)
    );
}

#[test]
fn a_leading_ineligible_statement_closes_the_batch_for_the_rest() {
    let db = relate::Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let before = committed_records(&db);
    conn.begin_write_batch();
    // The framed batch's first RUN cannot join: the batch never opens, and
    // the rest of that batch commits per statement (ADR-0642 §1).
    run(&mut conn, "MATCH (n) RETURN count(n)");
    for id in 1..=3 {
        run(&mut conn, &format!("CREATE (:N {{id: {id}}})"));
    }
    assert_eq!(committed_records(&db) - before, 3);
    assert!(conn.commit_write_batch().unwrap().is_none());
    assert_eq!(
        scalar_one(&mut conn, "MATCH (n:N) RETURN count(n)"),
        relate::Value::Int(3)
    );
}

#[test]
fn without_an_armed_batch_each_statement_commits_alone() {
    let db = relate::Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let before = committed_records(&db);
    for id in 1..=3 {
        run(&mut conn, &format!("CREATE (:N {{id: {id}}})"));
    }
    assert_eq!(committed_records(&db) - before, 3);
}

#[test]
fn a_failing_batched_statement_keeps_the_prefix_and_drops_its_own_write() {
    let db = relate::Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    run(
        &mut conn,
        "CREATE CONSTRAINT uniq FOR (n:N) REQUIRE n.id IS UNIQUE",
    );
    let before = committed_records(&db);
    conn.begin_write_batch();
    run(&mut conn, "CREATE (:N {id: 1})");
    let error = err_text(conn.query("CREATE (:N {id: 1})"));
    assert!(
        error.contains("ConstraintValidationFailed"),
        "unexpected error: {error}"
    );
    // The failing statement's own write is discarded; the batch's earlier
    // statement still commits, like the auto-commit statements they replace.
    assert!(conn.commit_write_batch().unwrap().is_some());
    assert_eq!(committed_records(&db) - before, 1);
    assert_eq!(
        scalar_one(&mut conn, "MATCH (n:N) RETURN count(n)"),
        relate::Value::Int(1)
    );
}

/// ADR-0642: a pipelined write batch is not the user's transaction, so a
/// ROLLBACK must not discard statements that already answered SUCCESS on the
/// wire — it ends the batch (committing it) and finds no transaction.
#[test]
fn a_rollback_ends_the_batch_instead_of_discarding_its_statements() {
    let db = relate::Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let before = committed_records(&db);
    conn.begin_write_batch();
    run(&mut conn, "CREATE (:N {id: 1})");
    let error = conn.rollback().expect_err("a batch is not a transaction");
    assert!(
        matches!(&error, relate::Error::Semantic(message) if message == "no transaction"),
        "unexpected error: {error}"
    );
    assert_eq!(
        committed_records(&db) - before,
        1,
        "the batch's statement committed rather than being discarded"
    );
    assert_eq!(
        scalar_one(&mut conn, "MATCH (n:N) RETURN count(n)"),
        relate::Value::Int(1)
    );
}
