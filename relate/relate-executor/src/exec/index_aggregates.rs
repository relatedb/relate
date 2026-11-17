//! ADR-0611: grouped and extremum aggregates over one labeled property,
//! answered from the property's existing index postings instead of a label
//! scan — `MATCH (n:L) RETURN n.p, count(*)` and `MATCH (n:L) RETURN
//! min(n.p), max(n.p), count(n.p)`. Declared beside `index_aggregates_tests`.
//!
//! [`recognize`] decides the shape in two rungs — [`grouped_shape`], then
//! [`folds_shape`] (every decline is `None`, the scan answers); the
//! answerers read the graph's posting reads (`range_extremum_at`,
//! `for_each_posted_value_at`), which check every posted id against the
//! statement's snapshot and never build a pair, and fold into the shared
//! charged group table. The eligibility gate (view exactness, one unbound
//! seed row) is the ladder's — [`stats_short_circuit`](super::stats_short_circuit)
//! checks it once before any rung runs.

use relate_core::{Result, Value};
use relate_ir::{Clause, Expr, OrderItem, Projection};
use relate_names::SmallName;

use crate::ctx::ExecCtx;
use crate::eval::key_of;
use crate::graph::Extremum;
use crate::row::Row;

use super::order::order_on_columns;
use super::project::return_result;
use super::scope::projection_names;
use super::shortcut_common::{
    finish_grouped_rows, is_node_count, match_return_pattern, single_label_node, CountedGroups,
    GroupedPage,
};
use super::QueryResult;

/// Where the key sits among the two output cells of a grouped row.
#[derive(Clone, Copy)]
pub(super) enum CellOrder {
    KeyThenCount,
    CountThenKey,
}

/// A recognized statement over `(label, key)` — the node variable is
/// resolved away: every item reads it only through `n.key`.
pub(super) enum Shape<'a> {
    /// `n.key` beside a node count: one row per distinct value, ordered
    /// and paged by the statement's own [`GroupedPage`].
    Grouped {
        cells: CellOrder,
        page: GroupedPage<'a>,
    },
    /// Only `min`/`max`/`count` of `n.key`: one row.
    Folds { folds: Vec<Fold> },
}

/// The aggregates a fold statement may ask of one property.
#[derive(Clone, Copy)]
pub(super) enum Fold {
    Count,
    Min,
    Max,
}

pub(super) struct Recognized<'a> {
    pub(super) label: &'a SmallName,
    pub(super) key: &'a SmallName,
    pub(super) items: &'a [Projection],
    pub(super) shape: Shape<'a>,
}

/// `expr` is `kind(var.key)` for the admitted kinds (`min`, `max`,
/// `count`): the fold and the property key.
fn property_fold<'a>(expr: &'a Expr, var: &SmallName) -> Option<(Fold, &'a SmallName)> {
    let Expr::Function {
        name,
        distinct: false,
        args,
    } = expr
    else {
        return None;
    };
    let kind = name.with_str(relate_ir::classify_aggregate)?;
    let [arg] = &args[..] else {
        return None;
    };
    let key = key_of(arg, var)?;
    let fold = match kind {
        relate_ir::AggregateKind::Count => Fold::Count,
        relate_ir::AggregateKind::Min => Fold::Min,
        relate_ir::AggregateKind::Max => Fold::Max,
        _ => return None,
    };
    Some((fold, key))
}

/// The two cells of a grouped row — `n.p` beside a node count, in either
/// order: which cell is the key, the key itself, and the count item.
fn grouped_cells<'a>(
    items: &'a [Projection],
    var: &SmallName,
) -> Option<(CellOrder, &'a SmallName, &'a Projection)> {
    let [a, b] = items else {
        return None;
    };
    match (key_of(&a.expr, var), key_of(&b.expr, var)) {
        (Some(key), None) => Some((CellOrder::KeyThenCount, key, b)),
        (None, Some(key)) => Some((CellOrder::CountThenKey, key, a)),
        _ => None,
    }
}

/// The grouped rung: `n.p` beside one node count, ordering and paging
/// admitted. `None` for every other shape — the fold rung decides those.
fn grouped_shape<'a>(
    var: &SmallName,
    items: &'a [Projection],
    order: &'a [OrderItem],
    skip: Option<&'a Expr>,
    limit: Option<&'a Expr>,
) -> Option<(CellOrder, &'a SmallName, GroupedPage<'a>)> {
    let (cells, key, count) = grouped_cells(items, var)?;
    // The count must be the pattern's own node count — the scan's to
    // resolve otherwise. (The node-variable output-column gate is
    // `order_on_columns`'s own precondition, below.)
    if !is_node_count(&count.expr, var) {
        return None;
    }
    let order = order_on_columns(order, items, var)?;
    Some((cells, key, GroupedPage { order, skip, limit }))
}

/// The fold rung: only `min`/`max`/`count` of one property with no
/// ordering and no paging (both declined here, the rung's own boundary) —
/// the property and its folds. `None` for every other shape.
fn folds_shape<'a>(
    var: &SmallName,
    items: &'a [Projection],
    order: &[OrderItem],
    skip: Option<&Expr>,
    limit: Option<&Expr>,
) -> Option<(&'a SmallName, Vec<Fold>)> {
    if !order.is_empty() || skip.is_some() || limit.is_some() {
        return None;
    }
    let found: Vec<(Fold, &SmallName)> = items
        .iter()
        .map(|p| property_fold(&p.expr, var))
        .collect::<Option<_>>()?;
    let key = found.first()?.1;
    if found.iter().any(|(_, k)| *k != key) {
        return None;
    }
    Some((key, found.into_iter().map(|(fold, _)| fold).collect()))
}

/// Recognize the shape: exactly `[Match, Return]` (the `MATCH` half is
/// [`match_return_pattern`]); the MATCH is the plain one-node single-label
/// pattern ([`single_label_node`]) with a bound variable; the RETURN has
/// no DISTINCT or explicit grouping and is either `n.p` beside one node
/// count ([`grouped_shape`]), or only `min(n.p)`/`max(n.p)`/`count(n.p)`
/// over one property with no ordering or paging ([`folds_shape`]).
pub(super) fn recognize(clauses: &[Clause]) -> Option<Recognized<'_>> {
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
    let (var, label) = single_label_node(pattern)?;
    if let Some((cells, key, page)) =
        grouped_shape(var, items, order, skip.as_ref(), limit.as_ref())
    {
        return Some(Recognized {
            label,
            key,
            items,
            shape: Shape::Grouped { cells, page },
        });
    }
    let (key, folds) = folds_shape(var, items, order, skip.as_ref(), limit.as_ref())?;
    Some(Recognized {
        label,
        key,
        items,
        shape: Shape::Folds { folds },
    })
}

/// ADR-0611: recognize the shape and answer it from the property's
/// postings. `Ok(None)` = decline to the clause loop's scan: the shape is
/// not recognized or the store has no usable posting set (the view and
/// seed gate is the ladder's, checked before any rung runs —
/// [`stats_short_circuit`](super::stats_short_circuit)). A declined
/// statement costs the recognizer only; no pair is ever built.
pub(super) fn index_aggregates_result(
    ctx: ExecCtx<'_>,
    clauses: &[Clause],
    scope: Option<&Row>,
) -> Result<Option<(Vec<Row>, QueryResult)>> {
    let graph = ctx.graph;
    let Some(Recognized {
        label,
        key,
        items,
        shape,
    }) = recognize(clauses)
    else {
        return Ok(None);
    };
    let (label, key) = (label.to_string(), key.to_string());
    let ts = graph.snapshot_ts();
    match shape {
        Shape::Folds { folds } => {
            let Some(values) = fold_values(ctx, &label, &key, ts, &folds)? else {
                return Ok(None);
            };
            // The same exactness window the grouped arm closes: the
            // index reads ran at the statement's `ts` under the ladder's
            // one pre-check — a writer that committed since would let a
            // stale end answer min/max. Decline to the scan instead.
            if !graph.sees_exact_base_stats() {
                return Ok(None);
            }
            let columns = projection_names(items);
            let mut rows: Vec<Row> = vec![columns.into_iter().zip(values).collect()];
            let result = return_result(graph, items, clauses, &mut rows)?;
            Ok(Some((rows, result)))
        }
        Shape::Grouped { cells, page } => {
            let Some(grouped) = group_values(ctx, &label, &key, ts, items, cells)? else {
                return Ok(None);
            };
            let answer = finish_grouped_rows(ctx, clauses, items, page, grouped, scope)?;
            Ok(Some(answer))
        }
    }
}

/// One value per fold, `Ok(None)` when the graph cannot answer one, the
/// walk's error when one fires.
fn fold_values(
    ctx: ExecCtx<'_>,
    label: &str,
    key: &str,
    ts: u64,
    folds: &[Fold],
) -> Result<Option<Vec<Value>>> {
    let base = ctx.graph.base();
    let mut values = Vec::with_capacity(folds.len());
    for fold in folds {
        let value = match fold {
            Fold::Min => base.range_extremum_at(label, key, Extremum::Min, ts),
            Fold::Max => base.range_extremum_at(label, key, Extremum::Max, ts),
            Fold::Count => {
                let mut count = 0u64;
                // The walk's decline is the fold's decline — a swallowed
                // `Ok(None)` would answer a silent zero.
                base.for_each_posted_value_at(label, key, ts, |_| {
                    count += 1;
                    Ok(())
                })?
                .map(|()| {
                    Value::Int(
                        i64::try_from(count).expect("a group count past i64 is a broken store"),
                    )
                })
            }
        };
        let Some(value) = value else {
            return Ok(None);
        };
        values.push(value);
    }
    Ok(Some(values))
}

/// One row per distinct value of `key` (the first-seen representation, as
/// the scan's grouping keeps), carrying its node count, plus the null group
/// for the labeled nodes holding no posted value — folded into the shared
/// charged group table. `None` when the graph cannot answer.
fn group_values(
    ctx: ExecCtx<'_>,
    label: &str,
    key: &str,
    ts: u64,
    items: &[Projection],
    cells: CellOrder,
) -> Result<Option<Vec<Row>>> {
    let graph = ctx.graph;
    // ADR-0463: the group table is a retained container, charged as groups
    // land; its size is the distinct values, not the nodes.
    let mut retained = graph.retain_bytes(0)?;
    let mut groups = CountedGroups::new();
    let names = projection_names(items);
    let walk = graph
        .base()
        .for_each_posted_value_at(label, key, ts, |value| {
            groups.add_one(graph, &mut retained, value, 1)
        });
    match walk {
        Ok(None) => return Ok(None),
        Err(e) => return Err(e),
        Ok(Some(())) => {}
    }
    // The nodes holding no value: the label's exact live count less the
    // counted ones, read in the same exactness window. Postings are id
    // sets, so a node cannot be counted twice under one key: `counted`
    // is at most the label's live count, and a smaller total is a broken
    // store — the same invariant the sibling group checks state.
    let counted: u64 = groups.total_count();
    let Some(total) = graph.base().windowed_stats_count(
        || graph.sees_exact_base_stats(),
        crate::graph::CountProbe::Label(label.to_string()),
    ) else {
        return Ok(None);
    };
    let nulls = total
        .checked_sub(counted)
        .expect("a label's live count below its posted ids is a broken store");
    if nulls > 0 {
        groups.add_one(graph, &mut retained, &Value::Null, nulls)?;
    }
    // The same exactness window the sibling shortcuts close: a writer
    // that committed between the postings walk (at the statement's `ts`)
    // and the label total (read as of now) would tear the two halves —
    // the postings of the old state, the count of the new. The view was
    // exact when the ladder admitted the statement; if it no longer is,
    // decline to the scan rather than answer across the tear.
    if !graph.sees_exact_base_stats() {
        return Ok(None);
    }
    let rows = groups.into_rows(graph, &mut retained, |keys, count| {
        let count =
            Value::Int(i64::try_from(count).expect("a group count past i64 is a broken store"));
        let [value]: [Value; 1] = keys.try_into().expect("one key column per group");
        let values = match cells {
            CellOrder::KeyThenCount => [value, count],
            CellOrder::CountThenKey => [count, value],
        };
        names.iter().cloned().zip(values).collect()
    })?;
    Ok(Some(rows))
}
