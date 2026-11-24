//! The retained transaction-record object family (ADR-0610): the
//! `transactions/` directory of lsn-windowed JSON parts a checkpoint
//! publishes and an open loads, codec-agnostic — every snapshot codec's
//! checkpoint crosses [`crate::checkpoint_at`], which runs
//! [`publish_checkpoint`] before the codec's own image, and every codec's
//! load crosses [`crate::open::load_with_checkpoint_lsn`], which runs
//! [`load_retained`] after the codec's own decode. The parts are
//! append-only: each checkpoint writes parts covering the retained
//! records past the family's covered lsn window in bounded batches
//! (each named by its own records' lsn span, never rewritten), so the
//! family needs no manifest reference and no orphan sweep — a crash
//! mid-publish leaves at most a complete part the next open reads and
//! the next checkpoint builds on, and the WAL still holds the same
//! records until a successful checkpoint recycles them (the table
//! dedups by `commit_ts`, so a record read from both a part and the
//! replay is retained once). The family is multi-tenant (ADR-0610): a
//! record of any type rides it, each row carrying its own `type` — a
//! part a pre-typed binary wrote simply has no `type` in its rows, and
//! the load reads those as the provenance family's.
//!
//! The family exists only while the `value-provenance` preview feature is
//! on: a store that never opts in retains no records, so no part is ever
//! written. The collapse (ADR-0610: provenance is bounded by the last
//! provenance-enabled open) rides the same seam — an open without the
//! opt-in clears the graph's retained table at its checkpoint, and
//! [`publish_checkpoint`] answers an empty table by dropping the family,
//! so the durable store follows the in-memory one.

use relate_core::{Error, Result};
use relate_engine_api::{GraphImport, TransactionRecord};
use serde::Deserialize;

use crate::family::{FlushSink, Flushed, PartFamily, PartKind, RowBatch, PART_CLOSE};
use crate::snapshot::{get_object_opt, put_object};
use crate::StoreTarget;

/// The `transactions/` retained-record family (ADR-0610): the shared
/// part-family plumbing bound to this family — the one spelling of its
/// identity every path consults (never re-derived beside it).
pub(crate) const FAMILY: PartFamily = PartFamily::new(PartKind::Transactions);

/// One part's decoded body: the retained records in `lsn`-ascending order.
/// The publish no longer serializes this struct — it streams the same
/// bytes row by row through [`RecordBatch`]'s envelope — so the struct is
/// the load's decode shape, and the envelope constants below are pinned
/// to its field spelling by the checkpoint/reopen roundtrips.
#[derive(Deserialize)]
struct PartBody {
    records: Vec<TransactionRecord>,
}

/// The part envelope's opening bytes — exactly what a one-shot
/// `serde_json::to_vec` of [`PartBody`] writes before the first record,
/// so a streamed part decodes through the load's unchanged
/// `from_slice::<PartBody>`.
const PART_OPEN: &[u8] = b"{\"records\":[";

/// The one checkpoint step (ADR-0610): publish the retained records past
/// the family's covered window as new parts, or — when the graph's
/// retained table is empty — drop the family, the durable half of the
/// collapse an open without the opt-in performs. Runs before the codec's
/// own image inside [`crate::checkpoint_at`], so a crash between the two
/// leaves at most a part the manifest does not yet cover and the WAL
/// still does.
///
/// The publish streams (ADR-0555): every retained record flows through
/// [`GraphExport::retained_transactions_into`](relate_engine_api::GraphExport::retained_transactions_into)
/// one at a time — no whole-table clone, no whole-interval Vec — and the
/// sink serializes each fresh record into a [`RecordBatch`] flushed as
/// its own part before it can grow past the batch budget, so a publish's
/// peak memory is one batch whatever the interval's dirty delta.
pub(crate) fn publish_checkpoint(target: &StoreTarget, graph: &dyn GraphImport) -> Result<()> {
    // The covered window — the family's one covered-tip helper, shared
    // with the `chains/` family's publish, so the window policy lands
    // once.
    let covered_hi = FAMILY.covered_hi(target)?;
    // `any` is the table's own emptiness: no record at all is the
    // collapse's drop condition, while a table whose records are all
    // already covered keeps its family and publishes nothing.
    let mut batch = RecordBatch::new(target);
    let mut any = false;
    graph.retained_transactions_into(&mut |record| {
        any = true;
        if record.lsn > covered_hi {
            batch.push(record)?;
        }
        Ok(())
    })?;
    if !any {
        FAMILY.drop_family(target, &FAMILY.parts(target)?)?;
        return Ok(());
    }
    batch.flush()
}

/// The publish's bounded batch (ADR-0555): the shared row appender plus
/// the family's own part naming — each flushed batch is one whole part,
/// named by the batch's own lsn span (the first and last record it
/// holds), the same grammar every part uses. Records serialize one at a
/// time into a buffer flushed as its own part before it can grow past
/// [`crate::FAMILY_PUBLISH_BATCH_BYTES`] plus the one record being
/// appended (a single record larger than the budget still lands alone,
/// and is refused by the part bound's own class through `put_object` if
/// it exceeds it). The publish's peak memory is the batch, never the
/// interval's dirty delta: a delta ten times the budget publishes as ten
/// parts through the same one-batch footprint.
struct RecordBatch<'a> {
    /// The shared row appender: the batch's bytes and the comma and
    /// budget discipline.
    rows: RowBatch,
    /// The family's flush half: the store the parts land in and the
    /// flushed part's name window.
    parts: PartSink<'a>,
}

/// The `transactions/` publish's flush half — [`RowBatch`]'s sink: one
/// filled batch is one whole part. The envelope closes here and the
/// part is written through `put_object`, whose bounded-length check is
/// the family's class, so an over-bound part is refused with the
/// class's own error — the reader's refusal by construction.
struct PartSink<'a> {
    /// The store the part lands in.
    target: &'a StoreTarget,
    /// The first and last record lsn in the batch — the flushed part's
    /// name window.
    lo: u64,
    hi: u64,
}

impl FlushSink for PartSink<'_> {
    fn flush_batch(&mut self, bytes: &mut Vec<u8>) -> Result<Flushed> {
        bytes.extend_from_slice(PART_CLOSE);
        put_object(self.target, &FAMILY.part_name(self.lo, self.hi), bytes)?;
        Ok(Flushed::Part)
    }
}

impl<'a> RecordBatch<'a> {
    fn new(target: &'a StoreTarget) -> Self {
        Self {
            rows: RowBatch::new(),
            parts: PartSink {
                target,
                lo: 0,
                hi: 0,
            },
        }
    }

    /// Append one record's serialized row, flushing the batch as its
    /// own part first when the row would cross the batch budget.
    fn push(&mut self, record: &TransactionRecord) -> Result<()> {
        if self.rows.row(PART_OPEN, record, &mut self.parts)? {
            self.parts.lo = record.lsn;
        }
        self.parts.hi = record.lsn;
        Ok(())
    }

    /// Flush the remaining records as the publish's last part — a
    /// no-op on an empty batch, so a fully covered interval publishes
    /// nothing.
    fn flush(&mut self) -> Result<()> {
        self.rows.flush(&mut self.parts)
    }
}

/// The one load step (ADR-0610): read every part and hand its records to
/// the graph's retained table. A part that does not decode fails the open
/// with a typed error naming the object — the same fail-closed posture as
/// a WAL record — never a silent skip.
pub(crate) fn load_retained(graph: &dyn GraphImport, target: &StoreTarget) -> Result<()> {
    for part in FAMILY.parts(target)? {
        let bytes = get_object_opt(target, &part.name)?.ok_or_else(|| {
            Error::storage(format!("missing retained-transaction part {}", part.name))
        })?;
        let body: PartBody = serde_json::from_slice(&bytes).map_err(|e| {
            Error::storage(format!(
                "failed to parse retained-transaction part {}: {e}",
                part.name
            ))
        })?;
        for record in body.records {
            graph.record_retained_transaction(record);
        }
    }
    Ok(())
}
