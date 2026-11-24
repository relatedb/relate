//! The streamed-generator feed spine (ADR-0555): the one copy of the
//! protocol every generator clause (`UNWIND`, `LOAD CSV`) shares — the
//! input iteration, the PROFILE stats, the `Iterator` timing wrap, the
//! ADR-0463 materializing charge — over a per-producer [`FeedSource`]
//! that owns exactly its own facts: how a source opens and how a row is
//! pulled.
//!
//! The inter-clause boundary's own type lives here too (ADR-0660):
//! [`RowStream`], the one input every clause arm receives, pulling bounded
//! [`RowBatch`]es. A generator spine is a source of that same stream — the
//! dispatcher parks a fused generator in the stream slot itself, never in
//! a channel beside it — and the stream carries the generator's PROFILE
//! facts until its consumer drains or stops it.

use std::rc::Rc;

use relate_core::tokens::WorkspaceCharge;
use relate_core::Result;

use crate::ctx::ExecCtx;
use crate::metrics::PlanMetricsSite;
use crate::row::Row;

use super::batch_tail::BATCH_ROWS;
use super::profile_tick::{ParkedFacts, StreamedStats};

/// The batch-construction stride between `retained_rows_of` repricings: the
/// walk is O(batch), so it is amortized over this many rows instead of per
/// row or once per batch (the tail past the last stride is priced at the
/// end of the loop).
const PRICE_CHECK_ROWS: usize = 64;

/// One batch of rows off a [`RowStream`] (ADR-0660), never more than
/// [`BATCH_ROWS`] — the ADR-0555 batch bound.
pub(crate) struct RowBatch(Vec<Row>);

impl RowBatch {
    /// The wrapped rows, moved out — the container's one handoff to the
    /// consumer that needs the `Vec` itself, so no batch is re-collected
    /// into a fresh buffer.
    pub(crate) fn into_vec(self) -> Vec<Row> {
        self.0
    }
}

impl IntoIterator for RowBatch {
    type Item = Row;
    type IntoIter = std::vec::IntoIter<Row>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// The rows a stream pulls from: the container a clause arm already holds,
/// or a sequence produced one row at a time (a generator clause's spine, or
/// a breaker answering from its own state). Every form is the same stream
/// to the consumer (ADR-0660).
enum Source<'a> {
    Held(std::vec::IntoIter<Row>),
    /// A produced sequence, batched as it is pulled.
    Produced(Box<dyn Iterator<Item = Result<Row>> + 'a>),
    /// A produced sequence whose every pull does work that must finish even
    /// when the consumer stops early — the `CALL { … } IN TRANSACTIONS`
    /// arm, whose pulls run and commit one batch each.
    Completing(Box<dyn Iterator<Item = Result<Row>> + 'a>),
    /// A generator clause's own spine: a produced sequence that also
    /// carries the materializing charge its collecting consumer pays.
    Generator(StreamedFeed<'a>),
}

/// The inter-clause boundary's one input (ADR-0660): a pull stream of
/// bounded [`RowBatch`]es. The consumer never branches on the source — the
/// held container and a generator clause are the same stream to it — and a
/// fused generator's own PROFILE facts ride the stream until its consumer
/// drains or stops it. The lifetime is the producer's borrow scope; the
/// held source borrows nothing.
pub(crate) struct RowStream<'a> {
    rows: Source<'a>,
    stopped: bool,
    /// A fused generator's facts: reported when this stream is drained,
    /// stopped or errored, so the clause's PROFILE record states the rows
    /// it produced, never a projected population. Shared, so a consumer
    /// that kept a handle reports the same fact once.
    facts: Option<Rc<ParkedFacts<'a>>>,
}

impl<'a> RowStream<'a> {
    /// Wrap the rows a clause arm already holds — the held path.
    pub(crate) fn held(rows: Vec<Row>) -> Self {
        Self {
            rows: Source::Held(rows.into_iter()),
            stopped: false,
            facts: None,
        }
    }

    /// Wrap held rows carrying the clause's PROFILE record — the held form
    /// that still reports: `probe()` and `is_held()` stay meaningful (the
    /// consumer's eager page and the projection's input charge read them),
    /// and the facts report at drain, stop or exhaustion like the produced
    /// forms'. The rows are in hand, so the count is known up front; the
    /// clause's own time is the arm's, not this stream's.
    pub(crate) fn held_profiled(
        rows: Vec<Row>,
        record: Option<(PlanMetricsSite<'a>, usize)>,
    ) -> Self {
        let stats = Rc::new(StreamedStats::new(record.is_some()));
        for row in &rows {
            stats.note_row(row);
        }
        Self {
            rows: Source::Held(rows.into_iter()),
            stopped: false,
            facts: ParkedFacts::new(stats, record),
        }
    }

    /// Wrap a sequence produced one row at a time — a breaker emitting
    /// from its own declared state, or a generator spine whose consumer
    /// never materializes it. The consumer batches it like the held source
    /// (ADR-0660).
    pub(crate) fn emitting(rows: impl Iterator<Item = Result<Row>> + 'a) -> Self {
        Self {
            rows: Source::Produced(Box::new(rows)),
            stopped: false,
            facts: None,
        }
    }

    /// Wrap a sequence whose every pull does work that must finish even
    /// when the consumer stops early — the `CALL { … } IN TRANSACTIONS`
    /// arm, whose pulls run and commit one batch each. A page's `LIMIT`
    /// stops the statement's rows, never the remaining batches: the
    /// materializing form ran every batch too, so [`Self::stop`] runs the
    /// rest and discards their rows.
    ///
    /// `record` is the clause's PROFILE position: with one attached, each
    /// pull is timed and each yielded row counted, so the clause reports
    /// the rows it actually produced once its consumer drains or stops the
    /// stream — never the empty probe of a produced source.
    pub(crate) fn emitting_to_completion(
        rows: impl Iterator<Item = Result<Row>> + 'a,
        record: Option<(PlanMetricsSite<'a>, usize)>,
    ) -> Self {
        let stats = Rc::new(StreamedStats::new(record.is_some()));
        let facts = ParkedFacts::new(Rc::clone(&stats), record);
        let mut inner = rows;
        let counted = std::iter::from_fn(move || {
            iterator_item(stats.counted_step(|| inner.next().transpose()))
        });
        Self {
            rows: Source::Completing(Box::new(counted)),
            stopped: false,
            facts,
        }
    }

    /// Wrap a generator clause's spine as the stream — the parked form,
    /// whose projection consumer may materialize it under the generator's
    /// own charge — carrying its PROFILE position so the clause reports the
    /// rows it generated once the consumer is done.
    pub(crate) fn profiled(
        feed: StreamedFeed<'a>,
        record: Option<(PlanMetricsSite<'a>, usize)>,
    ) -> Self {
        // The stats handle is derived from the feed that owns it, never
        // carried beside it — read before the feed moves into the source.
        let facts = ParkedFacts::new(feed.stats(), record);
        Self {
            rows: Source::Generator(feed),
            stopped: false,
            facts,
        }
    }

    /// Wrap a generated sequence that no consumer materializes — the
    /// expanding form, drained or stopped by the clause that follows —
    /// carrying the generator's PROFILE facts.
    pub(crate) fn expanding(
        feed: StreamedFeed<'a>,
        record: Option<(PlanMetricsSite<'a>, usize)>,
    ) -> Self {
        let facts = ParkedFacts::new(feed.stats(), record);
        let mut stream = Self::emitting(feed);
        stream.facts = facts;
        stream
    }

    /// The next batch, `None` once the stream is spent. An error ends the
    /// stream; a stopped stream yields nothing further. Exhaustion and an
    /// error both report a fused generator's facts.
    pub(crate) fn next_batch(&mut self) -> Result<Option<RowBatch>> {
        let mut batch: Vec<Row> = Vec::new();
        // The batch under construction is on the workspace ledger while it
        // fills — the same per-batch rule the drain and the write barrier
        // follow — so a pool too small for one batch's rows refuses as the
        // rows land, never after a whole unbilled container is in hand.
        // The reservation covers the construction window alone: it is
        // released here, and the caller bills the rows it takes — the
        // drain's retained total, the barrier's input charge. The landing
        // funnels and the aggregation fold instead charge the state they
        // build (the landed rows, the group table), relying on a
        // one-batch allowance: at most one batch of input rows rides
        // unpriced beside that state.
        let mut charge = WorkspaceCharge::empty();
        while batch.len() < BATCH_ROWS {
            match self.next_row()? {
                Some(row) => {
                    batch.push(row);
                    if batch.len().is_multiple_of(PRICE_CHECK_ROWS) {
                        charge.resize_with(|| crate::workspace::retained_rows_of(&batch))?;
                    }
                }
                None => break,
            }
        }
        if !batch.is_empty() {
            // The tail past the last chunk boundary is priced too, so the
            // construction window is covered end to end.
            charge.resize_with(|| crate::workspace::retained_rows_of(&batch))?;
        }
        drop(charge);
        if batch.is_empty() {
            Ok(None)
        } else {
            Ok(Some(RowBatch(batch)))
        }
    }

    /// The next single row, `None` once the stream is spent — the pull the
    /// `IN TRANSACTIONS` arm takes, whose batch size is its own batch
    /// boundary, not the stream's [`BATCH_ROWS`]. An error ends the stream
    /// at the pull that produced it (ADR-0660's error ordering); exhaustion
    /// and an error both report a fused generator's facts.
    pub(crate) fn next_row(&mut self) -> Result<Option<Row>> {
        if self.stopped {
            return Ok(None);
        }
        let next = match &mut self.rows {
            Source::Held(rows) => rows.next().map(Ok),
            Source::Produced(rows) | Source::Completing(rows) => rows.next(),
            Source::Generator(feed) => feed.next(),
        };
        match next {
            Some(Ok(row)) => Ok(Some(row)),
            Some(Err(error)) => {
                self.stopped = true;
                self.report_facts();
                Err(error)
            }
            None => {
                self.report_facts();
                Ok(None)
            }
        }
    }

    /// The consumer's early exit: the producer generates nothing further,
    /// and the rows it did produce are its reported fact. A completing
    /// source ([`Self::emitting_to_completion`]) runs out its remaining
    /// work first — the consumer stops the rows, not the transactions —
    /// and the rows past the page are discarded; a batch's error still
    /// surfaces.
    pub(crate) fn stop(&mut self) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        if !matches!(self.rows, Source::Completing(_)) {
            self.stopped = true;
            self.report_facts();
            return Ok(());
        }
        while self.next_batch()?.is_some() {}
        self.stopped = true;
        Ok(())
    }

    /// The rows already in hand, for the shape and warm-up decisions that
    /// read a row before the walk starts; empty for a produced source.
    pub(crate) fn probe(&self) -> &[Row] {
        match &self.rows {
            Source::Held(rows) => rows.as_slice(),
            Source::Produced(_) | Source::Completing(_) | Source::Generator(_) => &[],
        }
    }

    /// Whether the stream is the empty held container — the shape a folded
    /// `MATCH` leaves beside its aggregation. A produced source is never
    /// this: its rows are not in hand. Derived from the two accessors, so
    /// the `Source` discriminator keeps one matcher.
    pub(crate) fn is_empty_held(&self) -> bool {
        self.is_held() && self.probe().is_empty()
    }

    /// Whether the rows are a container already in hand rather than
    /// produced as the consumer pulls them. The projection's eager page
    /// reads it: a held input pages before evaluating the clause's items,
    /// a produced one pages inside its streaming fold.
    pub(crate) fn is_held(&self) -> bool {
        matches!(self.rows, Source::Held(_))
    }

    /// The fused generator's facts, for the consumer that reads the time
    /// it excludes from its own record after the walk. The stream may
    /// already have reported them; the handle keeps the time reachable.
    pub(super) fn facts(&self) -> Option<Rc<ParkedFacts<'a>>> {
        self.facts.clone()
    }

    fn report_facts(&self) {
        if let Some(facts) = &self.facts {
            facts.report();
        }
    }

    /// Take the rows of a held stream, refusing a produced one: the
    /// sink-projected feed's container is the sink's own rows, never a
    /// generator's.
    pub(crate) fn into_held(self) -> Result<Vec<Row>> {
        let stopped = self.stopped;
        match self.rows {
            Source::Held(rows) => Ok(held_or_stopped(rows, stopped)),
            Source::Produced(_) | Source::Completing(_) | Source::Generator(_) => Err(
                relate_core::Error::internal("a sink-projected feed arrived as a generated feed"),
            ),
        }
    }

    /// Drain the stream into the held vector a collecting consumer takes.
    /// A generator spine materializes through its own charged form, so the
    /// retained container is billed exactly as the generator's clause
    /// bills it; a produced sequence collects through the same charged
    /// batch loop as [`Self::drain`] (its PROFILE facts report at
    /// exhaustion, as the streaming walk's do), never a bare `collect`.
    pub(crate) fn materialize(self) -> Result<Vec<Row>> {
        // The stopped-stream rule owns every source here, the generator's
        // materializing form included: a stopped stream hands over nothing
        // whatever entry point the consumer calls (`held_or_stopped`'s
        // contract, stated for the whole stream rather than the held arm).
        if self.stopped {
            return Ok(Vec::new());
        }
        let Self { rows, facts, .. } = self;
        match rows {
            Source::Generator(feed) => feed.materialize(),
            Source::Held(rows) => {
                if let Some(facts) = &facts {
                    facts.report();
                }
                Ok(rows.collect())
            }
            produced => RowStream {
                rows: produced,
                stopped: false,
                facts,
            }
            .drain(),
        }
    }

    /// Drain the stream into the held vector a not-yet-migrated clause arm
    /// still takes — the one handoff of the incremental migration
    /// (ADR-0660); it disappears as the arms become transformers. The
    /// vector is one retaining container, so every batch is billed as it
    /// lands (ADR-0463): a generated source drained here refuses under the
    /// workspace cap the way [`Self::materialize`] does, never after the
    /// whole expansion is already in hand.
    pub(crate) fn drain(&mut self) -> Result<Vec<Row>> {
        // A held container is handed over, not rebuilt: its buffer moves
        // out whole (the rows are already charged by whoever held them),
        // so a clause boundary costs no copy.
        if let Source::Held(rows) = &mut self.rows {
            let rows = std::mem::take(rows);
            self.report_facts();
            return Ok(held_or_stopped(rows, self.stopped));
        }
        let mut rows = Vec::new();
        let mut retained = WorkspaceCharge::empty();
        while let Some(batch) = self.next_batch()? {
            rows.extend(batch);
            retained.resize_with(|| crate::workspace::retained_rows_of(&rows))?;
        }
        Ok(rows)
    }

    /// The write clauses' barrier (ADR-0660's `pass` disposition): `SET`,
    /// `REMOVE`, `DELETE`, `MERGE`, `CREATE` and `FOREACH` pull the input
    /// one bounded batch at a time and hand each batch to `write` before
    /// pulling the next, and the write's transformed rows are emitted as
    /// this stream's own rows — one batch in flight, never the statement's
    /// whole output. A consumer that stops early (a page, a `LIMIT`) stops
    /// the rows, never the writes: the completing stream runs every
    /// remaining batch on `stop`, the `CALL { … } IN TRANSACTIONS`
    /// contract. `record` is the clause's PROFILE position: the emitted
    /// rows are counted as they are pulled, so the clause reports the rows
    /// its batches produced.
    pub(crate) fn write_barrier<'b>(
        self,
        mut write: impl FnMut(Vec<Row>) -> Result<Vec<Row>> + 'b,
        record: Option<(PlanMetricsSite<'a>, usize)>,
    ) -> Result<RowStream<'b>>
    where
        'a: 'b,
    {
        let mut stream = self;
        let mut pending: std::vec::IntoIter<Row> = Vec::new().into_iter();
        // The one batch in flight as this stream's state: the rows the last
        // write handed back, charged for as long as the consumer holds them.
        let mut retained = WorkspaceCharge::empty();
        let rows = std::iter::from_fn(move || loop {
            if let Some(row) = pending.next() {
                return Some(Ok(row));
            }
            let batch = match stream.next_batch() {
                Ok(Some(batch)) => batch,
                Ok(None) => return None,
                Err(error) => return Some(Err(error)),
            };
            let input_rows = batch.into_vec();
            // The batch is on the ledger while the write holds it.
            let input_charge = match WorkspaceCharge::retain_with(|| {
                crate::workspace::retained_rows_of(&input_rows)
            }) {
                Ok(charge) => charge,
                Err(error) => return Some(Err(error)),
            };
            let written = write(input_rows);
            drop(input_charge);
            let written = match written {
                Ok(written) => written,
                Err(error) => return Some(Err(error)),
            };
            if let Err(error) =
                retained.resize_with(|| crate::workspace::retained_rows_of(&written))
            {
                return Some(Err(error));
            }
            pending = written.into_iter();
        });
        Ok(RowStream::<'b>::emitting_to_completion(rows, record))
    }
}

/// The held handoff's one rule: a stopped stream hands over nothing — the
/// consumer stopped the rows, so `drain`, `materialize` and `into_held`
/// answer alike whatever entry point a consumer happens to call.
fn held_or_stopped(rows: std::vec::IntoIter<Row>, stopped: bool) -> Vec<Row> {
    if stopped {
        Vec::new()
    } else {
        rows.collect()
    }
}

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
    /// cloned off the feed by [`RowStream::profiled`] before a consumer
    /// takes it.
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
        iterator_item(Rc::clone(&self.stats).counted_step(|| self.generate()))
    }
}

/// The Iterator protocol's one reshape of a producer step's answer:
/// `Ok(None)` is exhaustion, every error an item.
fn iterator_item(generated: Result<Option<Row>>) -> Option<Result<Row>> {
    match generated {
        Ok(Some(row)) => Some(Ok(row)),
        Ok(None) => None,
        Err(error) => Some(Err(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_are_bounded_by_batch_rows() {
        let rows = (0..=BATCH_ROWS).map(|_| Row::new()).collect();
        let mut stream = RowStream::held(rows);
        let first = stream
            .next_batch()
            .expect("the held stream is infallible")
            .expect("a first batch");
        assert_eq!(first.into_iter().count(), BATCH_ROWS);
        let tail = stream
            .next_batch()
            .expect("the held stream is infallible")
            .expect("a tail batch");
        assert_eq!(tail.into_iter().count(), 1);
        assert!(stream
            .next_batch()
            .expect("the held stream is infallible")
            .is_none());
    }

    /// ADR-0660's write barrier: a write over an input longer than one
    /// batch is handed one bounded batch at a time — never the whole
    /// input — and its transformed rows are emitted as the stream's own
    /// rows, one batch in flight on the ledger.
    #[test]
    fn the_write_barrier_holds_one_batch_not_the_whole_input() {
        let batches = 4;
        let rows: Vec<Row> = (0..batches * BATCH_ROWS).map(|_| Row::new()).collect();
        let mut ledger = crate::test_support::roomy_ledger();
        let guard = relate_core::tokens::LedgerGuard::install(&mut ledger);
        let stream = RowStream::held(rows);
        let mut widths = Vec::new();
        let mut out = stream
            .write_barrier(
                |batch| {
                    widths.push(batch.len());
                    Ok(batch)
                },
                None,
            )
            .expect("the held stream is infallible");
        let mut emitted = 0usize;
        while let Some(batch) = out.next_batch().expect("the held stream is infallible") {
            emitted += batch.into_vec().len();
        }
        drop(out);
        assert_eq!(widths, vec![BATCH_ROWS; batches]);
        assert_eq!(emitted, batches * BATCH_ROWS);
        let one_batch = crate::workspace::retained_rows_bytes(BATCH_ROWS, 0);
        let (_, peak, _) = relate_core::tokens::workspace_meter().expect("a ledger is installed");
        // The input batch is in flight while the write runs; the emitted
        // batch is the stream's one row container. Both are one batch —
        // never the whole input beside the whole answer.
        assert!(
            peak <= 2 * one_batch,
            "the barrier billed {peak} bytes, more than one batch in flight plus one emitted"
        );
        drop(guard);
    }

    /// The one typed refusal assertion the capped-pool pins share: the
    /// charge that crossed the pool is the workspace limit, by variant and
    /// name, never a message scan.
    fn assert_pool_refusal(error: relate_core::Error) {
        assert!(
            matches!(
                error,
                relate_core::Error::ResourceLimit { name, .. }
                    if name == relate_core::Error::CONCURRENT_LOGICAL_WORKSPACE_LIMIT
            ),
            "the answer's charge must be the pool's: {error}"
        );
    }

    /// The write's transformed output streams: a write over more answer
    /// than the pool holds answers under it, because only one batch is ever
    /// in flight — the collected form is what refuses (the sibling
    /// `materialize` pin).
    #[test]
    fn the_write_barrier_streams_an_answer_past_the_pool() {
        let one_batch = crate::workspace::retained_rows_bytes(BATCH_ROWS, 0);
        let mut ledger = crate::test_support::capped_ledger(3 * one_batch);
        let guard = relate_core::tokens::LedgerGuard::install(&mut ledger);
        let rows: Vec<Row> = (0..4 * BATCH_ROWS).map(|_| Row::new()).collect();
        let mut stream = RowStream::held(rows)
            .write_barrier(Ok, None)
            .expect("the held stream is infallible");
        let mut emitted = 0usize;
        while let Some(batch) = stream
            .next_batch()
            .expect("a streamed answer never crosses the pool")
        {
            emitted += batch.into_vec().len();
        }
        assert_eq!(emitted, 4 * BATCH_ROWS);
        drop(guard);
    }

    /// A produced sequence materializes through the charged batch loop:
    /// the collecting consumer's container refuses at the crossing batch,
    /// never after the whole result is already in hand.
    #[test]
    fn materialize_refuses_a_produced_answer_past_the_pool() {
        let one_batch = crate::workspace::retained_rows_bytes(BATCH_ROWS, 0);
        let mut ledger = crate::test_support::capped_ledger(3 * one_batch);
        let guard = relate_core::tokens::LedgerGuard::install(&mut ledger);
        let rows = (0..4 * BATCH_ROWS).map(|_| Ok::<Row, relate_core::Error>(Row::new()));
        assert_pool_refusal(
            RowStream::emitting(rows)
                .materialize()
                .expect_err("an answer past the pool must refuse"),
        );
        drop(guard);
    }

    #[test]
    fn stop_generates_nothing_further() {
        let mut stream = RowStream::held(vec![Row::new()]);
        stream.stop().expect("the held stream is infallible");
        assert!(stream
            .next_batch()
            .expect("the held stream is infallible")
            .is_none());
        assert!(stream
            .drain()
            .expect("the held stream is infallible")
            .is_empty());
    }

    /// A completing source runs out its remaining work when the consumer
    /// stops (ADR-0660: a page stops the rows, not the transactions); the
    /// rows past the page are discarded, never emitted.
    #[test]
    fn a_stop_finishes_a_completing_source() {
        let pulls = std::rc::Rc::new(std::cell::Cell::new(0));
        let counter = std::rc::Rc::clone(&pulls);
        let rows = (0..3 * BATCH_ROWS).map(move |_| {
            counter.set(counter.get() + 1);
            Ok(Row::new())
        });
        let mut stream = RowStream::emitting_to_completion(rows, None);
        assert_eq!(
            stream
                .next_batch()
                .expect("the produced stream is infallible")
                .expect("a first batch")
                .into_iter()
                .count(),
            BATCH_ROWS
        );
        stream.stop().expect("the produced stream is infallible");
        assert_eq!(
            pulls.get(),
            3 * BATCH_ROWS,
            "every remaining row was pulled"
        );
        assert!(stream
            .next_batch()
            .expect("the stopped stream yields nothing")
            .is_none());
    }

    /// A produced source batches exactly like a held one: the same batch
    /// bound, the same exhaustion, and no rows in hand to probe.
    #[test]
    fn a_produced_source_batches_like_a_held_one() {
        assert!(RowStream::emitting(std::iter::empty()).probe().is_empty());
        let mut stream = RowStream::emitting(
            (0..=BATCH_ROWS).map(|_| Ok::<Row, relate_core::Error>(Row::new())),
        );
        let first = stream
            .next_batch()
            .expect("the produced stream is infallible")
            .expect("a first batch");
        assert_eq!(first.into_iter().count(), BATCH_ROWS);
        assert_eq!(
            stream
                .next_batch()
                .expect("the produced stream is infallible")
                .expect("a tail batch")
                .into_iter()
                .count(),
            1
        );
        assert!(stream
            .next_batch()
            .expect("the produced stream is infallible")
            .is_none());
    }

    /// A producer's error ends the stream at the pull that produced it; a
    /// batch already delivered is not retracted.
    #[test]
    fn a_producer_error_ends_the_stream() {
        let rows: Vec<Result<Row>> = (0..=BATCH_ROWS)
            .map(|_| Ok(Row::new()))
            .chain([Err(relate_core::Error::internal("boom"))])
            .collect();
        let mut stream = RowStream::emitting(rows.into_iter());
        assert_eq!(
            stream
                .next_batch()
                .expect("the first batch is fine")
                .expect("a batch")
                .into_iter()
                .count(),
            BATCH_ROWS
        );
        assert!(stream.next_batch().is_err());
        assert!(stream
            .next_batch()
            .expect("the ended stream yields nothing")
            .is_none());
    }
}
