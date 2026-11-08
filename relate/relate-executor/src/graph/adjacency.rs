//! Adjacency access (ADR-0031/ADR-0033 typed chunks): degree, neighbor
//! iteration, and snapshot-visible traversal — split out of `mod.rs`.
use super::hop_counts;
use super::id_vec::IdVec;
use super::*;

fn flatten_adj(by_type: Option<&TypedAdj>) -> Vec<RelId> {
    match by_type {
        Some(m) => m.flatten(),
        None => Vec::new(),
    }
}

pub(super) fn adj_insert(
    map: &IdVec<TypedAdj>,
    node: NodeId,
    type_id: u32,
    id: RelId,
    ts: Option<i64>,
) {
    map.entry(node).or_default().insert(type_id, id, ts);
}

pub(super) fn adj_reposition(
    map: &IdVec<TypedAdj>,
    node: NodeId,
    type_id: u32,
    id: RelId,
    ts: Option<i64>,
) {
    map.entry(node).or_default().reposition(type_id, id, ts);
}

impl Graph {
    pub fn degree_out(&self, id: NodeId) -> Vec<RelId> {
        flatten_adj(self.out_adj.get(&id).as_deref())
    }

    /// Typed adjacency length without snapshot filtering. An approximate
    /// frontier for deciding whether a far-node index probe is cheaper than
    /// checking the reached neighbors. `types` resolved once at the walk's
    /// construction; the untyped filter ([`RelTypes::ids`] is `None`)
    /// counts every type.
    pub(crate) fn typed_adj_len(
        &self,
        id: NodeId,
        dir: relate_ir::Direction,
        types: &RelTypes,
    ) -> usize {
        let Some(ids) = types.ids() else {
            return self.adj_len(id, dir);
        };
        let len = |m: &IdVec<TypedAdj>| {
            let Some(adj) = m.get(&id) else {
                return 0;
            };
            ids.clone().map(|type_id| adj.chunk(type_id).len()).sum()
        };
        match dir {
            relate_ir::Direction::Outgoing => len(&self.out_adj),
            relate_ir::Direction::Incoming => len(&self.in_adj),
            relate_ir::Direction::Both => len(&self.out_adj) + len(&self.in_adj),
        }
    }

    /// One node's out-degree on one rel type (the typed adjacency chunk
    /// length) — the id-based core `typed_adj_len`'s per-type arm and the
    /// LABS-351 histogram maintenance both consume. No string re-interning:
    /// the caller holds the interned type id.
    pub(crate) fn out_chunk_len(&self, node: NodeId, type_id: u32) -> u64 {
        self.out_adj
            .get(&node)
            .map(|adj| adj.chunk(type_id).len() as u64)
            .unwrap_or(0)
    }

    /// A node's out-adjacency chunks, one `(rel type, out-degree)` pair
    /// per chunk: the module's one `(type, count)` chunk enumeration
    /// ([`hop_counts`], which the adjacency-delta estimate already
    /// consults), widened to the u64 the degree-statistic entries count
    /// in. The LABS-351 histogram maintenance
    /// ([`Graph::rebucket_added_labels`] /
    /// [`Graph::unbucket_removed_labels`]) and the admission estimate's
    /// histogram term all consult it. `Graph::incident_rels` hand-walks
    /// the same chunk map at hop granularity — the other sanctioned walk,
    /// which this accessor deliberately does not replace.
    pub(super) fn out_chunks(&self, node: NodeId) -> Vec<(u32, u64)> {
        hop_counts(&self.out_adj, node)
            .into_iter()
            .map(|(type_id, len)| (type_id, len as u64))
            .collect()
    }

    /// Adjacency length without allocation or type/overlay filtering —
    /// an approximate degree for choosing which side of an existence
    /// probe to walk (LABS-193).
    pub fn adj_len(&self, id: NodeId, dir: relate_ir::Direction) -> usize {
        let len = |m: &IdVec<TypedAdj>| {
            m.get(&id)
                .map(|a| a.chunks.iter().map(|(_, v)| v.len()).sum())
                .unwrap_or(0)
        };
        match dir {
            relate_ir::Direction::Outgoing => len(&self.out_adj),
            relate_ir::Direction::Incoming => len(&self.in_adj),
            relate_ir::Direction::Both => len(&self.out_adj) + len(&self.in_adj),
        }
    }

    pub fn degree_in(&self, id: NodeId) -> Vec<RelId> {
        flatten_adj(self.in_adj.get(&id).as_deref())
    }

    pub fn neighbors(&self, id: NodeId, dir: relate_ir::Direction) -> Vec<RelId> {
        self.neighbors_at(id, dir, self.read_ts())
    }

    /// Snapshot-visible neighbors. `types` is the name-keyed boundary
    /// spelling: it resolves once, then the walk compares interned ids
    /// ([`Self::try_for_each_neighbor_resolved_at`]). Hop ids are copied
    /// out from under the store lock before the callback runs, so a
    /// variable-length walk can re-enter this same store.
    pub fn for_each_neighbor_at<T: AsRef<str>>(
        &self,
        id: NodeId,
        dir: relate_ir::Direction,
        ts: u64,
        types: &[T],
        mut f: impl FnMut(RelId),
    ) {
        let types = self.rel_types(types);
        self.try_for_each_neighbor_resolved_at(id, dir, ts, &types, |rid| {
            f(rid);
            Ok::<(), std::convert::Infallible>(())
        })
        .expect("the closure is infallible");
    }

    /// The engine's resolved-walk core (LABS-801): every hop is a chunk
    /// lookup by interned id — no per-node interning, no string
    /// comparison, no dedup allocation. The engine's walks resolve once
    /// at their construction and come through here; the name-keyed
    /// [`Self::for_each_neighbor_at`] is the boundary's spelling.
    pub(crate) fn try_for_each_neighbor_resolved_at<E>(
        &self,
        id: NodeId,
        dir: relate_ir::Direction,
        ts: u64,
        types: &RelTypes,
        mut f: impl FnMut(RelId) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        use relate_ir::Direction;
        match dir {
            Direction::Outgoing => {
                self.for_each_typed(&self.out_adj, id, ts, types, &mut f)?;
            }
            Direction::Incoming => {
                self.for_each_typed(&self.in_adj, id, ts, types, &mut f)?;
            }
            Direction::Both => {
                let mut seen = HashSet::new();
                let mut dedup = |rid: RelId| -> std::result::Result<(), E> {
                    if seen.insert(rid) {
                        f(rid)?;
                    }
                    Ok(())
                };
                self.for_each_typed(&self.out_adj, id, ts, types, &mut dedup)?;
                self.for_each_typed(&self.in_adj, id, ts, types, &mut dedup)?;
            }
        }
        Ok(())
    }

    fn for_each_typed<E>(
        &self,
        map: &IdVec<TypedAdj>,
        id: NodeId,
        ts: u64,
        types: &RelTypes,
        f: &mut impl FnMut(RelId) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        // Copy the hop ids, then drop the store guard before visiting.
        // `IdVec`'s lock is not re-entrant. The variable-length walker
        // (`pattern_walk::walk_var_length`) and the GPM descent
        // (`gpm_rel_dfs`) call back into this same map from `f`; holding
        // the guard across that call deadlocks on the second hop.
        let hops = {
            let Some(by_type) = map.get(&id) else {
                return Ok(());
            };
            let mut hops = Vec::new();
            match types.ids() {
                None => {
                    for (_, vec) in &by_type.chunks {
                        hops.extend(vec.iter().map(|hop| hop.id));
                    }
                }
                Some(ids) => {
                    for tid in ids {
                        hops.extend(by_type.chunk(tid).iter().map(|hop| hop.id));
                    }
                }
            }
            hops
        };
        for rid in hops {
            if self.rel_visible_at(rid, ts) {
                f(rid)?;
            }
        }
        Ok(())
    }

    pub fn neighbors_at(&self, id: NodeId, dir: relate_ir::Direction, ts: u64) -> Vec<RelId> {
        let mut out = Vec::new();
        self.for_each_neighbor_at(id, dir, ts, &[] as &[String], |rid| out.push(rid));
        out
    }
}
