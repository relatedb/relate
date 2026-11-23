//! PROFILE per-clause metrics collection (ADR-0344 phase 2).
//!
//! [`ProfileMetrics`] implements the executor's [`PlanMetricsSink`]:
//! the dispatch loop reports each statement-level clause operator's rows
//! produced and elapsed time, keyed by `(stage, part, clause)` — the same walk
//! the plan tree construction uses, so the collected entries stamp onto
//! the PROFILE result's plan tree without a second mapping.

use std::cell::RefCell;
use std::collections::BTreeMap;

use relate_executor::{ClauseMetricsRecord, PlanMetricsSink, StatementMemoRecord};

/// The collected per-clause metric map type, owned by the plan tree (its
/// node type owns the metric shape and the record conversion).
use crate::plan_node::ClauseMetricsMap;

/// The PROFILE sink: one collected entry per statement-level clause.
/// Interior mutability (a `RefCell`) because the sink crosses the
/// executor boundary as `&dyn PlanMetricsSink` — dispatch is
/// single-threaded per statement, so the borrow is uncontended.
pub(crate) struct ProfileMetrics {
    collected: RefCell<ClauseMetricsMap>,
    /// The statement's constructor-memo facts (ADR-0654 §3), reported once
    /// at the statement's end and attached at the plan root.
    statement_memos: RefCell<StatementMemoRecord>,
}

impl ProfileMetrics {
    pub(crate) fn new() -> Self {
        Self {
            collected: RefCell::new(BTreeMap::new()),
            statement_memos: RefCell::new(StatementMemoRecord::default()),
        }
    }

    /// The statement-level memo facts the executor reported (ADR-0654 §3);
    /// all zeros when no constructor memo was probed.
    pub(crate) fn statement_memos(&self) -> StatementMemoRecord {
        *self.statement_memos.borrow()
    }

    /// The collected entries, `(stage, part, clause)` keyed — the
    /// stage keeps NEXT-chain stages' keys disjoint.
    pub(crate) fn collected(&self) -> ClauseMetricsMap {
        self.collected.borrow().clone()
    }
}

impl PlanMetricsSink for ProfileMetrics {
    fn record_clause(&self, record: ClauseMetricsRecord) {
        let key = (record.stage, record.part, record.clause);
        self.collected.borrow_mut().insert(key, record.into());
    }

    fn record_statement_memos(&self, record: StatementMemoRecord) {
        *self.statement_memos.borrow_mut() = record;
    }
}
