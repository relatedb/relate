//! The aggregation's bills: the one [`GroupBill`] for everything the
//! aggregation retains (the group table's bytes and what retaining
//! accumulators keep) and the [`InputBill`] for the input vector the fold
//! holds. Both the group table and the accumulator columns bill through
//! these, so the module sits beneath `table`, `columns`, `site` and `pool`
//! and none of them imports another for it.

use relate_core::tokens::{peak_hold, WorkspaceCharge};
use relate_core::Result;

use crate::overlay::{ExecGraph, RetainedTotal};
use crate::row::Row;
use crate::workspace::RetainedRows;

/// The one bill for the aggregation's retained bytes: the group table
/// (index, keys, seeds, and accumulator slots) and, as rows settle, what
/// retaining accumulators keep (`collect` operands, percentile samples,
/// the payload a `min`/`max` holds, parked errors). The expression
/// budget's running total and the workspace ledger are fed the same figure.
pub(super) struct GroupBill {
    /// The aggregation's one retaining container: the group table and
    /// what its retaining accumulators keep grow it together.
    retained: RetainedTotal,
    /// The workspace-ledger twin of the table bytes billed so far (its own
    /// [`WorkspaceCharge::bytes`] is the running total). Held
    /// unconditionally, as every other owned structure here holds its
    /// charge: an empty charge is a no-op while no ledger is installed, so
    /// there is no second "accounting is off" spelling to keep in step.
    workspace: WorkspaceCharge,
}

/// The one bill for the input vector the fold holds: its measured bytes
/// and the workspace-ledger charge that follows them down as each row's
/// payload moves into the group table.
pub(super) struct InputBill {
    retained: RetainedRows,
    charge: WorkspaceCharge,
}

impl InputBill {
    /// Bill the whole input vector up front.
    pub(super) fn open(rows: &[Row]) -> Result<Self> {
        let retained = RetainedRows::measure(rows);
        let charge = WorkspaceCharge::retain(retained.bytes())?;
        peak_hold("aggregation");
        Ok(Self { retained, charge })
    }

    /// Release `row`'s share: its payload moves on to the group table.
    pub(super) fn release(&mut self, row: &Row) -> Result<()> {
        self.retained.release_row(row);
        self.charge.resize(self.retained.bytes())
    }
}

impl GroupBill {
    pub(super) fn open(graph: &ExecGraph<'_>) -> Result<Self> {
        Ok(Self {
            retained: graph.retain_bytes(0)?,
            workspace: WorkspaceCharge::empty(),
        })
    }

    /// Bill `bytes` of retained aggregation state, ahead of the landing or
    /// the append so a refusal never leaves that state retained. Group
    /// landings and accumulator settles share this path, so a `collect`
    /// payload reserves the same ledger bytes the expression cap already
    /// counts.
    pub(super) fn add(&mut self, graph: &ExecGraph<'_>, bytes: u64) -> Result<()> {
        self.retained.add_bytes(graph, bytes)?;
        let total = self.workspace.bytes().saturating_add(bytes);
        self.workspace.resize_with(|| total)?;
        peak_hold("aggregation");
        Ok(())
    }

    /// The bytes billed to the aggregation's running total so far: the
    /// expression total's own figure, read only through the bill that owns
    /// it so the paired workspace ledger cannot be fed a different one.
    #[cfg(test)]
    pub(super) fn total(&self) -> u64 {
        self.retained.total()
    }
}
