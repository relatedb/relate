//! The projection's post-merge pipeline: the landing container every
//! merge path hands its built rows to, the DISTINCT first-seen pass, and
//! the finish tail that pages the merged table — split from `project.rs`
//! (the same move that produced `order.rs`). The family now reads as
//! three: the merge paths stay in `project.rs`, the classification lives
//! in `classification.rs`, and the container and the tail live here.

use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::Expr;

use super::classification::WhereMode;
use crate::distinct_set::DistinctSet;
use crate::eval::eval;
use crate::eval::{filter_rows_where_clause, plain_where_holds};
use crate::overlay::{ExecGraph, RetainedTotal};
use crate::row::{Row, RowNames};

use super::classification::ProjectionClass;
use super::feed::RowStream;
use super::order::{
    page, park_order_keys, sort_rows_by_parked_keys, unpark_order_keys, PageBounds, PageCursor,
};
use super::scope::{visible_keys, ProjectionFields};
use super::top_k::TopKWindow;
use super::{missing_column_error, read_declared_columns, take_declared_columns, Missing};

/// The columns a DISTINCT keys, borrowed when the projection is one
/// column wide (a one-element slice: no column clone, no row vector). The
/// key set itself is resolved by [`DistinctPass`]: visible keys for a `*`
/// projection, output names otherwise. The arity-1 *keying* lives in
/// [`DistinctSet::insert_row`] alone; this is only the borrow.
///
/// Both arms report a miss the way the output conversion does — the
/// borrowed arm through [`missing_column_error`], the owned arm through the
/// one declared-column read ([`read_declared_columns`]) — so the key read
/// and the extraction cannot disagree about what a missing column means.
fn distinct_columns<'r>(row: &'r Row, names: &[String]) -> Result<std::borrow::Cow<'r, [Value]>> {
    match names {
        [name] => {
            let value = row.get(name).ok_or_else(|| missing_column_error(name))?;
            Ok(std::borrow::Cow::Borrowed(std::slice::from_ref(value)))
        }
        names => Ok(std::borrow::Cow::Owned(read_declared_columns(
            row,
            names,
            Missing::Error,
        )?)),
    }
}

/// The `DISTINCT` first-seen pass (ADR-0660): a row is kept only when its
/// key columns are new, so the pass holds its charged seen-set
/// ([`DistinctSet`]) and the answer, never the input population. The one
/// spelling of the key read and the membership decision — the landing
/// container's gate ([`land_feed`]) and the post-merge vector pass
/// ([`retain_distinct`]) both keep rows through [`keep`][Self::keep]. The
/// key columns are the
/// projection's own output names, or — for a `*` shape — the first row's
/// visible keys, resolved once because every row of one projection carries
/// the same scope.
struct DistinctPass<'a> {
    class: &'a ProjectionClass<'a>,
    seen: DistinctSet,
    star_columns: Option<Vec<String>>,
}

impl<'a> DistinctPass<'a> {
    fn new(class: &'a ProjectionClass<'a>) -> Self {
        Self {
            class,
            seen: DistinctSet::default(),
            star_columns: None,
        }
    }

    /// The seen-set's own charged bytes — the pass's share of a container's
    /// retained figure.
    fn retained_bytes(&self) -> u64 {
        self.seen.retained_bytes()
    }

    /// Whether the row's key columns have not been seen before — the one
    /// membership decision, whose charge the set owns.
    fn keep(&mut self, graph: &ExecGraph<'_>, row: &Row) -> Result<bool> {
        let keys = if self.class.shape.star() {
            let columns = self
                .star_columns
                .get_or_insert_with(|| visible_keys(graph, row));
            distinct_columns(row, columns)?
        } else {
            distinct_columns(row, &self.class.out_names)?
        };
        self.seen.insert_row(&keys)
    }
}

/// The post-merge DISTINCT stage: first-seen keep over the merged rows,
/// keyed through [`DistinctPass`] — the one first-seen helper, here over a
/// vector rather than a stream. The returned charge is the reservation for
/// the **pre-pass** input: the whole `merged` vector is alive for the
/// duration of the pass, so the charge is taken from it before any row is
/// dropped, and the caller resizes it down as the kept buffer and the later
/// stages ([`page`], the output conversion) shrink. The set owns its own
/// workspace charge and drops (releasing it) at the end of the pass.
fn retain_distinct(
    graph: &ExecGraph<'_>,
    class: &ProjectionClass<'_>,
    merged: Vec<Row>,
) -> Result<(Vec<Row>, relate_core::tokens::WorkspaceCharge)> {
    let retained_rows = crate::workspace::charge_row_buffer(&merged)?;
    let mut distinct = DistinctPass::new(class);
    let mut kept = Vec::new();
    for row in merged {
        if distinct.keep(graph, &row)? {
            kept.push(row);
        }
    }
    drop(distinct);
    Ok((kept, retained_rows))
}

/// The merged rows and how they were landed — the rows beside the one
/// shape that says what the landing container already did to them. The
/// funnels report the container they built, so the tail reads what was
/// landed rather than re-deriving the window decision from the
/// classification.
pub(crate) struct Landed {
    pub(crate) rows: Vec<Row>,
    pub(crate) tail: LandedTail,
}

/// What the landing container already did: one shape, not three flags.
/// [`Window`][Self::Window] selected its ordered page from the unfiltered
/// stream (the tail sorts nothing — the window's `skip + limit` rows are
/// still cut by the tail's [`page`]), [`Page`][Self::Page] answered the
/// whole tail on the stream (DISTINCT, page and the trailing WHERE), and
/// [`Collected`][Self::Collected] holds a table the tail still sorts,
/// deduplicates and pages. `distinct_ran` says the collected container's
/// first-seen gate already deduped that table.
#[derive(Clone, Copy)]
pub(crate) enum LandedTail {
    Collected { distinct_ran: bool },
    Window,
    Page,
}

impl LandedTail {
    /// The untouched collected tail — what a container that ran no stage
    /// (the aggregate path, the count-only sink) reports.
    pub(crate) fn untouched() -> Self {
        Self::Collected {
            distinct_ran: false,
        }
    }

    /// The landing the seam carried, with its rows.
    pub(crate) fn into_landed(self, rows: Vec<Row>) -> Landed {
        Landed { rows, tail: self }
    }
}

impl Landed {
    /// Run the landing's remaining stages over its own rows — the pair
    /// stays attached, so one landing's tail never runs on another
    /// landing's rows.
    pub(crate) fn finish(
        self,
        graph: &ExecGraph<'_>,
        params: &BTreeMap<String, Value>,
        class: &ProjectionClass<'_>,
        bounds: PageBounds,
    ) -> Result<Vec<Row>> {
        let Landed { rows, tail } = self;
        tail.finish(graph, params, class, rows, bounds)
    }
}

/// The projection's output container — the one landing policy every merge
/// path reads, as one of the containers the clause can have. They are
/// disjoint states, so they are variants rather than nullable fields: the
/// window shape carries a heap, the collected shape the shared retaining
/// rows, and the page shape its own [`PageState`] over those rows. The
/// container answers its own [`Self::tail`], so no caller translates
/// variant to tail.
pub(crate) enum OutputContainer<'k, 'a> {
    /// ADR-0463's retaining rows ([`LandedRows`]): the projection's whole
    /// output, held for the finish tail.
    Collected { rows: LandedRows<'k> },
    /// ADR-0555 §3's bounded top-k window: at most `skip + limit` entries,
    /// charged as entries enter and leave. The selection sees the
    /// **unfiltered** stream: the `WITH` grammar is `WITH projection
    /// [ORDER BY] [SKIP] [LIMIT] [WHERE]`, so the trailing `WHERE` filters
    /// the page this window produced, and [`LandedTail::finish`] applies it
    /// to the window's output. Filtering here would take the page from the
    /// filtered rows instead.
    Window { window: TopKWindow<'k, 'a> },
    /// The pass-through's output page (ADR-0660), its stage order owned by
    /// [`PageState`]. [`Self::is_full`] answers the page-full stop: the
    /// funnel stops the producer there, so it generates nothing further.
    Page { page: PageState<'k, 'a> },
}

/// The retaining rows the two streamed landing shapes share: ADR-0463's
/// charged output buffer and the optional streamed-`DISTINCT` first-seen
/// pass, with the one gate/keep spelling both shapes call.
pub(crate) struct LandedRows<'k> {
    retained: RetainedTotal,
    out: Vec<Row>,
    distinct: Option<DistinctPass<'k>>,
}

impl<'k> LandedRows<'k> {
    /// The empty retaining rows for `class`, its gate included when the
    /// shape's `DISTINCT` streams.
    fn open<'a>(graph: &ExecGraph<'_>, class: &'k ProjectionClass<'a>) -> Result<Self>
    where
        'a: 'k,
    {
        Ok(Self {
            retained: graph.retain_bytes(0)?,
            out: Vec::new(),
            distinct: class.streams_distinct().then(|| DistinctPass::new(class)),
        })
    }

    /// Whether the row's key columns are new — the one first-seen probe,
    /// read by [`Self::land`] and [`Self::adopt`].
    fn gate(&mut self, graph: &ExecGraph<'_>, row: &Row) -> Result<bool> {
        match self.distinct.as_mut() {
            Some(distinct) => distinct.keep(graph, row),
            None => Ok(true),
        }
    }

    /// Charge and keep one gated row.
    fn keep(&mut self, graph: &ExecGraph<'_>, row: Row) -> Result<()> {
        self.retained.add_row(graph, row.values())?;
        self.out.push(row);
        Ok(())
    }

    /// One row built as the output lands: gated, then kept.
    fn land(&mut self, graph: &ExecGraph<'_>, row: Row) -> Result<()> {
        if self.gate(graph, &row)? {
            self.keep(graph, row)?;
        }
        Ok(())
    }

    /// Take the caller's vector as the output — the pass-through's input
    /// vector *is* its output vector (ADR-0555): its buffer is charged for
    /// the pass, the gate compacts it in place, and the survivors are the
    /// answer. Adoption is a fresh container's: an already-landed one would
    /// drop the rows it holds, so the caller's contract is stated here.
    fn adopt(mut self, graph: &ExecGraph<'_>, rows: Vec<Row>) -> Result<Vec<Row>> {
        debug_assert!(
            self.out.is_empty(),
            "adoption takes a fresh retaining container"
        );
        let mut rows = rows;
        if self.distinct.is_some() {
            // The whole vector is held while the gate compacts it; the
            // charge is released as it shrinks (the `retain_distinct`
            // pre-pass shape).
            let mut held = crate::workspace::charge_row_buffer(&rows)?;
            let mut kept = 0;
            for index in 0..rows.len() {
                if self.gate(graph, &rows[index])? {
                    rows.swap(kept, index);
                    kept += 1;
                }
            }
            rows.truncate(kept);
            held.resize_with(|| crate::workspace::row_buffer_bytes(&rows))?;
            drop(held);
        }
        // Every survivor is billed through `retained`, once, whichever path
        // produced it.
        for row in &rows {
            self.retained.add_row(graph, row.values())?;
        }
        Ok(rows)
    }

    fn finish(self) -> Vec<Row> {
        self.out
    }

    fn retained_bytes(&self) -> u64 {
        self.retained.total()
            + self
                .distinct
                .as_ref()
                .map_or(0, DistinctPass::retained_bytes)
    }
}

/// The pass-through page's own state (ADR-0660): the shared retaining rows
/// beside the running [`PageCursor`] and the trailing filter, with its
/// per-row landing — the stage order the `WITH` grammar fixes: `DISTINCT`,
/// `SKIP`, `LIMIT`, then the trailing `WHERE` on the answered page.
pub(crate) struct PageState<'k, 'a> {
    rows: LandedRows<'k>,
    predicate: Option<&'a Expr>,
    page: PageCursor,
}

impl PageState<'_, '_> {
    fn land(
        &mut self,
        graph: &ExecGraph<'_>,
        params: &BTreeMap<String, Value>,
        row: Row,
    ) -> Result<()> {
        if !self.rows.gate(graph, &row)? {
            return Ok(());
        }
        if !self.page.admit() {
            return Ok(());
        }
        if plain_where_holds(graph, &row, self.predicate, params)? {
            self.rows.keep(graph, row)?;
        }
        Ok(())
    }

    fn is_full(&self) -> bool {
        self.page.is_exhausted()
    }

    fn finish(self) -> Vec<Row> {
        self.rows.finish()
    }

    fn retained_bytes(&self) -> u64 {
        self.rows.retained_bytes()
    }
}

impl<'k, 'a> OutputContainer<'k, 'a> {
    /// The bounded top-k window at `keep` entries — the typed constructor
    /// for a caller that has already resolved [`ProjectionClass::window_keep`],
    /// so no caller re-derives the window decision to check the variant it
    /// asked for.
    pub(crate) fn window(class: &'k ProjectionClass<'a>, keep: std::num::NonZeroUsize) -> Self {
        OutputContainer::Window {
            window: TopKWindow::new(&class.order_keys, keep),
        }
    }

    /// The container for the clause's `bounds`: the bounded window when the
    /// ordered shape holds, the retaining rows otherwise. The window
    /// decision and the parked-key plan are the classification's own — read
    /// from it, never handed back as loose parameters.
    pub(crate) fn open(
        graph: &ExecGraph<'_>,
        class: &'k ProjectionClass<'a>,
        bounds: PageBounds,
    ) -> Result<Self>
    where
        'a: 'k,
    {
        Ok(match class.window_keep(bounds) {
            Some(keep) => Self::window(class, keep),
            None => OutputContainer::Collected {
                rows: LandedRows::open(graph, class)?,
            },
        })
    }

    /// The pass-through page's container: the shared retaining rows, the
    /// page's own counters and the trailing filter, read from the
    /// classification and the clause's bounds exactly as [`Self::open`]
    /// reads the others.
    pub(crate) fn page(
        graph: &ExecGraph<'_>,
        class: &'k ProjectionClass<'a>,
        bounds: PageBounds,
    ) -> Result<Self>
    where
        'a: 'k,
    {
        Ok(OutputContainer::Page {
            page: PageState {
                rows: LandedRows::open(graph, class)?,
                predicate: class.plain_where(),
                page: PageCursor::new(bounds),
            },
        })
    }

    /// The feed funnel's container: the pass-through page for a produced
    /// feed whose clause filters, dedups or pages, the window or retaining
    /// rows otherwise. Held-ness and the page rule are stated here, once,
    /// where the container is chosen.
    pub(crate) fn feed_landing(
        graph: &ExecGraph<'_>,
        class: &'k ProjectionClass<'a>,
        bounds: PageBounds,
        held: bool,
    ) -> Result<Self>
    where
        'a: 'k,
    {
        if !held && class.takes_page_container(bounds) {
            Self::page(graph, class, bounds)
        } else {
            Self::open(graph, class, bounds)
        }
    }

    /// What this container already did, as the landing the seam carries —
    /// the one variant-to-tail mapping, so no funnel re-derives it.
    pub(crate) fn tail(&self) -> LandedTail {
        match self {
            OutputContainer::Window { .. } => LandedTail::Window,
            OutputContainer::Page { .. } => LandedTail::Page,
            OutputContainer::Collected { rows } => LandedTail::Collected {
                distinct_ran: rows.distinct.is_some(),
            },
        }
    }

    /// One built output row lands in its container. `graph` and `params`
    /// are the row's evaluation environment — one input for every arm.
    /// [`Self::is_full`] answers the page-full stop afterwards, so no arm
    /// returns it beside the landing.
    pub(crate) fn land(
        &mut self,
        graph: &ExecGraph<'_>,
        params: &BTreeMap<String, Value>,
        row: Row,
    ) -> Result<()> {
        match self {
            // The selection sees every row: the trailing WHERE filters the
            // window's output, in the finish tail.
            OutputContainer::Window { window } => window.push(graph, row)?,
            OutputContainer::Collected { rows } => rows.land(graph, row)?,
            OutputContainer::Page { page } => page.land(graph, params, row)?,
        }
        Ok(())
    }

    /// Whether the page is satisfied — the fact the bounded sink reads to
    /// stop projecting rows it would not keep (the page's own state, never a
    /// latch beside it).
    pub(crate) fn is_full(&self) -> bool {
        match self {
            OutputContainer::Page { page } => page.is_full(),
            OutputContainer::Window { .. } | OutputContainer::Collected { .. } => false,
        }
    }

    /// The container's rows: the window's best-first `skip + limit`, or the
    /// retaining buffer's output. Neither arm can fail — the window's and
    /// the rows' charges are released with them, the buffer moves out.
    pub(crate) fn finish(self) -> Vec<Row> {
        match self {
            OutputContainer::Window { window } => window.finish(),
            OutputContainer::Collected { rows } => rows.finish(),
            OutputContainer::Page { page } => page.finish(),
        }
    }

    /// The live container's charged bytes — the terminal-retention figure
    /// the PROFILE tail reports for a sink terminal.
    pub(crate) fn retained_bytes(&self) -> u64 {
        match self {
            OutputContainer::Window { window } => window.retained_bytes(),
            OutputContainer::Collected { rows } => rows.retained_bytes(),
            OutputContainer::Page { page } => page.retained_bytes(),
        }
    }

    /// Take rows the caller already built as this container's contents —
    /// only a container [`Self::open`] just built is adoptable (a landed one
    /// would drop the rows it holds). The retaining shape adopts the vector
    /// itself (ADR-0555, no second buffer beside the input); the window
    /// folds every row into its heap. A page is never adopted: the sink that
    /// takes the page lands rows itself.
    fn adopt(self, graph: &ExecGraph<'_>, rows: Vec<Row>) -> Result<Vec<Row>> {
        match self {
            OutputContainer::Collected { rows: charged } => charged.adopt(graph, rows),
            OutputContainer::Window { mut window } => {
                for row in rows {
                    window.push(graph, row)?;
                }
                Ok(window.finish())
            }
            OutputContainer::Page { .. } => Err(relate_core::Error::internal(
                "the pass-through page was handed a built table to adopt",
            )),
        }
    }
}

/// The pass-through's landing — the one entry point that owns the container
/// choice for an input the caller already built: the rows fold into the
/// bounded window when the ordered shape holds, and are adopted as the
/// collected container otherwise. There the input vector *is* the output
/// vector, so it is charged in place through that container's own
/// reservation (ADR-0463) and returned without a second buffer beside it
/// (ADR-0555). The sibling of [`land_feed`], for the shape whose container
/// is its input.
pub(crate) fn adopt_rows(
    graph: &ExecGraph<'_>,
    class: &ProjectionClass<'_>,
    bounds: PageBounds,
    rows: Vec<Row>,
) -> Result<Landed> {
    let container = OutputContainer::open(graph, class, bounds)?;
    let tail = container.tail();
    Ok(tail.into_landed(container.adopt(graph, rows)?))
}

/// The one merge funnel the two feed-shaped row-assembly paths end in:
/// open the landing container from the classification and the clause's
/// bounds (ADR-0463's retaining `Vec`, the ordered window's bounded heap, or
/// the pass-through's output page), pull the input's batches through it, and
/// hand back the container's rows. The path supplies only its own row
/// assembly. `aggregate_rows` does not land through a container — its charge
/// lives in the aggregation's own emission loop — and the pass-through seeds
/// its container instead ([`OutputContainer::adopt`]).
pub(crate) fn land_feed(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    class: &ProjectionClass<'_>,
    bounds: PageBounds,
    mut feed: RowStream<'_>,
    mut build: impl FnMut(Row) -> Result<Row>,
) -> Result<Landed> {
    let mut landing = OutputContainer::feed_landing(graph, class, bounds, feed.is_held())?;
    'batches: while let Some(batch) = feed.next_batch()? {
        for row in batch {
            // The row-assembly closure builds the output row only; the
            // clause's parked sort keys are the surface every merge path
            // shares, so the funnel parks them — one spelling of the park
            // step, and it runs after the closure has dropped the source
            // row.
            let mut row = build(row)?;
            park_order_keys(&class.order_keys, &mut row, |expr, scope| {
                eval(graph, scope, expr, params)
            })?;
            landing.land(graph, params, row)?;
            if landing.is_full() {
                // The page is satisfied: stop the producer, so it generates
                // nothing further.
                feed.stop()?;
                break 'batches;
            }
        }
    }
    let tail = landing.tail();
    Ok(tail.into_landed(landing.finish()))
}

impl LandedTail {
    /// The projection's finish tail, shared by every merge path — the
    /// pipeline's last stage, one arm per shape so the stages cannot
    /// disagree with the container the rows came out of: the page container
    /// answered the whole tail on the stream (only the result extraction is
    /// left), the window arrives ordered and bounded (page, then the
    /// trailing WHERE), and the collected table still sorts (its top-k
    /// cutoff included), deduplicates, pages and filters. The eager stop
    /// already paged on its own resolved bounds (`prepare_input`), so only
    /// the other shapes page here.
    fn finish(
        self,
        graph: &ExecGraph<'_>,
        params: &BTreeMap<String, Value>,
        class: &ProjectionClass<'_>,
        rows: Vec<Row>,
        bounds: PageBounds,
    ) -> Result<Vec<Row>> {
        let (windowed, streamed_distinct) = match self {
            LandedTail::Page => return output_rows(class, rows, None),
            LandedTail::Window => (true, false),
            LandedTail::Collected { distinct_ran } => (false, distinct_ran),
        };
        let ProjectionFields { distinct, .. } = class.fields;
        let page_bounds = if class.eager_paged {
            PageBounds::unpaged()
        } else {
            bounds
        };
        // The window shape's rows arrive already ordered and bounded by the
        // heap, so only the shapes that merge unordered rows sort here — on
        // the one cutoff the window decision reads too
        // ([`ProjectionClass::sort_cut`]).
        let mut merged = rows;
        if !windowed {
            sort_rows_by_parked_keys(&class.order_keys, &mut merged, class.sort_cut(bounds))?;
        }
        // The DISTINCT pass's row-buffer charge: Option-held from
        // [`retain_distinct`] — it exists only on the shapes the landing
        // funnel did not already dedup. Every stage that shrinks the table
        // re-measures it through [`TailRows::remeasure`], so one `Option` is
        // hand-sized once, not once per stage.
        let mut merged = if distinct && !streamed_distinct {
            let (kept, charge) = retain_distinct(graph, class, merged)?;
            TailRows {
                rows: kept,
                charge: Some(charge),
            }
        } else {
            TailRows {
                rows: merged,
                charge: None,
            }
        };
        merged.remeasure()?;
        merged.rows = page(merged.rows, page_bounds);
        merged.remeasure()?;
        // The grammar's one trailing WHERE, applied to the page the
        // projection body produced — `WITH projection [ORDER BY] [SKIP]
        // [LIMIT] [WHERE]` — on every shape the tail owns: the window's
        // selected rows and the collected shape's paged rows alike. The
        // mode is one fact, so the stage is one: the HAVING form reads
        // the per-group value the aggregation parked under its hidden
        // column (not-true drops, a non-boolean is the typed rejection
        // 22N01 — the one predicate policy, ADR-0458 §7), the plain form
        // evaluates on the row, and no WHERE at all passes the page
        // through untouched.
        merged.rows = match class.where_mode.as_ref() {
            Some(WhereMode::Having(plan)) => filter_parked_having(plan.parked(), merged.rows)?,
            // The plain predicate's one canonical home is the class's
            // own `plain_where()` — this arm names which mode it serves,
            // never a second derivation of the predicate.
            Some(WhereMode::Plain(_)) => {
                filter_rows_where_clause(graph, params, class.plain_where(), merged.rows)?
            }
            None => merged.rows,
        };
        merged.remeasure()?;
        output_rows(class, merged.rows, merged.charge.as_mut())
    }
}

/// The HAVING page filter: keep the rows whose parked per-group value
/// holds, dropping the hidden column from every row on the way out — the
/// survivors stop carrying it, the dropped rows release with it. The one
/// predicate policy (ADR-0458 §7) applies here: false and null drop, a
/// non-boolean is the typed rejection live raises (22N01), a list/path
/// converts.
fn filter_parked_having(parked: &str, rows: Vec<Row>) -> Result<Vec<Row>> {
    let mut out = Vec::with_capacity(rows.len());
    for mut row in rows {
        let keeps = row.shift_remove(parked).ok_or_else(|| {
            relate_core::Error::internal(
                "an aggregated row reached the tail without its parked HAVING value",
            )
        })?;
        if crate::eval::predicate_value_holds(&keeps)? {
            out.push(row);
        }
    }
    Ok(out)
}

/// The finish tail's table beside the row-buffer reservation it holds: the
/// stages that shrink the table re-measure through [`Self::remeasure`], so
/// the `Option<WorkspaceCharge>` bookkeeping lives once.
struct TailRows {
    rows: Vec<Row>,
    charge: Option<relate_core::tokens::WorkspaceCharge>,
}

impl TailRows {
    /// Re-price the held table, when this shape reserved a row buffer.
    fn remeasure(&mut self) -> Result<()> {
        remeasure_rows(self.charge.as_mut(), &self.rows)
    }
}

/// Re-price a row buffer's reservation, when the shape holds one — the one
/// spelling of the tail's `Option<WorkspaceCharge>` re-measure, read by
/// [`TailRows::remeasure`] and the output conversion.
fn remeasure_rows(
    charge: Option<&mut relate_core::tokens::WorkspaceCharge>,
    rows: &[Row],
) -> Result<()> {
    if let Some(charge) = charge {
        charge.resize_with(|| crate::workspace::row_buffer_bytes(rows))?;
    }
    Ok(())
}

/// How the survivors become output rows — the tail's second job, after the
/// selection above. A `*` row keeps exactly the incoming bindings and the
/// explicit items, so its parked sort keys come off; every other shape
/// keeps its declared columns, moved out of each *consumed* row so a
/// source row drops as its cells leave it and the output is built with no
/// second container beside the input (ADR-0555). A miss is the typed
/// internal break every other materialization site reports
/// (`Missing::Error`), never a panic.
fn output_rows(
    class: &ProjectionClass<'_>,
    mut merged: Vec<Row>,
    charge: Option<&mut relate_core::tokens::WorkspaceCharge>,
) -> Result<Vec<Row>> {
    if class.shape.star() {
        unpark_order_keys(&class.order_keys, &mut merged);
        return Ok(merged);
    }
    let names = RowNames::new(&class.out_names);
    let mut out: Vec<Row> = Vec::with_capacity(merged.len());
    for mut row in merged {
        let values = take_declared_columns(&mut row, &class.out_names, Missing::Error)?;
        out.push(Row::from_names(&names, values));
    }
    remeasure_rows(charge, &out)?;
    Ok(out)
}
