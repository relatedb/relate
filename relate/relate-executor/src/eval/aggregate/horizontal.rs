//! The horizontal-aggregate family — aggregation-subset vocabulary, not
//! filter policy: the subset as one type (membership gates construction,
//! so an out-of-subset kind cannot even build), the row-bound-list unpivot
//! that rewrites an aggregate node into its folded value, and the fold
//! itself, which delegates to the shared [`fold_aggregate`].

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};

use relate_core::{Result, Value};
use relate_ir::{AggregateKind, Expr};

use super::{fold_aggregate, FoldInput};
use crate::eval::{eval, expr_vars, is_agg_expr};
use crate::overlay::ExecGraph;
use crate::row::Row;

/// The horizontal subset as a type: the aggregates the row-bound-list
/// rewrite folds. Membership (`of`) gates construction and the wrapped
/// [`AggregateKind`] is handed straight to the one shared
/// [`fold_aggregate`] — a kind outside the subset cannot even construct,
/// so the old loud-fail on an out-of-subset kind is structural now.
/// The membership match is exhaustive with no wildcard: a kind added later
/// must be classified here, at the decision point, instead of silently
/// dropping out of the subset through a `matches!` bool projection.
#[derive(Clone, Copy, Debug)]
struct HorizontalAgg(AggregateKind);

impl HorizontalAgg {
    fn of(kind: AggregateKind) -> Option<Self> {
        match kind {
            AggregateKind::Count
            | AggregateKind::Sum
            | AggregateKind::Avg
            | AggregateKind::Min
            | AggregateKind::Max
            | AggregateKind::Collect => Some(Self(kind)),
            AggregateKind::StDev
            | AggregateKind::StDevP
            | AggregateKind::PercentileCont
            | AggregateKind::PercentileDisc => None,
        }
    }
}

/// A horizontal aggregate node: which one, its name (the accumulator's
/// error label), and its one argument — recognized in one place, so the
/// rewrite's guard classifies exactly once.
struct HorizontalCall<'a> {
    kind: HorizontalAgg,
    /// The aggregate's call name, rendered once at the call's recognition
    /// (display/error text, not a hot read).
    name: String,
    arg: &'a Expr,
}

fn horizontal_call(expr: &Expr) -> Option<HorizontalCall<'_>> {
    let Expr::Function {
        name,
        distinct,
        args,
    } = expr
    else {
        return None;
    };
    if *distinct || args.len() != 1 {
        return None;
    }
    let kind = HorizontalAgg::of(name.with_str(relate_ir::classify_aggregate)?)?;
    Some(HorizontalCall {
        kind,
        name: name.to_string(),
        arg: &args[0],
    })
}

/// Rewrite every horizontal aggregate over a row-bound list into its
/// folded runtime value: unpivot the list variables (zip-longest — the
/// row's list variables are independent bindings and may differ in
/// length; the shorter pads with null and the fold skips nulls,
/// ADR-0205), evaluate the argument once per cell, fold. A horizontal
/// aggregate with no row-bound list variable stays the original node —
/// the vertical aggregation path's input. The tree a rewrite did not
/// transform is borrowed back, never cloned, at every depth: the
/// aggregate-free check ([`is_agg_expr`], the transform's own first
/// decision) makes the hot paths — the per-candidate residual conjuncts,
/// evaluated once per scanned node — pay nothing, where the pre-Cow shape
/// cloned and dropped one full `Expr` per scanned node (GBL SF0.1 q8:
/// the CONTAINS seed filter paid one full clone per forum), and the
/// Cow-preserving child rebuild keeps that promise for a compound tree
/// whose aggregates all declined. A transformed one owns its rebuilt
/// nodes.
pub(crate) fn rewrite_horizontal_aggregates<'e>(
    graph: &ExecGraph<'_>,
    row: &Row,
    expr: &'e Expr,
    params: &BTreeMap<String, Value>,
) -> Result<Cow<'e, Expr>> {
    if !is_agg_expr(expr) {
        return Ok(Cow::Borrowed(expr));
    }
    match horizontal_call(expr) {
        Some(call) => {
            let mut vars = HashSet::new();
            expr_vars(call.arg, &mut vars);
            let list_vars: Vec<_> = vars
                .into_iter()
                .filter_map(|name| match row.get(&name) {
                    Some(Value::List(values)) => Some((name, values)),
                    _ => None,
                })
                .collect();
            // no row-bound list variable: the vertical path keeps this node
            let Some(len) = list_vars.iter().map(|(_, values)| values.len()).max() else {
                return Ok(Cow::Borrowed(expr));
            };
            let mut values = Vec::new();
            for index in 0..len {
                let mut iteration = row.clone();
                for (name, items) in &list_vars {
                    iteration.insert(
                        name.clone(),
                        items.get(index).cloned().unwrap_or(Value::Null),
                    );
                }
                values.push(eval(graph, &iteration, call.arg, params)?);
            }
            let value = horizontal_aggregate(call.kind, &call.name, values)?;
            Ok(Cow::Owned(Expr::RuntimeValue(value)))
        }
        // every other node — and a horizontal aggregate the list check
        // declined — travels through the IR's canonical child rebuild,
        // Cow-preserving: the children are rewritten first, and only a
        // tree some child of which was rebuilt owns anything — a tree
        // whose aggregates all declined, at any depth, borrows whole,
        // exactly like the aggregate-free gate above. Scope-bearing
        // variants (pattern/subquery forms) report no children, so they
        // borrow untouched, as `Expr::children` skips them. Both walkers
        // (`try_each_child` here, `map_children`'s rebuild) visit the
        // same children in the same order, so the rewrite stream and the
        // rebuild positions cannot drift.
        None => {
            let mut rewrites: Vec<Cow<'e, Expr>> = Vec::new();
            expr.try_each_child::<relate_core::Error>(&mut |child| {
                rewrites.push(rewrite_horizontal_aggregates(graph, row, child, params)?);
                Ok(())
            })?;
            if rewrites.iter().all(|r| matches!(r, Cow::Borrowed(_))) {
                return Ok(Cow::Borrowed(expr));
            }
            let mut rewrites = rewrites.into_iter();
            expr.map_children(&mut |_| {
                Ok(rewrites
                    .next()
                    .expect("map_children rebuilds exactly the children each_child visited")
                    .into_owned())
            })
            .map(Cow::Owned)
        }
    }
}

/// The subset's one fold: delegate to the shared [`fold_aggregate`], so
/// the horizontal rewrite and the grouped evaluator cannot drift.
/// ADR-0205: nulls skip, sum() over zero non-null values is
/// 0, durations sum; min/max keep the first of equal extremes; sum() and
/// avg() share the strict offer (LABS-709) — a non-numeric, non-duration
/// operand is a type error on both. The horizontal path has no percentile.
fn horizontal_aggregate(kind: HorizontalAgg, name: &str, values: Vec<Value>) -> Result<Value> {
    fold_aggregate(FoldInput::Many(kind.0), name, values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::{agg_fn, duration};
    use crate::graph::Graph;

    fn list_row(values: Vec<Value>) -> Row {
        let mut row = Row::new();
        row.insert("x".into(), Value::List(values.into()));
        row
    }

    /// Run `expr` through the rewrite and return the value it folded the
    /// row-bound list to, failing when the rewrite declined to fold it.
    fn rewritten(graph: &ExecGraph<'_>, row: &Row, expr: &Expr) -> Value {
        match rewrite_horizontal_aggregates(graph, row, expr, &BTreeMap::new()).unwrap() {
            Cow::Owned(Expr::RuntimeValue(value)) => value,
            other => panic!("horizontal aggregate was not folded: {other:?}"),
        }
    }

    /// The borrow contract: an aggregate-free tree and a declined
    /// aggregate node (no row-bound list variable) both hand the original
    /// node back — the hot paths the per-candidate conjuncts ride — while
    /// only a folded tree owns a rebuilt node. A nested declined
    /// aggregate borrows whole too (F-2-29): the child rebuild is
    /// Cow-preserving, so a `max(…) > 1` candidate conjunct pays no clone
    /// per scanned node either.
    #[test]
    fn untransformed_trees_borrow_instead_of_cloning() {
        let graph = Graph::default();
        let graph = ExecGraph::read(&graph);
        let empty = Row::new();
        // Aggregate-free: the transform's own is_agg_expr gate.
        let plain = Expr::Binary {
            op: relate_ir::BinaryOp::Gt,
            left: Box::new(Expr::Variable("n".into())),
            right: Box::new(Expr::Int(1)),
        };
        assert!(matches!(
            rewrite_horizontal_aggregates(&graph, &empty, &plain, &BTreeMap::new()).unwrap(),
            Cow::Borrowed(_)
        ));
        // An aggregate node with no row-bound list variable: the vertical
        // path's input stays the original node.
        assert!(matches!(
            rewrite_horizontal_aggregates(&graph, &empty, &agg_fn("max"), &BTreeMap::new())
                .unwrap(),
            Cow::Borrowed(_)
        ));
        // A nested declined aggregate: the compound tree around it borrows
        // whole — the pre-Cow rebuild cloned every such node.
        let nested = Expr::Binary {
            op: relate_ir::BinaryOp::Gt,
            left: Box::new(agg_fn("max")),
            right: Box::new(Expr::Int(1)),
        };
        assert!(matches!(
            rewrite_horizontal_aggregates(&graph, &empty, &nested, &BTreeMap::new()).unwrap(),
            Cow::Borrowed(_)
        ));
        // A folded tree still owns its rebuilt node.
        let foldable = agg_fn("max");
        let folded = rewrite_horizontal_aggregates(
            &graph,
            &list_row(vec![Value::Int(1), Value::Int(3)]),
            &foldable,
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(matches!(folded, Cow::Owned(_)));
    }

    /// ADR-0205 through the rewrite: a row-bound list folds per kind, nulls
    /// skipped by every kind (sum over the empty sample is 0, avg null).
    #[test]
    fn horizontal_rewrite_folds_row_bound_list_kinds() {
        let graph = Graph::default();
        let graph = ExecGraph::read(&graph);
        let row = list_row(vec![Value::Int(1), Value::Null, Value::Int(3)]);
        assert_eq!(rewritten(&graph, &row, &agg_fn("sum")), Value::Int(4));
        assert_eq!(rewritten(&graph, &row, &agg_fn("avg")), Value::Float(2.0));
        assert_eq!(rewritten(&graph, &row, &agg_fn("min")), Value::Int(1));
        assert_eq!(rewritten(&graph, &row, &agg_fn("max")), Value::Int(3));
        assert_eq!(rewritten(&graph, &row, &agg_fn("count")), Value::Int(2));
        assert_eq!(
            rewritten(&graph, &row, &agg_fn("collect")),
            Value::List(vec![Value::Int(1), Value::Int(3)].into())
        );
        let empty = list_row(vec![Value::Null]);
        assert_eq!(rewritten(&graph, &empty, &agg_fn("sum")), Value::Int(0));
        assert_eq!(rewritten(&graph, &empty, &agg_fn("avg")), Value::Null);
    }

    #[test]
    fn horizontal_rewrite_sum_and_avg_accept_durations() {
        let graph = Graph::default();
        let graph = ExecGraph::read(&graph);
        let row = list_row(vec![
            Value::Null,
            duration(0, 0, 1_000_000_000),
            duration(0, 0, 3_000_000_000),
        ]);
        assert_eq!(
            rewritten(&graph, &row, &agg_fn("sum")),
            duration(0, 0, 4_000_000_000)
        );
        assert_eq!(
            rewritten(&graph, &row, &agg_fn("avg")),
            duration(0, 0, 2_000_000_000)
        );
        let mixed = list_row(vec![duration(0, 0, 1), Value::Int(1)]);
        assert!(
            rewrite_horizontal_aggregates(&graph, &mixed, &agg_fn("sum"), &BTreeMap::new())
                .is_err(),
            "mixing durations and numbers is a type error"
        );
    }

    /// LABS-709: sum() and avg() share the strict offer — a non-numeric,
    /// non-duration operand is a type error on both, never silently skipped.
    #[test]
    fn horizontal_rewrite_sum_and_avg_reject_non_numeric_values() {
        let graph = Graph::default();
        let graph = ExecGraph::read(&graph);
        let row = list_row(vec![Value::String("x".into())]);
        for name in ["sum", "avg"] {
            assert!(
                rewrite_horizontal_aggregates(&graph, &row, &agg_fn(name), &BTreeMap::new())
                    .is_err(),
                "a non-numeric {name} operand is a type error"
            );
        }
    }

    /// LABS-709: min/max keep the FIRST of equal extremes, matching Neo4j
    /// and the grouped path (the shared [`fold_aggregate`]), so
    /// `max([1, 1.0])` is the Int on both paths.
    #[test]
    fn horizontal_rewrite_min_max_keep_the_first_of_equal_extremes() {
        let graph = Graph::default();
        let graph = ExecGraph::read(&graph);
        for name in ["min", "max"] {
            assert_eq!(
                rewritten(
                    &graph,
                    &list_row(vec![Value::Int(1), Value::Float(1.0)]),
                    &agg_fn(name)
                ),
                Value::Int(1),
                "{name}"
            );
            assert_eq!(
                rewritten(
                    &graph,
                    &list_row(vec![Value::Float(1.0), Value::Int(1)]),
                    &agg_fn(name)
                ),
                Value::Float(1.0),
                "{name}"
            );
        }
    }

    /// The subset is stated once, by [`HorizontalAgg::of`]: kinds outside it
    /// never fold horizontally, the subset's kinds map to themselves.
    #[test]
    fn horizontal_subset_excludes_stdev_and_percentiles() {
        for kind in [
            AggregateKind::StDev,
            AggregateKind::StDevP,
            AggregateKind::PercentileCont,
            AggregateKind::PercentileDisc,
        ] {
            assert!(HorizontalAgg::of(kind).is_none(), "{kind:?}");
        }
        for kind in [
            AggregateKind::Count,
            AggregateKind::Sum,
            AggregateKind::Min,
            AggregateKind::Max,
            AggregateKind::Avg,
            AggregateKind::Collect,
        ] {
            assert!(HorizontalAgg::of(kind).is_some(), "{kind:?}");
        }
    }
}
