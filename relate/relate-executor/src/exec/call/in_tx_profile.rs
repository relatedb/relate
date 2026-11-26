//! The `CALL { … } IN TRANSACTIONS` arm's per-batch PROFILE byte
//! accounting (REL8-2047, ADR-0681): one [`BatchProfile`] per batch holding
//! the opening [`BatchMarks`], the commit's [`BatchPeaks`] read against
//! them, and the two note paths onto the arm's shared facts cell — the one
//! concern the batch loop (`in_transactions.rs`) reads, kept in its own
//! module so the arm's own file stays its three concerns: batch
//! boundaries, per-batch commit and rollback, and the `ON ERROR` /
//! `REPORT STATUS` rows.

use std::rc::Rc;

use crate::metrics::{InTxBatchEnd, InTxStats};
use crate::overlay::ExecGraph;
use crate::row::Row;

/// One batch's PROFILE accounting: the opening marks beside the graph they
/// are read against, built once as the batch opens. "Is this arm
/// profiled?" is the one `Option` inside — every method states what it
/// does for a profiled batch and is a no-op (no byte read, no row walk)
/// for an unprofiled one, so the batch loop holds one value and no
/// `Option` of its own.
pub(super) struct BatchProfile<'a> {
    graph: &'a ExecGraph<'a>,
    /// The marks as the batch opened — present exactly when the arm has a
    /// facts cell, so an unprofiled batch never reads a byte.
    marks: Option<BatchMarks>,
}

/// One batch's opening byte marks (REL8-2047): the arm's facts cell beside
/// the scratch's created bytes and the redo log's reserved WAL bytes — the
/// opening snapshot the batch's peaks read against, named fields so no
/// positional pair crosses a boundary. The deltas over it ([`BatchPeaks`])
/// are the batch's own contribution at its commit or rollback.
struct BatchMarks {
    /// The arm's facts cell.
    stats: Rc<InTxStats>,
    /// The scratch's created bytes at the mark.
    scratch: u64,
    /// The redo log's reserved bytes at the mark — the WAL payload.
    wal: u64,
}

/// A batch's PROFILE peaks (REL8-2047): its commit's own contributions —
/// saturating deltas over its opening [`BatchMarks`]. The marks are the
/// snapshot; these are the deltas; the two meanings never share a type.
/// Zero on an unprofiled batch, where nothing reads them.
#[derive(Clone, Copy, Default)]
pub(super) struct BatchPeaks {
    /// The scratch the batch itself created.
    scratch: u64,
    /// The redo bytes the batch itself reserved — its WAL payload.
    wal: u64,
}

impl BatchMarks {
    /// The batch's own scratch contribution over the mark — the one
    /// saturating-delta spelling, shared by the commit and the rollback
    /// reads, so a shrink (a delete-heavy body over earlier batches'
    /// state) reports zero, never a wrap.
    fn scratch_delta(&self, graph: &ExecGraph<'_>) -> u64 {
        graph.scratch_created_bytes().saturating_sub(self.scratch)
    }

    /// The batch's own WAL-payload contribution over the mark.
    fn wal_delta(&self, graph: &ExecGraph<'_>) -> u64 {
        graph.redo_reserved_bytes().saturating_sub(self.wal)
    }
}

impl<'a> BatchProfile<'a> {
    /// Open the batch's accounting. `cell` is the arm's facts cell, `None`
    /// on an unprofiled arm — no cell, no marks, and no byte reads. The
    /// marks are the zero the batch's peaks read against, so a run without
    /// a per-batch commit sink (whose scratch accumulates to statement
    /// end) still reports one batch's bytes, never the running total.
    pub(super) fn open(cell: Option<&Rc<InTxStats>>, graph: &'a ExecGraph<'a>) -> Self {
        let marks = cell.map(|stats| BatchMarks {
            stats: Rc::clone(stats),
            scratch: graph.scratch_created_bytes(),
            wal: graph.redo_reserved_bytes(),
        });
        Self { graph, marks }
    }

    /// The batch's commit peaks, read against its opening marks while the
    /// redo is still reserved — the caller reads before the commit's
    /// rebase drains it, and hands the value to [`Self::committed`] once
    /// the commit succeeded, so a peak cannot be noted without having been
    /// read first. Zero on an unprofiled batch (no byte is read).
    pub(super) fn read_peaks(&self) -> BatchPeaks {
        match &self.marks {
            Some(marks) => BatchPeaks {
                scratch: marks.scratch_delta(self.graph),
                wal: marks.wal_delta(self.graph),
            },
            None => BatchPeaks::default(),
        }
    }

    /// Note a committed batch onto the arm's PROFILE facts — the peaks
    /// read before its commit, the input it held and the output it
    /// installed measured here.
    pub(super) fn committed(&self, peaks: BatchPeaks, input: &[Row], output: &[Row]) {
        if let Some(marks) = &self.marks {
            marks.stats.note_batch(InTxBatchEnd::Committed {
                input_bytes: crate::workspace::retained_rows_of(input),
                output_bytes: crate::workspace::retained_rows_of(output),
                scratch_bytes: peaks.scratch,
                wal_bytes: peaks.wal,
            });
        }
    }

    /// Note a rolled-back batch onto the arm's PROFILE facts — the input
    /// it held and the scratch it created, its own contribution over the
    /// opening marks, read before the restore rolls the state away. No
    /// output or WAL figure: its rows are gone with the rollback and its
    /// redo never committed.
    pub(super) fn rolled_back(&self, input: &[Row]) {
        if let Some(marks) = &self.marks {
            marks.stats.note_batch(InTxBatchEnd::RolledBack {
                input_bytes: crate::workspace::retained_rows_of(input),
                scratch_bytes: marks.scratch_delta(self.graph),
            });
        }
    }
}
