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
//! and [`label_set_groups_result`] gates on the view's exactness, then lets
//! the ordinary projection finish ordering and paging over the grouped rows.

use std::collections::HashMap;

use relate_core::{NodeId, Result, Value};
use relate_ir::{classify_aggregate, AggregateKind, Clause, Expr, OrderItem, Projection};
use relate_names::SmallName;

use crate::ctx::ExecCtx;
use crate::eval::eval;
use crate::group_key::GroupKey;
use crate::overlay::ExecGraph;
use crate::pattern_walk::named_call;
use crate::row::Row;
use crate::store::empty_properties;

use super::count_stats::{plain_match_pattern, whole_population_node_var};
use super::project::{project_rows, return_result, ProjectSpec, RowSource};
use super::{QueryResult, RowFeed};

/// The id every stand-in node carries. No key the recognizer admits reads a
/// node's id (a key depends on the node only through its label list), so
/// the value is never observed; a negative id cannot collide with a stored
/// node's.
const STAND_IN_NODE_ID: NodeId = -1;

/// What one RETURN item is in the recognized shape.
enum ItemRole {
    /// A group key: an expression over the node's label list alone.
    Key,
    /// The one `count(*)` / `count(n)`.
    Count,
}

/// A recognized statement: the node variable the keys read, each item's
/// role in item order, and the ORDER BY with every key rewritten onto an
/// output column (the grouped rows carry no node to evaluate against).
struct LabelGroupShape<'a> {
    var: &'a SmallName,
    items: &'a [Projection],
    roles: Vec<ItemRole>,
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

/// Rewrite one ORDER BY key onto an output column: a bare reference to an
/// output name stays, an expression equal to an output item becomes a
/// reference to that item's column. Anything else (an expression the
/// grouped rows cannot evaluate) is `None`.
fn order_on_columns(item: &OrderItem, items: &[Projection]) -> Option<OrderItem> {
    let column = match &item.expr {
        Expr::Variable(v) => items
            .iter()
            .enumerate()
            .find(|(i, p)| v.with_str(|text| p.output_name(*i) == text))
            .map(|(i, p)| p.output_name(i))?,
        expr => items
            .iter()
            .enumerate()
            .find(|(_, p)| p.expr == *expr)
            .map(|(i, p)| p.output_name(i))?,
    };
    Some(OrderItem {
        expr: Expr::Variable(SmallName::from_name(&column)),
        ascending: item.ascending,
    })
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
    let roles: Vec<ItemRole> = items
        .iter()
        .map(|p| {
            if is_node_count(&p.expr, var) {
                Some(ItemRole::Count)
            } else if is_label_key(&p.expr, var) {
                Some(ItemRole::Key)
            } else {
                None
            }
        })
        .collect::<Option<_>>()?;
    let counts = roles
        .iter()
        .filter(|r| matches!(r, ItemRole::Count))
        .count();
    if counts != 1 || roles.len() == 1 {
        return None;
    }
    if items
        .iter()
        .enumerate()
        .any(|(i, p)| var.with_str(|text| p.output_name(i) == text))
    {
        return None;
    }
    let order = order
        .iter()
        .map(|o| order_on_columns(o, items))
        .collect::<Option<Vec<_>>>()?;
    Some(LabelGroupShape {
        var,
        items,
        roles,
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
    sets: Vec<(std::sync::Arc<Vec<String>>, u64)>,
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
            (std::mem::size_of::<(std::sync::Arc<Vec<String>>, u64)>() as u64)
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
        for (p, role) in shape.items.iter().zip(&shape.roles) {
            if matches!(role, ItemRole::Key) {
                keys.push(eval(graph, &row, &p.expr, ctx.params)?);
            }
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
            for (name, role) in names.iter().zip(&shape.roles) {
                let value = match role {
                    ItemRole::Key => keys
                        .next()
                        .expect("every Key item evaluated one value per set"),
                    ItemRole::Count => Value::Int(
                        i64::try_from(count).expect("a group count past i64 is a broken store"),
                    ),
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
    seed_rows: &[Row],
    scope: Option<&Row>,
) -> Result<Option<(Vec<Row>, QueryResult)>> {
    let graph = ctx.graph;
    if !graph.sees_exact_base_stats() {
        return Ok(None);
    }
    if seed_rows.len() != 1 || !seed_rows[0].is_empty() {
        return Ok(None);
    }
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
