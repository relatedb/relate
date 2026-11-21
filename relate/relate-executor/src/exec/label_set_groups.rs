//! ADR-0589: the grouped label aggregates' answer from the maintained
//! label-set counts — `MATCH (n) RETURN labels(n)[0] AS kind, count(*)`
//! and its siblings answer in time proportional to the number of distinct
//! label sets instead of scanning every node. Declared beside
//! `label_set_groups_tests`, its contract tests.
//!
//! Three concerns, three functions: [`recognize_label_groups`] decides
//! whether the statement is the shape (every decline is `None`, the scan
//! answers), [`group_label_sets`] evaluates the key expressions once per
//! distinct label set through the ordinary evaluator and sums the counts,
//! and [`label_set_groups_result`] takes the base counts under the view's
//! exactness window (the seed gate is the caller's), then lets
//! the ordinary projection finish ordering and paging over the grouped rows.

use relate_core::{NodeId, Result, Value};
use relate_ir::{Clause, Expr, Projection};
use relate_names::SmallName;

use crate::ctx::ExecCtx;
use crate::eval::{eval, named_call};
use crate::graph::LabelSetCount;
use crate::row::Row;
use crate::store::empty_properties;

use super::order::order_on_columns;
use super::shortcut_common::{
    finish_grouped_rows, grouped_row, is_node_count, match_return_pattern,
    whole_population_node_var, CountedGroups, GroupedPage,
};
use super::QueryResult;

/// The id every stand-in node carries. No key the recognizer admits reads a
/// node's id (a key depends on the node only through its label list), so
/// the value is never observed; a negative id cannot collide with a stored
/// node's.
const STAND_IN_NODE_ID: NodeId = -1;

/// A recognized statement: the node variable the keys read, the position
/// of the one node count among the RETURN items (every other item is a
/// group key: an expression over the node's label list alone), and the
/// ORDER BY with every key rewritten onto an output column (the grouped
/// rows carry no node to evaluate against).
struct LabelGroupShape<'a> {
    var: &'a SmallName,
    items: &'a [Projection],
    count_at: usize,
    /// The key items, in item order — the count/key split the recognizer
    /// already made, carried so the grouping body never re-derives it.
    key_items: Vec<&'a Projection>,
    /// The paging half, carried once: the ORDER BY rewritten onto output
    /// columns and the statement's own SKIP and LIMIT.
    page: GroupedPage<'a>,
}

/// `expr` is `labels(var)`.
fn is_labels_of(expr: &Expr, var: &SmallName) -> bool {
    matches!(named_call(expr, "labels", 1), Some([Expr::Variable(v)]) if v == var)
}

/// Whether `expr` is a key over `var`'s label list alone — the admitted
/// grammar, closed on purpose: `labels(n)`, `size(labels(n))`, a literal
/// subscript of `labels(n)`, a label predicate on `n`. Every key is
/// evaluated once per label set, which is the per-node answer exactly
/// when the key reads nothing but the label list.
fn is_label_key(expr: &Expr, var: &SmallName) -> bool {
    if is_labels_of(expr, var) {
        return true;
    }
    if let Some([arg]) = named_call(expr, "size", 1) {
        return is_labels_of(arg, var);
    }
    match expr {
        Expr::Subscript { expr, index } => {
            is_labels_of(expr, var) && matches!(**index, Expr::Int(_))
        }
        Expr::LabelPredicate { expr, .. } => matches!(&**expr, Expr::Variable(v) if v == var),
        _ => false,
    }
}

/// Recognize the label-group shape. Deliberately conservative: exactly
/// `[Match, Return]`; the MATCH is the plain one-node whole-population
/// pattern with a bound variable ([`plain_match_pattern`],
/// [`whole_population_node_var`]); the RETURN has no DISTINCT or explicit
/// grouping, at least one key and exactly one node count among its items,
/// and an ORDER BY that names output columns only. No output column may be
/// named like the node variable (an alias-shadowed order key is the
/// scan's to resolve).
fn recognize_label_groups(clauses: &[Clause]) -> Option<LabelGroupShape<'_>> {
    let pattern = match_return_pattern(clauses)?;
    let Clause::Return {
        distinct: false,
        group_by: None,
        items,
        order,
        skip,
        limit,
    } = &clauses[1]
    else {
        return None;
    };
    let var = whole_population_node_var(pattern)?;
    let mut counts = items
        .iter()
        .enumerate()
        .filter(|(_, p)| is_node_count(&p.expr, var))
        .map(|(i, _)| i);
    let count_at = counts.next()?;
    if counts.next().is_some() || items.len() == 1 {
        return None;
    }
    let mut key_items: Vec<&Projection> = Vec::with_capacity(items.len() - 1);
    for (i, p) in items.iter().enumerate() {
        if i == count_at {
            continue;
        }
        if !is_label_key(&p.expr, var) {
            return None;
        }
        key_items.push(p);
    }
    // The node-variable gate is [`order_on_columns`]'s own precondition —
    // an alias-shadowed order key is the scan's to resolve.
    let page_order = order_on_columns(order, items, var)?;
    Some(LabelGroupShape {
        var,
        items,
        count_at,
        key_items,
        page: GroupedPage {
            order: page_order,
            skip: skip.as_ref(),
            limit: limit.as_ref(),
        },
    })
}

/// Evaluate the keys once per label set and sum the counts per distinct
/// key tuple: the grouped rows (one per group, first-seen in the sets'
/// deterministic order), each carrying every output column.
fn group_label_sets(
    ctx: ExecCtx<'_>,
    shape: &LabelGroupShape<'_>,
    sets: Vec<LabelSetCount>,
) -> Result<Vec<Row>> {
    let graph = ctx.graph;
    // The canonical output-name derivation — the same list the identity
    // projection builds. (A star item never reaches a recognized shape:
    // every item is the count or a label key.)
    let names = super::scope::projection_names(shape.items);
    // ADR-0463: the clone of the set map and the group table are retained
    // containers, charged to the expression budget as they land — a paged
    // (SKIP/LIMIT) statement materializes the whole table before it pages,
    // exactly as the scan's aggregation does.
    let mut retained = graph.retain_bytes(0)?;
    for row in &sets {
        retained.add_bytes(graph, label_set_retained_bytes(row))?;
    }
    let mut groups = CountedGroups::new();
    let var_name = shape.var.to_string();
    for (labels, count) in sets {
        let node = relate_core::Node {
            id: STAND_IN_NODE_ID,
            labels,
            properties: empty_properties(),
        };
        let mut row = Row::new();
        row.insert(var_name.clone(), Value::from_node(node));
        let mut keys = Vec::with_capacity(shape.key_items.len());
        for p in &shape.key_items {
            keys.push(eval(graph, &row, &p.expr, ctx.params)?);
        }
        groups.add(graph, &mut retained, keys, count)?;
    }
    // The one emission policy: the count in its `count_at` column, the
    // set's keys in the others — shortcut_common's row shape.
    groups.into_rows(graph, &mut retained, |keys, count| {
        grouped_row(&names, shape.count_at, keys, count)
    })
}

/// The retained bytes of one label-set row — the one statement of the
/// figure the exec aggregation charges for a label-set answer (ADR-0463):
/// the pair itself, its label-string buffer (a `Vec<String>` buffer —
/// `stride = size_of::<String>()`), and each string's own logical size
/// through the crate's one string-size spelling.
fn label_set_retained_bytes((labels, _): &LabelSetCount) -> u64 {
    (std::mem::size_of::<LabelSetCount>() as u64)
        .saturating_add(crate::workspace::buffer_bytes_saturating(
            labels.len(),
            std::mem::size_of::<String>() as u64,
        ))
        .saturating_add(
            labels
                .iter()
                .map(|l| relate_core::str_logical_bytes(l))
                .sum::<u64>(),
        )
}

/// ADR-0589: recognize the label-group shape and answer it from the
/// label-set counts. `Ok(None)` = decline to the clause loop's scan: the
/// shape is not recognized, the seed is not the statement's one unbound
/// row (a seeded stage's answer scales with its seeds), or the view is not
/// the latest committed state with nothing in flight — the same two-step
/// exactness window as the counting operator, the second step under the
/// count bundle's guard. `Ok(Some(..))` = answered. An evaluation error
/// of a key propagates, as it does from the scan's first node.
pub(super) fn label_set_groups_result(
    ctx: ExecCtx<'_>,
    clauses: &[Clause],
    scope: Option<&Row>,
) -> Result<Option<(Vec<Row>, QueryResult)>> {
    let graph = ctx.graph;
    let Some(shape) = recognize_label_groups(clauses) else {
        return Ok(None);
    };
    let Some(sets) = graph
        .base()
        .windowed_label_sets(|| graph.sees_exact_base_stats())
    else {
        return Ok(None);
    };
    let grouped = group_label_sets(ctx, &shape, sets)?;
    let (rows, result) =
        finish_grouped_rows(ctx, clauses, shape.items, shape.page, grouped, scope)?;
    Ok(Some((rows, result)))
}
