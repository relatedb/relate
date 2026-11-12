//! Aggregate folding (ADR-0036): the shared scaffolding both folds read
//! — the RETURN gate and [`FoldOutput`], [`Rank`] and the one projection
//! over it, `finish_folded_rows`, the derived group key. The folds
//! themselves live in their own modules: the one-hop fold in [`one_hop`],
//! the fixed-length path fold in [`path`] (its compile half in
//! [`plan`]), the streaming accumulators in [`acc`].

mod acc;

#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};

use relate_core::{Result, Value};
use relate_ir::{Clause, Expr, OrderItem, PathPattern, Projection};

use crate::eval::{expr_vars, is_agg_expr};
use crate::group_key::GroupKey;
use crate::overlay::{ExecGraph, MaterializeCache};
use crate::row::Row;
pub(in crate::exec) use acc::StreamAgg;

/// Shared fold scaffolding: the derived group key beside the key values
/// it was built from, so a repeat of the same key values reuses both.
/// The path fold's per-node key cache (path.rs `FoldCaches.keys`) stores
/// it; the one-hop fold builds one inline through [`CachedKey::from_vals`].
#[derive(Clone)]
struct CachedKey {
    key: Vec<GroupKey>,
    vals: Vec<Value>,
}

impl CachedKey {
    fn from_vals(vals: Vec<Value>) -> Self {
        let key = vals.iter().map(GroupKey::new).collect();
        Self { key, vals }
    }
}

use super::order::{
    folded_order_items, page, park_values, resolve_page_bounds, sort_rows_by_parked_keys,
    unpark_order_keys, OrderKeys,
};
use super::scope::{has_star_item, projection_collides_with_internal_names, result_column};
use super::QueryResult;

pub(crate) fn fold_hop_vars(path: &PathPattern) -> HashSet<String> {
    // Text render: the hop-var set is the String slot domain.
    let mut v = HashSet::new();
    for n in &path.nodes {
        if let Some(name) = &n.variable {
            v.insert(name.to_string());
        }
    }
    for r in &path.rels {
        if let Some(name) = &r.variable {
            v.insert(name.to_string());
        }
    }
    v
}

pub(crate) fn expr_within(expr: &Expr, allowed: &HashSet<String>) -> bool {
    let mut used = HashSet::new();
    expr_vars(expr, &mut used);
    used.iter().all(|x| allowed.contains(x))
}

#[cfg(test)]
thread_local! {
    static PATH_FOLD_BINDINGS: Cell<u64> = const { Cell::new(0) };
}

/// Surviving bindings that took the multi-hop aggregate fold since the last take.
#[cfg(test)]
pub(crate) fn take_path_fold_bindings() -> u64 {
    PATH_FOLD_BINDINGS.with(|count| count.replace(0))
}

pub(super) fn note_path_binding() {
    #[cfg(test)]
    PATH_FOLD_BINDINGS.with(|count| count.set(count.get() + 1));
}

/// Shared RETURN gate for both folds: no DISTINCT, no GROUP BY, no `RETURN *`,
/// at least one aggregate, no user alias in the internal `__` namespace
/// (the reservation [`super::scope::projection_collides_with_internal_names`]
/// owns — the contract and its rationale live there), and every `ORDER BY`
/// key answerable from the folded row (the fold keeps no input rows; see
/// [`folded_order_items`]). The gate fills the shared [`FoldOutput`]
/// directly — the run's params and Cypher version ride in, so no second
/// struct transcribes the gated fields beside it.
pub(super) fn fold_return<'a>(
    suffix: &'a [Clause],
    params: &'a BTreeMap<String, Value>,
) -> Option<FoldOutput<'a>> {
    let Clause::Return {
        distinct,
        group_by,
        items,
        order,
        skip,
        limit,
    } = suffix.first()?
    else {
        return None;
    };
    if *distinct
        || group_by.is_some()
        || has_star_item(items)
        || projection_collides_with_internal_names(items)
    {
        return None;
    }
    if !items.iter().any(|item| is_agg_expr(&item.expr)) {
        return None;
    }
    let order = order
        .iter()
        .zip(folded_order_items(order, items)?)
        .map(|(key, index)| FoldOrderKey {
            key,
            source: items[index].output_name(index),
        })
        .collect();
    Some(FoldOutput {
        items,
        order,
        skip: skip.as_ref(),
        limit: limit.as_ref(),
        params,
    })
}

/// What the RETURN gate fills and both folds finish through: the gated
/// projection shape — items, ORDER BY keys and their sources, SKIP and
/// LIMIT — beside the run's params and Cypher version. One struct, no
/// transcription twin.
pub(super) struct FoldOutput<'a> {
    items: &'a [Projection],
    /// Per `ORDER BY` key: the key and the output column its folded value
    /// was written to (`folded_order_items`), resolved once here — one entry
    /// per key, owned so the output outlives the [`FoldedReturn`] it was
    /// computed for.
    order: Vec<FoldOrderKey<'a>>,
    skip: Option<&'a Expr>,
    limit: Option<&'a Expr>,
    params: &'a BTreeMap<String, Value>,
}

/// One `ORDER BY` key's folded source: the key and the column its folded
/// value was written to.
struct FoldOrderKey<'a> {
    key: &'a OrderItem,
    source: String,
}

pub(super) fn finish_folded_rows(
    graph: &ExecGraph<'_>,
    output: &FoldOutput<'_>,
    mut out: Vec<Row>,
) -> Result<FoldedReturn> {
    let (skip_n, limit_n) = resolve_page_bounds(graph, output.params, output.skip, output.limit)?;
    if !output.order.is_empty() {
        // The folded row is the only row the fold kept: each key parks the
        // value of the item `folded_order_items` resolved for it, and the
        // shared sort entry point orders the rows by those parked columns.
        let keys =
            OrderKeys::parked_only(output.order.iter().map(|folded| folded.key), output.items);
        for row in out.iter_mut() {
            let parked: Vec<Value> = output
                .order
                .iter()
                .map(|folded| result_column(row, &folded.source).clone())
                .collect();
            park_values(&keys, row, parked);
        }
        sort_rows_by_parked_keys(&keys, &mut out, None)?;
        unpark_order_keys(&keys, &mut out);
    }
    Ok(folded_return(
        graph,
        page(out, skip_n, limit_n),
        output.items,
    ))
}

/// One projection item's rank within its half — the one classification
/// table both folds build over their items: `Key(i)` reads the fold's
/// key half at `i` (the path plan's key list, the one-hop fold's key
/// values), `Agg(i)` its aggregate half at `i` (the plan's count seats,
/// the one-hop fold's stream seats). Both folds project their output
/// rows through the one shared [`project_ranked_row`] over these ranks
/// — no parallel per-item table and no second projection loop exists
/// at either fold.
pub(super) enum Rank {
    Key(usize),
    Agg(usize),
}

/// Project one group into its output row — the one shared projection
/// over the [`Rank`] table both folds call: the key arm reads the
/// group's key values at the rank's position, the aggregate arm the
/// fold-supplied finisher at its own. Each fold supplies only its
/// aggregate accessor (the one-hop fold's `StreamSeat`, the path fold's
/// `(CountSeat, StreamAgg)` pair), so a new rank kind updates this one
/// match and both folds.
///
/// Input contract, stated at the boundary: `ranks` is the one-per-item
/// classification of `items` (asserted below), every `Key(i)` indexes
/// `key_vals` — the key half the classify pass sized; an out-of-range
/// rank fails loudly here, never silently drops a column — and every
/// `Agg(i)` is backed by the caller's `finish`. The item and its rank
/// travel together (zip), never re-joined by index.
pub(super) fn project_ranked_row<F>(
    items: &[Projection],
    ranks: &[Rank],
    key_vals: &[Value],
    finish: F,
) -> Row
where
    F: Fn(usize) -> Value,
{
    debug_assert_eq!(
        items.len(),
        ranks.len(),
        "ranks is the one-per-item classification of items"
    );
    let mut row = Row::new();
    for (i, (item, rank)) in items.iter().zip(ranks.iter()).enumerate() {
        let name = item.output_name(i);
        match rank {
            Rank::Key(key) => {
                row.insert(
                    name,
                    key_vals
                        .get(*key)
                        .expect("every Key rank indexes key_vals, the classify pass's key half")
                        .clone(),
                );
            }
            Rank::Agg(agg) => {
                row.insert(name, finish(*agg));
            }
        }
    }
    row
}

/// ADR-0036: fold one-hop MATCH + streamable RETURN aggregates (no row per edge).
/// A MATCH fold that already built the following RETURN.
pub(crate) struct FoldedReturn {
    pub rows: Vec<Row>,
    pub result: QueryResult,
    /// Clauses after the current MATCH that the fold consumed, including RETURN.
    pub consumed_after: usize,
}

mod one_hop;
mod path;
mod plan;
pub(crate) use one_hop::try_fold_one_hop_aggs;
pub(crate) use path::try_fold_path_aggs;
pub(in crate::exec) use plan::is_pred_op;

fn folded_return(graph: &ExecGraph<'_>, rows: Vec<Row>, items: &[Projection]) -> FoldedReturn {
    FoldedReturn {
        result: result_from_projected(graph, rows.clone(), items),
        rows,
        consumed_after: 1,
    }
}

pub(crate) fn result_from_projected(
    graph: &ExecGraph<'_>,
    rows: Vec<Row>,
    items: &[Projection],
) -> QueryResult {
    let columns: Vec<String> = items
        .iter()
        .enumerate()
        .map(|(i, p)| p.output_name(i))
        .collect();
    // The folded one-hop aggregate path (ADR-0036) binds labels-only entity
    // stubs (ADR-0033) and skips `project_rows`, so the RETURN-time refresh
    // (ADR-0170) has to happen here -- otherwise a projected node or
    // relationship would reach the caller clipped to its labels/type.
    let mut cache = MaterializeCache::default();
    let data: Vec<Vec<Value>> = rows
        .iter()
        .map(|r| {
            columns
                .iter()
                .map(|c| {
                    graph.materialize_cached(r.get(c).cloned().unwrap_or(Value::Null), &mut cache)
                })
                .collect()
        })
        .collect();
    QueryResult::intermediate(columns, data, graph.stats())
}
