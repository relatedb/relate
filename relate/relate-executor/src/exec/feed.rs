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

use relate_core::{Error, Result};

use crate::ctx::ExecCtx;
use crate::metrics::PlanMetricsSite;
use crate::row::Row;

use super::aggregate::Aggregation;
use super::profile_tick::{ParkedFacts, StreamedStats};
use super::project::{ProjectionInput, RowSource};
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

    /// The next row of the open source, `Ok(None)` once it is spent, an
    /// error when the source fails (the producer's natural answer: next
    /// row, done, or error — the one reshape into the `Iterator` item
    /// lives in [`StreamedFeed`]). The implementation samples
    /// `cancel_tick` once per loop iteration — including the iterations
    /// that skip a non-row (a blank record, a spent element) — and the
    /// spine notes every yielded row's PROFILE stats itself.
    fn pull(&mut self, ctx: ExecCtx<'_>) -> Result<Option<Row>>;
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
    /// cloned off the feed by [`take_input`] before a consumer takes it.
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
            if let Some(row) = self.source.pull(self.ctx)? {
                self.stats.note_row(&row);
                return Ok(Some(row));
            }
            let Some(row) = self.inputs.next() else {
                return Ok(None);
            };
            // An `Ok(false)` row contributes nothing: the next pull on the
            // spent source answers `None` and the loop moves on.
            self.source.open(self.ctx, row)?;
        }
    }

    /// Drain the feed into a held vector, charging each source's output to
    /// the running total before its first row is built: the clause's
    /// materializing form, for a consumer that cannot fold as rows arrive
    /// (ADR-0463 — the output vector is one retaining container; its
    /// running total is the guard's). The drain books the same PROFILE
    /// facts the streaming walk does — each row noted, the whole drain
    /// under the clause's clock — so a parked feed that resolves to a
    /// collecting consumer reports the rows it generated, whichever
    /// consumer drained it.
    pub(crate) fn materialize(self) -> Result<Vec<Row>> {
        let Self {
            ctx,
            inputs,
            source,
            stats,
        } = self;
        stats.timed_step(|| held_rows(ctx, inputs, source, Some(&stats)))
    }
}

/// The materializing drain: open each input row's source, charge its output
/// before its first row is built, pull the rows. `stats` is the PROFILE
/// facts of a feed whose rows are being collected; a source drained alone
/// (the `CALL` composition's materializing form, which has no streamed
/// clause to report) passes none, so nothing is fabricated for the rows a
/// consumer holds.
pub(super) fn held_rows(
    ctx: ExecCtx<'_>,
    inputs: impl IntoIterator<Item = Row>,
    mut source: Box<dyn FeedSource + '_>,
    stats: Option<&StreamedStats>,
) -> Result<Vec<Row>> {
    let mut retained = ctx.graph.retain_bytes(0)?;
    let mut rows = Vec::new();
    for row in inputs {
        // A row that contributes nothing is skipped, never billed.
        if !source.open(ctx, row)? {
            continue;
        }
        let (count, width) = source.charge_facts()?;
        retained.add_rows(ctx.graph, count, width)?;
        while let Some(row) = source.pull(ctx)? {
            if let Some(stats) = stats {
                stats.note_row(&row);
            }
            rows.push(row);
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

/// A streamed generator waiting for its consumer: the feed — the one
/// owner of the counters its rows are noted into — and the PROFILE
/// position its facts are reported at. One value per streamed clause, so
/// two of them in a statement never share counters.
pub(super) struct ParkedFeed<'a> {
    feed: StreamedFeed<'a>,
    record: Option<(PlanMetricsSite<'a>, usize)>,
}

/// The one feed slot the next projection consumes: a parked generator, or
/// the aggregation a streamed `MATCH` folded its rows into — one slot, so
/// "never both at once" is the type's own fact, not a precedence rule
/// between two `Option`s.
pub(super) enum PendingFeed<'a> {
    Generator(ParkedFeed<'a>),
    Folded(Box<Aggregation<'a>>),
}

/// The input a `WITH`/`RETURN` consumes, as the dispatcher's loop state
/// and the slot decide it: the slot's contents as a [`RowFeed`] — the
/// parked generator with the facts to report once it is drained, the
/// folded aggregation — the held rows when the slot is empty, or the
/// rows a match sink already projected ([`RowSource::SinkProjected`]).
/// The pre-projected form owns its rows at this seam, so it cannot be
/// built over a generated or folded feed: a sink projection only ever
/// feeds the clause right after it, and a slot still occupied there is an
/// engine wiring error, reported loudly rather than dropped.
pub(super) fn take_input<'a>(
    pending: &mut Option<PendingFeed<'a>>,
    rows: &mut Vec<Row>,
    rows_from: RowSource,
) -> Result<(ProjectionInput<'a>, Option<ParkedFacts<'a>>)> {
    if matches!(rows_from, RowSource::SinkProjected) {
        if pending.is_some() {
            return Err(Error::internal(
                "a sink-projected input arrived beside a parked feed",
            ));
        }
        return Ok((ProjectionInput::SinkProjected(std::mem::take(rows)), None));
    }
    Ok(match pending.take() {
        Some(PendingFeed::Generator(ParkedFeed { feed, record })) => {
            // The facts' handle is cloned off the feed that owns it
            // before the consumer takes the feed: the double-hold cannot
            // be spelled with a different counter.
            let facts = ParkedFacts::new(feed.stats(), record);
            (ProjectionInput::Feed(RowFeed::Generated(feed)), Some(facts))
        }
        Some(PendingFeed::Folded(aggregation)) => {
            (ProjectionInput::Feed(RowFeed::Folded(aggregation)), None)
        }
        None => (
            ProjectionInput::Feed(RowFeed::Held(std::mem::take(rows))),
            None,
        ),
    })
}

/// Park a generator clause's feed for the projection that consumes it:
/// the one shape of "fused into its consumer" every producer shares — the
/// feed (which owns its stats) and the facts' PROFILE position — so a
/// dispatcher arm parks with one call and its clause match stays a clause
/// match.
pub(super) fn park_generator<'a>(
    feed: StreamedFeed<'a>,
    record: Option<(PlanMetricsSite<'a>, usize)>,
    pending: &mut Option<PendingFeed<'a>>,
) {
    *pending = Some(PendingFeed::Generator(ParkedFeed { feed, record }));
}
