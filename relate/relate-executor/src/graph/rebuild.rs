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
//! inside `adjacency::adj_build`.

use super::adjacency::adj_build;
use super::shard::shard_rows;
use super::{live_version, Graph, LabelCache, RelRow};

#[cfg(test)]
thread_local! {
    /// Whole-graph adjacency builds (`rebuild_adj`) on this thread.
    static ADJ_BUILDS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Take this thread's whole-graph adjacency-build count, resetting it.
#[cfg(test)]
pub(super) fn take_adj_builds() -> u64 {
    ADJ_BUILDS.with(|n| n.replace(0))
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
        ADJ_BUILDS.with(|n| n.set(n.get() + 1));
        let rows = self.materialize_rel_rows();
        let labels = self.endpoint_labels(&rows);
        // The one start-keyed partition: every row of a node lands in one
        // shard, so the start-side build and both folds read whole nodes
        // and their outputs merge without coordination. The three read the
        // one materialization in materialize order — no fold's result
        // depends on row order.
        let start_shards = shard_rows(&rows, |row| row.start);
        self.out_adj.clear();
        self.in_adj.clear();
        self.sealed_out.clear();
        self.sealed_in.clear();
        adj_build(&self.out_adj, &self.in_adj, &rows, &start_shards);
        self.rebuild_degree_stats(&rows, &labels, &start_shards);
        self.rebuild_count_stats(&rows, &labels, &start_shards);
        // The wholesale rebuild replaces the histogram map: recompute the
        // incremental `used_bytes` so it tracks the new entry count (the
        // recount oracle includes the histogram term).
        self.recompute_used();
    }

    /// The rebuild's one materialization of the live rel rows — the seam
    /// `adj_build` and both folds consume, so the three read one pass over
    /// the rel store rather than three.
    fn materialize_rel_rows(&self) -> Vec<RelRow> {
        let mut rows: Vec<RelRow> = Vec::new();
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
    fn endpoint_labels(&self, rows: &[RelRow]) -> LabelCache {
        let mut labels = LabelCache::default();
        for row in rows {
            labels.ensure(self, row.start);
            labels.ensure(self, row.end);
        }
        labels
    }
}
