//! Shared test helpers, one family module each: the run harness, catalog-show
//! pins, result readers, write machinery, aggregate fixtures, graph builders,
//! and the frontier vocabulary. Each module keeps its helpers' one canonical
//! definition; this root re-exports them so call sites keep one path.

use std::cell::{Cell, RefCell};
use std::thread::LocalKey;

use crate::metrics::ClauseMetricsRecord;

mod aggregate;
mod catalog_show;
mod frontier;
mod graphs;
mod readers;
mod run;
mod write;

/// The one PROFILE records sink the exec test harnesses share
/// (`profiled`, `profiled_write_routed`): every clause record the dispatch
/// emits, collected until its harness takes them. Each harness owns only
/// its differing start-up plumbing (the read view, the write scratch).
pub(crate) struct ClauseRecords(pub(crate) RefCell<Vec<ClauseMetricsRecord>>);

impl crate::metrics::PlanMetricsSink for ClauseRecords {
    fn record_clause(&self, record: ClauseMetricsRecord) {
        self.0.borrow_mut().push(record);
    }

    fn record_statement_memos(&self, _record: crate::metrics::StatementMemoRecord) {}
}

pub(crate) use aggregate::*;
pub(crate) use catalog_show::*;
pub(crate) use frontier::*;
pub(crate) use graphs::*;
pub(crate) use readers::*;
pub(crate) use run::*;
pub(crate) use write::*;

/// Run `f` with the thread-local test override `slot` set to `value`,
/// restoring the previous setting afterwards (also on unwind, and nesting
/// composes: an inner scope restores the outer scope's value) — the one
/// scoped-override helper the exec seams share.
pub(crate) fn with_override<V: Copy + 'static, T>(
    slot: &'static LocalKey<Cell<Option<V>>>,
    value: V,
    f: impl FnOnce() -> T,
) -> T {
    struct Restore<V: Copy + 'static>(&'static LocalKey<Cell<Option<V>>>, Option<V>);
    impl<V: Copy + 'static> Drop for Restore<V> {
        fn drop(&mut self) {
            self.0.with(|c| c.set(self.1));
        }
    }
    let _restore = Restore(slot, slot.with(|c| c.replace(Some(value))));
    f()
}
