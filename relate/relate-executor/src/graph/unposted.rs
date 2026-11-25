//! The aggregate reads' trust mark for the equality pair and the range index.

use super::*;

/// The aggregate reads' trust mark, shared by the equality pair and the
/// range index: ids whose **current** version carries a value the pair
/// cannot post (a NaN, alone or inside a list or map). It reflects the
/// LIVE version alone — a superseded NaN must not over-decline and a
/// superseded posting must not under-decline — so only the live
/// reconciliation ([`Self::restate`]) and the node's removal
/// ([`Self::clear`]) write it; the posting step never does. Reads go
/// through `Deref` to the id set (`is_empty`, `len`, `contains`); there is
/// no `DerefMut`, so the two writers above stay the only way in.
#[derive(Debug, Default)]
pub(super) struct UnpostedMark(HashSet<NodeId>);

impl std::ops::Deref for UnpostedMark {
    type Target = HashSet<NodeId>;

    fn deref(&self) -> &HashSet<NodeId> {
        &self.0
    }
}

impl UnpostedMark {
    /// Restate `id`'s mark from its live version: marked when that
    /// version's value cannot be posted, cleared otherwise.
    pub(super) fn restate(&mut self, id: NodeId, unpostable: bool) {
        if unpostable {
            self.0.insert(id);
        } else {
            self.0.remove(&id);
        }
    }

    /// Drop `id`'s mark (a node leaving the pair, or about to be reposted).
    pub(super) fn clear(&mut self, id: NodeId) {
        self.0.remove(&id);
    }
}
