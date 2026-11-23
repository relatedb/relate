//! The wholesale rebuild — one home for the family ADR-0387 Decision 1
//! spreads across the graph module: the orchestration
//! ([`Graph::rebuild_adj`]), the one materialized row buffer it feeds
//! every consumer, and the folds' shared label cache. The consumers
//! stay in their family files — `adjacency.rs` installs,
//! `degree_stats.rs` and `count_state.rs` fold — and the sharding they
//! all share sits in its own leaf, `shard.rs`.
//!
//! [`Graph::rebuild_adj`] computes the start-keyed partition once and
//! hands it to every start-keyed consumer (the start-side build, both
//! folds); the end-side build keys on the other endpoint and partitions
//! inside `adjacency::adj_build`. Each temporary buffer is charged to the
//! open's transient tracker for exactly as long as it lives: the label
//! cache is built only after the adjacency build has returned, so it
//! never coexists with the end partition or the build's shard parts, and
//! everything is dropped before the closing `recompute_used`.

use std::collections::HashMap;

use relate_core::{record_transient_lease, NodeId, RelId};

use super::adjacency::adj_build;
#[cfg(test)]
use super::build_counter::BuildCounter;
use super::shard::shard_rows;
use super::{live_version, Graph};

/// One live rel row of a wholesale adjacency build — the seam
/// `rebuild_adj` materializes once and hands to `adjacency::adj_build`
/// and both rebuild folds (`rebuild_degree_stats`,
/// `rebuild_count_stats`) consume. Named fields rather than a positional
/// tuple: `start` and `end` share a type, so a swapped pair would compile
/// and only the adjacency tests would catch it.
pub(in crate::graph) struct RelRow {
    pub(in crate::graph) id: RelId,
    pub(in crate::graph) start: NodeId,
    pub(in crate::graph) type_id: u32,
    pub(in crate::graph) end: NodeId,
    pub(in crate::graph) ts: Option<i64>,
}

/// The wholesale rebuild folds' shared label cache: one live-label
/// read per distinct endpoint node ([`Graph::live_label_ids`]), the
/// same seam [`Graph::rebuild_count_stats`] and
/// [`Graph::rebuild_degree_stats`] both fold over — one spelling of
/// the ensure-then-expect read, never a hand-rolled map per file.
#[derive(Default)]
pub(in crate::graph) struct LabelCache(HashMap<NodeId, Vec<u32>>);

/// The one label-cache fill both label maps ride: `node`'s live labels
/// read exactly once into `map`, then the entry borrowed — the
/// cache-fill-and-borrow pattern, so no per-lookup clone of the live set
/// rides a walk. The two maps' *read* contracts differ and stay their
/// own: [`LabelCache::get`] is the apply fold's read-only view (its miss
/// is an invariant break, loudly), while the stats estimate's simulated
/// set (`BatchSim::node_labels`) is mutable — an `AddLabels` step extends
/// it and the pair phase reads the extended set back.
pub(in crate::graph) fn fill_labels<'m>(
    map: &'m mut HashMap<NodeId, Vec<u32>>,
    graph: &Graph,
    node: NodeId,
) -> &'m mut Vec<u32> {
    map.entry(node)
        .or_insert_with(|| graph.live_label_ids(node))
}

impl LabelCache {
    /// Read `node`'s live labels into the cache exactly once, through the
    /// one fill shape.
    pub(in crate::graph) fn ensure(&mut self, graph: &Graph, node: NodeId) {
        fill_labels(&mut self.0, graph, node);
    }

    /// The cached labels — the fold ensured the node's entry first, so
    /// a miss is the fold's own invariant broken, loudly.
    pub(in crate::graph) fn get(&self, node: NodeId) -> &[u32] {
        self.0
            .get(&node)
            .expect("the fold cached every endpoint's labels")
            .as_slice()
    }

    /// The bytes the cache holds: its table slots and each node's label ids.
    pub(in crate::graph) fn bytes(&self) -> u64 {
        let slots = self.0.capacity() * std::mem::size_of::<(NodeId, Vec<u32>)>();
        let ids: usize = self.0.values().map(Vec::capacity).sum();
        (slots + ids * std::mem::size_of::<u32>()) as u64
    }
}

#[cfg(test)]
thread_local! {
    /// Whole-graph adjacency builds (`rebuild_adj`) on this thread.
    static ADJ_BUILDS: BuildCounter = const { BuildCounter::new() };
}

/// Take this thread's whole-graph adjacency-build count, resetting it.
#[cfg(test)]
pub(super) fn take_adj_builds() -> u64 {
    ADJ_BUILDS.with(BuildCounter::take)
}

impl Graph {
    /// Derive adjacency from each relationship's live version. Each map is
    /// built group-and-sort — one sort per (node, type) run instead of a
    /// binary-search insert per hop — and a node's type chunks follow their
    /// first rel id, as an id-ordered load leaves them, not the map's hash
    /// order: `TypedAdj::from_runs` derives that order from each run's
    /// hops. The rebuild's other halves live behind their family seams:
    /// the (label, rel-type) histograms in
    /// [`Graph::rebuild_degree_stats`], the count bundle's rel-side and
    /// node fields in [`Graph::rebuild_count_stats`].
    pub(super) fn rebuild_adj(&self) {
        #[cfg(test)]
        ADJ_BUILDS.with(BuildCounter::bump);
        let rows = self.materialize_rel_rows();
        let rows_lease =
            record_transient_lease((rows.capacity() * std::mem::size_of::<RelRow>()) as u64);
        // The one start-keyed partition: every row of a node lands in one
        // shard, so the start-side build and both folds read whole nodes
        // and their outputs merge without coordination. The three read the
        // one materialization — no fold's result depends on row order (the
        // build sorts each shard's indices in place).
        let mut start_shards = shard_rows(&rows, |row| row.start);
        let start_lease = start_shards.lease();
        adj_build(self, &rows, &mut start_shards);
        // Only now, with the end partition and the build's buffers gone,
        // does the folds' label cache come into being.
        let labels = self.endpoint_labels(&rows);
        let labels_lease = record_transient_lease(labels.bytes());
        self.rebuild_degree_stats(&rows, &labels, &mut start_shards);
        self.rebuild_count_stats(&rows, &labels, &mut start_shards);
        drop((labels, labels_lease));
        drop((start_shards, start_lease));
        drop((rows, rows_lease));
        // The wholesale rebuild replaces the histogram map: recompute the
        // incremental `used_bytes` so it tracks the new entry count (the
        // recount oracle includes the histogram term).
        self.recompute_used();
    }

    /// The rebuild's one materialization of the live rel rows — the seam
    /// `adj_build` and both folds consume, so the three read one pass over
    /// the rel store rather than three.
    pub(super) fn materialize_rel_rows(&self) -> Vec<RelRow> {
        // Count the live rows first so the buffer is allocated once, at its
        // final size, instead of doubling past it.
        let mut live_rows = 0;
        self.rels
            .for_each(|_, vers| live_rows += usize::from(live_version(vers).is_some()));
        let mut rows: Vec<RelRow> = Vec::with_capacity(live_rows);
        self.rels.for_each(|id, vers| {
            let Some(ver) = live_version(vers) else {
                return;
            };
            let live = &ver.live;
            let ts = live.payload.ts(&self.interner);
            rows.push(RelRow {
                id,
                start: live.start,
                type_id: live.type_id,
                end: live.end,
                ts,
            });
        });
        rows
    }

    /// Both folds' shared label cache: one live-label read per distinct
    /// endpoint, built once on the calling thread and read by every shard.
    /// The cache cannot be per-shard: a fold shards by the row's start, so
    /// an end node is re-read by every shard holding a rel that points at
    /// it — label reads would scale with the shard count instead of the
    /// node count, which is what the folds spend most of their time on.
    pub(super) fn endpoint_labels(&self, rows: &[RelRow]) -> LabelCache {
        let mut labels = LabelCache::default();
        for row in rows {
            labels.ensure(self, row.start);
            labels.ensure(self, row.end);
        }
        labels
    }
}
