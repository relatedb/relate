//! The plan-grade probe (ADR-0687 decision 6): one execution's measured store
//! reads — the thread-local counters behind the PROFILE `Db Hits` metric, read
//! once at the statement's end and never through the PROFILE sink. The
//! `relate` crate arms it around a cache-served statement's dispatch and reads
//! it after, so the cache entry's grade moves outside every cache lock.
//!
//! A statement entry drives its plan from exactly one root row (the one
//! statement-entry operation mints it), so the per-execution read total is
//! the whole measurement — no per-input-row normalisation is recorded
//! anywhere, and a route that never reaches the executor reads a zero delta.

use super::profiling::store_reads;

/// One execution's plan-grade probe (ADR-0687 decision 6): the store-read
/// counters armed and snapshotted at [`Self::begin`], the reading taken at
/// [`Self::finish`]. Dropping an unfinished probe restores the counters' arm
/// flag and nothing else — an execution that never finished grades nothing,
/// and the counters it left armed never extend past the run.
pub struct GradeProbe {
    /// The counters' prior arm flag, `None` once restored (the drop after a
    /// `finish` restores nothing a second time).
    prior: Option<bool>,
    /// The store-read total at `begin`: the reading is the delta across the
    /// execution.
    reads: u64,
}

impl GradeProbe {
    /// Arm the store-read counters and snapshot them for one execution.
    pub fn begin() -> Self {
        Self {
            prior: Some(super::arm_store_profiling()),
            reads: store_reads().total(),
        }
    }

    /// The run's reading — the store reads the execution performed — and the
    /// arm flag's restore. Called once, after the execution returned.
    pub fn finish(mut self) -> u64 {
        let reads = store_reads()
            .total()
            .checked_sub(self.reads)
            .expect("the store-read counters only ever increase");
        self.restore();
        reads
    }

    /// Restore the arm flag `begin` saved — idempotent, so `finish` and the
    /// drop never restore twice.
    fn restore(&mut self) {
        if let Some(prior) = self.prior.take() {
            super::restore_store_profiling(prior);
        }
    }
}

impl Drop for GradeProbe {
    fn drop(&mut self) {
        self.restore();
    }
}
