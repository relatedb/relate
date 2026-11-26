//! The statement deadline (ADR-0692): a connection's budget cancels a
//! statement that outlasts it with the typed `Error::TimedOut`, a
//! `CALL … IN TRANSACTIONS` run keeps only its committed batches and is
//! never carried on by `ON ERROR CONTINUE`, and the connection stays
//! usable afterwards.

use std::time::Duration;

use relate::{Database, Error, Value};

/// A budget as a server default's resolved limit: not the caller's choice.
fn server_default(conn: &mut relate::Connection, budget: Option<Duration>) {
    conn.set_statement_limit(budget.map(|budget| relate::StatementLimit {
        budget,
        requested: false,
    }));
}

const LONG: &str = "UNWIND range(1, 2000000000) AS x RETURN count(x) AS n";

fn count(conn: &mut relate::Connection, cypher: &str) -> i64 {
    match &conn.query(cypher).unwrap().rows[0][0] {
        Value::Int(n) => *n,
        other => panic!("expected an Int, got {other:?}"),
    }
}

#[test]
fn a_statement_past_its_budget_fails_typed_and_the_connection_survives() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    server_default(&mut conn, Some(Duration::from_millis(50)));
    let error = conn.query(LONG).unwrap_err();
    assert!(
        matches!(
            error,
            Error::TimedOut {
                limit_ms: 50,
                requested: false
            }
        ),
        "{error:?}"
    );
    assert_eq!(error.to_string(), "query timed out after 50ms");
    assert_eq!(
        count(&mut conn, "RETURN 1 AS n"),
        1,
        "the next statement runs"
    );
}

#[test]
fn the_requested_flag_follows_who_set_the_budget() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    conn.set_statement_timeout(Some(Duration::from_secs(2)));
    assert_eq!(conn.statement_timeout(), Some(Duration::from_secs(2)));
    conn.set_statement_timeout(Some(Duration::from_millis(50)));
    assert!(matches!(
        conn.query(LONG).unwrap_err(),
        Error::TimedOut {
            requested: true,
            ..
        }
    ));
}

#[test]
fn a_statement_within_its_budget_and_an_unset_budget_are_untouched() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    server_default(&mut conn, Some(Duration::from_secs(30)));
    assert_eq!(
        count(&mut conn, "UNWIND range(1, 1000) AS x RETURN count(x) AS n"),
        1000
    );
    server_default(&mut conn, None);
    assert_eq!(conn.statement_timeout(), None);
    assert_eq!(
        count(&mut conn, "UNWIND range(1, 1000) AS x RETURN count(x) AS n"),
        1000
    );
}

#[test]
fn a_plain_cancel_is_still_cancelled_not_timed_out() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    server_default(&mut conn, Some(Duration::from_secs(60)));
    let token = relate::CancelToken::new();
    let stopper = token.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        stopper.cancel();
    });
    let error = conn
        .query_with_params_cancellable(LONG, Default::default(), token)
        .unwrap_err();
    handle.join().unwrap();
    assert!(matches!(error, Error::Cancelled), "{error:?}");
}

/// ADR-0692 §4: the deadline lands inside a batch body, that batch is
/// rolled back, and `ON ERROR CONTINUE` does not absorb the cancellation
/// (it would otherwise record the failed batch and run the next one).
#[test]
fn a_timeout_inside_a_batch_is_not_absorbed_by_on_error_continue() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    conn.query("UNWIND range(1, 3) AS i CREATE (:Done {i: i})")
        .unwrap();
    server_default(&mut conn, Some(Duration::from_millis(100)));
    let error = conn
        .query(
            "UNWIND range(1, 10) AS i \
             CALL { WITH i UNWIND range(1, 2000000000) AS x WITH i, count(x) AS c \
                    CREATE (:T {i: i, c: c}) } \
             IN TRANSACTIONS OF 1 ROWS ON ERROR CONTINUE \
             RETURN count(*) AS n",
        )
        .unwrap_err();
    assert!(matches!(error, Error::TimedOut { .. }), "{error:?}");
    server_default(&mut conn, None);
    assert_eq!(
        count(&mut conn, "MATCH (t:T) RETURN count(t) AS n"),
        0,
        "the in-flight batch rolled back"
    );
    assert_eq!(count(&mut conn, "MATCH (d:Done) RETURN count(d) AS n"), 3);
}

/// Batches that committed before the deadline stay committed.
#[test]
fn batches_committed_before_the_deadline_stay() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    server_default(&mut conn, Some(Duration::from_millis(150)));
    let error = conn
        .query(
            "UNWIND range(1, 3000000) AS i \
             CALL { WITH i CREATE (:T {i: i}) } IN TRANSACTIONS OF 1000 ROWS \
             RETURN count(*) AS n",
        )
        .unwrap_err();
    assert!(matches!(error, Error::TimedOut { .. }), "{error:?}");
    server_default(&mut conn, None);
    let kept = count(&mut conn, "MATCH (t:T) RETURN count(t) AS n");
    assert!(kept < 3_000_000, "the statement stopped at its deadline");
    assert_eq!(kept % 1000, 0, "only whole batches stay committed: {kept}");
}

/// A zero duration is "no bound", never an already-expired deadline.
#[test]
fn a_zero_budget_means_no_bound() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    conn.set_statement_timeout(Some(Duration::ZERO));
    assert_eq!(conn.statement_timeout(), None);
    assert_eq!(count(&mut conn, "RETURN 1 AS n"), 1);
    server_default(&mut conn, Some(Duration::ZERO));
    assert_eq!(conn.statement_timeout(), None);
}
