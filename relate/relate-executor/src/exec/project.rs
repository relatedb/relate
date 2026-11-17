//! Projection (`RETURN`/`WITH`): expressions, ORDER BY, DISTINCT,
//! SKIP/LIMIT with top-k selection.

use std::cell::OnceCell;
use std::collections::{BTreeMap, HashSet};

use relate_core::{Error, Result, Value};
use relate_ir::{Clause, Projection};

use crate::ctx::ExecCtx;
use crate::eval::{eval, expr_vars, is_agg_expr};
use crate::overlay::{ExecGraph, MaterializeCache};
use crate::row::Row;

use super::aggregate::{aggregate_ex, Aggregation, WhereMode};
use super::classification::{may_cut, ProjectionClass, ProjectionShape};
use super::order::{page, PageBounds};
use super::pattern_policies::MatchFold;
use super::post_merge::{adopt_rows, finish_projection, land_feed, Landed};
use super::scope::{
    is_internal_column, return_columns, star_scope_names, visible_keys, ProjectionFields,
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

/// The next clause's projection fields and decided shape, when it is a
/// `WITH`/`RETURN` at all — the prologue every follower-fold predicate
/// reads. A shape [`ProjectionShape::decide`] rejects yields `None`: the
/// clause never folds a feed, and the projection itself fails closed at
/// the same decision when the clause reaches it.
fn next_fold_shape<'a>(
    next: Option<&'a Clause>,
) -> Option<(ProjectionFields<'a>, ProjectionShape)> {
    let fields = next.and_then(ProjectionFields::from_clause)?;
    let shape = ProjectionShape::decide(&fields).ok()?;
    Some((fields, shape))
}

/// Whether the clause after an `UNWIND` or a `MATCH` consumes its rows as a
/// stream: a `WITH` or `RETURN` whose projection folds an aggregation as rows
/// arrive ([`ProjectionShape::streams`] — an aggregating shape with no `*`
/// item, since a `*` groups by every carried variable and needs the rows'
/// shape up front). Every other follower takes the rows as a held vector, as
/// ever.
pub(crate) fn streams_into_projection(next: Option<&Clause>) -> bool {
    next_fold_shape(next).is_some_and(|(_, shape)| shape.streams())
}

/// Whether the clause after an `UNWIND` keeps its generator: a follower that
/// folds rows as rows arrive — the aggregation, or a clause that *may* take
/// the ordered window. Every other follower still takes the rows as a held
/// vector. `UNWIND` is the one producer whose output is always generatable,
/// so it is the one this answers; a `MATCH` keeps
/// [`streams_into_projection`], whose answer the match chain also reads for
/// its stage runs.
///
/// Parking is deliberately the *static* half of the fold decision, answered
/// from the clause's fields and shape alone: no page bounds are resolved
/// here and no per-clause plan is built. The projection resolves the bounds
/// once, from its own classification, and its `materialize_feed` decides
/// fold against collect — so a parked feed that resolves to no window (a
/// zero page, an overflowing `skip + limit`) collects there, a
/// nondeterministic bound is one draw rather than two, and the shapes that
/// never fold (`DISTINCT`, `SKIP`-only, unordered) are not parked at all.
///
/// The window half is [`may_cut`], the classification's own bounds-free
/// precondition — read here without its aggregating mask, so the park
/// admits a superset of [`ProjectionClass::window_keep`] by construction.
pub(crate) fn unwind_streams_into(next: Option<&Clause>) -> bool {
    next_fold_shape(next).is_some_and(|(fields, shape)| shape.streams() || may_cut(&fields))
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

pub(crate) fn project_rows(
    ctx: ExecCtx<'_>,
    feed: RowFeed<'_>,
    fields: &ProjectionFields<'_>,
    inflate: bool,
    source: RowSource,
    scope: Option<&Row>,
) -> Result<Vec<Row>> {
    let ExecCtx { graph, params, .. } = ctx;
    // The clause's one classification: every decision below reads it, so
    // the paths cannot disagree on the shape — and the shape's decision
    // ([`ProjectionShape::decide`]) owns the DISTINCT-* rejection.
    let class = ProjectionClass::new(fields, ProjectionShape::decide(fields)?);
    // The clause's paging bounds, resolved once, and the bounded window of
    // the ordered `LIMIT` shape (ADR-0555 §3) derived from them — the one
    // pair of facts the feed, the non-aggregating merge paths and the
    // finish tail all read, so a nondeterministic bound cannot size the
    // window differently from the page it feeds. `aggregate_rows` takes no
    // bounds: the aggregating shape's group table is its own declared
    // breaker, which is exactly what `window_keep` masks.
    let bounds = class.page_bounds(graph, params)?;
    // An empty page answers nothing, decided here before any container is
    // opened or any row is built: a `LIMIT 0` costs O(1) whatever the
    // input, and no merge path ever carries a zero-capacity container.
    if bounds.answers_nothing() {
        return Ok(Vec::new());
    }
    let feed = materialize_feed(feed, &class, bounds)?;
    // The windowed shape no longer reserves the sort's merged buffer — the
    // window's own container is bounded — but the feed it drains is live for
    // this whole call, and the producer releases its own reservation as it
    // returns ([`UnwindFeed::materialize`]). Nothing else on the path prices
    // it, so reserve it here for as long as this projection holds it: the
    // admission pool then keeps pricing what the statement really holds,
    // rather than only the `skip + limit` window it answers with.
    let _held_feed = if class.window_keep(bounds).is_some() {
        Some(relate_core::tokens::WorkspaceCharge::retain_with(|| {
            crate::workspace::row_buffer_bytes(feed.probe())
        })?)
    } else {
        None
    };
    // One cache for the whole projection: its expressions never write, so
    // the graph state, and with it each payload's refresh, is fixed here.
    let mut cache = MaterializeCache::default();
    let mut refresh = if inflate {
        Refresh::Materialize(&mut cache)
    } else {
        Refresh::Identity
    };
    // One dispatch over both facts: a pre-projected feed has to say what it
    // means for every shape, so a shape added later cannot inherit the
    // pass-through by accident.
    let merged = match (source, class.shape) {
        // The sink already projected these rows, so the pass-through's only
        // mutation is the inflate refresh, in place, and the landing owns
        // the container (ADR-0463's retaining `Vec`, or the ordered
        // window's bounded heap).
        (RowSource::SinkProjected, ProjectionShape::Plain) => {
            let RowFeed::Held(mut rows) = feed else {
                return Err(Error::internal(
                    "a sink-projected feed arrived as a generated feed",
                ));
            };
            refresh.rows(graph, &mut rows);
            adopt_rows(graph, params, &class, bounds, rows)?
        }
        // `sinkable_projection` declines every aggregating and `*` shape, so
        // a sink-projected feed never arrives with one — and none of them
        // has a pre-projected policy to fall back on.
        (RowSource::SinkProjected, _) => {
            return Err(Error::internal(
                "a sink-projected feed reached a shape the sink never projects",
            ))
        }
        (RowSource::Bindings, shape) => match shape {
            ProjectionShape::Star => star_rows(graph, params, feed, &class, &mut refresh, bounds)?,
            // The aggregating path lands no container: its charge lives in
            // the aggregation's own emission loop, and its rows are an
            // unordered table the tail still sorts.
            ProjectionShape::Aggregate { .. } => Landed {
                rows: aggregate_rows(graph, params, feed, &class, scope, &mut refresh)?,
                windowed: false,
            },
            ProjectionShape::Plain => {
                plain_rows(graph, params, feed, &class, &mut refresh, bounds)?
            }
        },
    };
    finish_projection(graph, params, &class, merged, bounds)
}

/// The feed as the merge paths consume it — the pipeline's materialization
/// stage: a collected row vector, or — when the projection folds a
/// generated feed ([`ProjectionClass::folds_feed`]: the aggregation, or the
/// ordered window over its once-resolved bounds) — the rows still flowing.
/// A parked feed that resolves to no window is collected here under the one
/// retaining charge, and the eager pre-projection page applies at this
/// stage, so a `LIMIT 1` must not evaluate a later row whose projection
/// raises an error (CIP-27).
fn materialize_feed<'a>(
    feed: RowFeed<'a>,
    class: &ProjectionClass<'_>,
    bounds: PageBounds,
) -> Result<RowFeed<'a>> {
    let rows = match feed {
        RowFeed::Held(rows) => rows,
        // A generated feed stays generated for the shapes that fold it as
        // rows arrive — the aggregation and the ordered window
        // ([`ProjectionClass::folds_feed`]): the window's `land` is a
        // per-row fold, every row is evaluated to compare it, so the
        // producer is never materialized on those shapes. The collecting
        // shapes still land below.
        unwind @ RowFeed::Unwind(_) if class.folds_feed(bounds) => return Ok(unwind),
        folded @ RowFeed::Folded(_) => return Ok(folded),
        // The generator's own materializing form — the one place a feed
        // that cannot fold becomes rows, so its charge cannot drift from
        // the one `exec_unwind` pays.
        RowFeed::Unwind(unwind) => unwind.materialize()?,
    };
    // The eager shape pages here, on the clause's one resolved bounds; every
    // other shape leaves the page to the finish tail.
    Ok(RowFeed::Held(if class.eager_paged {
        page(rows, bounds)
    } else {
        rows
    }))
}

/// The `RETURN *` mode: the visible incoming bindings plus the explicit
/// items, with every output row charged as it lands.
fn star_rows(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    feed: RowFeed<'_>,
    class: &ProjectionClass<'_>,
    refresh: &mut Refresh<'_>,
    bounds: PageBounds,
) -> Result<Landed> {
    // ADR-0463 (LABS-781): the star branch's output rows are as retaining
    // as the plain branch's — same guard, same landing policy — after the
    // spent source row is dropped so `owned_heap` sees the output
    // container's real ownership. The ordered-window shape lands the same
    // built row into the bounded heap instead.
    let built = |r: Row| -> Result<Row> {
        let mut n = Row::new();
        for (k, v) in &r {
            if !is_internal_column(k) {
                n.insert(k.clone(), refresh.apply(graph, v.clone()));
            }
        }
        for (projection, name) in &class.value_items {
            let value = eval(graph, &r, &projection.expr, params)?;
            n.insert(name.clone(), refresh.apply(graph, value));
        }
        drop(r);
        Ok(n)
    };
    land_feed(graph, params, class, bounds, feed, built)
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
/// landing policy per output row. The items, the parked-key plan and the
/// plain WHERE are the classification's own — read from it, never handed
/// back as loose parameters.
fn plain_rows(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    feed: RowFeed<'_>,
    class: &ProjectionClass<'_>,
    refresh: &mut Refresh<'_>,
    bounds: PageBounds,
) -> Result<Landed> {
    let order_keys = &class.order_keys;
    let where_mode = class.where_mode;
    let mut keep = HashSet::new();
    if let Some(w) = where_mode.and_then(WhereMode::plain) {
        expr_vars(w, &mut keep);
    }
    for expr in order_keys.exprs() {
        expr_vars(expr, &mut keep);
    }
    // ADR-0463: the projected rows are one retaining container. Each output
    // row's payloads plus its buffers grow the running total; the
    // per-allocation checks alone cannot see a many-row result whose rows
    // each build a large but under-cap value. The ordered-window shape
    // (ADR-0555 §3) lands the same built row into the bounded top-k heap
    // instead, its plain WHERE read as each row arrives.
    let built = |mut row: Row| -> Result<Row> {
        // The item values first, against the incoming row, then the kept
        // bindings (a `WITH` filter or an `ORDER BY` key may name one the
        // projection does not produce), then the items over them — the
        // projected row a key is evaluated against.
        let projected: Vec<(String, Value)> = class
            .value_items
            .iter()
            .map(|(projection, name)| {
                let val = eval(graph, &row, &projection.expr, params)?;
                Ok((name.clone(), refresh.apply(graph, val)))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut row2 = Row::new();
        let kept: Vec<String> = row
            .keys()
            .filter(|name| keep.contains(name.as_str()))
            .cloned()
            .collect();
        for name in kept {
            let value = row
                .take_value(&name)
                .expect("a key just read from the row is bound in it");
            row2.insert(name, value);
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
        Ok(row2)
    };
    land_feed(graph, params, class, bounds, feed, built)
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
