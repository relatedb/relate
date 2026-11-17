//! Per-structure heap census of a live graph — a measurement harness, not
//! an engine surface: research-gated (the ADR-0476 quarantine), read by the
//! `heap_census` example in relate-query-latency through the research
//! feature.
//!
//! Every byte number either reuses the ADR-0042/0202 accounting walks
//! (`memory.rs`, the structure-byte families' own accessors) or is marked
//! estimated. Counts are exact walks. Nothing here mutates the graph.

use std::collections::HashSet;
use std::mem::size_of;

use relate_core::Value;

use crate::memory::{self, hashset_i64_bytes, ChainCensus, OVERHEAD_MAP_ENTRY};

use super::adjacency::AdjSide;

use super::Graph;

pub use super::declared::DeclaredCensus;

#[derive(Debug, Default, Clone)]
pub struct HeapCensus {
    // ---- nodes ----
    pub node_ids: usize,
    pub node_dense_span: usize,
    pub node_side_entries: usize,
    pub node_chains: ChainCensus,
    pub node_label_refs: u64,
    pub node_props: u64,
    pub node_string_bytes: u64,
    pub node_slot_bytes: u64,
    pub node_chain_bytes: u64,
    pub node_record_bytes: u64,
    // ---- relationships ----
    pub rel_ids: usize,
    pub rel_dense_span: usize,
    pub rel_side_entries: usize,
    pub rel_chains: ChainCensus,
    pub rel_props: u64,
    pub rel_string_bytes: u64,
    pub rel_slot_bytes: u64,
    pub rel_chain_bytes: u64,
    pub rel_record_bytes: u64,
    // ---- adjacency ----
    pub out: AdjacencyCensus,
    pub r#in: AdjacencyCensus,
    // ---- label index (estimated) ----
    pub label_index_buckets: usize,
    pub label_index_entries: u64,
    pub label_index_bytes: u64,
    // ---- declared indexes (estimated) ----
    pub declared: DeclaredCensus,
    // ---- count state / degree histograms ----
    pub count_rel_types: usize,
    pub count_pair_entries: usize,
    pub count_label_entries: usize,
    pub count_node_total: u64,
    pub count_stats_bytes: u64,
    pub degree_hist_entries: usize,
    pub degree_hist_bytes: u64,
    pub constraint_key_bytes: u64,
    // ---- interner ----
    pub interner_keys: usize,
    pub interner_bytes: u64,
    // ---- totals ----
    pub node_version_stride: u64,
    pub rel_version_stride: u64,
    pub structure_bytes: u64,
    pub used_bytes: u64,
    pub accounted_bytes: u64,
}

/// One direction's adjacency: node count, chunk/hop counts, and the floor
/// split (ADR-0599). Both directions go through [`Self::of`]. Hops count the
/// live delta and the sealed stream (`TypedAdj::chunk_lens`), so a folded
/// chunk keeps its degree; `sealed_chunks` names how many are folded. The
/// byte terms partition the tier's whole floor: `delta_bytes` is the delta
/// map's slot array plus every chunk's reserved heap; the sealed tier is
/// `arena_bytes` charged at the exact encoded length plus `index_bytes` the
/// span table, side map, entries and tombstones ([`Self::sealed_bytes`]),
/// and `dead_bytes` the arena ranges a fold replaced, the share of
/// `arena_bytes` the next compaction reclaims.
#[derive(Debug, Default, Clone, Copy)]
pub struct AdjacencyCensus {
    pub nodes: u64,
    pub chunks: u64,
    pub sealed_chunks: u64,
    pub hops: u64,
    pub delta_bytes: u64,
    pub arena_bytes: u64,
    pub index_bytes: u64,
    pub dead_bytes: u64,
}

impl AdjacencyCensus {
    /// The sealed tier's whole floor: the arena at its exact encoded length
    /// plus the index structures around it.
    pub fn sealed_bytes(&self) -> u64 {
        self.arena_bytes + self.index_bytes
    }

    fn of(side: AdjSide<'_>) -> Self {
        let tier = side.sealed().tier_bytes();
        let mut c = Self {
            arena_bytes: tier.arena,
            index_bytes: tier.index,
            dead_bytes: tier.dead,
            ..Self::default()
        };
        side.delta().for_each(|id, adj| {
            let region = side.sealed().region(id);
            c.nodes += 1;
            c.chunks += adj.chunks.len() as u64;
            c.sealed_chunks += region.map_or(0, |s| s.chunk_count()) as u64;
            c.hops += adj
                .chunk_lens(region)
                .map(|(_, delta, sealed)| (delta + sealed) as u64)
                .sum::<u64>();
            c.delta_bytes += adj.floor_bytes();
        });
        c.delta_bytes += side.delta().array_bytes();
        c
    }
}

impl HeapCensus {
    /// Physical-ish sum: slot arrays + record heap (property vectors, string
    /// payloads) + adjacency + the *actual* label and declared-index
    /// structures + small state. Unlike [`Self::accounted_bytes`], this is
    /// not a pure partition: the engine's logical node record already
    /// charges label/range postings, so those two terms appear twice across
    /// the two totals (they are a few MB of logical charge).
    pub fn physical_total(&self) -> u64 {
        self.node_slot_bytes
            + self.node_record_bytes
            + self.node_chain_bytes
            + self.rel_slot_bytes
            + self.rel_record_bytes
            + self.rel_chain_bytes
            + self.out.delta_bytes
            + self.out.sealed_bytes()
            + self.r#in.delta_bytes
            + self.r#in.sealed_bytes()
            + self.label_index_bytes
            + self.declared.range_bytes
            + self.declared.text_bytes
            + self.declared.point_bytes
            + self.count_stats_bytes
            + self.degree_hist_bytes
            + self.constraint_key_bytes
            + self.interner_bytes
    }
}

impl Graph {
    /// Walk every retained structure once and return its size. Scratch
    /// measurement API for the heap-census harness — no engine code path
    /// calls it.
    #[doc(hidden)]
    pub fn heap_census(&self) -> HeapCensus {
        let mut c = HeapCensus::default();
        let charge = self.index_charge();

        // Nodes: slot array, per-version chains, logical record account.
        let ids = self.nodes.slot_counts();
        c.node_dense_span = ids.span();
        c.node_side_entries = ids.side;
        c.node_ids = ids.len();
        c.node_slot_bytes = self.nodes.array_bytes();
        self.nodes.for_each(|_id, vers| {
            c.node_chains.count(vers.shape());
            c.node_chain_bytes += memory::chain_bytes(vers.shape(), super::node_version_stride());
            for v in vers.iter() {
                c.node_record_bytes +=
                    memory::live_node_account_live(v.live, &self.interner, &charge);
                c.node_label_refs += v.live.labels.len() as u64;
                for (_kid, value) in v.live.props.iter() {
                    c.node_props += 1;
                    if let Value::String(s) = value {
                        c.node_string_bytes += s.len() as u64;
                    }
                }
            }
        });

        // Relationships.
        let ids = self.rels.slot_counts();
        c.rel_dense_span = ids.span();
        c.rel_side_entries = ids.side;
        c.rel_ids = ids.len();
        c.rel_slot_bytes = self.rels.array_bytes();
        self.rels.for_each(|_id, vers| {
            c.rel_chains.count(vers.shape());
            c.rel_chain_bytes += memory::chain_bytes(vers.shape(), super::rel_version_stride());
            for v in vers.iter() {
                c.rel_record_bytes += memory::live_rel_account_live(v.live);
                match &v.live.payload {
                    crate::store::RelPayload::Triple { id, .. } => {
                        c.rel_props += 3;
                        c.rel_string_bytes += id.len() as u64;
                    }
                    crate::store::RelPayload::Other(props) => {
                        for (_kid, value) in props.iter() {
                            c.rel_props += 1;
                            if let Value::String(s) = value {
                                c.rel_string_bytes += s.len() as u64;
                            }
                        }
                    }
                }
            }
        });

        // Adjacency: both directions, every relationship twice.
        c.out = AdjacencyCensus::of(self.out_side());
        c.r#in = AdjacencyCensus::of(self.in_side());

        // Label index: DashMap slots plus each bucket's HashSet allocation.
        for bucket in self.indexes.label_index.iter() {
            c.label_index_buckets += 1;
            let set = bucket.value();
            c.label_index_entries += set.len() as u64;
            c.label_index_bytes += OVERHEAD_MAP_ENTRY
                + size_of::<HashSet<i64>>() as u64
                + hashset_i64_bytes(set.capacity());
        }

        // Declared indexes (the upstream LDBC profile has 16 RANGE pairs).
        c.declared = self.declared_index_census();

        // Count state and degree histograms.
        {
            let stats = self.count_stats.lock().unwrap();
            c.count_rel_types = stats.rel.rel_type_counts.len();
            c.count_pair_entries = stats.rel.pair_counts.len();
            c.count_label_entries = stats.label_counts.len();
            c.count_node_total = stats.node_total;
        }
        c.count_stats_bytes = self.count_state_structure_bytes();
        c.degree_hist_entries = self.degree_histograms.lock().unwrap().len();
        c.degree_hist_bytes = self.degree_stats_structure_bytes();
        c.constraint_key_bytes = self.constraint_keys_structure_bytes();

        // Interner.
        c.interner_keys = self.interner.key_count();
        c.interner_bytes = self.interner.bytes();

        // Strides and the engine's own totals.
        c.node_version_stride = super::node_version_stride();
        c.rel_version_stride = super::rel_version_stride();
        c.structure_bytes = self.structure_bytes();
        c.used_bytes = self.used_bytes();
        c.accounted_bytes = self.accounted_bytes();
        c
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use relate_core::Value;

    use super::Graph;

    /// The census reads one model: hops count delta plus sealed
    /// (`TypedAdj::chunk_lens`) and sealed chunks are named, so a fold moves
    /// hops from the delta to the sealed stream instead of erasing them.
    #[test]
    fn census_counts_hops_across_the_fold() {
        let g = Graph::default();
        crate::graph::test_support::folded_floor_chunk(&g);

        let c = g.heap_census();
        let floor = crate::adjacency::COMPACTION_DELTA_FLOOR as u64;
        assert_eq!(c.out.hops, floor, "sealed hops still count");
        assert_eq!(c.out.sealed_chunks, 1);
        assert_eq!(c.r#in.hops, floor);
        assert_eq!(c.r#in.sealed_chunks, 1);
    }

    /// The floor split (ADR-0599) is exposed per side: on a store holding
    /// both sealed and delta content every byte term is present and
    /// non-negative — the content-bearing ones positive — and the dead
    /// ranges sit inside the arena.
    #[test]
    fn census_exposes_the_floor_split() {
        let g = Graph::default();
        let (a, b, _, _) = crate::graph::test_support::folded_floor_chunk(&g);
        // one more rel: the delta tier holds a hop beside the sealed chunk
        g.begin_write().unwrap();
        g.create_rel(
            a,
            b,
            "T".into(),
            BTreeMap::from([(
                "ts".into(),
                Value::Int(crate::adjacency::COMPACTION_DELTA_FLOOR as i64),
            )]),
        )
        .unwrap();
        g.commit_write();

        let c = g.heap_census();
        for side in [&c.out, &c.r#in] {
            assert!(side.chunks > 0, "a delta chunk exists");
            assert!(side.hops > 0, "sealed plus delta hops exist");
            assert!(side.delta_bytes > 0, "the delta tier holds the fresh hop");
            assert!(side.arena_bytes > 0, "the arena holds the folded stream");
            assert!(side.index_bytes > 0, "the fold created an entry and span");
            assert!(
                side.dead_bytes <= side.arena_bytes,
                "dead ranges sit in the arena"
            );
        }
    }
}
