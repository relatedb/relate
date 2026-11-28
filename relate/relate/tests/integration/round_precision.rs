//! `round()` semantics: ADR-0336 bounds its precision to the int32 range
//! (above it the ArgumentError names the bounds; a precision at or above the
//! value's own decimal scale is the identity, live-verified against
//! Neo4j 2026.10.1), and the implicit mode is HALF_UP toward positive
//! infinity rather than the wire `HALF_UP` (away from zero).

use crate::common;

use relate::{Error, Value};
use relate_core::ErrorKind;

use common::{connection, error_class_literal, refused_literal, row, rows};

#[test]
fn round_precision_bounds_are_argument_errors() {
    let mut conn = connection();
    let out_of_range = "RETURN round(2.5, 2147483648) AS r";
    let negative = "RETURN round(2.5, -1) AS r";
    assert_eq!(
        refused_literal(&mut conn, out_of_range),
        Error::argument_value_kind(
            ErrorKind::NumberOutOfRange,
            "Invalid input for precision value in function 'round()': \
             Expected an integer between -2147483648 and 2147483647, but got: 2147483648"
        )
    );
    assert_eq!(
        refused_literal(&mut conn, negative),
        Error::argument_value_kind(
            ErrorKind::NumberOutOfRange,
            "Precision argument to 'round()' cannot be negative"
        )
    );
    // Live Neo4j 2026.10.1 reports both under the ArgumentError class.
    assert_eq!(error_class_literal(&mut conn, negative), "ArgumentError");
}

/// Live Neo4j 2026.10.1: a precision at or above the value's own decimal
/// scale returns the value unchanged — no zero padding, no cap error. The
/// corpus rows this pins are `round(1.9951, 5)`/`(…, 2147483647)` and
/// `round(3.141592653589793, 500)`.
#[test]
fn round_high_precision_is_the_identity() {
    let mut c = connection();
    assert_eq!(
        row(
            &mut c,
            "RETURN round(1.9951, 5) AS p5, round(1.9951, 2147483647) AS pMax"
        ),
        vec![Value::Float(1.9951), Value::Float(1.9951)]
    );
    assert_eq!(
        row(&mut c, "RETURN round(3.141592653589793, 500) AS rounded"),
        vec![Value::Float(std::f64::consts::PI)]
    );
    // An in-range precision at the value's own scale, the plain form.
    assert_eq!(
        row(&mut c, "RETURN round(2.5, 2) AS r"),
        vec![Value::Float(2.5)]
    );
}

// The implicit `round()` mode is HALF_UP toward positive infinity — not the
// wire `HALF_UP` (away from zero) — and values past 2^52 are the identity.
#[test]
fn round_implicit_mode_and_large_magnitudes() {
    let mut c = connection();
    assert_eq!(
        row(
            &mut c,
            "RETURN round(-1.5) AS a, round(-1.5, 0, 'HALF_UP') AS b, \
                    round(-0.5) AS c, round(0.5) AS d, round(1.5) AS e",
        ),
        vec![
            Value::Float(-1.0),
            Value::Float(-2.0),
            Value::Float(0.0),
            Value::Float(1.0),
            Value::Float(2.0)
        ]
    );
    // Beyond 2^52 there is no fractional part to round: scaling by the
    // precision used to inject ulp noise (sqrt(1e308) with precision 5
    // came back as 1.0000000000000002e+154).
    assert_eq!(
        row(&mut c, "RETURN round(sqrt(1e308), 5) AS a")[0],
        Value::Float(1e308_f64.sqrt())
    );
    assert_eq!(
        row(&mut c, "RETURN round(1e308) AS a")[0],
        Value::Float(1e308)
    );
    // The odd ulp in [2^52, 2^53): `x + 0.5` ties up one ulp, while live
    // Neo4j (Java's exact `Math.round`) keeps the identity.
    assert_eq!(
        row(
            &mut c,
            "RETURN round(4503599627370497.0) AS a, round(-4503599627370497.0) AS b"
        ),
        vec![
            Value::Float(4503599627370497.0),
            Value::Float(-4503599627370497.0)
        ]
    );
    assert_eq!(
        row(&mut c, "RETURN round(1.5) = round(1.5, 0) AS same")[0],
        Value::Bool(true)
    );
}

/// `round()` rounds the shortest decimal spelling (Java's
/// `BigDecimal.valueOf(...).setScale`), not the binary image: 265.335 is
/// `265.33499999999997` as a double, but its shortest spelling is
/// `265.335`, so two decimals give 265.34 (live Neo4j 2026.10.1).
#[test]
fn round_follows_the_shortest_decimal_spelling() {
    let mut c = connection();
    assert_eq!(
        row(&mut c, "RETURN round(265.335, 2) AS rounded"),
        vec![Value::Float(265.34)]
    );
    assert_eq!(
        row(&mut c, "RETURN round(2.675, 2) AS a, round(1.005, 2) AS b"),
        vec![Value::Float(2.68), Value::Float(1.01)]
    );
}

/// An explicit positive precision with no mode is HALF_UP (ties away from
/// zero); precision zero with no mode stays Java's `Math.round`
/// (`floor(x + 0.5)`, ties toward positive infinity). Live-verified sweep.
#[test]
fn round_default_mode_matches_live() {
    let mut c = connection();
    let got = rows(
        &mut c,
        "UNWIND [5.55, 2.55, 1.66, 1.11, -1.11, -1.66, -2.55, -5.55] AS x \
         RETURN x, round(x, 1) AS rounded ORDER BY x",
    );
    let want = [
        (-5.55, -5.6),
        (-2.55, -2.6),
        (-1.66, -1.7),
        (-1.11, -1.1),
        (1.11, 1.1),
        (1.66, 1.7),
        (2.55, 2.6),
        (5.55, 5.6),
    ];
    assert_eq!(got.len(), want.len());
    for (row, (x, rounded)) in got.iter().zip(want) {
        assert_eq!(*row, vec![Value::Float(x), Value::Float(rounded)]);
    }
    // Precision zero, no mode.
    assert_eq!(
        row(&mut c, "RETURN round(-2.5) AS a, round(-2.5, 0) AS b"),
        vec![Value::Float(-2.0), Value::Float(-2.0)]
    );
    // The wire HALF_UP mode is away from zero at precision zero too.
    assert_eq!(
        row(&mut c, "RETURN round(-2.5, 0, 'HALF_UP') AS a"),
        vec![Value::Float(-3.0)]
    );
}
