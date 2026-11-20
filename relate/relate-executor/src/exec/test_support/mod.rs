//! Shared test helpers, one family module each: the run harness, catalog-show
//! pins, result readers, write machinery, aggregate fixtures, graph builders,
//! and the frontier vocabulary. Each module keeps its helpers' one canonical
//! definition; this root re-exports them so call sites keep one path.

use std::cell::RefCell;

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
}

pub(crate) use aggregate::*;
pub(crate) use catalog_show::*;
pub(crate) use frontier::*;
pub(crate) use graphs::*;
pub(crate) use readers::*;
pub(crate) use run::*;
pub(crate) use write::*;
