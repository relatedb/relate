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

use super::aggregate::WhereMode;
use super::classification::ProjectionClass;
use super::order::{
    page, park_order_keys, sort_rows_by_parked_keys, unpark_order_keys, PageBounds,
};
use super::scope::{visible_keys, ProjectionFields};
use super::top_k::TopKWindow;
use super::RowFeed;
use super::{missing_column_error, read_declared_columns, take_declared_columns, Missing};

/// The columns a DISTINCT keys, borrowed when the projection is one
/// column wide (a one-element slice: no column clone, no row vector). The
/// key set itself is resolved by the caller ([`finish_projection`]):
/// visible keys for a `*` projection, output names otherwise. The arity-1
/// *keying* lives in [`DistinctSet::insert_row`] alone; this is only the
/// borrow.
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

/// The post-merge DISTINCT stage: first-seen keep over the merged rows,
/// keyed by `key_columns`, resolved once by the caller (a `*` row keys its
/// visible keys, a named projection its output columns). The returned
/// charge is the reservation for the **pre-pass** input: the whole
/// `merged` vector is alive for the duration of the pass, so the charge is
/// taken from it before any row is dropped, and the caller resizes it down
/// as the kept buffer and the later stages ([`page`], the output
/// conversion) shrink. The set owns its own workspace charge and drops
/// (releasing it) at the end of the pass.
fn retain_distinct(
    merged: Vec<Row>,
    key_columns: &[String],
) -> Result<(Vec<Row>, relate_core::tokens::WorkspaceCharge)> {
    let retained_rows = relate_core::tokens::WorkspaceCharge::retain_with(|| {
        crate::workspace::row_buffer_bytes(&merged)
    })?;
    let mut seen = DistinctSet::default();
    let mut kept = Vec::new();
    for row in merged {
        if seen.insert_row(&distinct_columns(&row, key_columns)?)? {
            kept.push(row);
        }
    }
    drop(seen);
    Ok((kept, retained_rows))
}

/// The merged rows and how they were landed: the window shape's rows arrive
/// ordered and bounded by the heap, every other shape's are an unordered
/// table the tail still sorts and filters. The funnels report the container
/// they built, so the tail reads what was landed rather than re-deriving
/// the window decision from the classification.
pub(crate) struct Landed {
    pub(crate) rows: Vec<Row>,
    pub(crate) windowed: bool,
}

/// The projection's output container — the one landing policy every merge
/// path reads, as one of the two containers the clause can have. They are
/// disjoint states, so they are variants rather than nullable fields: the
/// window shape carries a heap and no `Vec`, the collected shape a `Vec`
/// and no heap, and neither constructs the other's state.
enum OutputContainer<'k, 'a> {
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
}

impl<'k, 'a> OutputContainer<'k, 'a> {
    /// The container for the clause's `bounds`: the bounded window when the
    /// ordered shape holds, the retaining `Vec` otherwise. The window
    /// decision, the parked-key plan and the plain WHERE are the
    /// classification's own — read from it, never handed back as loose
    /// parameters.
    fn open(
        graph: &ExecGraph<'_>,
        class: &'k ProjectionClass<'a>,
        bounds: PageBounds,
    ) -> Result<Self> {
        Ok(match class.window_keep(bounds) {
            Some(keep) => OutputContainer::Window {
                window: TopKWindow::new(&class.order_keys, keep),
                predicate: class.where_mode.and_then(WhereMode::plain),
            },
            None => OutputContainer::Collected {
                retained: graph.retain_bytes(0)?,
                out: Vec::new(),
            },
        })
    }

    /// One built output row lands in its container. `graph` and `params`
    /// are the row's evaluation environment — one input for both arms.
    fn land(
        &mut self,
        graph: &ExecGraph<'_>,
        params: &BTreeMap<String, Value>,
        row: Row,
    ) -> Result<()> {
        match self {
            OutputContainer::Window { window, predicate } => {
                if let Some(predicate) = predicate {
                    if !where_holds(graph, params, &row, predicate)? {
                        return Ok(());
                    }
                }
                window.push(graph, row)
            }
            OutputContainer::Collected { retained, out } => {
                retained.add_row(graph, row.values())?;
                out.push(row);
                Ok(())
            }
        }
    }

    /// Whether this container is the bounded window — the fact the finish
    /// tail reads from what the funnel landed rather than re-deriving it.
    fn is_window(&self) -> bool {
        matches!(self, OutputContainer::Window { .. })
    }

    /// The container's rows: the window's best-first `skip + limit`, or the
    /// `Vec`'s whole output. Neither arm can fail — the window's charge is
    /// released with it, the `Vec` moves out.
    fn finish(self) -> Vec<Row> {
        match self {
            OutputContainer::Window { window, .. } => window.finish(),
            OutputContainer::Collected { out, .. } => out,
        }
    }

    /// Take rows the caller already built as this container's contents —
    /// the pass-through's input vector *is* its output vector. The
    /// collected arm adopts it through this container's own reservation
    /// (ADR-0463), the same running charge [`Self::land`] grows, never a
    /// second buffer beside the input (ADR-0555) and never a second charge
    /// owner; the window arm folds each row through the same landing policy
    /// every other merge path uses, predicate included.
    fn adopt(
        mut self,
        graph: &ExecGraph<'_>,
        params: &BTreeMap<String, Value>,
        rows: Vec<Row>,
    ) -> Result<Vec<Row>> {
        match self {
            OutputContainer::Collected { mut retained, .. } => {
                for row in &rows {
                    retained.add_row(graph, row.values())?;
                }
                Ok(rows)
            }
            OutputContainer::Window { .. } => {
                for row in rows {
                    self.land(graph, params, row)?;
                }
                Ok(self.finish())
            }
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
    })
}

/// The one merge funnel the two feed-shaped row-assembly paths end in:
/// open the landing container from the classification and the clause's
/// bounds (ADR-0463's retaining `Vec`, or the ordered window's bounded
/// heap), fold the feed through it, and hand back the container's rows.
/// The path supplies only its own row assembly. `aggregate_rows` does not
/// land through a container — its charge lives in the aggregation's own
/// emission loop — and the pass-through seeds its container instead
/// ([`OutputContainer::adopt`]).
pub(crate) fn land_feed(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    class: &ProjectionClass<'_>,
    bounds: PageBounds,
    feed: RowFeed<'_>,
    mut build: impl FnMut(Row) -> Result<Row>,
) -> Result<Landed> {
    let mut landing = OutputContainer::open(graph, class, bounds)?;
    let windowed = landing.is_window();
    feed.try_for_each(|row| {
        // The row-assembly closure builds the output row only; the clause's
        // parked sort keys are the surface every merge path shares, so the
        // funnel parks them — one spelling of the park step, and it runs
        // after the closure has dropped the source row.
        let mut row = build(row)?;
        park_order_keys(&class.order_keys, &mut row, |expr, scope| {
            eval(graph, scope, expr, params)
        })?;
        landing.land(graph, params, row)
    })?;
    Ok(Landed {
        rows: landing.finish(),
        windowed,
    })
}

/// The projection's finish tail, shared by every merge path — the
/// pipeline's last stage: the plain WHERE filter over the merged rows (the
/// collected shape only; the window shape filtered as each row landed),
/// then the paging stages (the parked-key sort with its top-k selection
/// for the shapes that merge unordered rows, the DISTINCT first-seen pass
/// with its row-buffer charge re-measured at each shrink, the page
/// itself), then the result extraction. The eager stop already paged on
/// its own resolved bounds (`prepare_feed`), so the tail pages only
/// on the non-eager path.
pub(crate) fn finish_projection(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    class: &ProjectionClass<'_>,
    landed: Landed,
    bounds: PageBounds,
) -> Result<Vec<Row>> {
    // The window shape is the container the merge path landed — read from
    // what was landed rather than re-derived here, so the WHERE-filter
    // policy cannot disagree with the container the rows came out of.
    let Landed { rows, windowed } = landed;
    let mut merged = rows;
    let ProjectionFields { distinct, .. } = class.fields;
    // The ordered-window shape filtered each row as it landed, before the
    // selection; filtering again here would evaluate the predicate twice
    // per surviving row (a nondeterministic predicate would flake).
    if !windowed {
        if let Some(w) = class.where_mode.and_then(WhereMode::plain) {
            merged = filter_rows_where(graph, params, w, merged)?;
        }
    }
    // The eager shape's feed already paged on these bounds; every other
    // shape pages here.
    let page_bounds = if class.eager_paged {
        PageBounds {
            skip: 0,
            limit: None,
        }
    } else {
        bounds
    };
    // The window shape's rows arrive already ordered and bounded by the
    // heap, so only the shapes that merge unordered rows sort here — on the
    // one cutoff the window decision reads too ([`ProjectionClass::sort_cut`]).
    if !windowed {
        sort_rows_by_parked_keys(&class.order_keys, &mut merged, class.sort_cut(bounds))?;
    }
    // The DISTINCT key columns, resolved once rather than re-derived per
    // row: a `*` projection keys its visible keys — read from the first
    // merged row, so every row keys the same columns in the same order —
    // and a named projection its output columns. An empty row set has no
    // keys to compare, so the degenerate empty plan is never read.
    let key_columns: Vec<String> = if class.shape.star() {
        merged.first().map(visible_keys).unwrap_or_default()
    } else {
        class.out_names.clone()
    };
    // The DISTINCT pass's row-buffer charge: Option-held from
    // [`retain_distinct`] — it exists only on the distinct shape, and
    // every later re-measure (the page, the output conversion) walks the
    // held charge instead of re-checking the mode beside an empty
    // sentinel.
    let mut retained_rows = if distinct {
        let (kept, charge) = retain_distinct(merged, &key_columns)?;
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
