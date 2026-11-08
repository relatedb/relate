//! Aggregate fixtures: aggregate-call builders, row groups, and charge-cap assertions.
use std::collections::BTreeMap;

use relate_core::Value;
use relate_ir::Expr;

use crate::overlay::ExecGraph;
use crate::row::Row;

/// A one-argument aggregate call over a caller-chosen operand — the
/// parameterized form of the shape the aggregate tests, the charge tests,
/// the horizontal rewrite's tests, and the match-predicate test all build.
/// One definition, so the spelling cannot drift between them.
pub(crate) fn agg_fn_over(name: &str, arg: Expr) -> Expr {
    Expr::Function {
        name: name.into(),
        distinct: false,
        args: vec![arg],
    }
}

/// Evaluate `expr` over `group` through the production path: discover the
/// aggregate calls, stream every row through the group's accumulators, and
/// read the resolved values back — the one entry the aggregate tests drive.
pub(crate) fn agg_eval_rows(
    graph: &ExecGraph<'_>,
    group: &[Row],
    expr: &Expr,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> relate_core::Result<Value> {
    use crate::exec::aggregate::{agg_eval, AggScope, AggSites};
    let sites = AggSites::discover(graph, [expr], [], params, cypher_version)?;
    let mut accs = sites.open_group();
    let mut total = graph.retain_bytes(0)?;
    for row in group {
        accs.offer(graph, row, params, cypher_version)?;
        accs.settle(graph, &mut total)?;
    }
    let folded = accs.finish(graph, group.first(), params, cypher_version);
    agg_eval(
        graph,
        AggScope::of_group(&folded, group.first()),
        expr,
        params,
        cypher_version,
    )
}

/// [`agg_fn_over`] over the shared `x` column.
pub(crate) fn agg_fn(name: &str) -> Expr {
    agg_fn_over(name, Expr::Variable("x".into()))
}

/// A group of one row per `x` binding (the aggregate tests' row fixture,
/// shared with the charge-test family's module). One definition, so the
/// two test modules cannot drift between spellings: the i64 convenience
/// delegates to [`group_with_vals`], the one row-building loop.
pub(crate) fn group_with_xs(xs: &[i64]) -> Vec<Row> {
    group_with_vals(&xs.iter().map(|&i| Value::Int(i)).collect::<Vec<_>>())
}

/// [`group_with_xs`] with caller-chosen values — the one row-building loop
/// both spellings read.
pub(crate) fn group_with_vals(vals: &[Value]) -> Vec<Row> {
    vals.iter()
        .map(|v| {
            let mut row = Row::new();
            row.insert("x".into(), v.clone());
            row
        })
        .collect()
}

/// The two-argument percentile call over the shared `x` column, with the
/// literal percentile `p`.
pub(crate) fn percentile_fn(name: &str, p: f64) -> Expr {
    Expr::Function {
        name: name.into(),
        distinct: false,
        args: vec![Expr::Variable("x".into()), Expr::Float(p)],
    }
}

/// A duration value for the aggregation test modules (one definition,
/// three call sites: aggregate_tests, pushdown_tests, match_clause).
pub(crate) fn duration(months: i64, days: i64, nanos: i128) -> Value {
    Value::Temporal(relate_core::temporal::Temporal::Duration {
        months,
        days,
        nanos,
    })
}

/// The one capacity-refusal assertion for the charge-pin family (the
/// configured cap is spelled once per test, then checked here — never a
/// second pattern literal beside the configure call). Returns the
/// refusal's reported `used` figure for the pins that bound its
/// overshoot. Shared by the aggregate-charge and projection test homes.
pub(crate) fn assert_expression_cap(error: &relate_core::Error, cap: u64) -> u64 {
    let relate_core::Error::Capacity {
        used,
        limit,
        kind: relate_core::CapacityKind::Expression,
        ..
    } = error
    else {
        panic!("the typed expression-capacity refusal at cap {cap}: {error}");
    };
    assert_eq!(*limit, cap, "{error}");
    *used
}

/// The one crossing-element bound the LABS-781 pins assert: the
/// refusal's reported figure is the element that crossed the cap —
/// overshoot bounded by that one element's own byte figure — never the
/// whole container the fold-time pre-charge used to report.
pub(crate) fn assert_crossing_element(used: u64, cap: u64, one_element: u64) {
    assert!(
        used > cap && used <= cap + one_element,
        "the refusal reports the crossing element ({used} over cap {cap}), \
not the whole container"
    );
}

/// The one group-landing bound the fold-served and keyed pins assert:
/// the workspace entry delta at `boundary` — the largest slot-growth
/// step the pin's group count can hit — carrying the pin's largest key
/// (`largest_key`), plus the one-element key-half vals buffer the
/// table retains beside the key.
pub(crate) fn group_landing_bound(boundary: usize, largest_key: &Value) -> u64 {
    crate::workspace::group_entry_insert_delta(
        boundary,
        &[crate::group_key::GroupKey::new(largest_key)],
    ) + crate::workspace::value_buffer_bytes(1)
}

/// [`assert_crossing_element`] for a landed row: one generous
/// `bindings`-wide row figure.
pub(crate) fn assert_crossing_row(used: u64, cap: u64, bindings: usize) {
    assert_crossing_element(
        used,
        cap,
        crate::workspace::retained_rows_bytes(1, bindings),
    );
}
