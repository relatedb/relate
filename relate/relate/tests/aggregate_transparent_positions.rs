//! LABS-744: aggregates under aggregation-transparent positions.
//!
//! The front admits aggregate calls beneath unary operators, property reads,
//! `IS :: <type>`, `IS NORMALIZED` and temporal comparisons; the aggregate
//! evaluator folds those operands too instead of handing the aggregate call
//! to scalar evaluation (which rejected it).

mod common;

use relate::Value;

use common::{connection, scalar_one};

fn scalar(query: &str) -> Value {
    scalar_one(&mut connection(), query)
}

#[test]
fn unary_minus_over_count_star() {
    assert_eq!(
        scalar("UNWIND [1, 2, 3] AS n RETURN -count(*) AS a"),
        Value::Int(-3)
    );
}

#[test]
fn unary_minus_over_sum() {
    assert_eq!(
        scalar("UNWIND [1, 2, 3] AS n RETURN -sum(n) AS a"),
        Value::Int(-6)
    );
}

#[test]
fn not_over_aggregate_comparison() {
    assert_eq!(
        scalar("UNWIND [1, 2, 3] AS n RETURN NOT (count(*) > 2) AS a"),
        Value::Bool(false)
    );
}

#[test]
fn is_type_over_aggregate() {
    assert_eq!(
        scalar("UNWIND [1, 2] AS n RETURN sum(n) IS :: INTEGER AS a"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar("UNWIND [1, 2] AS n RETURN sum(n) IS NOT :: STRING AS a"),
        Value::Bool(true)
    );
}

#[test]
fn empty_group_sum_through_unary() {
    assert_eq!(scalar("UNWIND [] AS n RETURN -sum(n) AS a"), Value::Int(0));
}

#[test]
fn is_normalized_over_aggregate() {
    assert_eq!(
        scalar("UNWIND [1, 2] AS n RETURN max(toString(n)) IS NORMALIZED AS a"),
        Value::Bool(true)
    );
}

#[test]
fn temporal_compare_over_aggregate() {
    assert_eq!(
        scalar("UNWIND [2020, 2021] AS y RETURN max(date({year: y})) < date('2022-01-01') AS a"),
        Value::Bool(true)
    );
}
