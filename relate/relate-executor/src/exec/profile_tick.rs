//! The per-clause PROFILE tick and the streamed-generator facts (`UNWIND`,
//! `LOAD CSV`) the clause dispatcher (`dispatch.rs`) reports through it.

use std::cell::Cell;
// `web_time` on wasm32: `std::time::Instant::now()` traps on
// `wasm32-unknown-unknown`, and PROFILE is a user-reachable path in the
// browser build — the clock must read, not panic (the facade uses the
// same shim).
use web_time::Instant;

use crate::eval::rows_logical_bytes;
use crate::metrics::{ClauseMetricsRecord, PlanMetricsSite};
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::feed::ParkedFacts;
use super::match_clause::{LadderFacts, MatchOperator};

/// The expression-built meter's typed per-clause snapshot (ADR-0463): a
/// snapshot taken at the arm's start and its delta at the record. Unlike
/// `StoreReads`, `PhaseTimes` and `FrontierCounters` — monotone counters
/// with a checked delta — this meter is a live allocation figure that
/// falls when an arm releases expression memory, so its delta saturates
/// and yields a bare byte count, never a counter type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ExpressionBytes(u64);

impl ExpressionBytes {
    fn now(graph: &ExecGraph<'_>) -> Self {
        Self(graph.expression_built_bytes())
    }

    fn since(self, start: Self) -> u64 {
        self.0.saturating_sub(start.0)
    }
}

/// The two PROFILE facts a clause's `MATCH` operator projects to
/// (LABS-1117): the ADR-0468 `batched` flag and the traversal rung. Both
/// derive from the ladder's one returned value — the dispatcher's only
/// projection, so the two cannot disagree, and the `None` operator (no
/// ladder ran) is stated once.
fn match_facts(operator: Option<MatchOperator>) -> (bool, Option<crate::metrics::KernelRung>) {
    match operator {
        None => (false, None),
        Some(MatchOperator::BatchChain) => (true, None),
        Some(MatchOperator::Rung(rung)) => (false, Some(rung)),
    }
}

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
    t0: Instant,
    /// The store-read arm flag's prior value (LABS-805), captured at the
    /// arm and restored by the drop — a pre-armed thread (the test
    /// builds' default) is never disarmed by a profiled clause.
    store_profiling_was_armed: bool,
    /// The read-kind snapshot at the arm's start (LABS-369): the tick's
    /// `reads` is the delta against it, three thread-local reads per
    /// clause, never a per-row cost — and the one snapshot, since the
    /// store-access total is the kinds' sum.
    reads: crate::metrics::StoreReads,
    /// The phase-time snapshot at the arm's start (LABS-448): the tick's
    /// `phases` is the delta against it — and arming the phase stamps is
    /// this tick's first act, disarming its last, so the stamps never run
    /// outside a profiled statement's arm.
    phases: crate::metrics::PhaseTimes,
    /// The zone-skip counter snapshot at the arm's start (LABS-645): the
    /// tick's `zone_skips` is the delta against it — one thread-local
    /// read per clause, the store-access snapshot's pattern.
    zone_skips: u64,
    /// The frontier family's counter snapshot at the arm's start
    /// (ADR-0478): the tick's `frontier` is the delta against it — one
    /// thread-local read per clause, the zone-skip snapshot's pattern.
    frontier: crate::metrics::FrontierCounters,
    /// The view whose expression-built meter the tick samples at the arm's
    /// start and reads again in `record` (ADR-0463) — the third counter
    /// family it owns, beside `reads` and `phases`.
    graph: &'a ExecGraph<'a>,
    /// The meter's typed snapshot when the arm started; `record` reports
    /// `ExpressionBytes::now(..).since(start)`, the same delta protocol as
    /// `StoreReads` and `PhaseTimes`.
    expression_built_bytes_start: ExpressionBytes,
    /// Time a fused producer spent inside this arm and reports itself (a
    /// streamed `UNWIND`'s generator), excluded from this clause's time.
    excluded_ns: u128,
    /// Rows a fused consumer took as they were emitted (a streamed `MATCH`
    /// folds them into its aggregation): the clause's row count when its own
    /// output holds none.
    pub(super) streamed_rows: Option<u64>,
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
        t0: Instant::now(),
        store_profiling_was_armed,
        reads: super::store_reads(),
        phases: crate::ctx::phase_times(),
        zone_skips: super::zone_skips(),
        frontier: super::frontier_counters(),
        graph,
        expression_built_bytes_start: ExpressionBytes::now(graph),
        excluded_ns: 0,
        streamed_rows: None,
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
    pub(super) fn record(&self, clause: usize, rows: &[Row], ladder: Option<LadderFacts>) {
        let (batched, kernel) = match_facts(ladder.map(|facts| facts.operator));
        let frontier_decline = ladder.and_then(|facts| facts.frontier_decline);
        let reads = super::store_reads().since(self.reads);
        self.site.sink.record_clause(ClauseMetricsRecord {
            stage: self.site.stage,
            part: self.site.part,
            clause,
            rows: self.streamed_rows.unwrap_or(rows.len() as u64),
            memory_bytes: rows_logical_bytes(rows),
            expression_built_bytes: ExpressionBytes::now(self.graph)
                .since(self.expression_built_bytes_start),
            time_ns: self
                .t0
                .elapsed()
                .as_nanos()
                .saturating_sub(self.excluded_ns),
            reads,
            phases: crate::ctx::phase_times().since(self.phases),
            start: super::take_walk_start(),
            kernel,
            batched,
            zone_skips: super::zone_skips() - self.zone_skips,
            frontier: super::frontier_counters().since(self.frontier),
            frontier_decline,
        });
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
    timed: bool,
}

impl StreamedStats {
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
        if self.timed && self.rows.get() == 1 {
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
}

/// Close a fused generator after its consumer drained it: the consumer's
/// clock excludes the generator's time, and the clause reports its own.
pub(super) fn finish_parked_feed(
    parked: Option<ParkedFacts<'_>>,
    tick: Option<&mut ProfileTick<'_>>,
) {
    let Some(ParkedFacts { stats, record }) = parked else {
        return;
    };
    if let Some(tick) = tick {
        tick.excluded_ns = stats.time_ns();
    }
    record_streamed_clause(record, &stats);
}

/// Report a streamed generator's PROFILE facts after its consumer drained
/// it: the rows it generated, one row's size as its in-flight memory (it
/// yields one row at a time, so its working set is one row, not the
/// retained total), no expression bytes (no list was built), and the time
/// its generator ran, which the consumer's own record excludes.
fn record_streamed_clause(record: Option<(PlanMetricsSite<'_>, usize)>, stats: &StreamedStats) {
    let Some((site, clause)) = record else {
        return;
    };
    site.sink.record_clause(ClauseMetricsRecord {
        stage: site.stage,
        part: site.part,
        clause,
        rows: stats.rows(),
        memory_bytes: stats.row_bytes(),
        expression_built_bytes: 0,
        time_ns: stats.time_ns(),
        reads: Default::default(),
        phases: Default::default(),
        start: None,
        kernel: None,
        batched: false,
        zone_skips: 0,
        frontier: Default::default(),
        frontier_decline: None,
    });
}
