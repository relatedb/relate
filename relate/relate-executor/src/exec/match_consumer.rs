//! The bounded sinks a `MATCH`'s rows fold into as they are emitted
//! (ADR-0555 §3, ADR-0660): the ordered top-k window of a following
//! `ORDER BY … LIMIT`, the pass-through page of a following named
//! `DISTINCT` (filter, first-seen gate and page applied per row), or the
//! count-only sink of a provably empty page. One type over the projection's
//! one landing container policy — the container owns the selection, the
//! seen-set keying, the per-row plain `WHERE` filter, the page and the
//! charge; the sink supplies the sink projection and the charge for a rung's
//! finished container.
//!
//! The admission ([`open_sink`]) reads the clause's fields, shape and
//! bounds once, claims the part's classification slot only on the accepting
//! path, and declines every shape whose bounded form the projection's own
//! path owns.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::Clause;

use crate::ctx::ExecCtx;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::classification::{ProjectionClass, ProjectionShape};
use super::kernels::program::bound_is_row_invariant;
use super::match_chain::{Landing, LandingKind};
use super::match_clause::charge_and_fold;
use super::pattern_policies::RowFold;
use super::post_merge::{LandedTail, OutputContainer};
use super::project::RowSource;
use super::scope::ProjectionFields;
use super::sink::{finish_match_row, SinkProjection};

/// Which bounded container the admission selected.
enum ContainerKind {
    Window,
    Page,
}

/// A `MATCH`'s emitted rows folded straight into the projection's bounded
/// landing container. `projection` evaluates the following clause's items
/// per row when the walk hands over raw bindings; it is `None` when the
/// caller already projected them (the match chain's last stage).
pub(crate) struct BoundedSink<'a> {
    container: RefCell<OutputContainer<'a, 'a>>,
    projection: Option<SinkProjection<'a>>,
    params: &'a BTreeMap<String, Value>,
    offered: Cell<u64>,
    /// The tail stages the container ran, carried across the seam.
    tail: LandedTail,
}

impl<'a> BoundedSink<'a> {
    fn open(
        graph: &ExecGraph<'_>,
        class: &'a ProjectionClass<'a>,
        bounds: super::order::PageBounds,
        kind: ContainerKind,
        projection: Option<SinkProjection<'a>>,
        params: &'a BTreeMap<String, Value>,
    ) -> Result<Self> {
        let (container, tail) = match kind {
            ContainerKind::Window => {
                let container = OutputContainer::open(graph, class, bounds)?;
                if !container.is_window() {
                    return Err(relate_core::Error::internal(
                        "the match window sink opened a container the shape never takes",
                    ));
                }
                (
                    container,
                    LandedTail {
                        windowed: true,
                        ..Default::default()
                    },
                )
            }
            ContainerKind::Page => (
                OutputContainer::page(graph, class, bounds)?,
                LandedTail {
                    windowed: false,
                    streamed_distinct: class.streams_distinct(),
                    funnel_filtered_where: class.plain_where().is_some(),
                    streamed_page: true,
                },
            ),
        };
        Ok(Self {
            container: RefCell::new(container),
            projection,
            params,
            offered: Cell::new(0),
            tail,
        })
    }

    /// The sink's landing — the container's rows and the tail facts it ran,
    /// schemas shared — beside the number of rows the producer offered it.
    pub(crate) fn finish(self) -> Landing<'a> {
        let mut rows = self.container.into_inner().finish();
        Row::share_schemas(&mut rows);
        Landing {
            offered: self.offered.get(),
            consumed: 0,
            kind: LandingKind::Rows {
                rows,
                source: RowSource::SinkLanded(self.tail),
            },
        }
    }

    /// The live container's charged bytes (the PROFILE terminal-retention
    /// fact, read before the landing leaves the run).
    pub(crate) fn retained_bytes(&self) -> u64 {
        self.container.borrow().retained_bytes()
    }
}

impl RowFold for BoundedSink<'_> {
    /// A rung's finished container headed to the sink is retained while it
    /// is folded: charge it first, as a stage does, so a kernel-built
    /// container cannot ride the pool uncharged and make the sink's bounded
    /// state the only figure the cap sees.
    fn offer_container(&self, graph: &ExecGraph<'_>, rows: Vec<Row>) -> Result<()> {
        charge_and_fold(self, graph, rows)
    }

    fn offer(&self, graph: &ExecGraph<'_>, row: Row) -> Result<()> {
        self.offered.set(self.offered.get() + 1);
        // The page's own state says it is satisfied; later rows are
        // counted and never projected.
        if self.container.borrow().is_full() {
            return Ok(());
        }
        let row = match &self.projection {
            Some(projection) => finish_match_row(graph, &row, projection, self.params)?,
            None => row,
        };
        self.container.borrow_mut().land(graph, self.params, row)?;
        Ok(())
    }
}

/// The count-only sink for a provably empty page (`LIMIT 0`): the answer
/// needs no rows, so it counts what it is offered and stores nothing — no
/// container, no projection, no keying.
pub(crate) struct EmptySink {
    offered: Cell<u64>,
}

impl EmptySink {
    fn new() -> Self {
        Self {
            offered: Cell::new(0),
        }
    }

    fn finish(self) -> Landing<'static> {
        Landing {
            offered: self.offered.get(),
            consumed: 0,
            kind: LandingKind::Rows {
                rows: Vec::new(),
                // Nothing ran on zero rows: the type's own untouched tail.
                source: RowSource::SinkLanded(LandedTail::default()),
            },
        }
    }
}

impl RowFold for EmptySink {
    fn offer(&self, _: &ExecGraph<'_>, _: Row) -> Result<()> {
        self.offered.set(self.offered.get() + 1);
        Ok(())
    }

    /// The count-only sink still holds whatever a rung hands it while it
    /// counts: the canonical charge-then-fold covers it, and `offer`'s own
    /// increment counts the rows.
    fn offer_container(&self, graph: &ExecGraph<'_>, rows: Vec<Row>) -> Result<()> {
        charge_and_fold(self, graph, rows)
    }
}

/// The bounded sink the admission chose: a container fold, or the count-only
/// sink of a provably empty page.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Sink<'a> {
    Bounded(BoundedSink<'a>),
    Empty(EmptySink),
}

impl<'a> Sink<'a> {
    pub(crate) fn as_fold(&self) -> &dyn RowFold {
        match self {
            Sink::Bounded(sink) => sink,
            Sink::Empty(sink) => sink,
        }
    }

    pub(crate) fn finish(self) -> Landing<'a> {
        match self {
            Sink::Bounded(sink) => sink.finish(),
            Sink::Empty(sink) => sink.finish(),
        }
    }

    pub(crate) fn retained_bytes(&self) -> u64 {
        match self {
            Sink::Bounded(sink) => sink.retained_bytes(),
            Sink::Empty(_) => 0,
        }
    }
}

/// The bounded sink for the clause after a `MATCH` (or after a run's last
/// stage), when its shape has one: an `ORDER BY … LIMIT` over the top-k
/// window, a named `DISTINCT` over the pass-through page, or a provably
/// empty page over the count-only sink. `None` keeps the held path — no
/// bounded form, a classification slot the part already claimed, a non-plain
/// shape, or page bounds that may resolve differently at the projection.
pub(crate) fn open_sink<'a>(
    ctx: ExecCtx<'a>,
    next: &'a Clause,
    class_slot: &'a std::cell::OnceCell<ProjectionClass<'a>>,
    projection: Option<SinkProjection<'a>>,
) -> Result<Option<Sink<'a>>> {
    let ExecCtx { graph, params, .. } = ctx;
    if class_slot.get().is_some() {
        return Ok(None);
    }
    let Some(fields) = ProjectionFields::from_clause(next) else {
        return Ok(None);
    };
    let Ok(shape) = ProjectionShape::decide(&fields) else {
        return Ok(None);
    };
    // The sink's landing facts describe a named projection; a `*` shape's
    // rows key every visible binding, which the merged pass owns.
    if !shape.plain() {
        return Ok(None);
    }
    // The class is built locally and stored only once the admission accepts:
    // a declined candidate must leave the part's one classification slot free
    // for a later `MATCH`'s fold.
    let class = ProjectionClass::new(graph, &fields, shape);
    // A non-provable bound (a `rand()` limit) is the held path's: the sink
    // and the projection must resolve one page.
    if !bounds_are_provable(&fields) {
        return Ok(None);
    }
    let bounds = class.page_bounds(graph, params)?;
    // A provably empty page needs no rows: the count-only sink, so no input
    // row is projected or keyed for an answer known to be empty.
    if bounds.answers_nothing() {
        return Ok(Some(Sink::Empty(EmptySink::new())));
    }
    let kind = if class.window_keep(bounds).is_some() {
        ContainerKind::Window
    } else if class.streams_distinct() {
        ContainerKind::Page
    } else {
        return Ok(None);
    };
    let class = class_slot.get_or_init(|| class);
    Ok(Some(Sink::Bounded(BoundedSink::open(
        graph, class, bounds, kind, projection, params,
    )?)))
}

/// Whether the clause's page bounds resolve to one value: a literal or
/// parameter does, a nondeterministic call (`rand()`) does not — the sink
/// resolves them here and the projection resolves them again when it runs.
fn bounds_are_provable(fields: &ProjectionFields<'_>) -> bool {
    fields.limit.is_none_or(bound_is_row_invariant)
        && fields.skip.is_none_or(bound_is_row_invariant)
}
