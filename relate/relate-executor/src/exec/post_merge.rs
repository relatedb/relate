//! The projection's post-merge pipeline: the landing container every
//! merge path hands its built rows to, the DISTINCT first-seen pass, and
//! the finish tail that pages the merged table — split from `project.rs`
//! (the same move that produced `order.rs`). The family now reads as
//! three: the merge paths stay in `project.rs`, the classification lives
//! in `classification.rs`, and the container and the tail live here.

use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::Expr;

use crate::distinct_set::DistinctSet;
use crate::eval::eval;
use crate::eval::{filter_rows_where, where_holds};
use crate::overlay::{ExecGraph, RetainedTotal};
use crate::row::{Row, RowNames};

use super::classification::ProjectionClass;
use super::feed::RowStream;
use super::order::{
    page, park_order_keys, sort_rows_by_parked_keys, unpark_order_keys, PageBounds,
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
pub(crate) struct DistinctPass<'a> {
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
    let retained_rows = relate_core::tokens::WorkspaceCharge::retain_with(|| {
        crate::workspace::row_buffer_bytes(&merged)
    })?;
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

/// The merged rows and how they were landed: the window shape's rows arrive
/// ordered and bounded by the heap, the page shape's are already filtered,
/// deduplicated and paged, and every other shape's are an unordered table
/// the tail still sorts and filters. The funnels report the container they
/// built, so the tail reads what was landed rather than re-deriving the
/// window decision from the classification.
pub(crate) struct Landed {
    pub(crate) rows: Vec<Row>,
    pub(crate) windowed: bool,
    /// The `DISTINCT` first-seen pass already ran on the landing stream
    /// ([`DistinctPass`]), so the finish tail must not run it again.
    pub(crate) streamed_distinct: bool,
    /// The landing funnel already applied the plain `WHERE` — an explicit
    /// fact the tail reads, never re-derived from the distinct flag above
    /// (a shape that filters without distincting re-filtering, or one that
    /// distincts without filtering skipping a filter it never ran, is a
    /// wiring the type now states).
    pub(crate) funnel_filtered_where: bool,
    /// The landing container was the output page ([`OutputContainer::Page`]):
    /// it filtered, deduplicated and paged each row as it arrived, so the
    /// finish tail only converts the rows it answered.
    pub(crate) streamed_page: bool,
}

/// [`Landed`]'s facts without its rows — the copy a dispatcher threads
/// between clauses so the projection adopts the landing it was handed.
#[derive(Clone, Copy, Default)]
pub(crate) struct LandedTail {
    pub(crate) windowed: bool,
    pub(crate) streamed_distinct: bool,
    pub(crate) funnel_filtered_where: bool,
    pub(crate) streamed_page: bool,
}

impl LandedTail {
    /// The landing the seam carried, with its rows.
    pub(crate) fn into_landed(self, rows: Vec<Row>) -> Landed {
        Landed {
            rows,
            windowed: self.windowed,
            streamed_distinct: self.streamed_distinct,
            funnel_filtered_where: self.funnel_filtered_where,
            streamed_page: self.streamed_page,
        }
    }
}

/// What a landing container wants next: [`Land::More`] for another row,
/// [`Land::Full`] when its output page is complete — the funnel then stops
/// the producer instead of pulling a row nobody will answer with.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Land {
    More,
    Full,
}

/// The projection's output container — the one landing policy every merge
/// path reads, as one of the containers the clause can have. They are
/// disjoint states, so they are variants rather than nullable fields: the
/// window shape carries a heap and no `Vec`, the collected and page shapes a
/// `Vec` and no heap, and no arm constructs another's state.
pub(crate) enum OutputContainer<'k, 'a> {
    /// ADR-0463's retaining `Vec`: each landed row grows a running charge
    /// and the buffer holds the projection's whole output.
    Collected {
        retained: RetainedTotal,
        out: Vec<Row>,
    },
    /// ADR-0555 §3's bounded top-k window: at most `skip + limit` entries,
    /// charged as entries enter and leave. The plain WHERE filters each row
    /// *here*, as it arrives, because the selection must see the filtered
    /// stream — the collected shape filters after the merge, in
    /// [`finish_projection`]. Both spellings reach one clause: a `WHERE`
    /// written before the page (`WITH … WHERE … ORDER BY … LIMIT`) is
    /// planned as its own `WHERE` projection with no parked keys, but one
    /// written after it (`WITH … ORDER BY … LIMIT … WHERE`) stays in the
    /// clause that carries `order`/`skip`/`limit`, so the window is handed
    /// a plain predicate and must apply it. The container carries only what
    /// it owns — the heap and the clause's resolved predicate; the
    /// evaluation environment arrives with each row, in
    /// [`OutputContainer::land`]'s arguments.
    Window {
        window: TopKWindow<'k, 'a>,
        predicate: Option<&'a Expr>,
    },
    /// The pass-through's output page (ADR-0660): the plain WHERE, the
    /// `DISTINCT` first-seen gate and the `SKIP`/`LIMIT` page apply to each
    /// built row as it arrives, so the container holds only the answered
    /// page (and, on a `DISTINCT` shape, the charged seen-set), never the
    /// input population. [`Land::Full`] answers the page-full stop: the
    /// funnel stops the producer there, so it generates nothing further.
    Page {
        retained: RetainedTotal,
        out: Vec<Row>,
        distinct: Option<DistinctPass<'k>>,
        predicate: Option<&'a Expr>,
        skip_left: usize,
        limit_left: Option<usize>,
    },
}

impl<'k, 'a> OutputContainer<'k, 'a> {
    /// The container for the clause's `bounds`: the bounded window when the
    /// ordered shape holds, the retaining `Vec` otherwise. The window
    /// decision, the parked-key plan and the plain WHERE are the
    /// classification's own — read from it, never handed back as loose
    /// parameters.
    pub(crate) fn open(
        graph: &ExecGraph<'_>,
        class: &'k ProjectionClass<'a>,
        bounds: PageBounds,
    ) -> Result<Self> {
        Ok(match class.window_keep(bounds) {
            Some(keep) => OutputContainer::Window {
                window: TopKWindow::new(&class.order_keys, keep),
                predicate: class.plain_where(),
            },
            None => OutputContainer::Collected {
                retained: graph.retain_bytes(0)?,
                out: Vec::new(),
            },
        })
    }

    /// The pass-through page's container: the filter, the charged seen-set
    /// and the page's own counters, read from the classification and the
    /// clause's bounds exactly as [`Self::open`] reads the others.
    pub(crate) fn page(
        graph: &ExecGraph<'_>,
        class: &'k ProjectionClass<'a>,
        bounds: PageBounds,
    ) -> Result<Self>
    where
        'a: 'k,
    {
        Ok(OutputContainer::Page {
            retained: graph.retain_bytes(0)?,
            out: Vec::new(),
            distinct: class.streams_distinct().then(|| DistinctPass::new(class)),
            predicate: class.plain_where(),
            skip_left: bounds.skip,
            limit_left: bounds.limit,
        })
    }

    /// One built output row lands in its container. `graph` and `params`
    /// are the row's evaluation environment — one input for every arm.
    pub(crate) fn land(
        &mut self,
        graph: &ExecGraph<'_>,
        params: &BTreeMap<String, Value>,
        row: Row,
    ) -> Result<Land> {
        match self {
            OutputContainer::Window { window, predicate } => {
                if let Some(predicate) = predicate {
                    if !where_holds(graph, params, &row, predicate)? {
                        return Ok(Land::More);
                    }
                }
                window.push(graph, row)?;
            }
            OutputContainer::Collected { retained, out } => {
                retained.add_row(graph, row.values())?;
                out.push(row);
            }
            OutputContainer::Page {
                retained,
                out,
                distinct,
                predicate,
                skip_left,
                limit_left,
            } => {
                if let Some(predicate) = predicate {
                    if !where_holds(graph, params, &row, predicate)? {
                        return Ok(Land::More);
                    }
                }
                if let Some(distinct) = distinct.as_mut() {
                    if !distinct.keep(graph, &row)? {
                        return Ok(Land::More);
                    }
                }
                if *skip_left > 0 {
                    *skip_left -= 1;
                    return Ok(Land::More);
                }
                retained.add_row(graph, row.values())?;
                out.push(row);
                if let Some(left) = limit_left.as_mut() {
                    *left -= 1;
                    if *left == 0 {
                        return Ok(Land::Full);
                    }
                }
            }
        }
        Ok(Land::More)
    }

    /// Whether this container is the bounded window — the fact the finish
    /// tail reads from what the funnel landed rather than re-deriving it.
    pub(crate) fn is_window(&self) -> bool {
        matches!(self, OutputContainer::Window { .. })
    }

    /// Whether the page is satisfied — the fact the bounded sink reads to
    /// stop projecting rows it would not keep (the page's own state, never a
    /// latch beside it).
    pub(crate) fn is_full(&self) -> bool {
        matches!(
            self,
            OutputContainer::Page {
                limit_left: Some(0),
                ..
            }
        )
    }

    /// Whether this container is the pass-through page — the fact the finish
    /// tail reads to skip its own filter, DISTINCT and page.
    fn is_page(&self) -> bool {
        matches!(self, OutputContainer::Page { .. })
    }

    /// The container's rows: the window's best-first `skip + limit`, or the
    /// `Vec`'s whole output. Neither arm can fail — the window's and the
    /// page's charges are released with them, the `Vec` moves out.
    pub(crate) fn finish(self) -> Vec<Row> {
        match self {
            OutputContainer::Window { window, .. } => window.finish(),
            OutputContainer::Collected { out, .. } => out,
            OutputContainer::Page { out, .. } => out,
        }
    }

    /// The live container's charged bytes — the terminal-retention figure
    /// the PROFILE tail reports for a sink terminal.
    pub(crate) fn retained_bytes(&self) -> u64 {
        match self {
            OutputContainer::Window { window, .. } => window.retained_bytes(),
            OutputContainer::Collected { retained, .. } => retained.total(),
            OutputContainer::Page {
                retained, distinct, ..
            } => retained.total() + distinct.as_ref().map_or(0, DistinctPass::retained_bytes),
        }
    }

    /// Take rows the caller already built as this container's contents —
    /// the pass-through's input vector *is* its output vector. The
    /// collected arm adopts it through this container's own reservation
    /// (ADR-0463), the same running charge [`Self::land`] grows, never a
    /// second buffer beside the input (ADR-0555) and never a second charge
    /// owner; every other container folds each row through the one landing
    /// policy, predicate and page included.
    fn adopt(
        mut self,
        graph: &ExecGraph<'_>,
        params: &BTreeMap<String, Value>,
        rows: Vec<Row>,
    ) -> Result<Vec<Row>> {
        if let OutputContainer::Collected { retained, .. } = &mut self {
            for row in &rows {
                retained.add_row(graph, row.values())?;
            }
            return Ok(rows);
        }
        for row in rows {
            if matches!(self.land(graph, params, row)?, Land::Full) {
                break;
            }
        }
        Ok(self.finish())
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
    params: &BTreeMap<String, Value>,
    class: &ProjectionClass<'_>,
    bounds: PageBounds,
    rows: Vec<Row>,
) -> Result<Landed> {
    let container = OutputContainer::open(graph, class, bounds)?;
    let windowed = container.is_window();
    Ok(Landed {
        rows: container.adopt(graph, params, rows)?,
        windowed,
        streamed_distinct: false,
        funnel_filtered_where: false,
        streamed_page: false,
    })
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
    // The pass-through page's container (ADR-0660): a *produced* feed whose
    // clause filters, dedups or pages as rows arrive, so its producer must
    // stop at the page. A held feed already took the eager page and pages
    // in the finish tail, so it lands through the collected container
    // instead. A plain `WHERE` counts: it filters per row on the stream
    // (the `pass` disposition), never by landing the rows it is about to
    // drop into the collected container.
    let page = !feed.is_held()
        && class.streams_pass()
        && (class.streams_distinct()
            || class.plain_where().is_some()
            || bounds.skip > 0
            || bounds.limit.is_some());
    let mut landing = if page {
        OutputContainer::page(graph, class, bounds)?
    } else {
        OutputContainer::open(graph, class, bounds)?
    };
    let windowed = landing.is_window();
    let streamed_page = landing.is_page();
    // The `DISTINCT` first-seen pass is the landing gate on this shape
    // ([`ProjectionClass::streams_distinct`]): a row reaches the container
    // only when its key columns are new, so the pass retains its seen-set
    // and the answer, never the input. The clause's plain `WHERE` runs
    // *before* it — the order the finish tail applies — because a
    // predicate is not required to be constant across a key group (a
    // nondeterministic one never is), so the funnel filters here whenever
    // it gates and the tail then skips its own filter (reading the
    // explicit `funnel_filtered_where` fact, never re-deriving it from the
    // distinct pass). The page container owns its own
    // predicate-then-gate order.
    let mut distinct = (!page && class.streams_distinct()).then(|| DistinctPass::new(class));
    let gated_where = if distinct.is_some() {
        class.plain_where()
    } else {
        None
    };
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
            if let Some(predicate) = gated_where {
                if !where_holds(graph, params, &row, predicate)? {
                    continue;
                }
            }
            if let Some(distinct) = distinct.as_mut() {
                if !distinct.keep(graph, &row)? {
                    continue;
                }
            }
            if matches!(landing.land(graph, params, row)?, Land::Full) {
                // The page is satisfied: stop the producer, so it generates
                // nothing further.
                feed.stop()?;
                break 'batches;
            }
        }
    }
    Ok(Landed {
        rows: landing.finish(),
        windowed,
        streamed_distinct: distinct.is_some(),
        funnel_filtered_where: gated_where.is_some(),
        streamed_page,
    })
}

/// The projection's finish tail, shared by every merge path — the
/// pipeline's last stage: the plain WHERE filter over the merged rows (the
/// collected shape only; the window shape filtered as each row landed),
/// then the paging stages (the parked-key sort with its top-k selection
/// for the shapes that merge unordered rows, the DISTINCT first-seen pass
/// with its row-buffer charge re-measured at each shrink, the page
/// itself), then the result extraction. The eager stop already paged on
/// its own resolved bounds (`prepare_input`), and the page container applied
/// its own filter, DISTINCT and page as rows arrived — so the tail pages
/// only on the shapes that landed a table.
pub(crate) fn finish_projection(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    class: &ProjectionClass<'_>,
    landed: Landed,
    bounds: PageBounds,
) -> Result<Vec<Row>> {
    // The window shape is the container the merge path landed — read from
    // what was landed rather than re-derived here, so the WHERE-filter
    // policy cannot disagree with the container the rows came out of. A
    // distinct the landing funnel already applied is not run again.
    let Landed {
        rows,
        windowed,
        streamed_distinct,
        funnel_filtered_where,
        streamed_page,
    } = landed;
    // The page container answered the whole tail on the stream; only the
    // result extraction is left.
    if streamed_page {
        return output_rows(class, rows, None);
    }
    let mut merged = rows;
    let ProjectionFields { distinct, .. } = class.fields;
    // The ordered-window shape filtered each row as it landed, before the
    // selection, and the landing funnel filtered before its own DISTINCT
    // gate — filtering again here would evaluate the predicate twice per
    // surviving row (a nondeterministic predicate would flake). The skip
    // reads the funnel's own filter fact, never the distinct flag.
    if !windowed && !funnel_filtered_where {
        if let Some(w) = class.plain_where() {
            merged = filter_rows_where(graph, params, w, merged)?;
        }
    }
    // The eager shape's feed already paged on these bounds; every other
    // shape pages here.
    let page_bounds = if class.eager_paged {
        PageBounds::unpaged()
    } else {
        bounds
    };
    // The window shape's rows arrive already ordered and bounded by the
    // heap, so only the shapes that merge unordered rows sort here — on the
    // one cutoff the window decision reads too ([`ProjectionClass::sort_cut`]).
    if !windowed {
        sort_rows_by_parked_keys(&class.order_keys, &mut merged, class.sort_cut(bounds))?;
    }
    // The DISTINCT pass's row-buffer charge: Option-held from
    // [`retain_distinct`] — it exists only on the shapes the landing funnel
    // did not already dedup, and every later re-measure (the page, the
    // output conversion) walks the held charge instead of re-checking the
    // mode beside an empty sentinel.
    let mut retained_rows = if distinct && !streamed_distinct {
        let (kept, charge) = retain_distinct(graph, class, merged)?;
        merged = kept;
        Some(charge)
    } else {
        None
    };
    if let Some(charge) = retained_rows.as_mut() {
        charge.resize_with(|| crate::workspace::row_buffer_bytes(&merged))?;
    }
    merged = page(merged, page_bounds);
    if let Some(charge) = retained_rows.as_mut() {
        charge.resize_with(|| crate::workspace::row_buffer_bytes(&merged))?;
    }
    let out = output_rows(class, merged, retained_rows.as_mut())?;
    drop(retained_rows);
    Ok(out)
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
    if let Some(charge) = charge {
        charge.resize_with(|| crate::workspace::row_buffer_bytes(&out))?;
    }
    Ok(out)
}
