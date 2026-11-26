//! The per-clause PROFILE tick and the streamed-generator facts (`UNWIND`,
//! `LOAD CSV`) the clause dispatcher (`dispatch.rs`) reports through it.
//! The parked record every streamed form reports at — with the `CALL { … }
//! IN TRANSACTIONS` batch-facts cell it can carry (REL8-2047) — lives in
//! the PROFILE vocabulary module (`crate::metrics`) beside the site and
//! the record it is built from.

use std::cell::Cell;
use std::rc::Rc;
// `web_time` on wasm32: `std::time::Instant::now()` traps on
// `wasm32-unknown-unknown`, and PROFILE is a user-reachable path in the
// browser build — the clock must read, not panic (the facade uses the
// same shim).
use web_time::Instant;

use relate_core::Result;

use crate::eval::rows_logical_bytes;
use crate::metrics::{ParkedRecord, PlanMetricsSite};
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::match_clause::LadderFacts;
use super::profile_chain::clause_record;
use super::{DeltaSnapshot, StageDelta};

/// The PROFILE path's per-clause snapshot: the site's sink and its
/// `(stage, part)` position, plus the arm-start facts the tick owns — the
/// `Instant`, the store-read and phase-time snapshots, and the view whose
/// expression-bytes meter it samples. Owning the site makes the half-states
/// — a tick with no sink to report to, a sink with no position to key it —
/// unrepresentable. Each metric is the delta the clause's arm moved its
/// counter, so the facts ride the executor's existing counters with no
/// second counting mechanism.
pub(super) struct ProfileTick<'a> {
    /// The statement's PROFILE site, held whole — the sink↔position
    /// pairing stays the site type's invariant, never re-spelled as
    /// parallel flat fields here.
    site: PlanMetricsSite<'a>,
    /// The six-metric snapshot at the arm's start (LABS-369's read kinds,
    /// LABS-448's phase times, LABS-645's zone skips, ADR-0478's frontier
    /// counters, ADR-0463's expression meter, the clock) — the one
    /// snapshot family the streamed stage windows share
    /// (`profile_chain.rs`'s `DeltaSnapshot`); `record` reads its delta.
    start: DeltaSnapshot,
    /// The store-read arm flag's prior value (LABS-805), captured at the
    /// arm and restored by the drop — a pre-armed thread (the test
    /// builds' default) is never disarmed by a profiled clause.
    store_profiling_was_armed: bool,
    /// The view whose expression-built meter the snapshot samples — kept
    /// beside it for `record`'s end-of-arm reading.
    graph: &'a ExecGraph<'a>,
    /// Time a fused producer spent inside this arm and reports itself (a
    /// streamed `UNWIND`'s generator), excluded from this clause's time.
    excluded_ns: u128,
    /// The whole fact a fused consumer took as rows were emitted (a
    /// streamed `MATCH` folds them into its aggregation, or feeds the run
    /// of stages after it): the rows that flowed through, and the run's
    /// window totals — the work the arm's own delta must subtract, so the
    /// head clause's record states its own clause's work and never the
    /// run's (the immediate-downstream rule the stages apply to each
    /// other, applied once more at the arm boundary). The streamed clause
    /// itself holds nothing: its record's memory bytes are zero, stated
    /// at the record — not carried here as data.
    pub(super) streamed: Option<StreamedClause>,
}

/// One streamed clause's PROFILE facts: the rows its consumer took as
/// they were emitted, and the run's window totals the arm's delta
/// subtracts. The absence — a directly fed fold, whose offer-time
/// grouping is the head clause's own work, the established fold
/// semantics — is interpreted once, at the arm's construction site, as
/// the default delta: nothing to subtract.
#[derive(Clone, Copy, Default)]
pub(super) struct StreamedClause {
    pub(super) rows: u64,
    pub(super) downstream: StageDelta,
}

impl Drop for ProfileTick<'_> {
    /// Disarm the phase stamps (LABS-448) and restore the store-read
    /// counters' prior arm state (LABS-805): the arm is over, so the
    /// next clause's stamps and counter flushes arm with its own tick —
    /// and a non-PROFILEd statement following never pays an `Instant` or
    /// moves a counter. The restore, never an unconditional disarm: a
    /// pre-armed thread (the test builds' default) keeps its state.
    fn drop(&mut self) {
        crate::ctx::disarm_phase_profiling();
        super::restore_store_profiling(self.store_profiling_was_armed);
    }
}

pub(super) fn profile_tick<'a>(
    graph: &'a ExecGraph<'a>,
    site: PlanMetricsSite<'a>,
) -> ProfileTick<'a> {
    // The clause's clearing boundary (LABS-369): a non-PROFILE statement
    // records walk starts without ever taking one, so the slot must be
    // emptied here, before the clause's arm runs — otherwise the next
    // PROFILE statement's first walking clause would drop its own fresh
    // start and report the earlier statement's walk.
    let _ = super::take_walk_start();
    // LABS-448: arm the phase stamps for this arm — the only site that
    // does, so the stamps' `Instant` reads happen exclusively inside a
    // profiled clause arm. LABS-805: arm the store-read counters beside
    // them — the walk tallies' per-run flushes land only while armed —
    // keeping the flag's prior value for the drop's restore.
    crate::ctx::arm_phase_profiling();
    let store_profiling_was_armed = super::arm_store_profiling();
    ProfileTick {
        site,
        start: DeltaSnapshot::now(graph),
        store_profiling_was_armed,
        graph,
        excluded_ns: 0,
        streamed: None,
    }
}

impl ProfileTick<'_> {
    /// Report one clause operator's metrics — rows produced, store
    /// accesses, row-materialization bytes, elapsed — to the sink the
    /// tick carries, at the position the statement-level walker keyed.
    /// `batched` (ADR-0468, LABS-359) says the clause ran the batch
    /// scan/filter/project chain; `zone_skips` (LABS-645) counts the
    /// conjunct-batch evaluations its zone metadata provably replaced
    /// with the correct empty survivor set — the delta of the counter
    /// snapshot the tick took, which only ever increases (the
    /// store-access subtraction's invariant, never a clamp). `frontier`
    /// (ADR-0478) carries the frontier family's run counters — the
    /// delta of the counter snapshot the tick took, the same
    /// subtraction's shape.
    /// `ladder` is the clause's `MATCH` ladder result (LABS-1117); the
    /// tick projects the `batched` flag and the rung from its operator
    /// here — the one projection, so the two facts cannot disagree and a
    /// non-`MATCH` clause's `None` carries both — and the frontier decline
    /// (LABS-1437) from the same value.
    pub(super) fn record(&self, clause: usize, held_rows: &[Row], ladder: Option<LadderFacts>) {
        // The arm's own facts: the one snapshot family's delta (the same
        // `DeltaSnapshot` the streamed stage windows accumulate) — minus a
        // streamed run's window totals, so the head clause's record states
        // its own clause's work and never the run's — minus the time a
        // fused producer reported for itself. A streamed clause holds
        // nothing: zero memory bytes, stated here.
        let mut own = DeltaSnapshot::now(self.graph).since(self.start);
        let (rows, memory_bytes) = match self.streamed {
            Some(StreamedClause { rows, downstream }) => {
                own = own.minus(downstream);
                (rows, 0)
            }
            None => (held_rows.len() as u64, rows_logical_bytes(held_rows)),
        };
        own.time_ns = own.time_ns.saturating_sub(self.excluded_ns);
        self.site.sink.record_clause(clause_record(
            &self.site,
            clause,
            own,
            rows,
            memory_bytes,
            super::take_walk_start(),
            ladder,
        ));
    }
}

/// What a streamed generator clause (`UNWIND`, `LOAD CSV`) reports to
/// PROFILE once its consumer drained it. The feed yields one row at a
/// time, so the memory it holds is one row: the row's size is recorded
/// (batch of one × row size), not a retained total. The generation time is
/// measured only when PROFILE asked for it, so the plain path never reads
/// a clock per row.
#[derive(Default)]
pub(crate) struct StreamedStats {
    rows: Cell<u64>,
    /// Logical bytes of one generated row: the in-flight working set.
    row_bytes: Cell<u64>,
    time_ns: Cell<u128>,
    /// The clock policy alone: whether `timed_step` reads a clock per
    /// step. The row count and the first row's size are recorded whatever
    /// this says — one walk on the first row, never a per-row cost.
    timed: bool,
}

impl StreamedStats {
    /// `timed` is the PROFILE clock policy only — an untimed feed still
    /// counts its rows and records the first row's size.
    pub(crate) fn new(timed: bool) -> Self {
        Self {
            timed,
            ..Self::default()
        }
    }

    pub(super) fn rows(&self) -> u64 {
        self.rows.get()
    }

    pub(super) fn row_bytes(&self) -> u64 {
        self.row_bytes.get()
    }

    /// Nanoseconds spent generating rows (zero unless timed).
    pub(super) fn time_ns(&self) -> u128 {
        self.time_ns.get()
    }

    /// Record one generated row: the count, and the first row's size as
    /// the feed's in-flight working set.
    pub(super) fn note_row(&self, row: &Row) {
        self.rows.set(self.rows.get() + 1);
        if self.rows.get() == 1 {
            self.row_bytes
                .set(rows_logical_bytes(std::slice::from_ref(row)));
        }
    }

    /// Run one generator step under the PROFILE clock — the one timing
    /// wrap every streamed producer shares. An untimed feed runs bare: the
    /// clock is read per step only when PROFILE asked for the clause's
    /// time.
    pub(super) fn timed_step<T>(&self, step: impl FnOnce() -> T) -> T {
        if !self.timed {
            return step();
        }
        let started = Instant::now();
        let out = step();
        self.time_ns
            .set(self.time_ns.get() + started.elapsed().as_nanos());
        out
    }

    /// Run one producer step under the PROFILE clock and record the row it
    /// produced — the one counted-step wrap every timed producer shares,
    /// the generator spine and the completing constructors alike.
    pub(super) fn counted_step(
        &self,
        step: impl FnOnce() -> Result<Option<Row>>,
    ) -> Result<Option<Row>> {
        let out = self.timed_step(step);
        if let Ok(Some(row)) = out.as_ref() {
            self.note_row(row);
        }
        out
    }
}

/// A streamed generator's PROFILE facts: the statistics its generator
/// filled and the PROFILE record its facts are reported at. The stream
/// owns them; a shared handle keeps them reachable after the stream is
/// consumed, so the projection can exclude the generator's time from its
/// own record.
pub(super) struct ParkedFacts<'a> {
    stats: Rc<StreamedStats>,
    record: ParkedRecord<'a>,
    /// Reported once: the stream reports at drain, stop or error, and the
    /// consumer's close reports again only to read the time it excludes.
    reported: Cell<bool>,
}

impl<'a> ParkedFacts<'a> {
    /// The facts both parked forms carry — the stats handle the feed owns
    /// plus the PROFILE record — reported once. `None` for a stream with
    /// no record: the one spelling of "this stream reports nothing", so
    /// the constructors store the answer directly and `facts.is_some()`
    /// states "this stream has a clause record".
    pub(super) fn new(
        stats: Rc<StreamedStats>,
        record: Option<ParkedRecord<'a>>,
    ) -> Option<Rc<Self>> {
        record.map(|record| {
            Rc::new(Self {
                stats,
                record,
                reported: Cell::new(false),
            })
        })
    }

    /// Report the generator's own clause record, once.
    pub(super) fn report(&self) {
        if self.reported.replace(true) {
            return;
        }
        record_streamed_clause(&self.record, &self.stats);
    }
}

/// Close a fused generator after its consumer drained it: the consumer's
/// clock excludes the generator's time, and the clause reports its own.
pub(super) fn finish_parked_feed(
    parked: Option<std::rc::Rc<ParkedFacts<'_>>>,
    tick: Option<&mut ProfileTick<'_>>,
) {
    let Some(parked) = parked else {
        return;
    };
    if let Some(tick) = tick {
        tick.excluded_ns = parked.stats.time_ns();
    }
    parked.report();
}

/// Report a streamed generator's PROFILE facts after its consumer drained
/// it: the rows it generated, one row's size as its in-flight memory (it
/// yields one row at a time, so its working set is one row, not the
/// retained total), no expression bytes (no list was built), and the time
/// its generator ran, which the consumer's own record excludes. The
/// record's cell — the `IN TRANSACTIONS` arm's accumulated batch facts
/// (REL8-2047), absent on every other streamed clause — stamps onto the
/// record at this one site.
fn record_streamed_clause(record: &ParkedRecord<'_>, stats: &StreamedStats) {
    let site = &record.site;
    let mut noted = clause_record(
        site,
        record.clause,
        // The generator's own facts are its time alone: it reads no store,
        // builds no expression bytes, walks nothing.
        StageDelta {
            time_ns: stats.time_ns(),
            ..StageDelta::default()
        },
        stats.rows(),
        stats.row_bytes(),
        None,
        None,
    );
    // REL8-2047: the batch facts the arm's loop noted land on its own
    // record — the one clause kind that runs batches, so the stamp has one
    // site, never a parameter every other record constructor passes as
    // zero.
    noted.intx = record.intx_facts();
    site.sink.record_clause(noted);
}
