//! `left()`, `right()` and `substring()` null propagation on a start/length
//! argument (FunctionExpressionsITPart4). Live Neo4j 2026.10.1 probes
//! (2026-11-08) — every null position nulls the call, before a sibling
//! argument's range check; the string argument's type check still runs
//! first (`left(1, null)` is a type error). A *present* non-numeric
//! start/length is that same kind of type error (probed 2026-11-10: a
//! literal reports `Neo.ClientError.Statement.SyntaxError`, a
//! runtime-typed value `Neo.ClientError.Statement.TypeError`), never the
//! absent-argument default.

mod common;

use common::connection;
use relate::Value;
use relate_core::integer_args;

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

/// A wrong-typed start/length raises the type rejection rather than being
/// folded into the absent-argument default — at plan time for a literal,
/// from the argument coercion for a runtime-typed value. A null sibling
/// still nulls the call first, and a genuinely absent argument keeps the
/// default.
#[test]
fn non_numeric_start_and_length_are_type_errors() {
    let mut c = connection();
    for (statement, site) in [
        (
            "RETURN substring('abcdef', 'x') AS r",
            &integer_args::SUBSTRING_START,
        ),
        (
            "RETURN substring('abcdef', 1, 'x') AS r",
            &integer_args::SUBSTRING_LENGTH,
        ),
        (
            "RETURN left('abcdef', 'x') AS r",
            &integer_args::LEFT_LENGTH,
        ),
        (
            "RETURN right('abcdef', 'x') AS r",
            &integer_args::RIGHT_LENGTH,
        ),
        // A temporal constructor call the front end folds to an eager
        // value (live Neo4j 2026.10.1 raises the same 22N27 TypeError).
        (
            "RETURN substring('abcdef', date('2020-01-01')) AS r",
            &integer_args::SUBSTRING_START,
        ),
        (
            "RETURN substring('abcdef', head(['x', '1'])) AS r",
            &integer_args::SUBSTRING_START,
        ),
        (
            "RETURN substring('abcdef', 1, head(['x', '2'])) AS r",
            &integer_args::SUBSTRING_LENGTH,
        ),
        (
            "RETURN left('abcdef', head(['x', '1'])) AS r",
            &integer_args::LEFT_LENGTH,
        ),
        (
            "RETURN right('abcdef', head(['x', '1'])) AS r",
            &integer_args::RIGHT_LENGTH,
        ),
    ] {
        let error = c.query(statement).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("type error: InvalidArgumentType: {}", site.type_error),
            "{statement}"
        );
        assert_eq!(
            relate::binding_error_metadata(&error),
            ("TypeError", "TypeError"),
            "{statement}"
        );
    }

    // The null pre-scan still wins, and the absent-length/start defaults
    // stay.
    let r = c
        .query(
            "RETURN substring('abcdef', head([null, 'x'])) AS a, \
             substring('abcdef') AS b, left('abcdef') AS c",
        )
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::Null, s("abcdef"), s("")]]);
}
