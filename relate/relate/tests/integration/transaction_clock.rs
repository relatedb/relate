//! The `.transaction` clock is one instant for a whole explicit transaction
//! (Neo4j's temporal-function contract), `.statement` and the default
//! constructors move per statement, `.realtime` per call, and an autocommit
//! statement is its own transaction.

use crate::common;

use std::thread::sleep;
use std::time::Duration;

use relate::Value;

use common::{connection, scalar_one};

const NAP: Duration = Duration::from_millis(20);

fn read(conn: &mut relate::Connection, expr: &str) -> Value {
    scalar_one(conn, &format!("RETURN {expr} AS t"))
}

#[test]
fn transaction_clock_is_constant_across_statements_of_one_transaction() {
    for kind in ["date", "time", "localtime", "datetime", "localdatetime"] {
        let mut conn = connection();
        let tx = format!("{kind}.transaction()");
        let stmt = "datetime.statement()";
        conn.begin().unwrap();
        let (t1, s1) = (read(&mut conn, &tx), read(&mut conn, stmt));
        sleep(NAP);
        let (t2, s2) = (read(&mut conn, &tx), read(&mut conn, stmt));
        assert_eq!(t1, t2, "{kind}.transaction() must not move");
        assert_ne!(s1, s2, "datetime.statement() must move per statement");
        conn.commit().unwrap();
    }
}

#[test]
fn a_new_transaction_gets_a_new_instant_after_commit_and_rollback() {
    let mut conn = connection();
    let mut seen = Vec::new();
    for finish in [true, false, true] {
        conn.begin().unwrap();
        seen.push(read(&mut conn, "datetime.transaction()"));
        sleep(NAP);
        assert_eq!(
            seen.last().unwrap(),
            &read(&mut conn, "datetime.transaction()")
        );
        if finish {
            conn.commit().unwrap();
        } else {
            conn.rollback().unwrap();
        }
        sleep(NAP);
    }
    assert_ne!(seen[0], seen[1]);
    assert_ne!(seen[1], seen[2]);
}

#[test]
fn a_failed_statement_does_not_move_the_transaction_clock() {
    let mut conn = connection();
    conn.begin().unwrap();
    let t1 = read(&mut conn, "datetime.transaction()");
    sleep(NAP);
    assert!(conn.query("RETURN 1/0 AS x").is_err());
    conn.rollback().unwrap();
    conn.begin().unwrap();
    sleep(NAP);
    assert_ne!(t1, read(&mut conn, "datetime.transaction()"));
    conn.rollback().unwrap();
}

#[test]
fn autocommit_statements_are_their_own_transaction() {
    let mut conn = connection();
    // Within one statement the three clocks of an autocommit statement agree.
    let same = scalar_one(
        &mut conn,
        "RETURN datetime.transaction() = datetime.statement() AS t",
    );
    assert_eq!(same, Value::Bool(true));
    let t1 = read(&mut conn, "datetime.transaction()");
    sleep(NAP);
    let t2 = read(&mut conn, "datetime.transaction()");
    assert_ne!(
        t1, t2,
        "separate autocommit statements are separate transactions"
    );
}

#[test]
fn timestamp_and_default_constructors_stay_per_statement_and_realtime_per_call() {
    let mut conn = connection();
    conn.begin().unwrap();
    let ts1 = read(&mut conn, "timestamp()");
    let dt1 = read(&mut conn, "datetime()");
    sleep(NAP);
    assert_ne!(ts1, read(&mut conn, "timestamp()"));
    assert_ne!(dt1, read(&mut conn, "datetime()"));
    conn.commit().unwrap();
    // Constant within one statement.
    assert_eq!(
        scalar_one(
            &mut conn,
            "UNWIND [1,2] AS i WITH collect(timestamp()) AS c RETURN c[0] = c[1] AS t"
        ),
        Value::Bool(true)
    );
}
