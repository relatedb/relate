//! Aggregate fixtures: aggregate-call builders, row groups, and charge-cap assertions.
use std::collections::BTreeMap;

use relate_core::Value;
use relate_ir::{classify_aggregate, AggregateKind, Expr, SINGLE_INPUT_AGGREGATES};

use crate::exec::aggregate::{agg_eval, discover, AggScope, FoldedGroup, GroupAccs};
use crate::overlay::ExecGraph;
use crate::row::Row;

/// A bare node value (label `V`, no properties): the one fixture the
/// group-key, seed and footprint tests build node keys from.
pub(crate) fn node_value(id: i64) -> Value {
    Value::Node(std::sync::Arc::new(relate_core::Node {
        id,
        labels: std::sync::Arc::new(vec!["V".into()]),
        properties: std::sync::Arc::default(),
    }))
}

/// A one-argument aggregate call over a caller-chosen operand — the
/// parameterized form of the shape the aggregate tests, the charge tests,
/// the horizontal rewrite's tests, and the match-predicate test all build.
/// One definition, so the spelling cannot drift between them.
pub(crate) fn agg_fn_over(name: &str, arg: Expr) -> Expr {
    agg_call(name, arg, false)
}

/// The one plain aggregate-call construction the plain and DISTINCT
/// fixtures share.
fn agg_call(name: &str, arg: Expr, distinct: bool) -> Expr {
    Expr::Function {
        name: name.into(),
        distinct,
        args: vec![arg],
    }
}

/// The shared operand column every aggregate fixture reads.
pub(crate) const OPERAND_COLUMN: &str = "x";

/// Stream every row of `group` through the accumulators of the calls found
/// in `exprs` (the production discover / open / offer / settle protocol),
/// then resolve the group: its folded calls and the retained total the
/// accumulators ended on.
pub(crate) fn fold_group_rows<'e>(
    graph: &ExecGraph<'_>,
    group: &[Row],
    exprs: impl IntoIterator<Item = &'e Expr>,
    params: &BTreeMap<String, Value>,
) -> relate_core::Result<(FoldedGroup<'e>, u64)> {
    let table = discover(graph, exprs, [], params)?;
    let mut accs = GroupAccs::open(graph, table)?;
    for row in group {
        accs.offer(graph, row, params)?;
        accs.settle(graph)?;
    }
    let billed = accs.billed();
    let folded = accs.finish(graph, group.first(), params);
    Ok((folded, billed))
}

/// Evaluate `expr` over `group` through the production path: discover the
/// aggregate calls, stream every row through the group's accumulators, and
/// read the resolved values back — the one entry the aggregate tests drive.
pub(crate) fn agg_eval_rows(
    graph: &ExecGraph<'_>,
    group: &[Row],
    expr: &Expr,
    params: &BTreeMap<String, Value>,
) -> relate_core::Result<Value> {
    let (folded, _) = fold_group_rows(graph, group, [expr], params)?;
    agg_eval(
        graph,
        AggScope::of_group(&folded, group.first()),
        expr,
        params,
    )
}

/// [`agg_fn_over`] over the shared `x` column.
pub(crate) fn agg_fn(name: &str) -> Expr {
    agg_fn_over(name, Expr::Variable(OPERAND_COLUMN.into()))
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
            row.insert(OPERAND_COLUMN.into(), v.clone());
            row
        })
        .collect()
}

/// [`percentile_fn_over`] over the shared `x` column, with the literal
/// percentile `p`.
pub(crate) fn percentile_fn(name: &str, p: f64) -> Expr {
    percentile_fn_over(name, Expr::Variable(OPERAND_COLUMN.into()), p)
}

/// [`percentile_fn`] with `DISTINCT`: the call keeps its own seen-set and
/// operand values (the retained path) instead of sharing the pool.
pub(crate) fn distinct_percentile(name: &str, p: f64) -> Expr {
    percentile_call(name, Expr::Variable(OPERAND_COLUMN.into()), p, true)
}

/// The canonical single-input aggregate vocabulary with its kinds: `count`
/// (whose keyword path the table omits by contract) plus
/// [`SINGLE_INPUT_AGGREGATES`], classified once. The charge pins iterate
/// this, so a kind added to the vocabulary is covered by construction.
pub(crate) fn single_input_calls() -> impl Iterator<Item = (&'static str, AggregateKind)> {
    ["count"]
        .into_iter()
        .chain(SINGLE_INPUT_AGGREGATES.iter().copied())
        .map(|name| {
            let kind = classify_aggregate(name).expect("the table names aggregate kinds");
            (name, kind)
        })
}

/// [`agg_fn`] with `DISTINCT`: the call folds each first-seen operand
/// through its seen-set.
pub(crate) fn distinct_fn(name: &str) -> Expr {
    agg_call(name, Expr::Variable(OPERAND_COLUMN.into()), true)
}

/// The two-argument percentile call over a caller-chosen operand, with the
/// literal percentile `p` — the parameterized form, so the parked-failure
/// and pool fixtures build it over a `p`-property row with one spelling.
pub(crate) fn percentile_fn_over(name: &str, arg: Expr, p: f64) -> Expr {
    percentile_call(name, arg, p, false)
}

/// The one percentile-call construction the plain and DISTINCT fixtures
/// share.
fn percentile_call(name: &str, arg: Expr, p: f64, distinct: bool) -> Expr {
    Expr::Function {
        name: name.into(),
        distinct,
        args: vec![arg, Expr::Float(p)],
    }
}

/// Assert the charge partition once over a 100,000-row group: a
/// stream-folding single-input kind under `build` stays under `cap`, a
/// retaining kind refuses at it. Both charge pins call this with their own
/// builder and cap, so the partition rule has one home.
pub(crate) fn assert_charge_partition(label: &str, cap: u64, build: fn(&str) -> Expr) {
    let mut g = crate::graph::Graph::default();
    g.configure_expression_memory_limit(Some(cap));
    let graph = ExecGraph::read(&g);
    let group = group_with_xs(&(0..100_000).collect::<Vec<_>>());
    for (name, kind) in single_input_calls() {
        let call = format!("{label} {name}");
        let result = agg_eval_rows(&graph, &group, &build(name), &BTreeMap::new());
        if crate::eval::StreamFold::for_kind(kind).is_some() {
            result.unwrap_or_else(|error| panic!("{call} streams, yet refused: {error}"));
        } else {
            let error = result
                .err()
                .unwrap_or_else(|| panic!("{call} retains, yet answered a value"));
            assert_expression_cap(&error, cap);
        }
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
