//! Temporal constructor and duration-bound arity: a call with more than the
//! grammar's two arguments is the typed `InvalidNumberOfArguments`
//! rejection, never a silent construction from the first argument — under
//! every preamble spelling (the preamble selects nothing). The
//! `duration.between`/`duration.in*` family's exactly-two contract lives
//! here too, beside its sibling arity policies.

mod common;

use relate::Value;

use common::{connection, err_text, scalar_one};

fn arity_error(query: &str) {
    let mut conn = connection();
    let err = err_text(conn.query(query).map(|_| ()));
    assert!(
        err.contains("InvalidNumberOfArguments"),
        "{query}: expected InvalidNumberOfArguments, got: {err}"
    );
}

#[test]
fn three_argument_constructors_are_a_typed_arity_error() {
    for query in [
        "RETURN date('2024-06-27', 'yyyy-MM-dd', 'x') AS x",
        "RETURN duration('P1D', 'y', 3) AS x",
        "RETURN LOCAL_TIME('14:30:45', 'HH:mm:ss', 'x') AS x",
        "RETURN zoned_datetime('2024-06-27T14:30:45+02:00', 'x', 'y') AS x",
    ] {
        arity_error(query);
    }
    // The zero-, one- and two-argument forms keep constructing.
    let mut conn = connection();
    for query in [
        "RETURN date('2024-06-27') AS x",
        "RETURN date('2024-06-27', 'yyyy-MM-dd') AS x",
        "RETURN date() AS x",
    ] {
        assert!(conn.query(query).is_ok(), "{query}");
    }
}

#[test]
fn a_cypher_25_preamble_keeps_the_three_argument_rejection() {
    for query in [
        "CYPHER 25 RETURN date('2024-06-27', 'yyyy-MM-dd', 'x') AS x",
        "CYPHER 25 RETURN duration('P1D', 'y', 3) AS x",
    ] {
        arity_error(query);
    }
    // The one-argument form still runs under the preamble.
    let mut conn = connection();
    assert!(conn
        .query("CYPHER 25 RETURN date('2024-06-27') AS x")
        .is_ok());
}

/// C5: `duration.between` and its `duration.in*` siblings take exactly two
/// arguments (verified live: 42I13 both ways), and the rejection names the
/// spelling the caller wrote.
#[test]
fn duration_between_requires_exactly_two_arguments() {
    let mut c = connection();
    for (query, name) in [
        (
            "RETURN duration.between(date('2021-01-01')) AS x",
            "duration.between()",
        ),
        (
            "RETURN duration.between(date('2021-01-01'), date('2021-01-02'), 1) AS x",
            "duration.between()",
        ),
        (
            "RETURN duration.inDays(date('2021-01-01')) AS x",
            "duration.indays()",
        ),
        (
            "RETURN duration.inMonths(date('2021-01-01'), date('2021-01-02'), 1) AS x",
            "duration.inmonths()",
        ),
    ] {
        let err = err_text(c.query(query).map(|_| ()));
        assert!(
            err.contains("InvalidNumberOfArguments"),
            "{query}: expected an arity rejection, got: {err}"
        );
        assert!(err.contains(name), "{query}: {err}");
    }
    // The two-argument form still answers.
    assert_eq!(
        scalar_one(
            &mut c,
            "RETURN toString(duration.inDays(date('2021-01-01'), date('2021-01-03'))) AS x",
        ),
        Value::String("P2D".into())
    );
}
