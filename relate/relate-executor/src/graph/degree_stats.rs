//! LABS-351 / LABS-370: the graph's degree statistics — the
//! (label, rel-type) out-degree histograms (the estimate family: the
//! high-water buckets, their bump/rebucket/unbucket seams, the
//! retained-structure recount's histogram share, and the wholesale
//! rebuild that republishes the degree-statistics set). The exact
//! count state — the bundle the counting operator answers from —
//! lives in its own sibling `count_state.rs` (LABS-851's structure
//! round: the family outgrew one file and split the same way
//! `degree_stats_incident.rs` and `degree_stats_estimate.rs` split
//! before). The `Graph` struct (graph/mod.rs) keeps the fields; the
//! seams live in these files as `impl Graph` blocks (the borrowing
//! stays on `&self`, which owns the adjacency, the rel store probe
//! and the label reads the family needs).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use relate_core::NodeId;

use super::count_state::CountDelta;
use super::Graph;
use super::LabelCache;

impl Graph {
    /// The histogram's bucket count (LABS-311: one constant, consulted at
    /// every site — the field, the bumps, the rebuild fold, the tail
    /// scan). 21 buckets cover degrees to 2^20 (one million), the
    /// realistic supernode ceiling.
    pub(super) const DEGREE_BUCKETS: usize = 21;
    /// One histogram entry's charge: the (u32, u32) key, the bucket
    /// counts, and the map's per-entry overhead (the same floor-estimate
    /// style as the adjacency terms). Charged to `used_bytes` on every
    /// entry creation so the incremental capacity accounting tracks the
    /// recount oracle (security review: an uncharged query-controlled map
    /// bypasses the memory-limit enforcement).
    pub(super) const HISTOGRAM_ENTRY_BYTES: u64 = (std::mem::size_of::<(u32, u32)>()
        + Self::DEGREE_BUCKETS * std::mem::size_of::<u64>()
        + 48
        + 64) as u64;
    /// floor(log2(degree)) for degree >= 1: bucket 0 counts degree-1
    /// nodes, bucket k counts 2^k..2^(k+1)-1. Clamped into the last
    /// bucket above 2^20.
    pub(super) fn degree_bucket(degree: u64) -> usize {
        (degree.max(1).ilog2() as usize).min(Self::DEGREE_BUCKETS - 1)
    }

    /// The histogram family's share of the retained-structure recount
    /// (the `structure_bytes` oracle): the histogram entries are
    /// retained structure — an uncharged query-controlled map would
    /// bypass the memory-limit enforcement (the Capacity check compares
    /// `used_bytes`). One constant, consulted at every site (LABS-311):
    /// the recount charges the same HISTOGRAM_ENTRY_BYTES the
    /// incremental bumps do. The count bundle's share composes beside
    /// this term at the oracle's one site (`structure_bytes`),
    /// beside the constraint-key term.
    pub(super) fn degree_stats_structure_bytes(&self) -> u64 {
        let histograms = self.degree_histograms.lock().unwrap();
        (histograms.len() as u64).saturating_mul(Self::HISTOGRAM_ENTRY_BYTES)
    }

    /// The committed degree-histogram map's keys — the estimate's
    /// seed.
    pub(super) fn degree_histogram_keys(&self) -> HashSet<(u32, u32)> {
        self.degree_histograms
            .lock()
            .unwrap()
            .keys()
            .copied()
            .collect()
    }

    /// Move the from-node up the degree buckets for every label it
    /// carries: called after a create's adjacency insert, with `before`
    /// the pre-insert out-degree on the rel type (LABS-351). The
    /// high-water semantics mean a move is the only outcome (never
    /// down); the create path guarantees `after = before + 1`, so the
    /// bucket moves at most one step.
    pub(super) fn bump_degree_histograms(&self, node: NodeId, type_id: u32, before: u64) {
        // One loop, both cases (stellar std-3): `old` is None for a node's
        // first edge (nothing to decrement — a degree-0 before would
        // collide with the degree-1 bucket), Some(bucket) for a move; the
        // same-bucket move changes nothing.
        let after = self.out_chunk_len(node, type_id);
        let new = Self::degree_bucket(after);
        let old = (before > 0).then(|| Self::degree_bucket(before));
        if old.is_some_and(|old| old == new) {
            return;
        }
        let labels = self.live_label_ids(node);
        let mut histograms = self.degree_histograms.lock().unwrap();
        for label in labels {
            let entry = self.histogram_entry(&mut histograms, label, type_id);
            if let Some(old) = old {
                entry[old] = entry[old].saturating_sub(1);
            }
            entry[new] = entry[new].saturating_add(1);
        }
    }

    /// The get-or-create of one histogram entry with its `used_bytes`
    /// entry charge — the one charge policy both histogram bumps share
    /// (the memory-limit charge discipline lives once, not copied
    /// verbatim per bump site). The wholesale rebuild is the one
    /// exception and stays inline in [`Graph::rebuild_degree_stats`]:
    /// its entry charges are recomputed wholesale by `recompute_used`
    /// at the caller's tail, so charging there would double-count.
    fn histogram_entry<'m>(
        &self,
        histograms: &'m mut HashMap<(u32, u32), [u64; Self::DEGREE_BUCKETS]>,
        label: u32,
        type_id: u32,
    ) -> &'m mut [u64; Self::DEGREE_BUCKETS] {
        match histograms.entry((label, type_id)) {
            std::collections::hash_map::Entry::Vacant(v) => {
                self.used_bytes
                    .fetch_add(Self::HISTOGRAM_ENTRY_BYTES, Ordering::Relaxed);
                v.insert([0; Self::DEGREE_BUCKETS])
            }
            std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
        }
    }

    /// The node's out-degree buckets enter the histograms of every
    /// newly added label (each per-type out-degree, bucketed, once per
    /// label). Bounded by the node's typed adjacency chunks.
    pub(super) fn rebucket_added_labels(&self, node: NodeId, added_ids: &[u32]) {
        if added_ids.is_empty() {
            return;
        }
        let mut histograms = self.degree_histograms.lock().unwrap();
        for (type_id, degree) in self.out_chunks(node) {
            let bucket = Self::degree_bucket(degree);
            for label in added_ids {
                let entry = self.histogram_entry(&mut histograms, *label, type_id);
                entry[bucket] = entry[bucket].saturating_add(1);
            }
        }
    }

    /// The node's out-degree buckets leave the histograms of every
    /// actually-removed label (saturating: a high-water degree can exceed
    /// the current live count). The paired helper of
    /// [`Graph::rebucket_added_labels`]: one input domain (&[u32] ids),
    /// one guard, one direction each.
    pub(super) fn unbucket_removed_labels(&self, node: NodeId, removed_ids: &[u32]) {
        if removed_ids.is_empty() {
            return;
        }
        let mut histograms = self.degree_histograms.lock().unwrap();
        for (type_id, degree) in self.out_chunks(node) {
            let bucket = Self::degree_bucket(degree);
            for label in removed_ids {
                if let Some(entry) = histograms.get_mut(&(*label, type_id)) {
                    entry[bucket] = entry[bucket].saturating_sub(1);
                }
            }
        }
    }

    /// LABS-351: the tail of a (label, rel-type) out-degree distribution —
    /// the conservative floor of the highest observed degree
    /// (2^k for the highest non-empty bucket k), high-water across
    /// deletes. `None` when nothing of that pair was ever written. This
    /// is the estimate the mean degree cannot express: one 50k-degree
    /// supernode in a 55-average label.
    pub fn pattern_degree_tail(&self, label: &str, rel_type: &str) -> Option<u64> {
        let label_id = self.interner.get_id(label)?;
        let type_id = self.interner.get_id(rel_type)?;
        let histograms = self.degree_histograms.lock().unwrap();
        Self::bucket_tail(histograms.get(&(label_id, type_id))?)
    }

    /// The floor of the highest observed degree in one histogram (2^k
    /// for the highest non-empty bucket k) — the one bucket-to-degree
    /// reading both tail lookups share. `None` for an empty histogram.
    fn bucket_tail(buckets: &[u64; Self::DEGREE_BUCKETS]) -> Option<u64> {
        buckets
            .iter()
            .rposition(|&count| count > 0)
            .map(|highest| 1u64 << highest)
    }

    /// LABS-991: the highest tail ([`Graph::pattern_degree_tail`]'s floor)
    /// over every labeled node's histogram for `rel_type`, or — for `None` —
    /// the per-type maxima summed — each term the same log2-bucket floor
    /// as [`Graph::pattern_degree_tail`], so the sum is an estimate on that
    /// scale, not a strict upper bound. Nodes without a label sit in no
    /// histogram. `None` when no histogram entry matches.
    pub(crate) fn pattern_any_degree_tail(&self, rel_type: Option<&str>) -> Option<u64> {
        let wanted = match rel_type {
            Some(text) => Some(self.interner.get_id(text)?),
            None => None,
        };
        let histograms = self.degree_histograms.lock().unwrap();
        let mut per_type: HashMap<u32, u64> = HashMap::new();
        for (&(_, type_id), buckets) in histograms.iter() {
            if wanted.is_some_and(|w| w != type_id) {
                continue;
            }
            let Some(tail) = Self::bucket_tail(buckets) else {
                continue;
            };
            let slot = per_type.entry(type_id).or_insert(0);
            *slot = (*slot).max(tail);
        }
        if per_type.is_empty() {
            return None;
        }
        Some(
            per_type
                .values()
                .fold(0u64, |sum, tail| sum.saturating_add(*tail)),
        )
    }

    /// Every per-label statistics change one label-set mutation makes: the
    /// degree re/unbucket, the incident pair counts, and the exact label
    /// counts. `delta` selects the direction — the one entry point both
    /// label seams call.
    pub(super) fn apply_label_delta_stats(&self, node: NodeId, labels: &[u32], delta: CountDelta) {
        match delta {
            CountDelta::Add => self.rebucket_added_labels(node, labels),
            CountDelta::Remove => self.unbucket_removed_labels(node, labels),
        }
        self.apply_incident_pair_counts(node, labels, delta);
        self.bump_label_counts(labels, delta);
    }

    /// The degree-stats half of the wholesale rebuild — the family seam
    /// ADR-0387 Decision 1 promises (the maps' read/bump/rebucket paths
    /// live in this file, the rebuild's folds included — the
    /// label-mutation incident walk lives in the family's incident
    /// sibling, `degree_stats_incident.rs`). Called by `rebuild_adj`
    /// (graph/index.rs) with the
    /// rebuild's rel rows projected to the (start, type, end) triples
    /// these folds read — the rel id and timestamp stay in index.rs
    /// where `adj_insert` consumes them. A pure fold over its input:
    /// the (label, rel-type) histograms come from the same rows (a
    /// node's per-type out-degree is the count of its rows of that
    /// type), not from the adjacency the caller built, so no
    /// call-ordering precondition rides the seam.
    ///
    /// The estimate family's wholesale rebuild: the histograms (the
    /// count bundle's rel-side and node-side fields publish wholesale
    /// through the count family's own [`Graph::rebuild_count_stats`] —
    /// one bundle, one guard, one publish).
    pub(super) fn rebuild_degree_stats(&self, rows: impl Iterator<Item = (NodeId, u32, NodeId)>) {
        let mut label_cache = LabelCache::default();
        // LABS-351: the per-(node, type) out-degrees counted off the
        // rows — the same number the out-adjacency chunk holds once
        // `adj_insert` ran. A fold-local map rather than an out_adj
        // read: it keeps this seam a pure function of its rows (no
        // call-ordering precondition rides it), at the cost of one
        // transient per-rebuild map. Bucket counts are
        // order-independent, so the map's iteration order does not
        // matter. Recomputed so the high-water mark recalibrates to the
        // honest live state after a restore.
        let mut out_degrees: HashMap<(NodeId, u32), u64> = HashMap::new();
        for (start, type_id, _end) in rows {
            // One label read per distinct node, the family's shared
            // cache seam.
            label_cache.ensure(self, start);
            *out_degrees.entry((start, type_id)).or_insert(0) += 1;
        }
        let mut histograms: HashMap<(u32, u32), [u64; Graph::DEGREE_BUCKETS]> = HashMap::new();
        for ((start, type_id), degree) in &out_degrees {
            let bucket = Self::degree_bucket(*degree);
            let labels = label_cache.get(*start);
            for label in labels.iter() {
                // Uncharged on purpose: the caller's tail runs
                // `recompute_used`, which retallies the entry charges
                // wholesale — charging here would double-count.
                let slot = histograms
                    .entry((*label, *type_id))
                    .or_insert([0; Graph::DEGREE_BUCKETS]);
                slot[bucket] += 1;
            }
        }
        // One publish for the histogram map — the estimate family's
        // own state; every other site takes exactly this one lock, so
        // no ordering cycle exists and a reader never sees one
        // rebuild's histograms mixed with the previous rebuild's.
        *self.degree_histograms.lock().unwrap() = histograms;
    }
}
