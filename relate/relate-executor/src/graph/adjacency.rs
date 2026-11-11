//! Adjacency access (ADR-0031/ADR-0033 typed chunks): degree, neighbor
//! iteration, and snapshot-visible traversal — split out of `mod.rs`.
//!
//! Not to be confused with [`crate::adjacency`], the store tier these
//! accessors read and write: the typed hop chunks, the sealed stream, and
//! the fold between them.
use super::hop_counts;
use super::id_vec::IdVec;
use super::*;

fn flatten_adj(map: &IdVec<TypedAdj>, sealed: &IdVec<SealedAdj>, node: NodeId) -> Vec<RelId> {
    map.get(&node)
        .map(|adj| adj.flatten(sealed.get(&node).as_deref()))
        .unwrap_or_default()
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
    sealed: &IdVec<SealedAdj>,
    node: NodeId,
    type_id: u32,
    id: RelId,
    ts: Option<i64>,
    xmax: u64,
) {
    let mut adj_entry = map.entry(node);
    // Only a sealed chunk can hold a position to supersede; a delta-only
    // hop is removed outright, so recording its tombstone would leave an
    // empty region — and its slot array — behind for nothing.
    if sealed
        .get(&node)
        .is_some_and(|region| region.chunk(type_id).is_some())
    {
        let mut sealed_entry = sealed.entry(node);
        adj_entry
            .or_default()
            .reposition(sealed_entry.or_default(), type_id, id, ts, xmax);
    } else {
        let adj = adj_entry.or_default();
        adj.remove(None, type_id, id);
        adj.insert(type_id, id, ts);
    }
}

/// Remove one hop from `node`'s `type_id` chunk (LABS-1116: the GC prunes
/// the hops of chains it removed, so adjacency never references a rel the
/// version map no longer holds). A node left with no chunks leaves no
/// adjacency entry behind — `remove_if` re-checks emptiness under the
/// store's write lock, so a racing insert that recreated a chunk wins and
/// the entry stays. A sealed region the removal emptied drops its side-table
/// entry too.
pub(super) fn adj_remove(
    map: &IdVec<TypedAdj>,
    sealed: &IdVec<SealedAdj>,
    node: NodeId,
    type_id: u32,
    id: RelId,
) {
    if let Some(mut adj) = map.get_mut(&node) {
        let mut region = sealed.get_mut(&node);
        adj.remove(region.as_deref_mut(), type_id, id);
    }
    map.remove_if(&node, |adj| adj.chunks.is_empty());
    sealed.remove_if(&node, |region| region.chunks.is_empty());
}

/// Record the versioned tombstone a delete places on `node`'s hop (ADR-0551).
/// A hop that only lives in the delta records one too — the write path
/// never decodes the sealed stream to find out, and the fold resolves it (a
/// tombstone whose id names no position is dropped; a deleted delta hop
/// folds in under its delete stamp). The delta entry is not a precondition:
/// `adj_remove` drops it one `IdVec` lock ahead of the sealed region's, and
/// the tombstone must land in that window too.
pub(super) fn adj_tombstone_deleted(
    sealed: &IdVec<SealedAdj>,
    node: NodeId,
    type_id: u32,
    id: RelId,
    xmax: u64,
) {
    sealed.entry(node).or_default().tombstone_deleted(
        type_id,
        id,
        xmax,
        crate::adjacency::Supersede::Deleted,
    );
}

impl Graph {
    /// Fold every due adjacency chunk's delta and reclaimed tombstones into
    /// its sealed stream (ADR-0551). Runs at the checkpoint (through
    /// [`Graph::gc_versions`]); a chunk below the watermark stays delta. The
    /// node's sealed side-table entry is taken only when a chunk is due, and
    /// dropped when its last chunk recedes.
    pub(super) fn compact_adjacency_at_watermark(&self, horizon: u64) {
        self.compact_side(&self.out_adj, &self.sealed_out, horizon);
        self.compact_side(&self.in_adj, &self.sealed_in, horizon);
        self.sealed_out
            .retain(|_, region| !region.chunks.is_empty());
        self.sealed_in.retain(|_, region| !region.chunks.is_empty());
    }

    fn compact_side(&self, delta: &IdVec<TypedAdj>, sealed: &IdVec<SealedAdj>, horizon: u64) {
        // Two passes: the map's one write guard must not span other nodes' folds.
        let mut due_nodes = Vec::new();
        delta.for_each(|node, adj| {
            if !adj
                .due_chunks(sealed.get(&node).as_deref(), horizon)
                .is_empty()
            {
                due_nodes.push(node);
            }
        });
        for node in due_nodes {
            let Some(mut adj) = delta.get_mut(&node) else {
                continue;
            };
            let due = adj.due_chunks(sealed.get(&node).as_deref(), horizon);
            if due.is_empty() {
                continue;
            }
            let mut sealed_entry = sealed.entry(node);
            adj.fold_due(sealed_entry.or_default(), &due, horizon);
        }
    }

    pub fn degree_out(&self, id: NodeId) -> Vec<RelId> {
        flatten_adj(&self.out_adj, &self.sealed_out, id)
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
        let len = |m: &IdVec<TypedAdj>, sealed: &IdVec<SealedAdj>| {
            let Some(adj) = m.get(&id) else {
                return 0;
            };
            let region = sealed.get(&id);
            ids.clone()
                .map(|type_id| adj.total_len(region.as_deref(), type_id))
                .sum()
        };
        match dir {
            relate_ir::Direction::Outgoing => len(&self.out_adj, &self.sealed_out),
            relate_ir::Direction::Incoming => len(&self.in_adj, &self.sealed_in),
            relate_ir::Direction::Both => {
                len(&self.out_adj, &self.sealed_out) + len(&self.in_adj, &self.sealed_in)
            }
        }
    }

    /// One node's out-degree on one rel type (the typed adjacency chunk
    /// length) — the id-based core `typed_adj_len`'s per-type arm and the
    /// LABS-351 histogram maintenance both consume. No string re-interning:
    /// the caller holds the interned type id.
    pub(crate) fn out_chunk_len(&self, node: NodeId, type_id: u32) -> u64 {
        // Delta guard first, sealed inside it — the order every other
        // adjacency accessor takes them in.
        self.out_adj
            .get(&node)
            .map(|adj| adj.total_len(self.sealed_out.get(&node).as_deref(), type_id) as u64)
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
        hop_counts(&self.out_adj, &self.sealed_out, node)
            .into_iter()
            .map(|(type_id, len)| (type_id, len as u64))
            .collect()
    }

    /// Adjacency length without allocation or type/overlay filtering —
    /// an approximate degree for choosing which side of an existence
    /// probe to walk (LABS-193).
    pub fn adj_len(&self, id: NodeId, dir: relate_ir::Direction) -> usize {
        let len = |m: &IdVec<TypedAdj>, sealed: &IdVec<SealedAdj>| {
            m.get(&id)
                .map(|a| {
                    a.chunk_lens(sealed.get(&id).as_deref())
                        .map(|(_, delta, sealed)| delta + sealed)
                        .sum()
                })
                .unwrap_or(0)
        };
        match dir {
            relate_ir::Direction::Outgoing => len(&self.out_adj, &self.sealed_out),
            relate_ir::Direction::Incoming => len(&self.in_adj, &self.sealed_in),
            relate_ir::Direction::Both => {
                len(&self.out_adj, &self.sealed_out) + len(&self.in_adj, &self.sealed_in)
            }
        }
    }

    pub fn degree_in(&self, id: NodeId) -> Vec<RelId> {
        flatten_adj(&self.in_adj, &self.sealed_in, id)
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
    /// comparison. The engine's walks resolve once at their construction
    /// and come through here; the name-keyed [`Self::for_each_neighbor_at`]
    /// is the boundary's spelling.
    ///
    /// The rid-only spelling of the one walk core,
    /// [`Self::try_for_each_hop_resolved_at`]: it discards the endpoints.
    pub(crate) fn try_for_each_neighbor_resolved_at<E>(
        &self,
        id: NodeId,
        dir: relate_ir::Direction,
        ts: u64,
        types: &RelTypes,
        mut f: impl FnMut(RelId) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        self.try_for_each_hop_resolved_at(id, dir, ts, types, |rid, _| f(rid))
    }

    /// The resolved-walk core's hop spelling: each visible relationship
    /// with the node it reaches. The peer comes from the one version-chain
    /// read that also answers visibility — a rel walk plus a `rel_ends_at`
    /// per relationship reads the same chain twice per hop, which every
    /// walk-then-`hop` loop (the traversal kernels, the shortest-path
    /// searches) paid per followed relationship.
    pub(crate) fn try_for_each_hop_resolved_at<E>(
        &self,
        id: NodeId,
        dir: relate_ir::Direction,
        ts: u64,
        types: &RelTypes,
        mut f: impl FnMut(RelId, NodeId) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        use relate_ir::Direction;
        let mut emit = |rid: RelId, start: NodeId, end: NodeId| {
            match crate::store::hop_other(id, start, end, dir) {
                Some(peer) => f(rid, peer),
                // A hop whose resolved endpoints do not answer to `dir`
                // (a stale hop of a reused rel id) is dropped — the
                // walk + `hop` spelling this replaces did the same.
                None => Ok(()),
            }
        };
        match dir {
            Direction::Outgoing => {
                self.for_each_typed_ends(
                    &self.out_adj,
                    &self.sealed_out,
                    id,
                    ts,
                    types,
                    &mut emit,
                )?;
            }
            Direction::Incoming => {
                self.for_each_typed_ends(&self.in_adj, &self.sealed_in, id, ts, types, &mut emit)?;
            }
            Direction::Both => {
                // Dedup by rel id, not endpoints: a self-loop sits in
                // both of a node's lists, and a reused rel id (WAL redo
                // replay keeps the old hop in the old end's adjacency)
                // can repeat within one list. The crate's engine-id set
                // alias — `UsedSet`, the FxHash/CoreMap rule
                // (ADR-0355 / LABS-337).
                let mut seen = crate::UsedSet::default();
                let mut dedup = |rid: RelId, start: NodeId, end: NodeId| {
                    if seen.insert(rid) {
                        emit(rid, start, end)?;
                    }
                    Ok(())
                };
                self.for_each_typed_ends(
                    &self.out_adj,
                    &self.sealed_out,
                    id,
                    ts,
                    types,
                    &mut dedup,
                )?;
                self.for_each_typed_ends(&self.in_adj, &self.sealed_in, id, ts, types, &mut dedup)?;
            }
        }
        Ok(())
    }

    /// The one typed-chunk walk skeleton: chunk selection by the resolved
    /// type filter, one version-chain read per hop (visibility and both
    /// endpoints — `rel_ends_at` answering `Some` is the same check
    /// `rel_visible_at` makes).
    fn for_each_typed_ends<E>(
        &self,
        map: &IdVec<TypedAdj>,
        sealed: &IdVec<SealedAdj>,
        id: NodeId,
        ts: u64,
        types: &RelTypes,
        f: &mut impl FnMut(RelId, NodeId, NodeId) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        // Copy the hop ids, then drop the store guard before visiting.
        // `IdVec`'s lock is not re-entrant. The variable-length walker
        // (`pattern_walk::walk_var_length`) and the GPM descent
        // (`gpm_rel_dfs`) call back into this same map from `f`; holding
        // the guard across that call deadlocks on the second hop. The walk
        // is the delta+sealed merge at `ts` (ADR-0551), delta first.
        let hops = {
            let Some(by_type) = map.get(&id) else {
                return Ok(());
            };
            let region = sealed.get(&id);
            let mut hops = Vec::new();
            match types.ids() {
                None => by_type.for_each_hop(region.as_deref(), ts, |hop| hops.push(hop.id)),
                Some(ids) => {
                    for tid in ids {
                        by_type
                            .for_each_hop_of(region.as_deref(), tid, ts, |hop| hops.push(hop.id));
                    }
                }
            }
            hops
        };
        for rid in hops {
            if let Some((start, end)) = self.rel_ends_at(rid, ts) {
                f(rid, start, end)?;
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
