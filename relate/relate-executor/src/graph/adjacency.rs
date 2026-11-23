//! Adjacency access (ADR-0031/ADR-0033 typed chunks): degree, neighbor
//! iteration, and snapshot-visible traversal — split out of `mod.rs`.
//!
//! Not to be confused with [`crate::adjacency`], the store tier these
//! accessors read and write: the typed hop chunks, the sealed stream, and
//! the fold between them.
use super::id_vec::IdVec;
use super::rebuild::RelRow;
use super::shard::{self, ShardPartition};
use super::*;
use crate::adjacency::{streams_admit, Hop, SealedRegion, SealedStream};

/// One adjacency chunk's type and delta + sealed hop count — named, because
/// the two integers swap silently as a tuple.
#[derive(Clone, Copy, Debug)]
pub(super) struct ChunkHops {
    pub(super) type_id: u32,
    pub(super) hops: usize,
}

/// Most hops one [`Graph::try_for_each_hop_batch_resolved_at`] call resolves.
pub(crate) const HOP_BATCH: usize = 256;

/// The reusable buffers of a batched hop read: the node's hop ids copied
/// out of the adjacency guard and the resolved `(peer, rel id)` batch. A
/// read keeps them across nodes, so a hop list is never allocated per node;
/// they grow to the largest degree seen. Cloning yields an empty value: a
/// copy of a search must never carry a node's hop list.
#[derive(Default)]
pub(crate) struct HopBuf {
    ids: Vec<RelId>,
    hops: Vec<(NodeId, RelId)>,
}

impl HopBuf {
    /// Forget the batch a collecting walk left behind.
    pub(crate) fn clear_hops(&mut self) {
        self.hops.clear();
    }

    /// Append one collected hop for [`Self::emit_batches`].
    pub(crate) fn push_hop(&mut self, peer: NodeId, rid: RelId) {
        self.hops.push((peer, rid));
    }

    /// Call `f` once per slice of at most [`HOP_BATCH`] collected hops: the
    /// one batching rule, shared by the walk that resolves per batch and
    /// the one that collects first.
    pub(crate) fn emit_batches<E>(
        &self,
        f: impl FnMut(&[(NodeId, RelId)]) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        self.hops.chunks(HOP_BATCH).try_for_each(f)
    }
}

impl Clone for HopBuf {
    fn clone(&self) -> Self {
        Self::default()
    }
}

/// One direction's adjacency pair: the live delta map and the sealed side
/// table beside it (ADR-0551), borrowed together. The pairing
/// (`out_adj`/`sealed_out`, `in_adj`/`sealed_in`) is stated once, in
/// [`Graph::out_side`] and [`Graph::in_side`]; every reader, writer and
/// estimator takes the side, so an out/in mix-up between the two halves
/// cannot be written.
#[derive(Clone, Copy)]
pub(super) struct AdjSide<'a> {
    delta: &'a IdVec<TypedAdj>,
    sealed: &'a SealedArena,
}

impl<'a> AdjSide<'a> {
    /// This side's live delta map.
    pub(super) fn delta(&self) -> &'a IdVec<TypedAdj> {
        self.delta
    }

    /// This side's sealed side table.
    pub(super) fn sealed(&self) -> &'a SealedArena {
        self.sealed
    }

    /// `node`'s delta chunks and its sealed region, both read guards taken
    /// delta first (the order every adjacency accessor takes them in).
    /// `None` when the node has no delta entry.
    pub(super) fn with_adj<R>(
        &self,
        node: NodeId,
        f: impl FnOnce(&TypedAdj, Option<SealedRegion<'_>>) -> R,
    ) -> Option<R> {
        let adj = self.delta.get(&node)?;
        let region = self.sealed.region(node);
        Some(f(&adj, region))
    }

    /// Drop both of this side's tables — the wholesale build's replacement
    /// step, so a rebuild clears the pair it installs into in one place.
    fn clear(&self) {
        self.delta.clear();
        self.sealed.clear();
    }

    /// Every physical hop id of `node` in merged order, no visibility filter.
    fn flatten(&self, node: NodeId) -> Vec<RelId> {
        self.with_adj(node, |adj, region| adj.flatten(region))
            .unwrap_or_default()
    }

    /// One node's adjacency floor in this direction: the delta hop bytes.
    /// The sealed tier is a graph-level arena (ADR-0599); the write path
    /// charges its events per region through `Graph::price_sealed_event`,
    /// never a whole-tier walk.
    pub(super) fn floor(&self, node: NodeId) -> u64 {
        self.delta.get(&node).map_or(0, |adj| adj.floor_bytes())
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
        let region = self
            .sealed
            .region(node)
            .filter(|region| region.has_chunk(type_id));
        match region {
            Some(region) => adj_entry
                .or_default()
                .reposition(region, type_id, id, ts, xmax),
            None => {
                let adj = adj_entry.or_default();
                adj.remove(None, type_id, id);
                adj.insert(type_id, id, ts);
            }
        }
    }

    /// Remove one hop from `node`'s `type_id` chunk (LABS-1116: the GC prunes
    /// the hops of chains it removed, so adjacency never references a rel the
    /// version map no longer holds). A node left with no chunks leaves no
    /// adjacency entry behind — `remove_if` re-checks emptiness under the
    /// store's write lock, so a racing insert that recreated a chunk wins and
    /// the entry stays. A sealed region the removal emptied drops its arena
    /// entry too (the fold's `put_chunk` with an empty stream drops it).
    pub(super) fn remove(&self, node: NodeId, type_id: u32, id: RelId) {
        if let Some(mut adj) = self.delta.get_mut(&node) {
            adj.remove(self.sealed.region(node), type_id, id);
        }
        self.delta.remove_if(&node, TypedAdj::is_empty);
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
        self.sealed.tombstone_deleted(
            node,
            type_id,
            id,
            xmax,
            crate::adjacency::Supersede::Deleted,
        );
    }

    /// One fold pass of this side (ADR-0551 / ADR-0599) — the checkpoint's
    /// watermark gate, which also notes stability baselines. Two phases:
    /// the map's one write guard must not span other nodes' folds. The
    /// collection walk asks each node whether it has work and keeps the
    /// nodes that answer; the write phase hands each of them to the tier,
    /// which selects, folds and notes under that node's guard.
    fn fold(&self, horizon: u64) {
        let mut nodes = Vec::new();
        self.delta.for_each(|node, adj| {
            if adj.has_fold_work(self.sealed.region(node), horizon) {
                nodes.push(node);
            }
        });
        for node in nodes {
            let Some(mut adj) = self.delta.get_mut(&node) else {
                continue;
            };
            adj.fold_at(self.sealed.region_or_default(node), horizon);
        }
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

    /// The sides a walk in `dir` reads, each with the scan direction it
    /// answers to: the one expansion of a (possibly `Both`) direction into
    /// adjacency pairs, outgoing side first.
    pub(super) fn sides(
        &self,
        dir: relate_ir::Direction,
    ) -> impl Iterator<Item = (AdjSide<'_>, relate_ir::Direction)> {
        use relate_ir::Direction;
        [
            (self.out_side(), Direction::Outgoing),
            (self.in_side(), Direction::Incoming),
        ]
        .into_iter()
        .filter(move |&(_, scan_dir)| dir == Direction::Both || dir == scan_dir)
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

/// Which endpoint of a [`RelRow`] one direction's grouping pass keys on —
/// the start side (the out adjacency) or the end side (the in adjacency).
#[derive(Clone, Copy)]
pub enum Side {
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

    /// This direction's adjacency pair — the one place the endpoint a
    /// direction partitions rows by meets the tables it installs into, so
    /// the two cannot be paired the wrong way round.
    fn adj(self, graph: &Graph) -> AdjSide<'_> {
        match self {
            Side::Start => graph.out_side(),
            Side::End => graph.in_side(),
        }
    }
}

/// One direction's wholesale build: the direction's delta keyed by its
/// [`Side`] endpoint, each (node, type) run handed to [`TypedAdj::from_runs`]
/// — no per-hop binary search or memmove, and the chunk order and hop order
/// are the store tier's, the order an id-ordered load's incremental
/// [`AdjSide::insert`]s leave (the wholesale build's pinned contract).
///
/// Sharded on that same endpoint (the partition its caller hands it), so
/// a shard owns every row of every node it carries and builds those
/// nodes whole. The per-node delta follows `from_runs`'s order (each run's
/// first rel id, hops `cmp_ts_desc`, both total orders over a node's own
/// hops), and the sealed chunks are encoded by the shards straight from
/// their sorted runs into streams of their own — the build pass's own
/// emission (ADR-0608), not a second walk, and no shard touches the
/// arena's lock. The streams are laid into the arena once, in shard order,
/// into buffers reserved at their final size, so the closing fit copies
/// nothing and no dead ranges are left to compact.
fn build_side(graph: &Graph, rows: &[RelRow], side: Side, shards: &mut ShardPartition) {
    // The replace-wholesale step belongs to the build that replaces: this
    // side's pair is emptied here, so a rebuild cannot leave a direction's
    // stale chunks behind.
    let adjacency = side.adj(graph);
    adjacency.clear();
    let parts = shard::run_shards(shards, |shard| build_shard(rows, shard, side, true));
    // Each shard's stream passed the arena wall on its own bytes; the sum is
    // what the install lays. Over the wall, no shard sees it, so the side is
    // built again with every run in the delta tier (ADR-0599) — the open
    // returns the same graph, only unsealed.
    let parts = if streams_admit(parts.iter().map(|(_, stream)| stream)) {
        parts
    } else {
        drop(parts);
        shard::run_shards(shards, |shard| build_shard(rows, shard, side, false))
    };
    let mut streams = Vec::with_capacity(parts.len());
    for (nodes, stream) in parts {
        for (node, adj) in nodes {
            adjacency.delta.insert(node, adj);
        }
        streams.push(stream);
    }
    adjacency.sealed.install(streams);
    adjacency.sealed.fit();
}

/// One shard's whole build: sort its row indices by `(endpoint, type)`,
/// then one [`TypedAdj`] per node from that node's runs, with the sealed
/// chunks encoded into the shard's own [`SealedStream`] when `seal` is set
/// (`false` leaves every run in the delta). The sort works the shard slice
/// in place — a shard's indices are a set, so the other consumers of a
/// shared partition never depend on their order.
fn build_shard(
    rows: &[RelRow],
    shard: &mut [usize],
    side: Side,
    seal: bool,
) -> (Vec<(NodeId, TypedAdj)>, SealedStream) {
    let endpoint = |row: &RelRow| side.endpoint(row);
    shard.sort_unstable_by(|&a, &b| {
        let (a, b) = (&rows[a], &rows[b]);
        endpoint(a)
            .cmp(&endpoint(b))
            .then(a.type_id.cmp(&b.type_id))
    });
    let mut built = Vec::new();
    let mut sealed = SealedStream::default();
    for node_run in shard.chunk_by(|&a, &b| endpoint(&rows[a]) == endpoint(&rows[b])) {
        built.push(build_node(rows, node_run, side, &mut sealed, seal));
    }
    sealed.shrink();
    (built, sealed)
}

/// One node's whole build from its `(endpoint, type)`-sorted row indices:
/// the per-type runs in one [`TypedAdj`], then the durable shape
/// (ADR-0608).
///
/// The durable shape: the build seals through its own gate — every run at
/// [`RESTORE_SEAL_HOPS`] hops or more, horizon 0 — encoding the run's
/// sorted hops directly into `sealed` rather than staging them in a delta
/// chunk to fold out again. The stream is the one the checkpoint's GC fold
/// would encode for the same hops, in the same chunk order, so a rebuilt
/// graph and a checkpoint-folded one cannot land on different layouts.
/// Both wholesale rebuilds (a restore and an aborted write's) emit it
/// here, in the one pass that derives adjacency, instead of a second
/// serial walk folding afterwards. Runs below the gate stay packed in the
/// delta.
fn build_node(
    rows: &[RelRow],
    node_run: &[usize],
    side: Side,
    sealed: &mut SealedStream,
    seal: bool,
) -> (NodeId, TypedAdj) {
    let node = side.endpoint(&rows[node_run[0]]);
    // One run per type: its type id and its hops, unsorted and keyless —
    // `TypedAdj::from_runs` derives the chunk-order key from the hops.
    let runs = node_run
        .chunk_by(|&a, &b| rows[a].type_id == rows[b].type_id)
        .map(|run| {
            let hops: Vec<Hop> = run
                .iter()
                .map(|&index| {
                    let row = &rows[index];
                    Hop {
                        ts: row.ts,
                        id: row.id,
                    }
                })
                .collect();
            (rows[run[0]].type_id, hops)
        });
    let adj = TypedAdj::from_runs(runs, |type_id, hops| {
        seal && sealed.seal_chunk(node, type_id, hops)
    });
    sealed.end_node();
    (node, adj)
}

/// Replace both adjacency pairs wholesale from the live rel rows: the
/// start side's delta keyed by each row's start, the end side's by its end,
/// with each pair's sealed table filled alongside it. Each side's pair is
/// emptied by [`build_side`] as it is built, so a rebuild cannot leave a
/// direction's stale chunks behind — this is the whole build, not a merge.
/// `rows` is read-only — the start side reads (and sorts in place) the
/// start-keyed partition `rebuild_adj` computed for every start-keyed
/// consumer, the end side partitions by its own endpoint here and drops
/// that partition before returning, and no fold that follows a rebuild
/// depends on the buffers' order.
///
/// Memory shape: the shard parts and the tables they fill coexist at
/// peak, but each node's `TypedAdj` is **moved** from its part into
/// the map, never copied — the build materializes one adjacency, not
/// two. Above it sit the rebuild's declared transients: the row buffer
/// and the index partitions (8 bytes per row per partition).
pub(super) fn adj_build(graph: &Graph, rows: &[RelRow], start_shards: &mut ShardPartition) {
    build_side(graph, rows, Side::Start, start_shards);
    let mut end_shards = shard::shard_rows(rows, |row| row.end);
    let _end_lease = end_shards.lease();
    build_side(graph, rows, Side::End, &mut end_shards);
}

impl Graph {
    /// Fold every due adjacency chunk's delta and reclaimed tombstones into
    /// its sealed stream (ADR-0551). Runs at the checkpoint (through
    /// [`Graph::gc_versions`]); a chunk below the watermark stays delta. A
    /// whole-graph rebuild does not fold here — it emits the durable shape in
    /// its own pass (ADR-0608). The node's sealed region is taken only when a
    /// chunk is due, and dropped when its last chunk recedes. The arena is
    /// compacted after the fold (ADR-0599): the fold appends the new encodings
    /// and marks the replaced spans dead, and compaction reclaims them.
    pub(super) fn compact_adjacency_at_watermark(&self, horizon: u64) {
        self.fold_adjacency_at_watermark(horizon);
        self.sealed_out.compact();
        self.sealed_in.compact();
    }

    /// The checkpoint pass's fold half, without the trailing compaction —
    /// the seam the reclaim test reads the dead ranges through before
    /// compaction reclaims them.
    pub(super) fn fold_adjacency_at_watermark(&self, horizon: u64) {
        self.out_side().fold(horizon);
        self.in_side().fold(horizon);
    }

    /// How many sealed chunks `node`'s adjacency holds on `side` — the out
    /// (`Side::Start`) or in (`Side::End`) direction — the wholesale build's
    /// emission (ADR-0608), read by the storage open tests across the crate
    /// boundary. Test seam behind the off-by-default `test-hooks` feature,
    /// never in a shipped build.
    #[cfg(feature = "test-hooks")]
    pub fn sealed_chunk_count(&self, node: NodeId, side: Side) -> usize {
        let sealed = match side {
            Side::Start => &self.sealed_out,
            Side::End => &self.sealed_in,
        };
        sealed.region(node).map_or(0, |region| region.chunk_count())
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
        self.sides(dir)
            .map(|(side, _)| side.typed_len(id, ids.clone()))
            .sum()
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
        self.sides(dir).map(|(side, _)| side.len(id)).sum()
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

    /// Call `f` once per batch of at most [`HOP_BATCH`] of `id`'s hops
    /// visible at `ts`, each as `(peer, rel id)`, in the walk's order. The
    /// node's hop ids are copied under the adjacency guard into `buf` and
    /// the guard is dropped before any endpoint is resolved or `f` runs
    /// (`IdVec`'s lock is not re-entrant); each batch then resolves the next
    /// slice of ids' endpoints and picks the peers. `Both` reads the
    /// outgoing list, then the incoming one; a self-loop is the one rel in
    /// both lists and is recognised by its endpoints, so the incoming pass
    /// skips it instead of keeping a per-node seen set (a reused rel id
    /// repeated within one list is therefore not collapsed, as in the
    /// directed arms).
    pub(crate) fn try_for_each_hop_batch_resolved_at<E>(
        &self,
        id: NodeId,
        dir: relate_ir::Direction,
        ts: u64,
        types: &RelTypes,
        buf: &mut HopBuf,
        mut f: impl FnMut(&[(NodeId, RelId)]) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        use relate_ir::Direction;
        for (side, scan_dir) in self.sides(dir) {
            // A self-loop is in both lists: the outgoing pass emits it, the
            // incoming pass of a `Both` walk skips it.
            let skip_self_loops = dir == Direction::Both && scan_dir == Direction::Incoming;
            self.batch_side(side, skip_self_loops, id, dir, ts, types, buf, &mut f)?;
        }
        Ok(())
    }

    /// One side's hops in batches: copy the ids under the guard, then resolve
    /// and hand out `HOP_BATCH` at a time, never an empty batch.
    #[allow(clippy::too_many_arguments)]
    fn batch_side<E>(
        &self,
        side: AdjSide<'_>,
        skip_self_loops: bool,
        id: NodeId,
        dir: relate_ir::Direction,
        ts: u64,
        types: &RelTypes,
        buf: &mut HopBuf,
        f: &mut impl FnMut(&[(NodeId, RelId)]) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        let HopBuf { ids, hops } = buf;
        ids.clear();
        self.copy_hop_ids(side, id, ts, types, ids);
        for chunk in ids.chunks(HOP_BATCH) {
            hops.clear();
            for &rid in chunk {
                let Some((start, stop)) = self.rel_ends_at(rid, ts) else {
                    continue;
                };
                if skip_self_loops && start == stop {
                    continue;
                }
                if let Some(peer) = crate::store::hop_other(id, start, stop, dir) {
                    hops.push((peer, rid));
                }
            }
            if !hops.is_empty() {
                f(hops)?;
            }
        }
        Ok(())
    }

    /// Append `id`'s visible hop ids on `side` at `ts` to `ids`, in merged
    /// order, and drop the store guard before returning. `IdVec`'s lock is
    /// not re-entrant, so no caller visits hops while holding it.
    fn copy_hop_ids(
        &self,
        side: AdjSide<'_>,
        id: NodeId,
        ts: u64,
        types: &RelTypes,
        ids: &mut Vec<RelId>,
    ) {
        side.with_adj(id, |by_type, region| match types.ids() {
            None => by_type.for_each_hop(region, ts, |hop| ids.push(hop.id)),
            Some(type_ids) => {
                for tid in type_ids {
                    by_type.for_each_hop_of(region, tid, ts, |hop| ids.push(hop.id));
                }
            }
        });
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
        let mut hops = Vec::new();
        self.copy_hop_ids(side, id, ts, types, &mut hops);
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
