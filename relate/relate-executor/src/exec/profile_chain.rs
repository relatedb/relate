//! The streamed MATCH chain's PROFILE sidecar (REL8-1904, beside
//! `profile_tick.rs`'s per-clause tick and the parked generator's
//! `StreamedStats`): the chain streams under PROFILE too — a held path
//! would materialize the clause's whole output, changing the memory shape
//! of the very statement PROFILE reports — and each consumed stage
//! observes itself instead: one window per batch flush and one over the
//! drain, the counter deltas accumulated into the stage's facts, and one
//! record per consumed clause when the run drains, with the immediate
//! downstream stage's totals subtracted out. `match_chain.rs` keeps the
//! stage traversal; this module owns the accounting vocabulary.

use std::cell::{Cell, OnceCell};

use relate_core::Result;

use crate::metrics::{BatchFacts, ClauseMetricsRecord, InTxBatches, PlanMetricsSite, WalkStart};
use crate::overlay::ExecGraph;

use super::match_clause::{LadderFacts, MatchOperator};
use super::pattern_policies::RowFold;
use super::{DeltaSnapshot, StageDelta};

/// One clause's record — the one spelling every record site shares
/// (`ProfileTick::record` for a clause arm, `StageStats::report` for a
/// consumed stage, the parked generator's record in `profile_tick.rs`):
/// the family delta's own figures, the per-site rows, memory, start and
/// time, and the ladder's facts projected once, here. The sites differ
/// only in those.
pub(crate) fn clause_record(
    site: &PlanMetricsSite<'_>,
    clause: usize,
    own: StageDelta,
    rows: u64,
    memory_bytes: u64,
    start: Option<WalkStart>,
    ladder: Option<LadderFacts>,
) -> ClauseMetricsRecord {
    let (batched, kernel, frontier_decline) = match ladder {
        None => (false, None, None),
        Some(LadderFacts {
            operator,
            frontier_decline,
        }) => {
            let (batched, kernel) = match operator {
                MatchOperator::BatchChain => (true, None),
                MatchOperator::Rung(rung) => (false, Some(rung)),
            };
            (batched, kernel, frontier_decline)
        }
    };
    ClauseMetricsRecord {
        stage: site.stage,
        part: site.part,
        clause,
        rows,
        memory_bytes,
        // `own.time_ns` is the record's time: the per-site adjustments
        // (the tick's excluded producer time) applied before it arrives.
        time_ns: own.time_ns,
        expression_built_bytes: own.expression_built.get(),
        reads: own.reads,
        phases: own.phases,
        start,
        kernel,
        batch: BatchFacts {
            ran: batched,
            zone_skips: own.zone_skips.get(),
        },
        frontier: own.frontier,
        frontier_decline,
        memo: own.memo,
        // REL8-2047: the `IN TRANSACTIONS` batch facts are arm-local, not
        // counter-snapshot deltas — the shared record zeroed here, and the
        // arm's parked facts stamp its accumulated cell onto the record at
        // the one reporting site that runs for that arm.
        intx: InTxBatches::default(),
    }
}

/// One stage's PROFILE facts — present only when the run observes itself
/// (`Stage::stats`), so every method here is already on the observing
/// path: an unprofiled run has no sidecar, no clock, no counter read, no
/// walk-start swap, `StreamedStats::timed_step`'s rule by construction.
pub(crate) struct StageStats {
    /// Rows the stage emitted to its consumer, counted at the emission
    /// boundary through [`CountingFold`]: a fold consumer takes rows as
    /// they are offered, so the transform's returned vector holds none of
    /// them and cannot be the count.
    rows_out: Cell<u64>,
    /// The accumulated window totals.
    totals: Cell<StageDelta>,
    /// The stage's first operator-ladder facts (LABS-1117).
    facts: OnceCell<LadderFacts>,
    /// The stage's first walk start (LABS-369), captured through the
    /// walk-start swap while the stage's own ladder runs.
    start: OnceCell<WalkStart>,
}

impl StageStats {
    pub(crate) fn new() -> Self {
        Self {
            rows_out: Cell::new(0),
            totals: Cell::new(StageDelta::default()),
            facts: OnceCell::new(),
            start: OnceCell::new(),
        }
    }

    /// The stage's first ladder facts, when a ladder ran.
    pub(crate) fn note_facts(&self, facts: LadderFacts) {
        let _ = self.facts.set(facts);
    }

    /// The stage's own walk start, taken while its ladder held the
    /// first-wins slot.
    pub(crate) fn note_start(&self, start: Option<WalkStart>) {
        if let Some(start) = start {
            let _ = self.start.set(start);
        }
    }

    /// Note rows the stage emitted to its consumer — the one write path
    /// for the emission-boundary count, both `CountingFold` arms through
    /// it.
    fn note_emitted(&self, rows: u64) {
        self.rows_out.set(self.rows_out.get() + rows);
    }

    /// Run `work` inside one PROFILE window: the elapsed time and the
    /// counter deltas land in the stage's totals, the walk-start slot is
    /// swapped for the duration (the stage's own first walk records; the
    /// producer's start is handed back on every path, `Drop`'s rule), and
    /// the result passes through.
    pub(crate) fn window<T>(
        &self,
        graph: &ExecGraph<'_>,
        work: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let _swap = super::WalkStartSwap::take();
        let start = DeltaSnapshot::now(graph);
        let out = work();
        let mut totals = self.totals.get();
        totals.accumulate(DeltaSnapshot::now(graph).since(start));
        self.totals.set(totals);
        out
    }

    /// Report this stage's own facts as one consumed clause's record:
    /// its emitted rows, its own transform's time, reads, phases and
    /// expression bytes (the immediate downstream stage's totals, which
    /// its windows contain, subtracted out), its first ladder facts and
    /// walk start, and `memory_bytes` — the retained output a terminal
    /// stage's collector holds until the barrier consumes it (every other
    /// stage's in-flight batch is bounded, so it reports zero). Returns
    /// the stage's totals for the upstream stage's subtraction.
    pub(crate) fn report(
        &self,
        site: &PlanMetricsSite<'_>,
        clause: usize,
        downstream: StageDelta,
        memory_bytes: u64,
    ) -> StageDelta {
        let own = self.totals.get().minus(downstream);
        site.sink.record_clause(clause_record(
            site,
            clause,
            own,
            self.rows_out.get(),
            memory_bytes,
            self.start.get().copied(),
            self.facts.get().copied(),
        ));
        self.totals.get()
    }
}

/// A stage's consumer that counts the rows offered to it before handing
/// each on — the emission boundary a fold consumer hides (it takes rows
/// as they are offered, so the transform's returned vector is empty).
/// Only an observing stage wraps its consumer in one.
pub(crate) struct CountingFold<'a> {
    inner: &'a dyn RowFold,
    stats: &'a StageStats,
}

impl<'a> CountingFold<'a> {
    pub(crate) fn new(inner: &'a dyn RowFold, stats: &'a StageStats) -> Self {
        Self { inner, stats }
    }
}

impl RowFold for CountingFold<'_> {
    fn offer(&self, graph: &ExecGraph<'_>, row: crate::row::Row) -> Result<()> {
        self.stats.note_emitted(1);
        self.inner.offer(graph, row)
    }

    fn offer_container(&self, graph: &ExecGraph<'_>, rows: Vec<crate::row::Row>) -> Result<()> {
        self.stats.note_emitted(rows.len() as u64);
        self.inner.offer_container(graph, rows)
    }
}
