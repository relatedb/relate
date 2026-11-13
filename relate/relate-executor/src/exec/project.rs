//! Projection (`RETURN`/`WITH`): expressions, ORDER BY, DISTINCT,
//! SKIP/LIMIT with top-k selection.

use std::cell::OnceCell;
use std::collections::{BTreeMap, HashSet};

use relate_core::{Error, Result, Value};
use relate_ir::{Clause, Expr, Projection};

use crate::ctx::ExecCtx;
use crate::distinct_set::DistinctSet;
use crate::eval::{eval, expr_vars, filter_rows_where, is_agg_expr};
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
use super::{materialize_rows_strict, QueryResult, RowFeed};

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

/// Which merge path owns a `RETURN`/`WITH` projection — the three
/// exclusive shapes, decided once at classification so no consumer
/// re-derives the combination from independent flags:
/// [`ProjectionShape::Plain`] (each row projects alone),
/// [`ProjectionShape::Star`] (a `*` item, nothing aggregating: the
/// visible-bindings merge) and [`ProjectionShape::Aggregate`] (aggregate
/// items, an explicit GROUP BY, or a DISTINCT with an aggregating
/// subclause).
#[derive(Clone, Copy)]
enum ProjectionShape {
    Plain,
    Star,
    Aggregate {
        /// A `*` item among the items of the aggregating shape: the group
        /// keys are every carried variable (ADR-0198 ruling 1), so a feed
        /// cannot fold into it as rows arrive
        /// ([`ProjectionShape::streams`]).
        star: bool,
    },
}

impl ProjectionShape {
    /// Decide the shape from the projection's fields — the one decision
    /// point every consumer reads. The one rejection the executor owns
    /// lives here: a `DISTINCT *` whose subclause aggregates. Validation
    /// admits the shape (DISTINCT makes the clause aggregating regardless
    /// of `*`), but the executor cannot give it HAVING semantics: the
    /// aggregate would evaluate per row through scalar `eval`, whose
    /// error is swallowed (the WHERE form returned zero rows, the ORDER
    /// BY form sorted on null keys), so the combination fails closed.
    fn decide(fields: &ProjectionFields<'_>) -> Result<Self> {
        let aggregating_subclause = fields.order.iter().any(|o| is_agg_expr(&o.expr))
            || fields.where_clause.is_some_and(is_agg_expr);
        let star = has_star_item(fields.items);
        let has_agg = fields.items.iter().any(|p| is_agg_expr(&p.expr))
            || fields.group_by.is_some()
            || (fields.distinct && aggregating_subclause);
        if fields.distinct && star && aggregating_subclause {
            return Err(Error::syntax(
                "InvalidAggregation: a DISTINCT * projection cannot aggregate in its ORDER BY or WHERE subclause",
            ));
        }
        Ok(if has_agg {
            ProjectionShape::Aggregate { star }
        } else if star {
            ProjectionShape::Star
        } else {
            ProjectionShape::Plain
        })
    }

    /// Whether a feed into this projection folds as rows arrive: an
    /// aggregating shape with no `*` item (a `*` groups by every carried
    /// variable, which needs the rows' shape up front). One decision for
    /// the dispatcher, which chooses the feed, and the projection, which
    /// consumes it.
    fn streams(&self) -> bool {
        matches!(self, ProjectionShape::Aggregate { star: false })
    }

    /// Whether the projection carries a `*` item — the shape a `*` owns
    /// outright, or the aggregating form that groups by every carried
    /// variable.
    fn star(&self) -> bool {
        matches!(
            self,
            ProjectionShape::Star | ProjectionShape::Aggregate { star: true }
        )
    }
}

/// A `RETURN`/`WITH` projection's row-independent classification — the
/// plan every projection path consults, decided once per clause: the
/// decided [`ProjectionShape`], the clause's fields (the one field
/// bundle every consumer reads), ADR-0453's WHERE split, the eager-page
/// decision, and the parked-key plan and output names built from the
/// same fields. `project_rows` and `open_fold` read this one plan, so
/// the streaming decision, the fold and the projection path cannot
/// disagree on the clause's shape — and no site re-destructures the
/// fields or hand-builds a `ProjectionFields` literal. The shape alone
/// ([`ProjectionShape::decide`]) answers the dispatcher's cheap
/// questions without building the per-key plan.
pub(crate) struct ProjectionClass<'a> {
    /// The clause's fields — the one field bundle (a copy of the
    /// [`ProjectionFields`] every consumer reads).
    fields: ProjectionFields<'a>,
    /// The decided shape: which merge path owns the clause.
    shape: ProjectionShape,
    /// ADR-0453's HAVING/plain split, decided once: an aggregating path
    /// whose WHERE carries aggregate calls takes them as HAVING
    /// (evaluated per group inside the aggregation); every other WHERE
    /// form — any WHERE on a non-aggregating path included — stays the
    /// plain post-merge filter.
    having: Option<&'a Expr>,
    plain_where: Option<&'a Expr>,
    /// The eager pre-projection page ([`pages_eagerly`]) on a
    /// non-aggregating shape: `SKIP`/`LIMIT` with no `ORDER BY`,
    /// DISTINCT, or filter.
    eager_paged: bool,
    /// The projection's parked-key plan — the one per-key scope decision
    /// every path below parks its keys with.
    order_keys: OrderKeys<'a>,
    /// The projection's output names.
    out_names: Vec<String>,
}

impl<'a> ProjectionClass<'a> {
    /// The plan over an already-decided shape — the one construction
    /// site of the parked-key plan and the output names.
    fn new(fields: &ProjectionFields<'a>, shape: ProjectionShape) -> Self {
        let aggregating = matches!(shape, ProjectionShape::Aggregate { .. });
        let having = fields
            .where_clause
            .filter(|w| aggregating && is_agg_expr(w));
        let plain_where = fields.where_clause.filter(|_| having.is_none());
        Self {
            fields: *fields,
            shape,
            having,
            plain_where,
            eager_paged: !aggregating && pages_eagerly(fields),
            order_keys: OrderKeys::new(fields.order, fields.items, fields.group_by.is_some()),
            out_names: projection_names(fields.items),
        }
    }

    /// The projection's per-group subclause bundle over this plan: the
    /// parked-key plan, the HAVING predicate, and the import scope — the
    /// one derivation both [`open_fold`] and the aggregation merge path
    /// hand [`Aggregation::open`].
    fn group_subclauses<'b>(&'b self, scope: Option<&'b Row>) -> GroupSubclauses<'b> {
        GroupSubclauses {
            order: &self.order_keys,
            having: self.having,
            plain_where: self.plain_where,
            import_scope: scope,
        }
    }
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

/// Whether a projection folds its input as it arrives, so an `UNWIND` or a
/// `MATCH` feeding it can generate rows one at a time instead of
/// materializing them: an aggregating projection with no `*` item (a `*`
/// groups by every carried variable, which needs the rows' shape up front).
/// The decision is the shape's ([`ProjectionShape::decide`] +
/// [`ProjectionShape::streams`]), answered without building the
/// parked-key plan. A shape `decide` rejects (the `DISTINCT *`
/// aggregation) never folds a feed: the projection itself fails closed
/// at the same decision when the clause reaches it, which every clause
/// in the list does.
pub(crate) fn streams_into_aggregation(fields: &ProjectionFields<'_>) -> bool {
    ProjectionShape::decide(fields).is_ok_and(|shape| shape.streams())
}

/// Whether the clause after an `UNWIND` or a `MATCH` consumes its rows as a
/// stream: a `WITH` or `RETURN` whose projection folds an aggregation as rows
/// arrive. Every other follower takes the rows as a held vector, as ever.
pub(crate) fn streams_into_projection(next: Option<&Clause>) -> bool {
    next.and_then(ProjectionFields::from_clause)
        .is_some_and(|fields| streams_into_aggregation(&fields))
}

/// Open the aggregation a streamed `MATCH` folds into, for the
/// `WITH`/`RETURN` whose shape [`ProjectionShape::streams`] accepts: the
/// plan [`project_rows`] would have built from the collected rows, minus
/// the rows. The parked-key plan is the class's own
/// ([`ProjectionClass::order_keys`]) — one construction, read here.
fn open_fold<'a>(
    graph: &ExecGraph<'_>,
    class: &'a ProjectionClass<'a>,
    params: &'a BTreeMap<String, Value>,
    scope: Option<&'a Row>,
) -> Result<Aggregation<'a>> {
    Aggregation::open(
        graph,
        &[],
        class.fields.items,
        class.fields.group_by,
        params,
        &class.group_subclauses(scope),
    )
}

/// The fold an uncapped `MATCH` streams into when the clause after it
/// aggregates (`next`), `None` when it does not stream or a fold already
/// parked its classification in `class_slot` (the slot keeps the plan
/// alive for the fold's borrow).
pub(crate) fn open_match_fold<'a>(
    graph: &ExecGraph<'_>,
    next: Option<&'a Clause>,
    class_slot: &'a OnceCell<ProjectionClass<'a>>,
    params: &'a BTreeMap<String, Value>,
    scope: Option<&'a Row>,
) -> Result<Option<MatchFold<'a>>> {
    if class_slot.get().is_some() {
        return Ok(None);
    }
    let Some(fields) = next.and_then(ProjectionFields::from_clause) else {
        return Ok(None);
    };
    // The shape answers without building the parked-key plan; a rejected
    // shape never folds (the projection fails closed at its own
    // classification, which the clause reaches because every clause
    // executes).
    let shape = match ProjectionShape::decide(&fields) {
        Ok(shape) if shape.streams() => shape,
        _ => return Ok(None),
    };
    // The slot owns the clause's one classification: the parked-key plan
    // is built exactly once, here, and kept alive for the fold's borrow
    // (a fold that already claimed the slot was declined above).
    let class = class_slot.get_or_init(|| ProjectionClass::new(&fields, shape));
    open_fold(graph, class, params, scope).map(|aggregation| Some(MatchFold::new(aggregation)))
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
    let ExecCtx { graph, params, .. } = ctx;
    // The clause's one classification: every decision below reads it, so
    // the paths cannot disagree on the shape — and the shape's decision
    // ([`ProjectionShape::decide`]) owns the DISTINCT-* rejection.
    let fields = spec.fields();
    let class = ProjectionClass::new(&fields, ProjectionShape::decide(&fields)?);
    // The match sink already evaluated these items. Eager paging never takes
    // that path, so the materialized bag is the full input the sort and the
    // page see.
    let preprojected = matches!(source, RowSource::SinkProjected);
    let (rows, stream) = materialize_feed(graph, params, feed, &class)?;
    // One cache for the whole projection: its expressions never write, so
    // the graph state, and with it each payload's refresh, is fixed here.
    let mut cache = MaterializeCache::default();
    let mut refresh = if inflate {
        Refresh::Materialize(&mut cache)
    } else {
        Refresh::Identity
    };
    let merged = if preprojected {
        preprojected_rows(graph, rows, &mut refresh)?
    } else {
        match class.shape {
            ProjectionShape::Star => star_rows(
                graph,
                params,
                rows,
                class.fields.items,
                &class.order_keys,
                &mut refresh,
            )?,
            ProjectionShape::Aggregate { .. } => aggregate_rows(
                graph,
                params,
                stream.unwrap_or(RowFeed::Held(rows)),
                &class,
                scope,
                &mut refresh,
            )?,
            ProjectionShape::Plain => plain_rows(graph, params, rows, &class, &mut refresh)?,
        }
    };
    finish_projection(graph, params, &class, merged)
}

/// The feed as the merge paths consume it — the pipeline's materialization
/// stage: a collected row vector, or — when the projection folds a stream
/// ([`ProjectionClass::streams`]) — the rows still flowing. A generated
/// feed reaching a non-streaming projection is collected here under the
/// one retaining charge, and the eager pre-projection page applies at this
/// stage, so a `LIMIT 1` must not evaluate a later row whose projection
/// raises an error (CIP-27).
fn materialize_feed<'a>(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    feed: RowFeed<'a>,
    class: &ProjectionClass<'_>,
) -> Result<(Vec<Row>, Option<RowFeed<'a>>)> {
    let (rows, stream) = match feed {
        RowFeed::Held(rows) => (rows, None),
        RowFeed::Unwind(unwind) if class.shape.streams() => {
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
    let rows = if class.eager_paged {
        let (skip_n, limit_n) =
            resolve_page_bounds(graph, params, class.fields.skip, class.fields.limit)?;
        page(rows, skip_n, limit_n)
    } else {
        rows
    };
    Ok((rows, stream))
}

/// The projection's finish tail, shared by every merge path — the
/// pipeline's last stage: the plain WHERE filter over the merged rows,
/// then the paging stages (the post-merge bounds, the parked-key sort
/// with its top-k selection, the DISTINCT first-seen pass with its
/// row-buffer charge re-measured at each shrink, the page itself), then
/// the result extraction. The eager stop already paged on its own
/// resolved bounds ([`materialize_feed`]), so the tail pages only on the
/// non-eager path.
fn finish_projection(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    class: &ProjectionClass<'_>,
    merged: Vec<Row>,
) -> Result<Vec<Row>> {
    let ProjectionFields {
        distinct,
        skip,
        limit,
        ..
    } = class.fields;
    let mut merged = if let Some(w) = class.plain_where {
        filter_rows_where(graph, params, w, merged)?
    } else {
        merged
    };
    let (skip_n, limit_n) = if class.eager_paged {
        (0, None)
    } else {
        resolve_page_bounds(graph, params, skip, limit)?
    };
    if !class.order_keys.is_empty() {
        let top_k = match (distinct, limit_n) {
            (false, Some(lim)) => skip_n.checked_add(lim),
            _ => None,
        };
        sort_rows_by_parked_keys(&class.order_keys, &mut merged, top_k)?;
    }
    // The DISTINCT pass's row-buffer charge: Option-held from
    // [`retain_distinct`] — it exists only on the distinct shape, and
    // every later re-measure (the page, the output conversion) walks the
    // held charge instead of re-checking the mode beside an empty
    // sentinel.
    let mut retained_rows = if distinct {
        let keys = if class.shape.star() {
            DistinctKeys::Visible
        } else {
            DistinctKeys::Columns(&class.out_names)
        };
        let (kept, charge) = retain_distinct(merged, keys)?;
        merged = kept;
        Some(charge)
    } else {
        None
    };
    if let Some(charge) = retained_rows.as_mut() {
        charge.resize_with(|| crate::workspace::row_buffer_bytes(&merged))?;
    }
    merged = page(merged, skip_n, limit_n);
    if let Some(charge) = retained_rows.as_mut() {
        charge.resize_with(|| crate::workspace::row_buffer_bytes(&merged))?;
    }
    if class.shape.star() {
        // The parked keys were sort surface only; a `*` row keeps exactly
        // the incoming bindings and the explicit items.
        unpark_order_keys(&class.order_keys, &mut merged);
        drop(retained_rows);
        return Ok(merged);
    }
    let names = RowNames::new(&class.out_names);
    let mut out = Vec::with_capacity(merged.len());
    for mut row in merged {
        let values = class
            .out_names
            .iter()
            .map(|name| {
                row.take_value(name)
                    .expect("projection rows carry every result column")
            })
            .collect();
        out.push(Row::from_names(&names, values));
    }
    if let Some(charge) = retained_rows.as_mut() {
        charge.resize_with(|| crate::workspace::row_buffer_bytes(&out))?;
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
                let value = eval(graph, &r, &projection.expr, params)?;
                n.insert(projection.output_name(index), refresh.apply(graph, value));
            }
            drop(r);
            park_order_keys(order_keys, &mut n, |expr, scope| {
                eval(graph, scope, expr, params)
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
/// emission loop. The clause's items, grouping keys, parked-key plan and
/// HAVING predicate are the classification's own — read from it, never
/// handed back as loose parameters.
fn aggregate_rows(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    feed: RowFeed<'_>,
    class: &ProjectionClass<'_>,
    scope: Option<&Row>,
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
            class.fields.items,
            class.fields.group_by,
            params,
            &class.group_subclauses(scope),
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
/// landing charge per output row. The items, the parked-key plan and the
/// plain WHERE are the classification's own — read from it, never handed
/// back as loose parameters.
fn plain_rows(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    rows: Vec<Row>,
    class: &ProjectionClass<'_>,
    refresh: &mut Refresh<'_>,
) -> Result<Vec<Row>> {
    let items = class.fields.items;
    let order_keys = &class.order_keys;
    let where_clause = class.plain_where;
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
                let val = eval(graph, &row, &projection.expr, params)?;
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
            eval(graph, scope, expr, params)
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
        let ptrs: Vec<_> = rows.iter().map(Row::schema_ptr).collect();
        Refresh::Materialize(&mut cache).rows(&view, &mut rows);
        assert_eq!(interned_schema_calls() - before, 0);
        // `Row` equality compares schema names by value; the pointers pin
        // that no row swapped its shared schema for an equal copy.
        assert_eq!(
            rows.iter().map(Row::schema_ptr).collect::<Vec<_>>(),
            ptrs,
            "the sweep replaced a row's shared schema"
        );
        assert_eq!(rows, snapshot);
    }
}
