//! Property reads by an already-interned key id (ADR-0541): the plan-token
//! counterparts of `node_prop_at` / `rel_prop_at`. A sibling of `mod.rs`.

use super::*;

impl Graph {
    /// [`Self::node_prop_at`] with the key already interned.
    pub(crate) fn node_prop_id_at(&self, id: NodeId, ts: u64, kid: u32) -> Option<Value> {
        self.nodes
            .get(&id)
            .and_then(|vers| visible_version(&vers, ts).and_then(|v| v.live.prop_by_id(kid)))
    }

    /// [`Self::rel_prop_at`] with the key already interned.
    ///
    /// Invariant: `key` is the name whose interned id is `kid`. The id
    /// serves a map-form payload, but a `Triple` payload stores its three
    /// properties as typed fields rather than by id, so it still matches on
    /// the key text.
    pub(crate) fn rel_prop_id_at(&self, id: RelId, ts: u64, key: &str, kid: u32) -> Option<Value> {
        self.rels.get(&id).and_then(|vers| {
            visible_version(&vers, ts).and_then(|v| v.live.payload.get_by_id(key, kid))
        })
    }
}
