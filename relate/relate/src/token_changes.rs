//! The per-token last-changed table and the full-invalidation epoch
//! (ADR-0687): one generation slot per dense interner token id (label,
//! relationship type or property key), recording the plan generation the
//! token last changed at, beside a coarser epoch that moves past every entry
//! when no footprint can describe the change (schema DDL, graph replacement).
//!
//! [`crate::Shared::publish_data_state`] is the one writer: it advances the
//! stats generation, then [`TokenChanges::record`]s the set the applied
//! writes reported (`Graph::take_applied_token_changes`) — stamping its
//! tokens at the generation just advanced to, or moving the epoch for a
//! schema op — so a plan compiled before that publish reads the change. The
//! admission check that reads the table holds the publication read guard —
//! the same guard a plan is stamped under.

use relate_executor::AppliedTokenChanges;

/// The store's per-token change record (ADR-0687): the generation every
/// interned token last changed at, and the full-invalidation epoch. Both are
/// read under the publication read guard, so an entry's footprint check and
/// its epoch check see one coherent pair.
#[derive(Debug, Default)]
pub(crate) struct TokenChanges {
    /// One generation slot per interned token id. A slot past the vector's
    /// end, or a zero, means the token has never changed in this table's
    /// history: the ids are dense and append-only, so [`Self::stamp`] grows
    /// the vector only to the largest id it has seen.
    last_changed: Vec<u64>,
    /// The full-invalidation epoch (ADR-0687 decision 4): advances on schema
    /// DDL (no token footprint describes an index, a constraint or a graph
    /// type) and on graph replacement (a replaced graph's interner ids are
    /// unrelated to the installed one's). Data commits never move it.
    epoch: u64,
}

impl TokenChanges {
    /// Fold one published commit's changed-token set in at `generation`: a
    /// schema op among them is the wholesale change no footprint describes, so
    /// the epoch moves instead — an entry planned before it re-plans whatever
    /// its footprint says, and one planned after it never needed these stamps:
    /// its plan already saw the post-commit state. Every other commit stamps
    /// its tokens, the fold a footprint check reads. The schema-versus-token
    /// decision therefore has one home, beside the replacement rule below.
    pub(crate) fn record(&mut self, changes: &AppliedTokenChanges, generation: u64) {
        if changes.schema() {
            self.advance_epoch();
        } else {
            self.stamp(changes.tokens(), generation);
        }
    }

    /// Stamp every id in `tokens` with `generation` — the fold of the
    /// changed-token set the applied writes reported.
    fn stamp(&mut self, tokens: impl IntoIterator<Item = u32>, generation: u64) {
        for token in tokens {
            let slot = token as usize;
            if self.last_changed.len() <= slot {
                self.last_changed.resize(slot + 1, 0);
            }
            self.last_changed[slot] = generation;
        }
    }

    /// Advance the full-invalidation epoch: every entry planned before it
    /// re-plans whatever its footprint says.
    pub(crate) fn advance_epoch(&mut self) {
        self.epoch += 1;
    }

    /// A graph replacement is the wholesale change no footprint describes:
    /// move the epoch past every entry and drop every slot, so a replaced
    /// graph's ids can never satisfy a footprint check.
    pub(crate) fn on_graph_replacement(&mut self) {
        self.advance_epoch();
        self.last_changed.clear();
    }

    /// The generation `token` last changed at; 0 when it never has. The
    /// admission check (ADR-0687 decision 1) reads this per footprint id.
    pub(crate) fn last_changed(&self, token: u32) -> u64 {
        self.last_changed.get(token as usize).copied().unwrap_or(0)
    }

    /// The epoch a plan records and the admission check re-reads; a mismatch
    /// re-plans regardless of footprint (ADR-0687 decision 4).
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Every token with a non-zero slot, in id order — the tests' view of
    /// what a commit stamped.
    #[cfg(test)]
    pub(crate) fn stamped_tokens(&self) -> Vec<u32> {
        self.last_changed
            .iter()
            .enumerate()
            .filter(|(_, generation)| **generation != 0)
            .map(|(token, _)| token as u32)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stamp grows the table to the largest id it sees and leaves every
    /// other slot at its previous value — an unrelated commit's stamps are
    /// not disturbed by a later one.
    #[test]
    fn stamp_sets_only_the_given_tokens() {
        let mut table = TokenChanges::default();
        table.stamp([3, 7], 1);
        assert_eq!(table.last_changed(3), 1);
        assert_eq!(table.last_changed(7), 1);
        assert_eq!(table.last_changed(0), 0);
        assert_eq!(table.last_changed(4), 0);
        // A later stamp moves its own tokens and keeps the earlier ones.
        table.stamp([4], 2);
        assert_eq!(table.last_changed(3), 1);
        assert_eq!(table.last_changed(4), 2);
        assert_eq!(table.stamped_tokens(), vec![3, 4, 7]);
    }

    /// An id past the current vector grows it, so the table stays dense from
    /// 0 the way the interner's ids are.
    #[test]
    fn stamp_grows_over_every_intervening_slot() {
        let mut table = TokenChanges::default();
        table.stamp([9], 5);
        assert_eq!(table.last_changed(9), 5);
        for token in 0..9 {
            assert_eq!(table.last_changed(token), 0, "slot {token}");
        }
    }

    /// Schema DDL moves the epoch and touches no slot; a graph replacement
    /// moves the epoch and drops every slot.
    #[test]
    fn epoch_advances_and_replacement_rebases_the_slots() {
        let mut table = TokenChanges::default();
        table.stamp([2], 4);
        table.advance_epoch();
        assert_eq!(table.epoch(), 1);
        assert_eq!(table.last_changed(2), 4, "the epoch touches no slot");
        table.on_graph_replacement();
        assert_eq!(table.epoch(), 2);
        assert_eq!(table.last_changed(2), 0, "the replacement drops the slots");
        assert!(table.stamped_tokens().is_empty());
    }
}
