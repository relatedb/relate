//! The per-clause PROFILE tick and the streamed-`UNWIND` facts the clause
//! dispatcher (`dispatch.rs`) reports through it.

use std::rc::Rc;
// `web_time` on wasm32: `std::time::Instant::now()` traps on
// `wasm32-unknown-unknown`, and PROFILE is a user-reachable path in the
// browser build — the clock must read, not panic (the facade uses the
// same shim).
use web_time::Instant;

use crate::ctx::{ClauseMetricsRecord, PlanMetricsSite};
use crate::eval::rows_logical_bytes;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::aggregate::Aggregation;
use super::match_clause::{LadderFacts, MatchOperator};
use super::unwind::{UnwindFeed, UnwindStats};
use super::RowFeed;

/// The expression-built meter's typed per-clause snapshot (ADR-0463): the
/// same delta protocol as `StoreReads` and `PhaseTimes` — a value taken at
/// the arm's start and a loud `since` delta, never a bare u64 pair.
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
fn match_facts(operator: Option<MatchOperator>) -> (bool, Option<crate::ctx::KernelRung>) {
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
    reads: crate::ctx::StoreReads,
    /// The phase-time snapshot at the arm's start (LABS-448): the tick's
    /// `phases` is the delta against it — and arming the phase stamps is
    /// this tick's first act, disarming its last, so the stamps never run
    /// outside a profiled statement's arm.
    phases: crate::ctx::PhaseTimes,
    /// The zone-skip counter snapshot at the arm's start (LABS-645): the
    /// tick's `zone_skips` is the delta against it — one thread-local
    /// read per clause, the store-access snapshot's pattern.
    zone_skips: u64,
    /// The frontier family's counter snapshot at the arm's start
    /// (ADR-0478): the tick's `frontier` is the delta against it — one
    /// thread-local read per clause, the zone-skip snapshot's pattern.
    frontier: crate::ctx::FrontierCounters,
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

/// Report a streamed `UNWIND`'s PROFILE facts after its consumer drained
/// it: the rows it generated, one row's size as its in-flight memory (it
/// yields one row at a time, so its working set is one row, not the
/// retained total), no expression bytes (no list was built), and the time
/// its generator ran, which the consumer's own record excludes.
/// What a streamed `UNWIND` reports once its consumer drained it: the
/// statistics its generator filled and the PROFILE position its facts are
/// reported at.
pub(super) struct UnwindFacts<'a> {
    pub(super) stats: Rc<UnwindStats>,
    pub(super) record: Option<(PlanMetricsSite<'a>, usize)>,
}

/// A streamed `UNWIND` waiting for its consumer: the generator and its own
/// facts. One value per streamed clause, so two of them in a statement never
/// share counters.
pub(super) struct ParkedUnwind<'a> {
    pub(super) feed: UnwindFeed<'a>,
    pub(super) facts: UnwindFacts<'a>,
}

/// The feed a `WITH`/`RETURN` consumes: the aggregation a `MATCH` folded its
/// rows into, the parked generator when an `UNWIND` was fused into it (with
/// the facts to report once it is drained), otherwise the held rows.
pub(super) fn take_feed<'a>(
    pending: &mut Option<ParkedUnwind<'a>>,
    folded: &mut Option<Aggregation<'a>>,
    rows: &mut Vec<Row>,
) -> (RowFeed<'a>, Option<UnwindFacts<'a>>) {
    if let Some(aggregation) = folded.take() {
        return (RowFeed::Folded(Box::new(aggregation)), None);
    }
    match pending.take() {
        Some(ParkedUnwind { feed, facts }) => (RowFeed::Unwind(feed), Some(facts)),
        None => (RowFeed::Held(std::mem::take(rows)), None),
    }
}

/// Close a fused `UNWIND` after its consumer drained it: the consumer's
/// clock excludes the generator's time, and the clause reports its own.
pub(super) fn finish_parked_unwind(
    parked: Option<UnwindFacts<'_>>,
    tick: Option<&mut ProfileTick<'_>>,
) {
    let Some(UnwindFacts { stats, record }) = parked else {
        return;
    };
    if let Some(tick) = tick {
        tick.excluded_ns = stats.time_ns();
    }
    record_streamed_unwind(record, &stats);
}

fn record_streamed_unwind(record: Option<(PlanMetricsSite<'_>, usize)>, unwound: &UnwindStats) {
    let Some((site, clause)) = record else {
        return;
    };
    site.sink.record_clause(ClauseMetricsRecord {
        stage: site.stage,
        part: site.part,
        clause,
        rows: unwound.rows(),
        memory_bytes: unwound.row_bytes(),
        expression_built_bytes: 0,
        time_ns: unwound.time_ns(),
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
