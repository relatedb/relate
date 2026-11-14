//! Adjacency access (ADR-0031/ADR-0033 typed chunks): degree, neighbor
//! iteration, and snapshot-visible traversal — split out of `mod.rs`.
//!
//! Not to be confused with [`crate::adjacency`], the store tier these
//! accessors read and write: the typed hop chunks, the sealed stream, and
//! the fold between them.
use super::id_vec::IdVec;
use super::shard;
use super::*;
use crate::adjacency::AdjHop;

/// One adjacency chunk's type and delta + sealed hop count — named, because
/// the two integers swap silently as a tuple.
#[derive(Clone, Copy, Debug)]
pub(super) struct ChunkHops {
    pub(super) type_id: u32,
    pub(super) hops: usize,
}

/// One direction's adjacency pair: the live delta map and the sealed side
/// table beside it (ADR-0551), borrowed together. The pairing
/// (`out_adj`/`sealed_out`, `in_adj`/`sealed_in`) is stated once, in
/// [`Graph::out_side`] and [`Graph::in_side`]; every reader, writer and
/// estimator takes the side, so an out/in mix-up between the two halves
/// cannot be written.
#[derive(Clone, Copy)]
pub(super) struct AdjSide<'a> {
    pub(super) delta: &'a IdVec<TypedAdj>,
    pub(super) sealed: &'a IdVec<SealedAdj>,
}

impl AdjSide<'_> {
    /// `node`'s delta chunks and its sealed region, both read guards taken
    /// delta first (the order every adjacency accessor takes them in).
    /// `None` when the node has no delta entry.
    pub(super) fn with_adj<R>(
        &self,
        node: NodeId,
        f: impl FnOnce(&TypedAdj, Option<&SealedAdj>) -> R,
    ) -> Option<R> {
        let adj = self.delta.get(&node)?;
        let region = self.sealed.get(&node);
        Some(f(&adj, region.as_deref()))
    }

    /// Every physical hop id of `node` in merged order, no visibility filter.
    fn flatten(&self, node: NodeId) -> Vec<RelId> {
        self.with_adj(node, |adj, region| adj.flatten(region))
            .unwrap_or_default()
    }

    /// One node's adjacency floor: the delta hop bytes plus its sealed
    /// side-table entry's compressed chunks and tombstones.
    pub(super) fn floor(&self, node: NodeId) -> u64 {
        self.delta.get(&node).map_or(0, |adj| adj.floor_bytes())
            + self
                .sealed
                .get(&node)
                .map_or(0, |region| region.floor_bytes())
    }

    /// Slot-array bytes of both tables — the term the census reads as
    /// `slot_bytes`.
    pub(super) fn array_bytes(&self) -> u64 {
        self.delta.array_bytes() + self.sealed.array_bytes()
    }

    /// One [`ChunkHops`] per chunk of `node`, in first-seen type order.
    pub(super) fn hop_counts(&self, node: NodeId) -> Vec<ChunkHops> {
        self.with_adj(node, |adj, region| {
            adj.chunk_lens(region)
                .map(|(type_id, delta, sealed)| ChunkHops {
                    type_id,
                    hops: delta + sealed,
                })
                .collect()
        })
        .unwrap_or_default()
    }

    pub(super) fn insert(&self, node: NodeId, type_id: u32, id: RelId, ts: Option<i64>) {
        self.delta.entry(node).or_default().insert(type_id, id, ts);
    }

    pub(super) fn reposition(
        &self,
        node: NodeId,
        type_id: u32,
        id: RelId,
        ts: Option<i64>,
        xmax: u64,
    ) {
        let mut adj_entry = self.delta.entry(node);
        // Only a sealed chunk can hold a position to supersede; a delta-only
        // hop is removed outright, so recording its tombstone would leave an
        // empty region — and its slot array — behind for nothing.
        if self
            .sealed
            .get(&node)
            .is_some_and(|region| region.chunk(type_id).is_some())
        {
            let mut sealed_entry = self.sealed.entry(node);
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
    /// the entry stays. A sealed region the removal emptied drops its
    /// side-table entry too.
    pub(super) fn remove(&self, node: NodeId, type_id: u32, id: RelId) {
        if let Some(mut adj) = self.delta.get_mut(&node) {
            let mut region = self.sealed.get_mut(&node);
            adj.remove(region.as_deref_mut(), type_id, id);
        }
        self.delta.remove_if(&node, |adj| adj.chunks.is_empty());
        self.sealed
            .remove_if(&node, |region| region.chunks.is_empty());
    }

    /// Record the versioned tombstone a delete places on `node`'s hop
    /// (ADR-0551). A hop that only lives in the delta records one too — the
    /// write path never decodes the sealed stream to find out, and the fold
    /// resolves it (a tombstone whose id names no position is dropped; a
    /// deleted delta hop folds in under its delete stamp). The delta entry is
    /// not a precondition: [`Self::remove`] drops it one `IdVec` lock ahead
    /// of the sealed region's, and the tombstone must land in that window
    /// too.
    pub(super) fn tombstone_deleted(&self, node: NodeId, type_id: u32, id: RelId, xmax: u64) {
        self.sealed.entry(node).or_default().tombstone_deleted(
            type_id,
            id,
            xmax,
            crate::adjacency::Supersede::Deleted,
        );
    }

    /// Fold every due chunk of this side at `horizon` (ADR-0551).
    fn compact(&self, horizon: u64) {
        // Two passes: the map's one write guard must not span other nodes' folds.
        let mut due_nodes = Vec::new();
        self.delta.for_each(|node, adj| {
            if !adj
                .due_chunks(self.sealed.get(&node).as_deref(), horizon)
                .is_empty()
            {
                due_nodes.push(node);
            }
        });
        for node in due_nodes {
            let Some(mut adj) = self.delta.get_mut(&node) else {
                continue;
            };
            let due = adj.due_chunks(self.sealed.get(&node).as_deref(), horizon);
            if due.is_empty() {
                continue;
            }
            let mut sealed_entry = self.sealed.entry(node);
            adj.fold_due(sealed_entry.or_default(), &due, horizon);
        }
        self.sealed.retain(|_, region| !region.chunks.is_empty());
    }

    /// Adjacency length without allocation or type/overlay filtering.
    fn len(&self, node: NodeId) -> usize {
        self.with_adj(node, |adj, region| {
            adj.chunk_lens(region)
                .map(|(_, delta, sealed)| delta + sealed)
                .sum()
        })
        .unwrap_or(0)
    }

    /// Length over the resolved `type_ids`.
    fn typed_len(&self, node: NodeId, type_ids: impl Iterator<Item = u32>) -> usize {
        self.with_adj(node, |adj, region| {
            type_ids.map(|type_id| adj.total_len(region, type_id)).sum()
        })
        .unwrap_or(0)
    }
}

impl Graph {
    /// The outgoing adjacency pair — the one place `out_adj` meets
    /// `sealed_out`.
    pub(super) fn out_side(&self) -> AdjSide<'_> {
        AdjSide {
            delta: &self.out_adj,
            sealed: &self.sealed_out,
        }
    }

    /// The incoming adjacency pair — the one place `in_adj` meets
    /// `sealed_in`.
    pub(super) fn in_side(&self) -> AdjSide<'_> {
        AdjSide {
            delta: &self.in_adj,
            sealed: &self.sealed_in,
        }
    }
}

/// Which endpoint of a [`RelRow`] one direction's grouping pass keys on.
#[derive(Clone, Copy)]
enum Side {
    Start,
    End,
}

impl Side {
    fn endpoint(&self, row: &RelRow) -> NodeId {
        match self {
            Side::Start => row.start,
            Side::End => row.end,
        }
    }
}

/// One direction's wholesale build: `map` keyed by the row's `side`
/// endpoint, each (node, type) run handed to [`TypedAdj::from_runs`] —
/// no per-hop binary search or memmove, and the chunk order and hop order
/// are the store tier's, the order an id-ordered load's incremental
/// [`adj_insert`]s leave (the wholesale build's pinned contract).
///
/// Sharded on that same endpoint (the partition its caller hands it), so
/// a shard owns every row of every node it carries and builds those
/// nodes whole. The install is the only writer, and the per-node result
/// is shard-independent: `from_runs` derives a node's chunk order from
/// each run's first rel id and sorts its hops `cmp_ts_desc`, both total
/// orders over a node's own hops.
fn build_side(map: &IdVec<TypedAdj>, rows: &[RelRow], side: Side, shards: &[Vec<usize>]) {
    let parts = shard::run_shards(shards, |shard| build_shard(rows, shard, side));
    for (node, adj) in parts.into_iter().flatten() {
        map.insert(node, adj);
    }
}

/// One shard's whole build: sort its row indices by `(endpoint, type)`,
/// then one [`TypedAdj`] per node from that node's runs. The sort works
/// a local copy of the caller's shard — the partition is shared
/// read-only across the rebuild's consumers, so this pass never
/// reorders it.
fn build_shard(rows: &[RelRow], shard: &[usize], side: Side) -> Vec<(NodeId, TypedAdj)> {
    let endpoint = |row: &RelRow| side.endpoint(row);
    let mut order = shard.to_vec();
    order.sort_unstable_by(|&a, &b| {
        let (a, b) = (&rows[a], &rows[b]);
        endpoint(a)
            .cmp(&endpoint(b))
            .then(a.type_id.cmp(&b.type_id))
    });
    let mut built = Vec::new();
    for node_run in order.chunk_by(|&a, &b| endpoint(&rows[a]) == endpoint(&rows[b])) {
        let node = endpoint(&rows[node_run[0]]);
        // One run per type: its type id and its hops, unsorted and keyless —
        // `TypedAdj::from_runs` derives the chunk-order key from the hops.
        let runs = node_run
            .chunk_by(|&a, &b| rows[a].type_id == rows[b].type_id)
            .map(|run| {
                let hops: Vec<AdjHop> = run
                    .iter()
                    .map(|&index| {
                        let row = &rows[index];
                        AdjHop {
                            ts: row.ts,
                            id: row.id,
                        }
                    })
                    .collect();
                (rows[run[0]].type_id, hops)
            });
        built.push((node, TypedAdj::from_runs(runs)));
    }
    built
}

/// Install both adjacency maps wholesale from the live rel rows: `out_map`
/// keyed by each row's start, `in_map` by its end. Both maps must be empty:
/// this is the whole build, not a merge. `rows` is read-only — the start
/// side reads the start-keyed partition `rebuild_adj` computed for every
/// start-keyed consumer, the end side partitions by its own endpoint here,
/// and no fold that follows a rebuild depends on the buffer's order.
///
/// Memory shape: the shard parts and the maps they fill coexist at
/// peak, but each node's `TypedAdj` is **moved** from its part into
/// the map, never copied — the build materializes one adjacency, not
/// two. Above it sit the rebuild's declared transients: the row buffer
/// and the index partitions (8 bytes per row per partition).
pub(super) fn adj_build(
    out_map: &IdVec<TypedAdj>,
    in_map: &IdVec<TypedAdj>,
    rows: &[RelRow],
    start_shards: &[Vec<usize>],
) {
    debug_assert!(
        out_map.is_empty() && in_map.is_empty(),
        "adj_build is a wholesale build, not a merge"
    );
    build_side(out_map, rows, Side::Start, start_shards);
    let end_shards = shard::shard_rows(rows, |row| row.end);
    build_side(in_map, rows, Side::End, &end_shards);
}

impl Graph {
    /// Fold every due adjacency chunk's delta and reclaimed tombstones into
    /// its sealed stream (ADR-0551). Runs at the checkpoint (through
    /// [`Graph::gc_versions`]); a chunk below the watermark stays delta. The
    /// node's sealed side-table entry is taken only when a chunk is due, and
    /// dropped when its last chunk recedes.
    pub(super) fn compact_adjacency_at_watermark(&self, horizon: u64) {
        self.out_side().compact(horizon);
        self.in_side().compact(horizon);
    }

    pub fn degree_out(&self, id: NodeId) -> Vec<RelId> {
        self.out_side().flatten(id)
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
        let len = |side: AdjSide<'_>| side.typed_len(id, ids.clone());
        match dir {
            relate_ir::Direction::Outgoing => len(self.out_side()),
            relate_ir::Direction::Incoming => len(self.in_side()),
            relate_ir::Direction::Both => len(self.out_side()) + len(self.in_side()),
        }
    }

    /// One node's out-degree on one rel type (the typed adjacency chunk
    /// length) — the id-based core `typed_adj_len`'s per-type arm and the
    /// LABS-351 histogram maintenance both consume. No string re-interning:
    /// the caller holds the interned type id.
    pub(crate) fn out_chunk_len(&self, node: NodeId, type_id: u32) -> u64 {
        self.out_side().typed_len(node, std::iter::once(type_id)) as u64
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
        self.out_side()
            .hop_counts(node)
            .into_iter()
            .map(|c| (c.type_id, c.hops as u64))
            .collect()
    }

    /// Adjacency length without allocation or type/overlay filtering —
    /// an approximate degree for choosing which side of an existence
    /// probe to walk (LABS-193).
    pub fn adj_len(&self, id: NodeId, dir: relate_ir::Direction) -> usize {
        match dir {
            relate_ir::Direction::Outgoing => self.out_side().len(id),
            relate_ir::Direction::Incoming => self.in_side().len(id),
            relate_ir::Direction::Both => self.out_side().len(id) + self.in_side().len(id),
        }
    }

    pub fn degree_in(&self, id: NodeId) -> Vec<RelId> {
        self.in_side().flatten(id)
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
                self.for_each_typed_ends(self.out_side(), id, ts, types, &mut emit)?;
            }
            Direction::Incoming => {
                self.for_each_typed_ends(self.in_side(), id, ts, types, &mut emit)?;
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
                self.for_each_typed_ends(self.out_side(), id, ts, types, &mut dedup)?;
                self.for_each_typed_ends(self.in_side(), id, ts, types, &mut dedup)?;
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
        side: AdjSide<'_>,
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
        let hops = side
            .with_adj(id, |by_type, region| {
                let mut hops = Vec::new();
                match types.ids() {
                    None => by_type.for_each_hop(region, ts, |hop| hops.push(hop.id)),
                    Some(ids) => {
                        for tid in ids {
                            by_type.for_each_hop_of(region, tid, ts, |hop| hops.push(hop.id));
                        }
                    }
                }
                hops
            })
            .unwrap_or_default();
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
