//! The one-hop fold accumulator's (`StreamAgg`) `sum` semantics: null
//! skipping, the zero-row sum, and duration accumulation — split from
//! `pushdown_tests.rs`, which keeps the pushdown shapes.

use super::agg_fold::StreamAgg;
use super::test_support::{agg_fn, duration};
use super::*;
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use relate_ir::Expr;

// ---- StreamAgg (one-hop fold accumulator) ----

fn row_with(v: Value) -> Row {
    let mut row = Row::new();
    row.insert("x".into(), v);
    row
}

fn run_sum(vals: &[Value]) -> Value {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let expr = agg_fn("sum");
    let (mut agg, operand) = StreamAgg::new(&expr).unwrap();
    for v in vals {
        offer_value(&mut agg, &graph, operand, v.clone()).unwrap();
    }
    agg.finish()
}

/// One operand value through a one-hop fold accumulator, charged to a
/// throwaway retained-bytes ledger.
fn offer_value(
    agg: &mut StreamAgg,
    graph: &ExecGraph<'_>,
    operand: Option<&Expr>,
    value: Value,
) -> Result<()> {
    agg.offer(
        graph,
        &row_with(value),
        operand,
        &BTreeMap::new(),
        &mut graph.retain_bytes(0).unwrap(),
    )
}

// ADR-0205: the one-hop fold skips nulls like the grouped path, and
// sum() over zero non-null rows is 0 on both paths.
#[test]
fn folded_sum_skips_nulls_and_only_null_is_zero() {
    assert_eq!(
        run_sum(&[Value::Int(1), Value::Null, Value::Int(3)]),
        Value::Int(4)
    );
    assert_eq!(run_sum(&[Value::Null, Value::Null]), Value::Int(0));
    assert_eq!(run_sum(&[]), Value::Int(0));
}

#[test]
fn folded_sum_accepts_durations_and_rejects_mixing() {
    // The shared duration builder (F-1-13/14: three copies had diverged
    // in spelling across the aggregation test modules).
    let d = |nanos: i128| duration(0, 0, nanos);
    assert_eq!(
        run_sum(&[d(1_000_000_000), Value::Null, d(3_000_000_000)]),
        d(4_000_000_000)
    );
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let expr = agg_fn("sum");
    let (mut agg, operand) = StreamAgg::new(&expr).unwrap();
    offer_value(&mut agg, &graph, operand, d(1)).unwrap();
    let err = offer_value(&mut agg, &graph, operand, Value::Int(1)).unwrap_err();
    assert!(err.to_string().contains("cannot mix numbers and durations"));
}
