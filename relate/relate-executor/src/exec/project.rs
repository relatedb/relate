//! Projection (`RETURN`/`WITH`): expressions, ORDER BY, DISTINCT,
//! SKIP/LIMIT with top-k selection.

use std::cell::OnceCell;
use std::collections::{BTreeMap, HashSet};

use relate_core::{Error, Result, Value};
use relate_ir::{Clause, Expr, Projection};

use crate::ctx::ExecCtx;
use crate::distinct_set::DistinctSet;
use crate::eval::{eval, expr_vars, is_agg_expr};
use crate::overlay::{ExecGraph, MaterializeCache};
use crate::row::{Row, RowNames};

use super::aggregate::{aggregate_ex, Aggregation, GroupSubclauses};
use super::order::{
    page, pages_eagerly, park_order_keys, resolve_page_bounds, sort_rows_by_parked_keys,
    unpark_order_keys, OrderKeys,
};
use super::pattern_policies::MatchFold;
use super::scope::{
    has_star_item, is_internal_column, is_star_item, projection_names, result_column,
    return_columns, star_scope_names, visible_keys, ProjectionFields,
};
use super::unwind::RowFeed;
use super::{materialize_rows_strict, QueryResult};

/// The statement's result columns for a `RETURN`/`WITH`: the shared
/// [`return_columns`] item set, with a `RETURN *`/`WITH *` item expanded
/// from the first row's visible keys — or, when the table is empty, from
/// the declared scope `star_scope_names` walks. This answers the *result*
/// shape the dispatcher materializes; the front's compile-time admission
/// set for the following clause is a different question, asked by
/// `clause_introduced_names` / `call_subquery_exports` in `relate-front`.
pub(crate) fn projection_result_columns(
    items: &[Projection],
    rows: &[Row],
    clauses: &[Clause],
) -> Vec<String> {
    return_columns(items, || match rows.first() {
        None => star_scope_names(clauses),
        Some(row) => {
            let mut keys = visible_keys(row);
            keys.sort();
            keys
        }
    })
}

/// The statement result of a `RETURN`: the result columns, the strict cell
/// move, and the intermediate result carrying the view's own stats. The
/// caller's PROFILE tick has already read the rows, so moving the cells is
/// safe.
pub(crate) fn return_result(
    graph: &ExecGraph<'_>,
    items: &[Projection],
    clauses: &[Clause],
    rows: &mut [Row],
) -> Result<QueryResult> {
    let columns = projection_result_columns(items, rows, clauses);
    // Projection rows are rectangular by contract (ADR-0339): every result
    // column is bound, so a miss is an engine contract break. Move the cells
    // out instead of cloning.
    let data = materialize_rows_strict(rows, &columns)?;
    // The view's own stats: a read view carries no scratch, so its stats are
    // already the default, and the read path's wrapper (`execute_read_at`)
    // resets them on the way out regardless — one policy, not a per-arm mode
    // branch.
    Ok(QueryResult::intermediate(columns, data, graph.stats()))
}

/// The named columns of `row`, cloned in order — the row a multi-column
/// DISTINCT keys.
fn row_columns(row: &Row, names: &[String]) -> Vec<Value> {
    names
        .iter()
        .map(|n| result_column(row, n).clone())
        .collect()
}

/// The columns a non-`*` DISTINCT keys, borrowed when the projection is
/// one column wide (a one-element slice: no column clone, no row vector).
/// The arity-1 *keying* lives in [`DistinctSet::insert_row`] alone; this
/// is only the borrow.
fn distinct_columns<'r>(row: &'r Row, names: &[String]) -> std::borrow::Cow<'r, [Value]> {
    match names {
        [name] => std::borrow::Cow::Borrowed(std::slice::from_ref(result_column(row, name))),
        names => std::borrow::Cow::Owned(row_columns(row, names)),
    }
}

/// What a merged row's DISTINCT key is: a `*` row keys its visible keys, a
/// named projection its output columns — decided once, not re-checked per
/// row.
enum DistinctKeys<'a> {
    Visible,
    Columns(&'a [String]),
}

impl DistinctKeys<'_> {
    /// One row's key, borrowed when the projection is one column wide.
    fn key<'r>(&self, row: &'r Row) -> std::borrow::Cow<'r, [Value]> {
        match self {
            DistinctKeys::Visible => std::borrow::Cow::Owned(row_columns(row, &visible_keys(row))),
            DistinctKeys::Columns(names) => distinct_columns(row, names),
        }
    }
}

/// The post-merge DISTINCT stage: first-seen keep over the merged rows,
/// keyed by the output columns (a `*` row keys its visible keys). The
/// returned charge holds the kept row buffer; the caller resizes it as
/// the paging stage ([`page`]) and the result materialization shrink the
/// buffer. The set owns its workspace charge and drops (releasing it) at
/// the end of the pass.
fn retain_distinct(
    merged: Vec<Row>,
    keys: DistinctKeys<'_>,
) -> Result<(Vec<Row>, relate_core::tokens::WorkspaceCharge)> {
    let retained_rows = relate_core::tokens::WorkspaceCharge::retain_with(|| {
        crate::workspace::row_buffer_bytes(&merged)
    })?;
    let mut seen = DistinctSet::default();
    let mut kept = Vec::new();
    for row in merged {
        if seen.insert_row(&keys.key(&row))? {
            kept.push(row);
        }
    }
    drop(seen);
    Ok((kept, retained_rows))
}

/// Where a `RETURN`/`WITH` projection's incoming rows come from. The sink's
/// caller is the only authority for the projected case: a row carries no
/// marker a user alias could forge, and an ordinary batch whose bindings
/// happen to bind every output name is still evaluated, not passed through.
pub(crate) enum RowSource {
    /// Bindings the projection must evaluate its items against.
    Bindings,
    /// The match sink already evaluated this clause's items on every row
    /// (their columns plus the parked `__ord*` sort keys), so only the
    /// sort, page, DISTINCT, and column-extraction stages are left.
    SinkProjected,
}

/// Field references of a `RETURN`/`WITH` projection clause.
#[derive(Clone, Copy)]
pub(crate) struct ProjectSpec<'a> {
    /// CIP-236 explicit grouping keys (None = infer from non-aggregating
    /// items; empty = `GROUP BY ()` global aggregation).
    pub(crate) group_by: Option<&'a [Expr]>,
    pub items: &'a [Projection],
    pub distinct: bool,
    pub order: &'a [relate_ir::OrderItem],
    pub skip: &'a Option<Expr>,
    pub limit: &'a Option<Expr>,
    pub where_clause: Option<&'a Expr>,
}

impl<'a> ProjectSpec<'a> {
    pub(crate) fn fields(&self) -> ProjectionFields<'a> {
        ProjectionFields {
            distinct: self.distinct,
            items: self.items,
            group_by: self.group_by,
            order: self.order,
            skip: self.skip.as_ref(),
            limit: self.limit.as_ref(),
            where_clause: self.where_clause,
        }
    }
}

/// The post-merge WHERE stage: keep the rows whose predicate reads true;
/// a failing filter expression propagates, the one error policy every
/// WHERE path shares. The HAVING form never reaches here (ADR-0453): the
/// caller passes only the plain filtering predicate.
fn filter_where(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    predicate: &Expr,
    rows: Vec<Row>,
) -> Result<Vec<Row>> {
    let mut filtered = Vec::with_capacity(rows.len());
    for row in rows {
        if crate::eval::predicate_value_holds(&eval(
            graph,
            &row,
            predicate,
            params,
            cypher_version,
        )?)? {
            filtered.push(row);
        }
    }
    Ok(filtered)
}

/// Whether a projection folds its input as it arrives, so an `UNWIND` or a
/// `MATCH` feeding it can generate rows one at a time instead of
/// materializing them: an aggregating projection with no `*` item (a `*`
/// groups by every carried variable, which needs the rows' shape up front).
/// One decision for the dispatcher, which chooses the feed, and the
/// projection, which consumes it.
pub(crate) fn streams_into_aggregation(fields: &ProjectionFields<'_>) -> bool {
    let aggregating_subclause = fields.order.iter().any(|o| is_agg_expr(&o.expr))
        || fields.where_clause.is_some_and(is_agg_expr);
    let has_agg = fields.items.iter().any(|p| is_agg_expr(&p.expr))
        || fields.group_by.is_some()
        || (fields.distinct && aggregating_subclause);
    has_agg && !has_star_item(fields.items)
}

/// Whether the clause after an `UNWIND` or a `MATCH` consumes its rows as a
/// stream: a `WITH` or `RETURN` whose projection folds an aggregation as rows
/// arrive. Every other follower takes the rows as a held vector, as ever.
pub(crate) fn streams_into_projection(next: Option<&Clause>) -> bool {
    next.and_then(ProjectionFields::from_clause)
        .is_some_and(|fields| streams_into_aggregation(&fields))
}

/// Whether a projection only maps rows: no aggregation, `DISTINCT`,
/// `ORDER BY`, `SKIP` or `LIMIT`, so each input row yields at most one
/// output row independent of every other and the clause can run on a stream
/// of bounded batches (a `WHERE` filters per row and streams). The
/// complement — the breakers — keep the held path.
pub(crate) fn maps_each_row(fields: &ProjectionFields<'_>) -> bool {
    !fields.distinct
        && fields.group_by.is_none()
        && fields.order.is_empty()
        && fields.skip.is_none()
        && fields.limit.is_none()
        && !fields.items.iter().any(|item| is_agg_expr(&item.expr))
        && !fields.where_clause.is_some_and(is_agg_expr)
}

/// Open the aggregation a streamed `MATCH` folds into, for the
/// `WITH`/`RETURN` that [`streams_into_aggregation`] accepts: the plan
/// [`project_rows`] would have built from the collected rows, minus the rows.
pub(crate) fn open_fold<'a>(
    graph: &ExecGraph<'_>,
    fields: &ProjectionFields<'a>,
    order: &'a OrderKeys<'a>,
    params: &'a BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    scope: Option<&'a Row>,
) -> Result<Aggregation<'a>> {
    Aggregation::open(
        graph,
        &RowFeed::Held(Vec::new()),
        fields.items,
        fields.group_by,
        params,
        cypher_version,
        &GroupSubclauses {
            order,
            having: fields.where_clause.filter(|w| is_agg_expr(w)),
            plain_where: fields.where_clause.filter(|w| !is_agg_expr(w)),
            import_scope: scope,
        },
    )
}

/// The fold an uncapped `MATCH` streams into when the clause after it
/// aggregates (`next`), `None` when it does not stream or a fold already
/// parked its order keys in `order_slot` (the slot keeps them alive for the
/// fold's borrow).
pub(crate) fn open_match_fold<'a>(
    graph: &ExecGraph<'_>,
    next: Option<&'a Clause>,
    order_slot: &'a OnceCell<OrderKeys<'a>>,
    params: &'a BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    scope: Option<&'a Row>,
) -> Result<Option<MatchFold<'a>>> {
    if order_slot.get().is_some() {
        return Ok(None);
    }
    let Some(fields) = next
        .and_then(ProjectionFields::from_clause)
        .filter(streams_into_aggregation)
    else {
        return Ok(None);
    };
    let order = order_slot
        .get_or_init(|| OrderKeys::new(fields.order, fields.items, fields.group_by.is_some()));
    open_fold(graph, &fields, order, params, cypher_version, scope)
        .map(|aggregation| Some(MatchFold::new(aggregation)))
}

/// The projection's payload-inflation policy: identity, or a materializing
/// refresh through the projection's one cache. One value carries both the
/// per-value policy and whether the post-projection row sweep runs, so the
/// two cannot disagree.
enum Refresh<'a> {
    Identity,
    Materialize(&'a mut MaterializeCache),
}

impl Refresh<'_> {
    /// The policy applied to one value.
    fn apply(&mut self, graph: &ExecGraph<'_>, value: Value) -> Value {
        match self {
            Refresh::Identity => value,
            Refresh::Materialize(cache) => graph.materialize_cached(value, cache),
        }
    }

    /// The post-projection sweep: refresh every visible column of every row.
    /// The parked `__ord*` sort keys keep the values the group or sink
    /// produced — they are compared, not returned, so they are not inflation
    /// surface.
    fn rows(&mut self, graph: &ExecGraph<'_>, rows: &mut [Row]) {
        if matches!(self, Refresh::Identity) {
            return;
        }
        for row in rows {
            for (key, value) in row.iter_mut() {
                if !is_internal_column(key) {
                    let taken = std::mem::replace(value, Value::Null);
                    *value = self.apply(graph, taken);
                }
            }
        }
    }
}

pub(crate) fn project_rows(
    ctx: ExecCtx<'_>,
    feed: RowFeed<'_>,
    spec: &ProjectSpec<'_>,
    inflate: bool,
    source: RowSource,
    scope: Option<&Row>,
) -> Result<Vec<Row>> {
    let ExecCtx {
        graph,
        params,
        cypher_version,
        ..
    } = ctx;
    let ProjectSpec {
        items,
        distinct,
        group_by,
        order,
        skip,
        limit,
        where_clause,
    } = *spec;
    let star = has_star_item(items);
    let has_agg = items.iter().any(|p| is_agg_expr(&p.expr));
    // ADR-0453: a DISTINCT projection with an aggregating subclause (an
    // ORDER BY key or the WHERE predicate) groups by its items and takes
    // the aggregation path, so the subclause's aggregate calls recompute
    // over those groups.
    let aggregating_subclause =
        order.iter().any(|o| is_agg_expr(&o.expr)) || where_clause.is_some_and(is_agg_expr);
    // A `*` item leaves no item list to group by, and validation admits the
    // shape because DISTINCT makes the clause aggregating regardless of `*`.
    // The executor cannot give it HAVING semantics either: the aggregate
    // would evaluate per row through scalar `eval`, whose error is swallowed
    // (the WHERE form returned zero rows, the ORDER BY form sorted on null
    // keys), so the combination fails closed.
    if star && distinct && aggregating_subclause {
        return Err(Error::syntax(
            "InvalidAggregation: a DISTINCT * projection cannot aggregate in its ORDER BY or WHERE subclause",
        ));
    }
    let subclause_agg = distinct && aggregating_subclause;
    // CIP-236: an explicit GROUP BY takes the aggregation path even without
    // aggregate items (the non-aggregating case acts as DISTINCT).
    let has_agg = has_agg || group_by.is_some() || subclause_agg;
    let out_names = projection_names(items);
    // The one per-key scope decision: every path below parks its keys with
    // this decision, and the projection's own grouping mode sets it.
    let order_keys = OrderKeys::new(order, items, group_by.is_some());
    // Without sorting, filtering, aggregation, or DISTINCT, paging may stop
    // row evaluation. In particular a LIMIT 1 must not evaluate a later row
    // whose projection raises an error (CIP-27).
    let eager_paged = !has_agg && pages_eagerly(&spec.fields());
    // The match sink already evaluated these items. Eager paging never takes
    // that path, so this bag is the full input the sort and the page see.
    let preprojected = matches!(source, RowSource::SinkProjected);
    // ADR-0453 decides the HAVING/plain split once, here: an aggregating
    // path whose WHERE carries aggregate calls takes them as HAVING
    // (evaluated per group inside the aggregation); every other WHERE
    // form — any WHERE on a non-aggregating path included — stays the
    // plain post-merge filter.
    let having = where_clause.filter(|w| has_agg && is_agg_expr(w));
    let plain_where = where_clause.filter(|_| having.is_none());
    // A generated feed reaches the aggregation unmaterialized; every other
    // consumer receives it collected, exactly as the clause would have left
    // it.
    let (rows, stream) = match feed {
        RowFeed::Held(rows) => (rows, None),
        RowFeed::Unwind(unwind) if streams_into_aggregation(&spec.fields()) => {
            (Vec::new(), Some(RowFeed::Unwind(unwind)))
        }
        folded @ RowFeed::Folded(_) => (Vec::new(), Some(folded)),
        RowFeed::Unwind(unwind) => {
            let mut retained = graph.retain_bytes(0)?;
            let mut rows = Vec::new();
            for row in unwind {
                let row = row?;
                retained.add_row(graph, row.values())?;
                rows.push(row);
            }
            (rows, None)
        }
    };
    let rows = if eager_paged {
        let (skip_n, limit_n) =
            resolve_page_bounds(graph, params, cypher_version, skip.as_ref(), limit.as_ref())?;
        page(rows, skip_n, limit_n)
    } else {
        rows
    };
    // One cache for the whole projection: its expressions never write, so
    // the graph state, and with it each payload's refresh, is fixed here.
    let mut cache = MaterializeCache::default();
    let mut refresh = if inflate {
        Refresh::Materialize(&mut cache)
    } else {
        Refresh::Identity
    };
    let mut merged = if preprojected {
        preprojected_rows(graph, rows, &mut refresh)?
    } else if star && !has_agg {
        star_rows(
            graph,
            params,
            cypher_version,
            rows,
            items,
            &order_keys,
            &mut refresh,
        )?
    } else if has_agg {
        aggregate_rows(
            graph,
            params,
            cypher_version,
            stream.unwrap_or(RowFeed::Held(rows)),
            items,
            group_by,
            &order_keys,
            having,
            plain_where,
            scope,
            &mut refresh,
        )?
    } else {
        plain_rows(
            graph,
            params,
            cypher_version,
            rows,
            items,
            &order_keys,
            where_clause,
            &mut refresh,
        )?
    };

    // The WHERE stage: whatever the split above left as the plain
    // predicate — a plain WHERE, or any WHERE on a non-aggregating path —
    // filters the merged rows here.
    if let Some(w) = plain_where {
        merged = filter_where(graph, params, cypher_version, w, merged)?;
    }

    // The eager stop already paged on its own resolved bounds; the
    // post-merge stage pages only on the non-eager path.
    let (skip_n, limit_n) = if eager_paged {
        (0, None)
    } else {
        resolve_page_bounds(graph, params, cypher_version, skip.as_ref(), limit.as_ref())?
    };

    if !order_keys.is_empty() {
        let top_k = match (distinct, limit_n) {
            (false, Some(lim)) => skip_n.checked_add(lim),
            _ => None,
        };
        sort_rows_by_parked_keys(&order_keys, &mut merged, top_k)?;
    }

    let mut retained_rows = relate_core::tokens::WorkspaceCharge::empty();
    if distinct {
        let keys = if star {
            DistinctKeys::Visible
        } else {
            DistinctKeys::Columns(&out_names)
        };
        let (kept, charge) = retain_distinct(merged, keys)?;
        merged = kept;
        retained_rows = charge;
        retained_rows.resize_with(|| crate::workspace::row_buffer_bytes(&merged))?;
    }

    merged = page(merged, skip_n, limit_n);
    if distinct {
        retained_rows.resize_with(|| crate::workspace::row_buffer_bytes(&merged))?;
    }

    if star {
        // The parked keys were sort surface only; a `*` row keeps exactly
        // the incoming bindings and the explicit items.
        unpark_order_keys(&order_keys, &mut merged);
        drop(retained_rows);
        return Ok(merged);
    }
    let names = RowNames::new(&out_names);
    let mut out = Vec::with_capacity(merged.len());
    for mut row in merged {
        let values = out_names
            .iter()
            .map(|name| {
                row.take_value(name)
                    .expect("projection rows carry every result column")
            })
            .collect();
        out.push(Row::from_names(&names, values));
    }
    if distinct {
        retained_rows.resize_with(|| crate::workspace::row_buffer_bytes(&out))?;
    }
    drop(retained_rows);
    Ok(out)
}

/// The pass-through mode: the match sink already evaluated the items, so
/// only the inflate refresh and the one landing charge remain.
fn preprojected_rows(
    graph: &ExecGraph<'_>,
    mut rows: Vec<Row>,
    refresh: &mut Refresh<'_>,
) -> Result<Vec<Row>> {
    // ADR-0463 (LABS-781): the pass-through's output container is as
    // retaining as the plain branch's — refreshed payloads under inflate,
    // and every row's buffers — so its landings grow the same running
    // total (one policy, every merge path).
    let mut retained = graph.retain_bytes(0)?;
    refresh.rows(graph, &mut rows);
    for row in &rows {
        retained.add_row(graph, row.values())?;
    }
    Ok(rows)
}

/// The `RETURN *` mode: the visible incoming bindings plus the explicit
/// items, with every output row charged as it lands.
fn star_rows(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    rows: Vec<Row>,
    items: &[Projection],
    order_keys: &OrderKeys<'_>,
    refresh: &mut Refresh<'_>,
) -> Result<Vec<Row>> {
    // ADR-0463 (LABS-781): the star branch's output rows are as retaining
    // as the plain branch's — same guard, same landing charge, after the
    // spent source row is dropped so `owned_heap` sees the output
    // container's real ownership.
    let mut retained = graph.retain_bytes(0)?;
    rows.into_iter()
        .map(|r| -> Result<Row> {
            let mut n = Row::new();
            for (k, v) in &r {
                if !is_internal_column(k) {
                    n.insert(k.clone(), refresh.apply(graph, v.clone()));
                }
            }
            for (index, projection) in items.iter().enumerate() {
                if is_star_item(projection) {
                    continue;
                }
                let value = eval(graph, &r, &projection.expr, params, cypher_version)?;
                n.insert(projection.output_name(index), refresh.apply(graph, value));
            }
            drop(r);
            park_order_keys(order_keys, &mut n, |expr, scope| {
                eval(graph, scope, expr, params, cypher_version)
            })?;
            // The star branch's output row landing in its final shape,
            // through the same one `add_row` charge.
            retained.add_row(graph, n.values())?;
            Ok(n)
        })
        .collect::<Result<Vec<_>>>()
}

/// The aggregation mode: HAVING (the aggregate-call WHERE) is evaluated per
/// group inside the aggregation, and the group rows were charged in the
/// emission loop.
#[allow(clippy::too_many_arguments)]
fn aggregate_rows<'a>(
    graph: &ExecGraph<'_>,
    params: &'a BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    feed: RowFeed<'_>,
    items: &'a [Projection],
    group_by: Option<&'a [Expr]>,
    order_keys: &'a OrderKeys<'a>,
    having: Option<&'a Expr>,
    plain_where: Option<&'a Expr>,
    scope: Option<&'a Row>,
    refresh: &mut Refresh<'_>,
) -> Result<Vec<Row>> {
    // ADR-0453: the caller routed the aggregate-call WHERE here as HAVING —
    // the aggregation path evaluates it per group (aggregate calls fold the
    // group, non-aggregate leaves read the projected row) and filters
    // there; the plain filter would evaluate the aggregates against the
    // aggregated row and fail.
    let mut agg = match feed {
        RowFeed::Folded(aggregation) => aggregation.finish(graph)?,
        feed => aggregate_ex(
            graph,
            feed,
            items,
            group_by,
            params,
            cypher_version,
            &GroupSubclauses {
                order: order_keys,
                having,
                plain_where,
                import_scope: scope,
            },
        )?,
    };
    refresh.rows(graph, &mut agg);
    // The rows were charged inside aggregate_ex's emission loop, as each
    // group row landed; the inflate refresh above materializes graph-cached
    // payloads into row-owned copies whose delta rides uncharged (a bounded
    // approximation: the subquery-scope materialization path, its payloads
    // already counted once at their construction or borrowed from the
    // store).
    Ok(agg)
}

/// The plain projection mode: each item over the incoming row, the kept
/// bindings the subclauses may name, then the parked order keys — one
/// landing charge per output row.
#[allow(clippy::too_many_arguments)]
fn plain_rows(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    rows: Vec<Row>,
    items: &[Projection],
    order_keys: &OrderKeys<'_>,
    where_clause: Option<&Expr>,
    refresh: &mut Refresh<'_>,
) -> Result<Vec<Row>> {
    let mut keep = HashSet::new();
    if let Some(w) = where_clause {
        expr_vars(w, &mut keep);
    }
    for expr in order_keys.exprs() {
        expr_vars(expr, &mut keep);
    }
    let mut out = Vec::new();
    // ADR-0463: the projected rows are one retaining container. Each output
    // row's payloads plus its buffers grow the running total; the
    // per-allocation checks alone cannot see a many-row result whose rows
    // each build a large but under-cap value.
    let mut retained = graph.retain_bytes(0)?;
    for mut row in rows {
        // The item values first, against the incoming row, then the kept
        // bindings (a `WITH` filter or an `ORDER BY` key may name one the
        // projection does not produce), then the items over them — the
        // projected row a key is evaluated against.
        let projected: Vec<(String, Value)> = items
            .iter()
            .enumerate()
            .filter(|(_, projection)| !is_star_item(projection))
            .map(|(i, projection)| {
                let name = projection.output_name(i);
                let val = eval(graph, &row, &projection.expr, params, cypher_version)?;
                Ok((name, refresh.apply(graph, val)))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut row2 = Row::new();
        let kept: Vec<String> = row
            .keys()
            .filter(|name| keep.contains(name.as_str()))
            .cloned()
            .collect();
        for name in kept {
            if let Some(value) = row.take_value(&name) {
                row2.insert(name, value);
            }
        }
        for (name, value) in projected {
            row2.insert(name, value);
        }
        // The source row is spent — its kept bindings moved, the rest
        // dropped — so the built row's values are the output container's
        // own and `owned_heap` sees their real ownership: a projected
        // variable read is a clone that shared the source row's `Arc`
        // while it lived, and charging before the drop would skip its
        // payload.
        drop(row);
        park_order_keys(order_keys, &mut row2, |expr, scope| {
            eval(graph, scope, expr, params, cypher_version)
        })?;
        // The built row landing in the output container, in its final
        // shape: the identity rule counts each distinct payload once
        // however many slots reference it (a projected alias and its
        // parked ORDER BY clone count the payload once, a fresh constructed
        // key its own), and the buffer figure carries the parked columns'
        // slots.
        retained.add_row(graph, row2.values())?;
        out.push(row2);
    }
    Ok(out)
}

#[cfg(test)]
mod refresh_tests {
    use super::*;
    use crate::graph::Graph;
    use crate::row::{interned_schema_calls, RowNames};

    /// The inflate sweep updates each visible column in place: the row keeps
    /// its schema, so a sweep over wide rows interns no schema (a
    /// remove-and-reinsert rebuilt and re-interned one per column per row —
    /// quadratic in the column count), and the column order and the parked
    /// `__ord*` key survive untouched.
    #[test]
    fn the_inflate_sweep_keeps_each_rows_schema() {
        let names: Vec<String> = ["a", "b", "c", "d", "e", "f", "g", "h", "__ord0"]
            .map(String::from)
            .to_vec();
        let schema = RowNames::new(&names);
        let mut rows: Vec<Row> = (0..1000)
            .map(|i| Row::from_names(&schema, (0..9).map(|c| Value::Int(i + c)).collect()))
            .collect();
        let graph = Graph::default();
        let view = ExecGraph::read(&graph);
        let mut cache = MaterializeCache::default();
        let before = interned_schema_calls();
        let snapshot = rows.clone();
        Refresh::Materialize(&mut cache).rows(&view, &mut rows);
        assert_eq!(interned_schema_calls() - before, 0);
        assert_eq!(rows, snapshot);
    }
}
