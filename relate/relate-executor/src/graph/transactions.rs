//! The graph's retained transaction-record table (ADR-0610): the
//! `commit_ts`-keyed map of retained [`TransactionRecord`]s the audit
//! surface reads. Populated at three sites — the live commit pipeline
//! (after a record is durable and applied), the WAL replay (a record
//! carrying a tail joins as it is applied), and the checkpoint family
//! load — and read by the checkpoint family publish.
//!
//! The retention policy is the table's own, stated once here: a record is
//! retained iff it is typed or carries provenance fields
//! ([`TransactionRecord::is_retained`]) — retention retains every typed
//! record, and on today's engine that set is exactly what a
//! `value-provenance`-enabled store produces, so a store that never opted
//! in writes no such record: the table stays empty and costs nothing
//! (ADR-0610's zero-cost guarantee).

use std::collections::BTreeMap;
use std::sync::RwLock;

use relate_core::{Error, Result, Value};
use relate_engine_api::TransactionRecord;

use super::Graph;

/// One storage `u64` as the audit surface's INTEGER cell (ADR-0610):
/// `commitTs` and `lsn` are `u64` facts of the retained records this
/// module holds, while every audit surface maps and lists them as Cypher
/// `INTEGER` — a value beyond that range is a typed error, never a
/// silently wrapped negative. The message names the shared audit
/// surface, not the calling function: the `provenance()`/`history()`
/// event maps and the `db.transactions` rows render the same fields
/// through it. Re-exported once at [`crate::graph::audit_integer`] (the
/// `provenance::PropertyEvent` precedent) for the eval and exec
/// surfaces that render.
pub(crate) fn audit_integer(value: u64, what: &str) -> Result<Value> {
    i64::try_from(value).map(Value::Int).map_err(|_| {
        Error::execution(format!(
            "ADR-0610 audit surface: {what} exceeds the INTEGER range"
        ))
    })
}

/// The retained-record table: keyed by `commit_ts` (the audit surface's
/// lookup key — the bookmark's sequence), ascending iteration for free. A
/// poisoned lock is recovered, not propagated: the table's entries are
/// complete records, and the swap-like operations (insert, clear) are
/// single-step, so recovery never observes a half-updated table.
#[derive(Default)]
pub(crate) struct TransactionTable(RwLock<BTreeMap<u64, TransactionRecord>>);

impl std::fmt::Debug for TransactionTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransactionTable")
            .field("records", &self.len())
            .finish_non_exhaustive()
    }
}

impl Graph {
    /// Retain one committed transaction's record (ADR-0610) — the one
    /// insert the live commit pipeline, the WAL replay and the checkpoint
    /// family load all funnel through. The table itself decides which
    /// records retain (only those carrying provenance fields), so no
    /// caller re-derives that policy.
    pub fn record_transaction(&self, record: TransactionRecord) {
        self.transactions.record(record);
    }

    /// The retained record of the commit at `commit_ts` (ADR-0610), if the
    /// commit retained — the lookup the provenance queries and
    /// `db.transactions` join on, and the shape a bookmark names
    /// (`relate:<n>`'s `n` is the commit timestamp).
    pub fn transaction_record(&self, commit_ts: u64) -> Option<TransactionRecord> {
        self.transactions.lookup(commit_ts)
    }

    /// Every retained record, ascending `commit_ts` (ADR-0610) — the
    /// closed observation surfaces' whole-table read (`db.transactions`
    /// and the doc-hidden test API); the checkpoint family publishes
    /// through [`Self::retained_transactions_into`], the streaming
    /// form.
    pub fn retained_transactions(&self) -> Vec<TransactionRecord> {
        self.transactions.records()
    }

    /// Push every retained record, ascending `commit_ts` (ADR-0610), to
    /// `sink` one at a time — the checkpoint family publish's bounded
    /// fold (ADR-0555): no whole-table Vec, no clone. The table's read
    /// guard is held for the fold's duration: a checkpoint's writer
    /// admission holds commits off the store while it dumps, and the
    /// replay and load paths are open-time single-threaded, so the
    /// fold's serialization and I/O contend with nobody.
    pub fn retained_transactions_into(
        &self,
        sink: &mut dyn FnMut(&TransactionRecord) -> Result<()>,
    ) -> Result<()> {
        self.transactions.records_into(sink)
    }

    /// Drop every retained record (ADR-0610) — the in-memory half of the
    /// collapse an open without the opt-in performs at its checkpoint.
    pub fn clear_transactions(&self) {
        self.transactions.clear();
    }
}

impl TransactionTable {
    /// The table's record count — the one length read spelled through
    /// the poison recovery ("a poisoned lock is recovered, not
    /// propagated"), so the Debug render and the record readers agree
    /// on the policy instead of Debug substituting a default under a
    /// poison the readers recover from.
    fn len(&self) -> usize {
        self.0.read().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// Retain one record if it is typed or carries provenance fields —
    /// the one policy site. Two sources can hand the same record (a part
    /// and the WAL replay both holding it after a mid-checkpoint crash):
    /// the key is the commit's own `commit_ts`, so the second insert
    /// replaces the first with the identical value.
    fn record(&self, record: TransactionRecord) {
        if !record.is_retained() {
            return;
        }
        let commit_ts = record.commit_ts;
        self.0
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(commit_ts, record);
    }

    /// The record of the commit at `commit_ts`, if retained.
    fn lookup(&self, commit_ts: u64) -> Option<TransactionRecord> {
        self.0
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(&commit_ts)
            .cloned()
    }

    /// Every retained record, ascending `commit_ts`.
    fn records(&self) -> Vec<TransactionRecord> {
        self.0
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .cloned()
            .collect()
    }

    /// Push every retained record, ascending `commit_ts`, to `sink` —
    /// [`Self::records`]'s streaming twin over the same table: the same
    /// order, no whole-table clone.
    fn records_into(&self, sink: &mut dyn FnMut(&TransactionRecord) -> Result<()>) -> Result<()> {
        for record in self.0.read().unwrap_or_else(|p| p.into_inner()).values() {
            sink(record)?;
        }
        Ok(())
    }

    /// Drop every retained record — the in-memory half of the collapse an
    /// open without the opt-in performs at its checkpoint (ADR-0610).
    fn clear(&self) {
        self.0.write().unwrap_or_else(|p| p.into_inner()).clear();
    }
}
