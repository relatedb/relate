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
use crate::graph::{Extremum, PropFold};
use crate::overlay::RetainedTotal;
use crate::row::Row;

use super::order::order_on_columns;
use super::project::return_result;
use super::scope::projection_names;
use super::shortcut_common::{
    count_value, finish_grouped_rows, grouped_row, is_node_count, match_return_pattern,
    single_label_node, CountedGroups, GroupedPage,
};
use super::QueryResult;

/// A recognized statement over `(label, key)` — the node variable is
/// resolved away: every item reads it only through `n.key`.
pub(super) enum Shape<'a> {
    /// `n.key` beside a node count: one row per distinct value, ordered
    /// and paged by the statement's own [`GroupedPage`]. `count_at` is
    /// the count item's position among the items — the one place the
    /// cell placement is decided (label_set_groups' idiom).
    Grouped {
        count_at: usize,
        page: GroupedPage<'a>,
    },
    /// Only `min`/`max`/`count` of `n.key`: one row.
    Folds { folds: Vec<FoldItem> },
}

/// One fold ask and the output column it lands in — the pairing the
/// fold arm reads, never a positionally aligned sibling vec beside the
/// items. The ask is the seam's own [`PropFold`], so the store's
/// vocabulary and this ladder's are one type.
pub(super) struct FoldItem {
    fold: PropFold,
    name: String,
}

pub(super) struct Recognized<'a> {
    pub(super) label: &'a SmallName,
    pub(super) key: &'a SmallName,
    pub(super) items: &'a [Projection],
    pub(super) shape: Shape<'a>,
}

/// `expr` is `kind(var.key)` for the admitted kinds (`min`, `max`,
/// `count`): the fold and the property key.
fn property_fold<'a>(expr: &'a Expr, var: &SmallName) -> Option<(PropFold, &'a SmallName)> {
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
        relate_ir::AggregateKind::Count => PropFold::Count,
        relate_ir::AggregateKind::Min => PropFold::Min,
        relate_ir::AggregateKind::Max => PropFold::Max,
        _ => return None,
    };
    Some((fold, key))
}

/// The two cells of a grouped row — `n.p` beside a node count, in either
/// order: the count item's position, the key itself, and the count item.
fn grouped_cells<'a>(
    items: &'a [Projection],
    var: &SmallName,
) -> Option<(usize, &'a SmallName, &'a Projection)> {
    let [a, b] = items else {
        return None;
    };
    match (key_of(&a.expr, var), key_of(&b.expr, var)) {
        (Some(key), None) => Some((1, key, b)),
        (None, Some(key)) => Some((0, key, a)),
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
) -> Option<(usize, &'a SmallName, GroupedPage<'a>)> {
    let (count_at, key, count) = grouped_cells(items, var)?;
    // The count must be the pattern's own node count — the scan's to
    // resolve otherwise. (The node-variable output-column gate is
    // `order_on_columns`'s own precondition, below.)
    if !is_node_count(&count.expr, var) {
        return None;
    }
    let order = order_on_columns(order, items, var)?;
    Some((count_at, key, GroupedPage { order, skip, limit }))
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
) -> Option<(&'a SmallName, Vec<FoldItem>)> {
    if !order.is_empty() || skip.is_some() || limit.is_some() {
        return None;
    }
    // Seeded from the first item: every later fold must read the same
    // property — a plain comparison, no accumulator.
    let (first, _) = items.split_first()?;
    let (_, key) = property_fold(&first.expr, var)?;
    let mut folds = Vec::with_capacity(items.len());
    for (i, p) in items.iter().enumerate() {
        let (fold, item_key) = property_fold(&p.expr, var)?;
        if item_key != key {
            return None;
        }
        folds.push(FoldItem {
            fold,
            name: p.output_name(i),
        });
    }
    Some((key, folds))
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
    if let Some((count_at, key, page)) =
        grouped_shape(var, items, order, skip.as_ref(), limit.as_ref())
    {
        return Some(Recognized {
            label,
            key,
            items,
            shape: Shape::Grouped { count_at, page },
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
    // The shape's reads, before any emission: the fold row, or the
    // grouped rows with their page.
    let read = match shape {
        Shape::Folds { folds } => {
            let Some(row) = fold_values(ctx, &label, &key, ts, folds)? else {
                return Ok(None);
            };
            Read::Folds(row)
        }
        Shape::Grouped { count_at, page } => {
            let Some(rows) = group_values(ctx, &label, &key, ts, items, count_at)? else {
                return Ok(None);
            };
            Read::Grouped { rows, page }
        }
    };
    // The one exactness-window close, after every read and before any
    // emission: the index reads ran at the statement's `ts` under the
    // ladder's one pre-check — a writer that committed since would
    // answer min/max from a stale end or tear the grouped halves.
    // Decline to the scan instead.
    if !graph.sees_exact_base_stats() {
        return Ok(None);
    }
    match read {
        Read::Folds(row) => {
            let mut rows: Vec<Row> = vec![row];
            let result = return_result(graph, items, clauses, &mut rows)?;
            Ok(Some((rows, result)))
        }
        Read::Grouped { rows, page } => {
            let answer = finish_grouped_rows(ctx, clauses, items, page, rows, scope)?;
            Ok(Some(answer))
        }
    }
}

/// What a recognized shape read, before the exactness window closes and
/// the emission runs.
enum Read<'a> {
    /// One row: each fold's value in its own named column — the
    /// `FoldItem` pairing, never a second aligned vec.
    Folds(Row),
    /// The grouped rows and the statement's own paging.
    Grouped {
        rows: Vec<Row>,
        page: GroupedPage<'a>,
    },
}

/// The fold statement's one row — each fold's value inserted under its
/// own `FoldItem` column as it resolves, the pairing consumed and never
/// re-joined — `Ok(None)` when the graph cannot answer one fold, the
/// walk's error when one fires.
fn fold_values(
    ctx: ExecCtx<'_>,
    label: &str,
    key: &str,
    ts: u64,
    folds: Vec<FoldItem>,
) -> Result<Option<Row>> {
    let base = ctx.graph.base();
    let mut row = Row::new();
    // ADR-0665 A/B (measured ordering): behind the declared postings, the
    // attached store's columnar fold — pre-built at attach, one pass over
    // the column — serves the shape no declared pair covers; its decline
    // falls to the postings path, which declines for the same missing
    // pair, and the scan answers.
    let labels = [label.to_string()];
    if !base.range_serves_key(&labels, key) {
        let wants: Vec<PropFold> = folds.iter().map(|item| item.fold).collect();
        // The seam's alignment contract: the answer holds one entry per
        // ask. A store answering a different length is broken — decline
        // this arm to the postings path below, never a partial row.
        // The seam's acceptance protocol validated the answer (aligned,
        // one entry per ask) — a decline is the whole arm's.
        if let Some(values) = base.prop_store_folds(&labels, key, &wants, ts) {
            for (item, value) in folds.iter().zip(values) {
                row.insert(item.name.clone(), value);
            }
            return Ok(Some(row));
        }
    }
    for item in folds {
        let value = match item.fold {
            PropFold::Min => base.range_extremum_at(label, key, Extremum::Min, ts),
            PropFold::Max => base.range_extremum_at(label, key, Extremum::Max, ts),
            PropFold::Count => {
                let mut count = 0u64;
                // The walk's decline is the fold's decline — a swallowed
                // `Ok(None)` would answer a silent zero.
                base.for_each_posted_value_at(label, key, ts, |_| {
                    count += 1;
                    Ok(())
                })?
                .map(|()| count_value(count))
            }
        };
        let Some(value) = value else {
            return Ok(None);
        };
        row.insert(item.name, value);
    }
    Ok(Some(row))
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
    count_at: usize,
) -> Result<Option<Vec<Row>>> {
    let graph = ctx.graph;
    // ADR-0463: the group table is a retained container, charged as groups
    // land; its size is the distinct values, not the nodes.
    let mut retained = graph.retain_bytes(0)?;
    let mut groups = CountedGroups::new();
    let names = projection_names(items);
    // ADR-0665 A/B (measured ordering): behind the declared postings, the
    // attached store's columnar distinct counts — one pass over the
    // column, the counts landing through the same charged table — serve
    // the shape no declared pair covers; a decline falls to the postings
    // walk, which declines for the same missing pair.
    let base = graph.base();
    let labels = [label.to_string()];
    let mut served = false;
    if !base.range_serves_key(&labels, key) {
        if let Some(pairs) = base.prop_store_grouped_counts(&labels, key, ts) {
            for (value, count) in pairs {
                groups.add_one(graph, &mut retained, &value, count)?;
            }
            served = true;
        }
    }
    if !served {
        let walk = base.for_each_posted_value_at(label, key, ts, |value| {
            groups.add_one(graph, &mut retained, value, 1)
        });
        // The walk's decline is the shape's decline; its error propagates.
        let Some(()) = walk? else {
            return Ok(None);
        };
    }
    // The nodes holding no value: the label's exact live count less the
    // counted ones, read in the same exactness window.
    // The walk's one decline protocol: a null group that cannot be read
    // in the exactness window declines the whole answer — never an
    // emission missing it.
    let Some(()) = null_group(ctx, label, &mut groups, &mut retained)? else {
        return Ok(None);
    };
    let rows = groups.into_rows(graph, &mut retained, |keys, count| {
        // The one emission policy: the count in its `count_at` column,
        // the group's key in the other — shortcut_common's row shape.
        grouped_row(&names, count_at, keys, count)
    })?;
    Ok(Some(rows))
}

/// The grouped answer's synthesized null group: the label's exact live
/// count less the counted ones, landing as the null key's count. `None`
/// (the caller's decline) when the label total cannot be read in the
/// same exactness window.
fn null_group(
    ctx: ExecCtx<'_>,
    label: &str,
    groups: &mut CountedGroups,
    retained: &mut RetainedTotal,
) -> Result<Option<()>> {
    let graph = ctx.graph;
    // Postings are id sets, so a node cannot be counted twice under one
    // key: `counted` is at most the label's live count. The postings walk
    // guarantees it by construction; the store-supplied arm (an
    // out-of-crate answer) cannot — a total below the counted ids is a
    // broken store, and the decline is the answer, never a panic.
    let counted: u64 = groups.total_count();
    // The windowed total's decline is the ladder's own (a writer
    // committed since the reads ran); the subtraction's decline is the
    // broken-store guard above.
    let Some(total) = graph.base().windowed_stats_count(
        || graph.sees_exact_base_stats(),
        crate::graph::CountProbe::Label(label.to_string()),
    ) else {
        return Ok(None);
    };
    let Some(nulls) = total.checked_sub(counted) else {
        return Ok(None);
    };
    if nulls > 0 {
        groups.add_one(graph, retained, &Value::Null, nulls)?;
    }
    Ok(Some(()))
}
