//! The streamed-generator feed spine (ADR-0555): the one copy of the
//! protocol every generator clause (`UNWIND`, `LOAD CSV`) shares — the
//! input iteration, the PROFILE stats, the `Iterator` timing wrap, the
//! ADR-0463 materializing charge — over a per-producer [`FeedSource`]
//! that owns exactly its own facts: how a source opens and how a row is
//! pulled.
//!
//! This is also the feed-fusion seam the dispatcher, the parked
//! generators and the projection consumers reach through
//! ([`ParkedFeed`], [`PendingFeed`], [`take_feed`]) — beside
//! [`super::RowFeed`], whose placement doc this module follows; the
//! PROFILE reporting of a parked feed's facts stays in
//! [`super::profile_tick`].

use std::rc::Rc;

use relate_core::Result;

use crate::ctx::ExecCtx;
use crate::metrics::PlanMetricsSite;
use crate::row::Row;

use super::aggregate::Aggregation;
use super::profile_tick::StreamedStats;
use super::RowFeed;

/// One streamed producer's source protocol. The spine
/// ([`StreamedFeed`]) drives it: opens each input row's source, pulls its
/// rows one at a time, and bills the materializing form.
pub(crate) trait FeedSource {
    /// Open `row`'s source. `Ok(false)`: this row contributes nothing (a
    /// null URL, an empty expansion) — the spine moves to the next input
    /// row; the inputs' exhaustion is the spine's own fact, read from its
    /// iterator. An implementation that loops while skipping (the null
    /// URL, the blank record) samples `cancel_tick` once per iteration,
    /// so a source of only skippable rows stays cancellable.
    fn open(&mut self, ctx: ExecCtx<'_>, row: Row) -> Result<bool>;

    /// The charge facts of the open source — the rows it will add and the
    /// carried width — read by the materializing form alone, before its
    /// first row is built. A source whose count costs a walk (the LOAD
    /// CSV record count re-walks the remaining text) pays it here only:
    /// the streamed path never counts what it will not bill.
    fn charge_facts(&self) -> Result<(usize, usize)>;

    /// The next row of the open source, `None` once it is spent. The
    /// implementation samples `cancel_tick` once per loop iteration —
    /// including the iterations that skip a non-row (a blank record, a
    /// spent element) — and the spine notes every yielded row's PROFILE
    /// stats itself.
    fn pull(&mut self, ctx: ExecCtx<'_>) -> Option<Result<Row>>;
}

/// The rows of a generator clause, produced one at a time and never
/// retained: the spine over a [`FeedSource`]. A consumer that folds rows
/// as they arrive (an aggregation) holds constant state however long the
/// source is, and a consumer that stops early never generates the rest.
pub(crate) struct StreamedFeed<'a> {
    ctx: ExecCtx<'a>,
    inputs: std::vec::IntoIter<Row>,
    source: Box<dyn FeedSource + 'a>,
    /// Shared with the dispatcher so the PROFILE record of this clause can
    /// report after a consumer drained the feed.
    stats: Rc<StreamedStats>,
}

impl<'a> StreamedFeed<'a> {
    /// The feed owns its own stats handle — built here from whether a
    /// PROFILE sink is attached, never carried beside the feed by every
    /// producer that parks one.
    pub(crate) fn new(
        ctx: ExecCtx<'a>,
        rows: Vec<Row>,
        source: Box<dyn FeedSource + 'a>,
        profiled: bool,
    ) -> Self {
        Self {
            ctx,
            inputs: rows.into_iter(),
            source,
            stats: Rc::new(StreamedStats::new(profiled)),
        }
    }

    /// The feed's own stats handle — the parked form's PROFILE facts,
    /// read by the parking helper beside the feed that owns them.
    pub(super) fn stats(&self) -> Rc<StreamedStats> {
        Rc::clone(&self.stats)
    }

    /// The next generated row: the open source's next row, or — once it is
    /// spent — the next input row's source. `Ok(None)` once the inputs are
    /// exhausted. The opener's charge facts are never read here: a
    /// generated row is never retained, so they serve the materializing
    /// form alone.
    fn generate(&mut self) -> Result<Option<Row>> {
        loop {
            if let Some(row) = self.source.pull(self.ctx) {
                let row = row?;
                self.stats.note_row(&row);
                return Ok(Some(row));
            }
            let Some(row) = self.inputs.next() else {
                return Ok(None);
            };
            if !self.source.open(self.ctx, row)? {
                continue;
            }
        }
    }

    /// Drain the feed into a held vector, charging each source's output to
    /// the running total before its first row is built: the clause's
    /// materializing form, for a consumer that cannot fold as rows arrive
    /// (ADR-0463 — the output vector is one retaining container; its
    /// running total is the guard's).
    pub(crate) fn materialize(self) -> Result<Vec<Row>> {
        held_rows(self.ctx, self.inputs, self.source)
    }

    /// The materializing form over a source alone — no stats: the streamed
    /// form's PROFILE facts exist only while a consumer pulls rows, so
    /// nothing is fabricated here for the rows a consumer holds.
    pub(crate) fn held(
        ctx: ExecCtx<'_>,
        rows: Vec<Row>,
        source: Box<dyn FeedSource + '_>,
    ) -> Result<Vec<Row>> {
        held_rows(ctx, rows.into_iter(), source)
    }
}

/// The materializing drain both entries share: open each input row's
/// source, charge its output before its first row is built, pull the rows.
fn held_rows(
    ctx: ExecCtx<'_>,
    inputs: std::vec::IntoIter<Row>,
    mut source: Box<dyn FeedSource + '_>,
) -> Result<Vec<Row>> {
    let mut retained = ctx.graph.retain_bytes(0)?;
    let mut rows = Vec::new();
    for row in inputs {
        // A row that contributes nothing is skipped, never billed.
        if !source.open(ctx, row)? {
            continue;
        }
        let (count, width) = source.charge_facts()?;
        let added = crate::workspace::retained_rows_bytes(count, width);
        retained.add_allocation(ctx.graph, added)?;
        while let Some(row) = source.pull(ctx) {
            rows.push(row?);
        }
    }
    Ok(rows)
}

impl Iterator for StreamedFeed<'_> {
    type Item = Result<Row>;

    fn next(&mut self) -> Option<Self::Item> {
        let generated = Rc::clone(&self.stats).timed_step(|| self.generate());
        // The Iterator protocol's one reshape of the generator's answer:
        // `Ok(None)` is exhaustion, every error an item.
        match generated {
            Ok(Some(row)) => Some(Ok(row)),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        }
    }
}

/// A streamed generator's PROFILE facts: the statistics its generator
/// filled and the PROFILE position its facts are reported at.
pub(super) struct ParkedFacts<'a> {
    pub(super) stats: Rc<StreamedStats>,
    pub(super) record: Option<(PlanMetricsSite<'a>, usize)>,
}

/// A streamed generator waiting for its consumer: the feed and its own
/// facts. One value per streamed clause, so two of them in a statement
/// never share counters.
pub(super) struct ParkedFeed<'a> {
    pub(super) feed: StreamedFeed<'a>,
    pub(super) facts: ParkedFacts<'a>,
}

/// The one feed slot the next projection consumes: a parked generator, or
/// the aggregation a streamed `MATCH` folded its rows into — one slot, so
/// "never both at once" is the type's own fact, not a precedence rule
/// between two `Option`s.
pub(super) enum PendingFeed<'a> {
    Generator(ParkedFeed<'a>),
    Folded(Box<Aggregation<'a>>),
}

/// The feed a `WITH`/`RETURN` consumes: the slot's contents as a
/// [`RowFeed`] — the parked generator with the facts to report once it is
/// drained, the folded aggregation — or the held rows when the slot is
/// empty.
pub(super) fn take_feed<'a>(
    pending: &mut Option<PendingFeed<'a>>,
    rows: &mut Vec<Row>,
) -> (RowFeed<'a>, Option<ParkedFacts<'a>>) {
    match pending.take() {
        Some(PendingFeed::Generator(ParkedFeed { feed, facts })) => {
            (RowFeed::Generated(feed), Some(facts))
        }
        Some(PendingFeed::Folded(aggregation)) => (RowFeed::Folded(aggregation), None),
        None => (RowFeed::Held(std::mem::take(rows)), None),
    }
}

/// Park a generator clause's feed for the projection that consumes it:
/// the one shape of "fused into its consumer" every producer shares — the
/// feed, its stats, the facts' PROFILE position — so a dispatcher arm
/// parks with one call and its clause match stays a clause match.
pub(super) fn park_generator<'a>(
    feed: StreamedFeed<'a>,
    record: Option<(PlanMetricsSite<'a>, usize)>,
    pending: &mut Option<PendingFeed<'a>>,
) {
    // The stats handle is derived from the feed that owns it, never
    // carried beside it.
    let stats = feed.stats();
    *pending = Some(PendingFeed::Generator(ParkedFeed {
        feed,
        facts: ParkedFacts { stats, record },
    }));
}
