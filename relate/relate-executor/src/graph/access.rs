//! The graph's read accessors: the version walk every `*_at` read takes,
//! the value materializers, the wholesale id enumerators, and the
//! source-scoped pull identity (ADR-0080).

use relate_core::{NodeId, RelId, Value};

use crate::redo::PullKind;

use super::chain::{Chain, VerRef};
use super::id_vec::IdVec;
use super::Graph;

/// The visible version at `ts` — the one walk every `*_at` read takes
/// over a version chain (LABS-337's extraction of the previously
/// five-times-spelled rev-find).
pub(super) fn visible_version<Live>(vers: &Chain<Live>, ts: u64) -> Option<VerRef<'_, Live>> {
    vers.visible_at(ts)
}

/// The LIVE version — the last `xmax == 0` one, not a ts-probe: the
/// maintained-statistics paths run inside the write window, where a rel
/// created earlier in the same batch is invisible at `commit_ts` and one
/// deleted in the batch keeps a live-looking pre-delete version. The
/// `xmax == 0` convention is the one every live-version reader in the graph
/// module uses (`live_label_ids`, the rebuild's live-rel scan, and the
/// constraint_keys family's snapshots and backfills).
pub(super) fn live_version<Live>(vers: &Chain<Live>) -> Option<VerRef<'_, Live>> {
    vers.live()
}

/// The ids of `map` visible at `ts`, ascending — the one collector behind
/// the wholesale node and relationship enumerators.
fn visible_ids<Live>(map: &IdVec<Chain<Live>>, ts: u64) -> Vec<i64> {
    let mut ids = Vec::new();
    map.for_each(|id, vers| {
        if visible_version(vers, ts).is_some() {
            ids.push(id);
        }
    });
    ids.sort();
    ids
}

impl Graph {
    pub fn node_value(&self, id: NodeId) -> Value {
        self.node(id).map(Value::from_node).unwrap_or(Value::Null)
    }

    pub fn rel_value(&self, id: RelId) -> Value {
        self.rel(id)
            .map(Value::from_relationship)
            .unwrap_or(Value::Null)
    }

    pub fn pull_identity_get(&self, source: &str, kind: PullKind, remote_id: &str) -> Option<i64> {
        self.pull_identity.get(source, kind, remote_id)
    }

    pub fn pull_identity_put(
        &self,
        source: String,
        kind: PullKind,
        remote_id: String,
        local_id: i64,
    ) {
        self.pull_identity.put(source, kind, remote_id, local_id);
    }

    pub fn drop_pull_identity(&self, kind: PullKind, local_id: i64) {
        self.pull_identity.remove(kind, local_id);
    }

    pub fn all_rel_ids(&self) -> Vec<RelId> {
        self.all_rel_ids_at(self.read_ts())
    }

    pub fn all_rel_ids_at(&self, ts: u64) -> Vec<RelId> {
        visible_ids(&self.rels, ts)
    }

    pub fn all_node_ids(&self) -> Vec<NodeId> {
        self.all_node_ids_at(self.read_ts())
    }

    pub fn all_node_ids_at(&self, ts: u64) -> Vec<NodeId> {
        visible_ids(&self.nodes, ts)
    }
}
