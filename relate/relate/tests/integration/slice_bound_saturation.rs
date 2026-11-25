//! ADR-0631's saturation pins, through the product engine: a list or
//! string slice whose dynamically-evaluated FLOAT bound exceeds the
//! int32 range answers with a saturated bound — the live Neo4j-dev
//! behavior the REL8-1864 parity report probed — instead of the
//! `NumberOutOfRange` ADR-0175's 2026.09 `asIntExact` image raised. An
//! oversized start selects nothing, an oversized end keeps the whole
//! sequence, and the negative spellings read as before-the-start. The
//! neighbors keep their rejections: a float *literal* bound fails at
//! plan time, and `substring()` / `left()` / `right()` and list indexing
//! stay in the `integer_argument_error_classes` suite.

use crate::common;

use common::connection;

#[test]
fn dynamic_slice_bounds_saturate_instead_of_erroring() {
    let mut conn = connection();
    let row = |x: relate::Value| vec![x];
    let empty_list = row(relate::Value::List(vec![].into()));
    let whole_list = row(relate::Value::List(
        vec![
            relate::Value::Int(1),
            relate::Value::Int(2),
            relate::Value::Int(3),
        ]
        .into(),
    ));
    let empty_string = row(relate::Value::String("".into()));
    let whole_string = row(relate::Value::String("hello".into()));
    for (statement, expected) in [
        ("RETURN [1,2,3][head([1.0/0.0, 'x'])..2] AS v", &empty_list),
        (
            "RETURN [1,2,3][head([2147483648.0, 'x'])..] AS v",
            &empty_list,
        ),
        ("RETURN [1,2,3][head([9.3e18, 'x'])..] AS v", &empty_list),
        ("RETURN [1,2,3][..head([-1.0/0.0, 'x'])] AS v", &empty_list),
        ("RETURN [1,2,3][head([-1.0/0.0, 'x'])..] AS v", &whole_list),
        ("RETURN [1,2,3][..head([9.3e18, 'x'])] AS v", &whole_list),
        // The string share of the one helper saturates identically.
        ("RETURN 'hello'[head([1.0/0.0, 'x'])..] AS v", &empty_string),
        (
            "RETURN 'hello'[head([-1.0/0.0, 'x'])..] AS v",
            &whole_string,
        ),
    ] {
        let result = conn.query(statement).unwrap();
        assert_eq!(common::first_column(&result), *expected, "{statement}");
    }
}

/// The neighbors keep their rejections (ADR-0631's unchanged half): the
/// float *literal* fails before execution, and `substring()` / `left()` /
/// `right()` keep the `NumberOutOfRange` argument-value class (their
/// rows live in `integer_argument_error_classes`).
#[test]
fn the_static_slice_gate_and_the_substring_sites_still_reject() {
    let mut conn = connection();
    let error = conn
        .query("RETURN [1,2,3][1.0..2] AS v")
        .expect_err("a float-literal slice bound fails at plan time");
    assert!(error.to_string().contains("InvalidArgumentType"), "{error}");
    let error = conn
        .query("RETURN substring('abcdef', head([1.0/0.0, 'x'])) AS v")
        .expect_err("substring keeps the int32-fit rejection");
    assert!(error.to_string().contains("NumberOutOfRange"), "{error}");
}
