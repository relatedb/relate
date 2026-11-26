//! The redo path's capacity admission (ADR-0042): the one place a record's
//! estimated growth is priced against the store's logical size limit, plus the
//! reservation (`Reserved`) a commit group's accepted slots advance. A
//! submodule of [`super`] because it reads the live graph and the resolved
//! config through `Shared` and nothing else; the file exists so the store
//! handle's own module stays a handle, not a ledger.

use relate_executor::{DenseSpans, RedoDelta, RedoOp};

use super::Shared;
use crate::{Error, Result};

impl Shared {
    /// The one capacity-admission policy of the redo path (ADR-0042) for a
    /// single record. Called under the caller's publication lock: it reads
    /// the live graph through the `GraphCell` (`estimate_redo_delta` and
    /// `total_used_bytes`). An unlimited store (`size_limit` is `None`)
    /// checks only the ADR-0599 structural walls — the delta, and with it
    /// the ADR-0439 term, is never derived. Otherwise the redo delta of
    /// `ops` is estimated and checked against the live usage. It reserves
    /// nothing
    /// forward and seeds from the committed spans — the group walk carries
    /// its own reservation and calls
    /// [`Shared::admit_redo_capacity_with`] directly.
    pub(crate) fn admit_redo_capacity(&self, ops: &[RedoOp]) -> Result<()> {
        // ADR-0599's structural limits hold with no size limit configured
        // too: the adjacency tier's field-width walls are refused here,
        // before the redo is logged, whatever `limit` says.
        self.graph.enforce_adjacency_limits(ops, &[])?;
        let Some(limit) = self.size_limit() else {
            return Ok(());
        };
        let estimate = self.graph.estimate_redo_delta(ops);
        self.check_capacity(0, limit, &estimate)?;
        Ok(())
    }

    /// [`Shared::admit_redo_capacity`] with the constraint-key entry growth
    /// already computed by the group admission walk (ADR-0439). `earlier`
    /// are the ops of slots this group commit already accepted — the
    /// adjacency tier's type wall counts this slot's creates on top of the
    /// group's (ADR-0599); the single-record path passes none. `reserved`'s
    /// spans are the walk's seed — a group prices each slot against the rungs
    /// its mates already reserved instead of the committed spans, so a rung
    /// crossing is charged once per group, never once per group-mate. The
    /// advanced spans come back in the returned `Reserved`, never written
    /// through the seed: they move only for an accepted slot, so a slot
    /// refused here leaves no phantom rung behind for the next mate.
    pub(crate) fn admit_redo_capacity_with(
        &self,
        ops: &[RedoOp],
        reserved: Reserved,
        constraint_growth: i64,
        earlier: &[&[RedoOp]],
    ) -> Result<Reserved> {
        self.graph.enforce_adjacency_limits(ops, earlier)?;
        let Some(limit) = self.size_limit() else {
            return Ok(reserved);
        };
        let (estimate, advanced) = self.graph.estimate_redo_delta_with_constraint_growth(
            ops,
            constraint_growth,
            reserved.spans,
        );
        let bytes = self.check_capacity(reserved.bytes, limit, &estimate)?;
        // The advance is kept only for a slot that actually reserved bytes
        // (`check_capacity`'s `total > 0`): a net-freeing slot reserves
        // nothing, so the rung it crossed was netted against its own frees
        // and a mate must re-charge it rather than ride it free.
        let spans = if estimate.total > 0 {
            advanced
        } else {
            reserved.spans
        };
        Ok(Reserved { bytes, spans })
    }

    /// The store's logical size limit (ADR-0042): `None` is unlimited — the
    /// resolved config's `0` sentinel, modelled once here so every reader
    /// (`Database::memory_usage` included) consults one shape instead of
    /// re-testing the sentinel.
    pub(crate) fn size_limit(&self) -> Option<u64> {
        match self.resolved.logical_graph_size_limit {
            0 => None,
            limit => Some(limit),
        }
    }

    /// The one capacity decision of the redo path (ADR-0042): `reserved` is
    /// what the caller already holds, so the estimate is priced against the
    /// live usage plus it. `limit` is the configured size limit — an
    /// unlimited store never reaches here, its entry points return at
    /// `size_limit` before deriving the estimate. A delete's tombstone
    /// spill never refuses a batch
    /// (ADR-0529): checkpoint GC returns it, and a delete at the cap must
    /// commit. A net-freeing batch (`total <= 0`, e.g. a DROP INDEX that
    /// re-accounts retained versions at a smaller charge) reserves nothing:
    /// the max carries the sign into the reservation, so the cast is safe by
    /// construction.
    fn check_capacity(&self, reserved: u64, limit: u64, estimate: &RedoDelta) -> Result<u64> {
        let used = self.total_used_bytes().saturating_add(reserved);
        let refusable = estimate.refusable();
        if refusable > 0 && used.saturating_add(refusable as u64) > limit {
            return Err(Error::capacity(used, limit));
        }
        Ok(reserved.saturating_add(estimate.total.max(0) as u64))
    }
}

/// The bytes and dense spans a commit group has already admitted. One value
/// because they move together: a slot that is accepted advances both, a slot
/// refused at any stage leaves both where they were, so the walk carries one
/// field and assigns it once. The spans advance only for a slot that
/// reserved bytes ([`Shared::check_capacity`]'s `total > 0`): a net-freeing
/// slot (`total <= 0`) reserves nothing, and the rung it may have crossed
/// was netted against its own frees, so a mate must not ride it for free.
#[derive(Clone, Copy)]
pub(crate) struct Reserved {
    pub(crate) bytes: u64,
    pub(crate) spans: DenseSpans,
}
