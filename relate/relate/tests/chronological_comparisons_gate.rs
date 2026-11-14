//! The `chronological-comparisons` preview gate, ENABLED side (ADR-0371):
//! with `RELATE_PREVIEW=chronological-comparisons` the four Cypher 25
//! chronological operators run, in the binary predicate and the simple-CASE
//! `WHEN IS …` spellings alike.
//!
//! `RELATE_PREVIEW` is process-global and read once per `Database::open`
//! (ADR-0371), so the enabled and closed sides are separate test binaries:
//! `chronological_comparisons_closed.rs` pins the authored
//! disabled-feature error and must never share a process with this file.

mod common;

use relate::Value;

use common::{connection, first_column_bools, set_preview_once};

#[test]
fn the_four_operators_evaluate_under_the_opt_in() {
    set_preview_once("chronological-comparisons");
    let mut c = connection();
    let result = c
        .query(
            "RETURN date('2020-01-01') IS BEFORE date('2020-01-02') AS before, \
                    date('2020-01-02') IS AFTER date('2020-01-01') AS after, \
                    date('2020-01-01') IS SAME INSTANT date('2020-01-01') AS same_instant, \
                    date('2020-01-01') IS SAME REPRESENTATION date('2020-01-01') AS same_rep",
        )
        .unwrap();
    assert_eq!(
        result.rows[0].as_slice(),
        [
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
        ]
    );
}

#[test]
fn negated_operators_are_the_logical_negation() {
    set_preview_once("chronological-comparisons");
    let mut c = connection();
    // The corpus's "Null operands and IS NOT stay null" policy: a NOT form
    // over a non-null comparison is its negation.
    let result = c
        .query(
            "RETURN date('2020-01-01') IS NOT BEFORE date('2020-01-01') AS not_before, \
                    date('2020-01-01') IS NOT SAME INSTANT date('2020-01-02') AS not_same, \
                    null IS NOT BEFORE date('2020-01-01') AS null_operand",
        )
        .unwrap();
    assert_eq!(
        result.rows[0].as_slice(),
        [Value::Bool(true), Value::Bool(true), Value::Null]
    );
}

#[test]
fn the_simple_case_when_is_spelling_uses_the_same_gate() {
    set_preview_once("chronological-comparisons");
    let mut c = connection();
    let result = c
        .query(
            "RETURN CASE date('2020-01-01') WHEN IS BEFORE date('2020-01-02') \
             THEN 'before' ELSE 'not' END AS result",
        )
        .unwrap();
    assert_eq!(result.rows[0][0], Value::String("before".into()));
}

#[test]
fn an_ordinary_comparison_needs_no_opt_in() {
    // The gate covers the chronological operator family only.
    set_preview_once("chronological-comparisons");
    let mut c = connection();
    let result = c.query("RETURN 1 < 2 AS r").unwrap();
    assert_eq!(first_column_bools(&result), vec![true]);
}
