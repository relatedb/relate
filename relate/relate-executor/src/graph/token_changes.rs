//! ADR-0687: the changed-token set the live mutation sites report.
//!
//! A commit's changed tokens cannot be reconstructed from its write records
//! afterwards: a delete's redo op carries only the entity id, and the dying
//! labels, relationship type and property keys live in the version the
//! removal tombstones. Each mutation seam therefore reports its tokens while
//! it still holds the version, [`Graph::take_applied_token_changes`] drains
//! the accumulated set, and the facade's publication folds it into the
//! per-token last-changed table (`relate`'s `token_changes`).
//!
//! ## What a seam reports
//!
//! One rule, stated here and pointed at by every seam:
//!
//! - **A create reports its classify token** — a node's label ids, a
//!   relationship's type id — and no property keys. A footprint holds the
//!   classify token's row count, and the (classify, key) cardinality a seek
//!   consult reads moves with that count, so the classify stamp covers the
//!   keys a create installs.
//! - **A property edit reports the keys it touched** beside the entity's
//!   classify tokens: a footprint's key ids are tokens of their own and
//!   nothing else would stamp them.
//! - **A delete reports the dying version's classify tokens and its
//!   property-key ids**: the tombstone is the last place the version's keys
//!   exist, so no later walk could report them (ADR-0687 decision 7).
//!
//! The asymmetry is the point: a create's keys live on in the installed
//! version and the classify stamp covers them, while a delete's keys do not
//! survive it.

use super::*;

/// The interned tokens the writes applied since the last drain changed —
/// label ids, relationship-type ids and property-key ids, one dense
/// append-only id space (`crate::store::interner`) — plus whether a schema
/// op landed among them.
#[derive(Debug, Default)]
pub struct AppliedTokenChanges {
    tokens: HashSet<u32>,
    schema: bool,
}

impl AppliedTokenChanges {
    /// The changed token ids, in no particular order, each once.
    pub fn tokens(&self) -> impl Iterator<Item = u32> + '_ {
        self.tokens.iter().copied()
    }

    /// Whether a schema op (an index, constraint or graph-type create/drop)
    /// landed: no token footprint describes one, so the publication moves
    /// the full-invalidation epoch instead of stamping a footprint.
    pub fn schema(&self) -> bool {
        self.schema
    }
}

impl Graph {
    /// Report `tokens` as changed by the writes applied since the last
    /// drain — the mutation seams' one call, at the point each still holds
    /// the entity version whose tokens it reports.
    pub(in crate::graph) fn note_changed_tokens(&self, tokens: impl IntoIterator<Item = u32>) {
        self.changed_tokens.lock().unwrap().tokens.extend(tokens);
    }

    /// Report a schema op among the applied writes.
    pub(in crate::graph) fn note_schema_change(&self) {
        self.changed_tokens.lock().unwrap().schema = true;
    }

    /// Drain the set the next publication folds in (ADR-0687): the commit
    /// tail, the follower's batch apply, and the reload each drain once,
    /// and an open's replay is the baseline, so it drains and discards.
    pub fn take_applied_token_changes(&self) -> AppliedTokenChanges {
        std::mem::take(&mut *self.changed_tokens.lock().unwrap())
    }
}
