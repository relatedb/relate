//! Scalar-function behavior parity with live Neo4j, each expectation
//! probed against the pinned oracle (2026.10.1, bolt://localhost:7687)
//! before it was pinned: `^`'s Java `Math.pow` corners, `toInteger`'s
//! boolean conversion, and `valueType`'s nested list union subsumption.

mod common;

use relate::Value;

use common::{connection, row};

/// Java's `Math.pow` (Neo4j) answers NaN when the base's absolute value is
/// exactly 1 and the exponent is not finite; IEEE `pow` (Rust's `powf`)
/// answers 1 there. The other NaN/infinity corners already agree.
#[test]
fn power_nan_and_infinity_corners_match_live() {
    let mut c = connection();
    let got = row(
        &mut c,
        "RETURN (0/0.0) ^ 1 AS a, 1 ^ (0/0.0) AS b, (-8) ^ (1/3.0) AS c, \
                0 ^ -1 AS d, 2 ^ (1/0.0) AS e, 0.5 ^ (1/0.0) AS f, \
                2 ^ (-1/0.0) AS g, (1/0.0) ^ 0 AS h, (1/0.0) ^ 1 AS i, \
                (1/0.0) ^ -1 AS j, 0 ^ 0 AS k, 0 ^ 2 AS l, \
                1 ^ (1/0.0) AS m, (-1) ^ (1/0.0) AS n, 1 ^ (-1/0.0) AS o, \
                (-1) ^ (-1/0.0) AS p",
    );
    let inf = f64::INFINITY;
    let want = [
        f64::NAN,
        f64::NAN,
        f64::NAN,
        inf,
        inf,
        0.0,
        0.0,
        1.0,
        inf,
        0.0,
        1.0,
        0.0,
        f64::NAN,
        f64::NAN,
        f64::NAN,
        f64::NAN,
    ];
    assert_eq!(got.len(), want.len());
    for (index, want) in want.into_iter().enumerate() {
        match got[index] {
            Value::Float(f) if want.is_nan() => assert!(f.is_nan(), "column {index}: {f}"),
            Value::Float(f) => assert_eq!(f, want, "column {index}"),
            ref other => panic!("column {index}: expected a float, got {other:?}"),
        }
    }
}

/// `toIntegerOrNull` (and `toInteger`) convert a boolean to 1/0; a value
/// with no conversion still yields null under the `OrNull` form.
#[test]
fn to_integer_or_null_converts_booleans() {
    let mut c = connection();
    assert_eq!(
        row(
            &mut c,
            "RETURN toIntegerOrNull(true) AS a, toIntegerOrNull(false) AS b, \
                    toInteger(true) AS c, toIntegerOrNull('three') AS d, \
                    toIntegerOrNull({}) AS e, toIntegerOrNull(null) AS f",
        ),
        vec![
            Value::Int(1),
            Value::Int(0),
            Value::Int(1),
            Value::Null,
            Value::Null,
            Value::Null,
        ]
    );
}

/// A nested list's element types merge by subtype subsumption: the
/// FLOAT-bearing list absorbs the INTEGER one, so the union renders as one
/// `LIST<INTEGER | FLOAT>` element type; a nullable-element list subsumes
/// the non-null list of the same members.
#[test]
fn value_type_merges_nested_list_unions() {
    let mut c = connection();
    assert_eq!(
        row(
            &mut c,
            "WITH [[1, 2], null, [1.0, 1, null]] AS val RETURN valueType(val) AS result",
        ),
        vec![Value::String("LIST<LIST<INTEGER | FLOAT>> NOT NULL".into())]
    );
    assert_eq!(
        row(&mut c, "RETURN valueType([[1, null], [2]]) AS result",),
        vec![Value::String(
            "LIST<LIST<INTEGER> NOT NULL> NOT NULL".into()
        )]
    );
}
