//! Projection (`RETURN`/`WITH`): expressions, ORDER BY, DISTINCT,
//! SKIP/LIMIT with top-k selection.

use std::cell::OnceCell;
use std::collections::{BTreeMap, HashSet};

use relate_core::{Error, Result, Value};
use relate_ir::{expr_row_vars, Clause, Projection};

use crate::ctx::ExecCtx;
use crate::eval::{eval, expr_vars, is_agg_expr};
use crate::overlay::{ExecGraph, MaterializeCache};
use crate::row::Row;

use super::aggregate::{aggregate_ex, Aggregation};
use super::classification::{ProjectionClass, ProjectionShape};
use super::feed::RowStream;
use super::order::{page, PageBounds};
use super::pattern_policies::MatchFold;
use super::post_merge::{adopt_rows, land_feed, Landed, LandedTail};
use super::scope::{return_columns, star_scope_names, visible_keys, ProjectionFields};
use super::{materialize_rows_strict, QueryResult, RowFeed};
use crate::consumer::{BatchFlow, StreamSink};

/// The statement's result columns for a `RETURN`/`WITH`: the shared
/// [`return_columns`] item set, with a `RETURN *`/`WITH *` item expanded
/// from the first row's visible keys — or, when the table is empty, from
/// the declared scope `star_scope_names` walks. This answers the *result*
/// shape the dispatcher materializes; the front's compile-time admission
/// set for the following clause is a different question, asked by
/// `clause_introduced_names` / `call_subquery_exports` in `relate-front`.
pub(crate) fn projection_result_columns(
    graph: &ExecGraph<'_>,
    items: &[Projection],
    rows: &[Row],
    clauses: &[Clause],
) -> Vec<String> {
    return_columns(items, || match rows.first() {
        None => star_scope_names(clauses),
        Some(row) => {
            let mut keys = visible_keys(graph, row);
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
    let columns = projection_result_columns(graph, items, rows, clauses);
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

/// The terminal `RETURN`'s streaming form (ADR-0682): pull the input one
/// bounded batch at a time, project it, and hand the result cells to the
/// consumer instead of landing them in the part's `Vec<Row>`. The pull is
/// the back-pressure — the next batch is pulled only after the consumer
/// returns — and a consumer answering [`BatchFlow::Stop`] stops the stream
/// and cancels the statement (`Error::Cancelled`, `"query cancelled"`).
///
/// Only the map-each-row shape reaches here (the dispatcher's gate): no
/// aggregation, `DISTINCT`, `ORDER BY`, `SKIP` or `LIMIT`, so each batch
/// projects independently of every other. The columns are announced once,
/// with the first batch — a `RETURN *` learns them from its first row, so
/// they cannot go out earlier — or alone when the answer is empty. The
/// returned result carries the columns and the view's own stats but no
/// rows: the consumer has them.
pub(crate) fn stream_terminal_return(
    ctx: ExecCtx<'_>,
    mut stream: RowStream<'_>,
    sink: &mut StreamSink<'_>,
    fields: &ProjectionFields<'_>,
    items: &[Projection],
    clauses: &[Clause],
    rows_from: RowSource,
) -> Result<QueryResult> {
    let graph = ctx.graph;
    // The one announce step: the statement's result columns, read off the
    // first batch's rows (or none, for an empty answer), handed over once.
    let announce = |sink: &mut StreamSink<'_>, rows: &[Row]| -> Result<Vec<String>> {
        let announced = projection_result_columns(graph, items, rows, clauses);
        sink.columns(&announced)?;
        Ok(announced)
    };
    let mut columns: Option<Vec<String>> = None;
    while let Some(batch) = stream.next_batch()? {
        let batch = batch.into_vec();
        let input = match rows_from {
            RowSource::Bindings => ProjectionInput::Feed(RowFeed::Held(RowStream::held(batch))),
            // The sink already evaluated this clause's items on every row;
            // per batch, only the column extraction is left.
            RowSource::SinkProjected => ProjectionInput::SinkProjected(batch),
            RowSource::SinkLanded(_) => {
                return Err(Error::internal(
                    "a landed sink reached the streaming terminal",
                ))
            }
        };
        let mut rows = project_rows(ctx, input, fields, true, None)?;
        let announced = match columns.take() {
            Some(announced) => announced,
            None => announce(sink, &rows)?,
        };
        let cells = materialize_rows_strict(&mut rows, &announced)?;
        columns = Some(announced);
        // ADR-0691: the streamed cells cross the same expansion check the
        // materialized result does, one batch at a time (a batch's memo
        // lives only while the batch does).
        graph.check_result_expansion(&cells)?;
        if matches!(sink.batch(cells)?, BatchFlow::Stop) {
            // The consumer's early exit is the stream's: stop the walk
            // (a completing source still runs out its own work), then
            // cancel the statement.
            stream.stop()?;
            return Err(Error::cancelled());
        }
    }
    let columns = match columns {
        Some(columns) => columns,
        // An empty answer still announces its columns once.
        None => announce(sink, &[])?,
    };
    Ok(QueryResult::intermediate(
        columns,
        Vec::new(),
        graph.stats(),
    ))
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
        && !fields.items_aggregate()
        && !fields.where_clause.is_some_and(is_agg_expr)
}

/// The next clause's projection fields and decided shape, when it is a
/// `WITH`/`RETURN` at all — the prologue the `MATCH` fold predicate reads.
/// A shape [`ProjectionShape::decide`] rejects yields `None`: the clause
/// never folds a feed, and the projection itself fails closed at the same
/// decision when the clause reaches it.
fn next_fold_shape<'a>(
    next: Option<&'a Clause>,
) -> Option<(ProjectionFields<'a>, ProjectionShape)> {
    let fields = next.and_then(ProjectionFields::from_clause)?;
    let shape = ProjectionShape::decide(&fields).ok()?;
    Some((fields, shape))
}

/// The fold pair when the clause after a `MATCH` consumes its rows as a
/// stream (a `WITH`/`RETURN` whose projection folds an aggregation as rows
/// arrive — [`ProjectionShape::streams`]): the one derivation both readers
/// take, [`streams_into_projection`]'s answer and [`open_match_fold`]'s
/// fields for the class.
fn folding_shape(next: Option<&Clause>) -> Option<(ProjectionFields<'_>, ProjectionShape)> {
    next_fold_shape(next).filter(|(_, shape)| shape.streams())
}

/// Whether the clause after a `MATCH` consumes its rows as a stream. Every
/// other follower takes the rows as a held vector, as ever.
pub(crate) fn streams_into_projection(next: Option<&Clause>) -> bool {
    folding_shape(next).is_some()
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
    let Some((fields, shape)) = folding_shape(next) else {
        return Ok(None);
    };
    // The slot owns the clause's one classification: the parked-key plan
    // is built exactly once, here, and kept alive for the fold's borrow
    // (a fold that already claimed the slot was declined above).
    let class = class_slot.get_or_init(|| ProjectionClass::new(graph, &fields, shape));
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
                if !graph.is_internal_key(key) {
                    let taken = std::mem::replace(value, Value::Null);
                    *value = self.apply(graph, taken);
                }
            }
        }
    }
}

/// Where a `RETURN`/`WITH` projection's incoming rows come from, as the
/// dispatcher's loop carries it between clauses. The sink's caller is the
/// only authority for the projected case: a row carries no marker a user
/// alias could forge, and an ordinary batch whose bindings happen to bind
/// every output name is still evaluated, not passed through. The
/// projection itself reads the decided [`ProjectionInput`], never this
/// flag.
#[derive(Clone, Copy)]
pub(crate) enum RowSource {
    /// Bindings the projection must evaluate its items against.
    Bindings,
    /// The match sink already evaluated this clause's items on every row
    /// (their columns plus the parked `__ord*` sort keys), so only the
    /// sort, page, DISTINCT, and column-extraction stages are left.
    SinkProjected,
    /// A bounded sink already landed these rows in the clause's own
    /// container; the carried facts say which tail stages it ran, so the
    /// projection adopts the landing instead of re-opening a container.
    SinkLanded(LandedTail),
}

/// The one input type of the projection: the rows it evaluates its items
/// against ([`RowFeed`]: held, generated or already folded), or the rows a
/// producer already projected — which owns them, so the sink shape can
/// never hold a generated or folded feed.
pub(crate) enum ProjectionInput<'a> {
    /// Bindings the projection evaluates its items against.
    Feed(RowFeed<'a>),
    /// The match sink's (or a grouped shortcut's) rows, already carrying
    /// this clause's columns plus the parked `__ord*` sort keys: only the
    /// sort, page, DISTINCT and column-extraction stages are left.
    SinkProjected(Vec<Row>),
    /// A bounded sink's landing: the rows in the clause's own container and
    /// the tail stages it already ran.
    SinkLanded(Landed),
}

impl ProjectionInput<'_> {
    /// The rows already in hand, for the decisions that read the row shape
    /// before the walk starts.
    fn probe(&self) -> &[Row] {
        match self {
            Self::Feed(feed) => feed.probe(),
            Self::SinkProjected(rows) => rows,
            Self::SinkLanded(landed) => &landed.rows,
        }
    }

    /// Finish this input when the clause abandons it — a `LIMIT 0` answers
    /// no rows, but it must not cancel the input's own work: a completing
    /// stream (`CALL { … } IN TRANSACTIONS`) runs its remaining batches —
    /// and reports its PROFILE facts — on `stop`, never on drop. The rule
    /// lives on the input type, so a new input kind states its own contract
    /// instead of inheriting "nothing to finish" by silence.
    fn complete(&mut self) -> Result<()> {
        // Exhaustive: every kind names its own answer, so a new input kind
        // fails to compile until it states one.
        match self {
            Self::Feed(RowFeed::Held(stream)) => stream.stop(),
            Self::Feed(RowFeed::Folded(_)) | Self::SinkProjected(_) | Self::SinkLanded(_) => Ok(()),
        }
    }

    /// The reservation the row table this input hands over owes the pool —
    /// the one home of the handoff policy [`prepare_input`] bills at every
    /// exit, read once the input has settled (after the collected
    /// materialize and page, never mid-pipeline). Rows the input itself
    /// holds are reserved here and released when its stage ends; rows
    /// owned elsewhere reserve nothing: a folded feed is priced where it
    /// was built, a generated feed by its producer, and the aggregating
    /// shape's collected rows by its own `InputBill` (InputBill's measure
    /// is the same row walk — a second reservation here would price one
    /// container twice and refuse statements whose real footprint fits
    /// the pool).
    fn reservation(
        &self,
        class: &ProjectionClass<'_>,
    ) -> Result<relate_core::tokens::WorkspaceCharge> {
        match self {
            // A folded feed's rows are priced where the feed was built.
            Self::Feed(RowFeed::Folded(_)) => Ok(relate_core::tokens::WorkspaceCharge::empty()),
            // A generated feed's rows are priced by its producer (its
            // probe is empty, so the buffer charge would be a no-op —
            // stated, not relied on).
            Self::Feed(RowFeed::Held(stream)) if !stream.is_held() => {
                Ok(relate_core::tokens::WorkspaceCharge::empty())
            }
            // The aggregating shape's own input bill reserves its
            // collected rows and releases them per landing.
            _ if matches!(class.shape, ProjectionShape::Aggregate { .. }) => {
                Ok(relate_core::tokens::WorkspaceCharge::empty())
            }
            // Every other input holds its rows for as long as it holds
            // them: the one reservation of the row table it carries.
            settled => crate::workspace::charge_row_buffer(settled.probe()),
        }
    }
}

pub(crate) fn project_rows(
    ctx: ExecCtx<'_>,
    input: ProjectionInput<'_>,
    fields: &ProjectionFields<'_>,
    inflate: bool,
    scope: Option<&Row>,
) -> Result<Vec<Row>> {
    let ExecCtx { graph, .. } = ctx;
    // The clause's one classification: every decision below reads it, so
    // the paths cannot disagree on the shape — and the shape's decision
    // ([`ProjectionShape::decide`]) owns the DISTINCT-* rejection.
    let class = ProjectionClass::new(graph, fields, ProjectionShape::decide(fields)?);
    project_classed(ctx, &class, input, inflate, scope)
}

/// [`project_rows`] over the caller's already-built classification — for
/// the grouped finishes, which park their folded rows through the class's
/// own parked-key plan ([`ProjectionClass::order_keys`]) before handing
/// them over, so one plan both writes and reads the parked columns and no
/// caller re-derives the plan the projection is about to build anyway.
pub(super) fn project_classed(
    ctx: ExecCtx<'_>,
    class: &ProjectionClass<'_>,
    mut input: ProjectionInput<'_>,
    inflate: bool,
    scope: Option<&Row>,
) -> Result<Vec<Row>> {
    let ExecCtx { graph, params, .. } = ctx;
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
        // The empty page stops the rows, never the input's own work (the
        // input type owns that rule).
        input.complete()?;
        return Ok(Vec::new());
    }
    // The prepared input carries its own reservation: whatever container it
    // hands back is priced for as long as this projection holds it — the
    // charge's owner is the handoff, never a caller that happens to know the
    // shape (the windowed shape's live feed was the one case priced here
    // before; a held table is the same fact).
    let (input, _input_charge) = prepare_input(input, class, bounds)?;
    // One cache for the whole projection: its expressions never write, so
    // the graph state, and with it each payload's refresh, is fixed here.
    let mut cache = MaterializeCache::default();
    let mut refresh = if inflate {
        Refresh::Materialize(&mut cache)
    } else {
        Refresh::Identity
    };
    let landed = merge_rows(graph, params, class, input, scope, &mut refresh, bounds)?;
    landed.finish(graph, params, class, bounds)
}

/// The merge dispatch: which path turns the prepared input into the
/// projection's landed rows. One dispatch over both facts — the input's
/// kind and the clause's shape — so a pre-projected input has to say what
/// it means for every shape, and a shape added later cannot inherit the
/// pass-through by accident.
fn merge_rows(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    class: &ProjectionClass<'_>,
    input: ProjectionInput<'_>,
    scope: Option<&Row>,
    refresh: &mut Refresh<'_>,
    bounds: PageBounds,
) -> Result<Landed> {
    match (input, class.shape) {
        // The sink already projected these rows, so the pass-through's only
        // mutation is the inflate refresh, in place, and the landing owns
        // the container (ADR-0463's retaining `Vec`, or the ordered
        // window's bounded heap).
        (ProjectionInput::SinkProjected(mut rows), ProjectionShape::Plain) => {
            refresh.rows(graph, &mut rows);
            adopt_rows(graph, class, bounds, rows)
        }
        // A bounded sink already ran the clause's container; the carried
        // landing's facts say so, and the only mutation is the in-place
        // inflate refresh.
        (ProjectionInput::SinkLanded(mut landed), ProjectionShape::Plain) => {
            refresh.rows(graph, &mut landed.rows);
            Ok(landed)
        }
        // `sinkable_projection` declines every aggregating and `*` shape, so
        // a pre-projected feed never arrives with one — and neither form has
        // a pre-projected policy to fall back on.
        (ProjectionInput::SinkProjected(_) | ProjectionInput::SinkLanded(_), _) => Err(
            Error::internal("a pre-projected feed reached a shape the sink never projects"),
        ),
        // The star and plain shapes consume the batch stream itself —
        // their row assembly lands each pulled row — so the arm hands
        // them the one input they can take, never a conversion that
        // refuses a variant at runtime.
        (ProjectionInput::Feed(RowFeed::Held(stream)), ProjectionShape::Star) => {
            star_rows(graph, params, stream, class, refresh, bounds)
        }
        // The aggregating path lands no container: its charge lives in
        // the aggregation's own emission loop, and its rows are an
        // unordered table the tail still sorts. It is also the one shape
        // that finishes a folded feed.
        (ProjectionInput::Feed(feed), ProjectionShape::Aggregate { .. }) => {
            Ok(LandedTail::untouched()
                .into_landed(aggregate_rows(graph, params, feed, class, scope, refresh)?))
        }
        (ProjectionInput::Feed(RowFeed::Held(stream)), ProjectionShape::Plain) => {
            plain_rows(graph, params, stream, class, refresh, bounds)
        }
        // The impossible pairing, stated once where the pairing is
        // decided: a folded feed is finished by the aggregating shape
        // alone (the streaming match sink builds one only for a folding
        // consumer).
        (
            ProjectionInput::Feed(RowFeed::Folded(_)),
            ProjectionShape::Star | ProjectionShape::Plain,
        ) => Err(Error::internal(
            "a folded feed reached a shape that cannot finish it",
        )),
    }
}

/// The input as the merge paths consume it — the pipeline's
/// materialization stage: a collected row vector, or — when the projection
/// folds a generated feed ([`ProjectionClass::folds_feed`]: the
/// aggregation, the ordered window over its once-resolved bounds, a
/// pass-through, a DISTINCT first-seen pass) — the rows still flowing. A
/// parked feed that resolves to no window is collected here under the one
/// retaining charge, and the eager pre-projection page applies at this
/// stage, so a `LIMIT 1` must not evaluate a later row whose projection
/// raises an error (CIP-27). The charge is the container's own reservation
/// ([`crate::workspace::charge_row_buffer`]): the rows it holds for as long
/// as it holds them — a streamed feed's rows belong to its producer and are
/// priced there, so that probe is empty and the reservation is a no-op.
fn prepare_input<'a>(
    input: ProjectionInput<'a>,
    class: &ProjectionClass<'_>,
    bounds: PageBounds,
) -> Result<(ProjectionInput<'a>, relate_core::tokens::WorkspaceCharge)> {
    // One exhaustive dispatch: every input kind states its own handoff, so
    // a new kind fails to compile until it states one. The stream forms
    // stay streams — a generated stream for the shapes that fold it as
    // rows arrive ([`ProjectionClass::folds_feed`]): the window's `land`
    // is a per-row fold, every row is evaluated to compare it, so the
    // producer is never materialized on those shapes; a folded feed is
    // already the producer's state, priced where it was built. Everything
    // else is collected: a pre-projected input stays pre-projected, every
    // collected feed becomes held rows.
    let (rows, wrap): (Vec<Row>, Rewrap<'a>) = match input {
        // A bounded sink's landing is already the clause's answer page (or
        // first-seen set); it carries its own facts and pages itself.
        ProjectionInput::SinkLanded(landed) => {
            let input = ProjectionInput::SinkLanded(landed);
            let charge = input.reservation(class)?;
            return Ok((input, charge));
        }
        ProjectionInput::SinkProjected(rows) => (rows, ProjectionInput::SinkProjected),
        ProjectionInput::Feed(RowFeed::Folded(folded)) => {
            let input = ProjectionInput::Feed(RowFeed::Folded(folded));
            let charge = input.reservation(class)?;
            return Ok((input, charge));
        }
        ProjectionInput::Feed(RowFeed::Held(stream))
            if class.folds_feed(bounds) && !stream.is_held() =>
        {
            // A streamed feed stays a stream — the guard already excluded
            // the held container (the one source whose probe is
            // non-empty) — and the reservation policy states where its
            // rows are priced.
            let input = ProjectionInput::Feed(RowFeed::Held(stream));
            let charge = input.reservation(class)?;
            return Ok((input, charge));
        }
        // The generator's own materializing form — the one place a feed
        // that cannot fold becomes rows, so its charge cannot drift from
        // the one the clause arm pays.
        ProjectionInput::Feed(RowFeed::Held(stream)) => (stream.materialize()?, held_input),
    };
    // The eager shape pages here, on the clause's one resolved bounds;
    // every other shape leaves the page to the finish tail.
    let input = wrap(if class.eager_paged {
        page(rows, bounds)
    } else {
        rows
    });
    // Every exit bills the same one policy: the input's own reservation,
    // read once the input has settled.
    let charge = input.reservation(class)?;
    Ok((input, charge))
}

/// How collected rows are handed back as the projection's input.
type Rewrap<'a> = fn(Vec<Row>) -> ProjectionInput<'a>;

/// Collected rows as the projection's feed input.
fn held_input(rows: Vec<Row>) -> ProjectionInput<'static> {
    ProjectionInput::Feed(RowFeed::Held(RowStream::held(rows)))
}

/// The `RETURN *` mode: the visible incoming bindings plus the explicit
/// items, with every output row charged as it lands.
fn star_rows(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    stream: RowStream<'_>,
    class: &ProjectionClass<'_>,
    refresh: &mut Refresh<'_>,
    bounds: PageBounds,
) -> Result<Landed> {
    // ADR-0463 (LABS-781): the star branch's output rows are as retaining
    // as the plain branch's — same guard, same landing policy — after the
    // spent source row is dropped so `owned_heap` sees the output
    // container's real ownership. The ordered-window shape lands the same
    // built row into the bounded heap instead.
    let built = |row| star_output_row(graph, params, class, refresh, row);
    land_feed(graph, params, class, bounds, stream, built)
}

/// The `*` shape's row assembly: the visible incoming bindings plus the
/// explicit items. Shared by the collected landing ([`star_rows`]) and the
/// pass-through page fold ([`land_feed`]).
fn star_output_row(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    class: &ProjectionClass<'_>,
    refresh: &mut Refresh<'_>,
    r: Row,
) -> Result<Row> {
    let mut n = Row::new();
    for (k, v) in &r {
        if !graph.is_internal_key(k) {
            n.insert(k.clone(), refresh.apply(graph, v.clone()));
        }
    }
    for (projection, name) in &class.value_items {
        let value = eval(graph, &r, &projection.expr, params)?;
        n.insert(name.clone(), refresh.apply(graph, value));
    }
    drop(r);
    Ok(n)
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
        // The breaker answers its own batch stream (ADR-0660); the inflate
        // refresh below rewrites each answered row in place.
        RowFeed::Held(stream) => aggregate_ex(
            graph,
            stream,
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
    stream: RowStream<'_>,
    class: &ProjectionClass<'_>,
    refresh: &mut Refresh<'_>,
    bounds: PageBounds,
) -> Result<Landed> {
    let keep = keep_bindings(class);
    // ADR-0463: the projected rows are one retaining container. Each output
    // row's payloads plus its buffers grow the running total; the
    // per-allocation checks alone cannot see a many-row result whose rows
    // each build a large but under-cap value. The ordered-window shape
    // (ADR-0555 §3) lands the same built row into the bounded top-k heap
    // instead; its trailing WHERE filters the window's page in the finish
    // tail.
    let built = |row| plain_output_row(graph, params, class, &keep, refresh, row);
    land_feed(graph, params, class, bounds, stream, built)
}

/// The bindings a plain projection keeps from its incoming row: the ones a
/// `WITH` filter or an `ORDER BY` key may name though the projection does
/// not produce them. The one derivation, read by [`plain_rows`] and the
/// pass-through page fold ([`land_feed`]).
fn keep_bindings(class: &ProjectionClass<'_>) -> HashSet<String> {
    // Which names the WHERE / `ORDER BY` keys read from the *input* row —
    // one walker for the whole clause. A plain projection keeps its input
    // names in scope for them (`WITH n.g AS g WHERE n.v > 4` still reads
    // `n`); a `DISTINCT` projection ends them, so a subquery body in the
    // WHERE that re-uses a dropped name binds it fresh (Neo4j-verified;
    // REL8-2023 issue 64) and only the subclause's own row-scope reads are
    // carried into the built row. The aggregating shapes take their own
    // seed derivation ([`super::aggregate::SeedStore`]), narrowed the same
    // way.
    let input_names: fn(&relate_ir::Expr, &mut HashSet<String>) = if class.fields.distinct {
        expr_row_vars
    } else {
        expr_vars
    };
    let mut keep = HashSet::new();
    if let Some(w) = class.plain_where() {
        input_names(w, &mut keep);
    }
    for expr in class.order_keys.exprs() {
        input_names(expr, &mut keep);
    }
    keep
}

/// The plain shape's row assembly: the item values first, against the
/// incoming row, then the kept bindings, then the items over them — the
/// projected row a key is evaluated against. Shared by the collected
/// landing ([`plain_rows`]) and the pass-through page fold
/// ([`land_feed`]).
fn plain_output_row(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    class: &ProjectionClass<'_>,
    keep: &HashSet<String>,
    refresh: &mut Refresh<'_>,
    mut row: Row,
) -> Result<Row> {
    let projected: Vec<(String, Value)> = class
        .value_items
        .iter()
        .map(|(projection, name)| {
            let val = eval(graph, &row, &projection.expr, params)?;
            Ok((name.clone(), refresh.apply(graph, val)))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut row2 = Row::new();
    for (name, value) in row.iter_mut() {
        if keep.contains(name.as_str()) {
            row2.insert(name.clone(), std::mem::replace(value, Value::Null));
        }
    }
    for (name, value) in projected {
        row2.insert(name, value);
    }
    // The source row is spent — its kept bindings moved, the rest dropped —
    // so the built row's values are the output container's own and
    // `owned_heap` sees their real ownership: a projected variable read is
    // a clone that shared the source row's `Arc` while it lived, and
    // charging before the drop would skip its payload.
    drop(row);
    Ok(row2)
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
