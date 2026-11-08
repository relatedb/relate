//! `left()`, `right()` and `substring()` null propagation on a start/length
//! argument (FunctionExpressionsITPart4). Live Neo4j 2026.10.1 probes
//! (2026-11-08) — every null position nulls the call, before a sibling
//! argument's range check; the string argument's type check still runs
//! first (`left(1, null)` is a type error).

mod common;

use common::connection;
use relate::Value;

fn s(v: &str) -> Value {
    Value::String(v.into())
}

#[test]
fn left_and_right_null_propagation() {
    let mut c = connection();
    let r = c
        .query(
            "RETURN left('HELLO', 4) AS a, left('HELLO', 17) AS b, left(null, 4) AS c, \
             left('HELLO', null) AS d, left(null, null) AS e",
        )
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![
            s("HELL"),
            s("HELLO"),
            Value::Null,
            Value::Null,
            Value::Null
        ]]
    );

    let r = c
        .query(
            "RETURN right('HELLO', 4) AS a, right(null, 4) AS b, \
             right('HELLO', null) AS c, right(null, null) AS d",
        )
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![s("ELLO"), Value::Null, Value::Null, Value::Null]]
    );

    // The null beats the sibling argument's range check.
    let r = c.query("RETURN left(null, -1) AS r").unwrap();
    assert_eq!(r.rows, vec![vec![Value::Null]]);
}

#[test]
fn substring_null_propagation() {
    let mut c = connection();
    let r = c
        .query(
            "RETURN substring('HELLO', 1) AS a, substring(null, 1) AS b, \
             substring('HELLO', null) AS c, substring(null, null) AS d",
        )
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![s("ELLO"), Value::Null, Value::Null, Value::Null]]
    );

    let r = c
        .query(
            "RETURN substring('HELLO', 1, 2) AS a, substring(null, 1, 2) AS b, \
             substring('HELLO', null, 2) AS c, substring('HELLO', 1, null) AS d",
        )
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![s("EL"), Value::Null, Value::Null, Value::Null]]
    );

    // Null wins over the sibling's range check in either direction.
    let r = c
        .query(
            "RETURN substring('HELLO', null, 3708444074355087135) AS a, \
             substring('HELLO', -1, null) AS b",
        )
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::Null, Value::Null]]);

    // A range error with no null anywhere is still raised.
    assert!(c.query("RETURN substring('HELLO', -1, 2) AS r").is_err());
}
