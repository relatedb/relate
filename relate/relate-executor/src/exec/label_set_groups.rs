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

use std::collections::HashMap;

use relate_core::{NodeId, Result, Value};
use relate_ir::{classify_aggregate, AggregateKind, Clause, Expr, OrderItem, Projection};
use relate_names::SmallName;

use crate::ctx::ExecCtx;
use crate::eval::{eval, named_call};
use crate::graph::LabelSetCount;
use crate::group_key::GroupKey;
use crate::overlay::ExecGraph;
use crate::row::Row;
use crate::store::empty_properties;

use super::count_stats::{plain_match_pattern, whole_population_node_var};
use super::order::folded_order_items;
use super::project::{project_rows, return_result, ProjectSpec, RowSource};
use super::{QueryResult, RowFeed};

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
    order: Vec<OrderItem>,
    skip: &'a Option<Expr>,
    limit: &'a Option<Expr>,
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

/// `count(*)`, or `count(var)` over the pattern's own node (bound on every
/// row, so equal to `count(*)`).
fn is_node_count(expr: &Expr, var: &SmallName) -> bool {
    match expr {
        Expr::CountStar => true,
        Expr::Function {
            name,
            distinct: false,
            args,
        } if name.with_str(classify_aggregate) == Some(AggregateKind::Count) => {
            matches!(&args[..], [Expr::Variable(v)] if v == var)
        }
        _ => false,
    }
}

/// Rewrite the ORDER BY onto output columns: each key resolves to its
/// projection item through the canonical [`folded_order_items`] (an
/// expression equal to an output item, or a bare reference to an output
/// name), and becomes a reference to that item's column. A key the
/// grouped rows cannot evaluate is `None`.
fn order_on_columns(order: &[OrderItem], items: &[Projection]) -> Option<Vec<OrderItem>> {
    let resolved = folded_order_items(order, items)?;
    Some(
        order
            .iter()
            .zip(resolved)
            .map(|(key, item)| OrderItem {
                expr: Expr::Variable(SmallName::from_name(&items[item].output_name(item))),
                ascending: key.ascending,
            })
            .collect(),
    )
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
    let [first, Clause::Return {
        distinct: false,
        group_by: None,
        items,
        order,
        skip,
        limit,
    }] = clauses
    else {
        return None;
    };
    let var = whole_population_node_var(plain_match_pattern(first)?)?;
    let mut counts = items
        .iter()
        .enumerate()
        .filter(|(_, p)| is_node_count(&p.expr, var))
        .map(|(i, _)| i);
    let count_at = counts.next()?;
    if counts.next().is_some() || items.len() == 1 {
        return None;
    }
    let keys_only = items
        .iter()
        .enumerate()
        .all(|(i, p)| i == count_at || is_label_key(&p.expr, var));
    if !keys_only {
        return None;
    }
    if items
        .iter()
        .enumerate()
        .any(|(i, p)| var.with_str(|text| p.output_name(i) == text))
    {
        return None;
    }
    let order = order_on_columns(order, items)?;
    Some(LabelGroupShape {
        var,
        items,
        count_at,
        order,
        skip,
        limit,
    })
}

/// Evaluate the keys once per label set and sum the counts per distinct
/// key tuple: the grouped rows (one per group, first-seen in the sets'
/// deterministic order), each carrying every output column.
fn group_label_sets(
    graph: &ExecGraph<'_>,
    ctx: ExecCtx<'_>,
    shape: &LabelGroupShape<'_>,
    sets: Vec<LabelSetCount>,
) -> Result<Vec<Row>> {
    let names: Vec<String> = shape
        .items
        .iter()
        .enumerate()
        .map(|(i, p)| p.output_name(i))
        .collect();
    // ADR-0463: the clone of the set map and the group table are retained
    // containers, charged to the expression budget as they land — a paged
    // (SKIP/LIMIT) statement materializes the whole table before it pages,
    // exactly as the scan's aggregation does.
    let mut retained = graph.retain_bytes(0)?;
    for (labels, _) in &sets {
        retained.add_bytes(
            graph,
            (std::mem::size_of::<LabelSetCount>() as u64)
                .saturating_add(crate::workspace::value_buffer_bytes(labels.len()))
                .saturating_add(labels.iter().map(|l| l.len() as u64).sum::<u64>()),
        )?;
    }
    let mut index: HashMap<Vec<GroupKey>, usize> = HashMap::new();
    // Per group: its first-seen key values (one per Key item, item order)
    // and its summed node count.
    let mut groups: Vec<(Vec<Value>, u64)> = Vec::new();
    let var_name = shape.var.to_string();
    for (labels, count) in sets {
        let node = relate_core::Node {
            id: STAND_IN_NODE_ID,
            labels,
            properties: empty_properties(),
        };
        let mut row = Row::new();
        row.insert(var_name.clone(), Value::from_node(node));
        let mut keys = Vec::new();
        for (_, p) in shape
            .items
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != shape.count_at)
        {
            keys.push(eval(graph, &row, &p.expr, ctx.params)?);
        }
        let identity: Vec<GroupKey> = keys.iter().map(GroupKey::new).collect();
        match index.get(&identity) {
            Some(&g) => {
                groups[g].1 = groups[g]
                    .1
                    .checked_add(count)
                    .expect("count overflow: a group sum past u64 is a broken store");
            }
            None => {
                retained.add_bytes(
                    graph,
                    crate::workspace::group_entry_insert_delta(groups.len(), &identity),
                )?;
                for key in &keys {
                    retained.add_owned(graph, key)?;
                }
                index.insert(identity, groups.len());
                groups.push((keys, count));
            }
        }
    }
    groups
        .into_iter()
        .map(|(keys, count)| {
            let mut keys = keys.into_iter();
            let mut row = Row::new();
            for (i, name) in names.iter().enumerate() {
                let value = if i == shape.count_at {
                    Value::Int(
                        i64::try_from(count).expect("a group count past i64 is a broken store"),
                    )
                } else {
                    keys.next()
                        .expect("every key item evaluated one value per set")
                };
                row.insert(name.clone(), value);
            }
            retained.add_row(graph, row.values())?;
            Ok(row)
        })
        .collect()
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
    let grouped = group_label_sets(graph, ctx, &shape, sets)?;
    // The finish: ordering and paging over the grouped rows by the
    // ordinary projection — an identity projection of the output columns,
    // the ORDER BY already rewritten onto them, the statement's own
    // SKIP/LIMIT.
    let identity: Vec<Projection> = shape
        .items
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let name = p.output_name(i);
            Projection {
                expr: Expr::Variable(SmallName::from_name(&name)),
                alias: Some(SmallName::from_name(&name)),
                source_text: None,
            }
        })
        .collect();
    let mut rows = project_rows(
        ctx,
        RowFeed::Held(grouped),
        &ProjectSpec {
            items: &identity,
            distinct: false,
            group_by: None,
            order: &shape.order,
            skip: shape.skip,
            limit: shape.limit,
            where_clause: None,
        },
        true,
        RowSource::Bindings,
        scope,
    )?;
    let result = return_result(graph, shape.items, clauses, &mut rows)?;
    Ok(Some((rows, result)))
}
