//! One label's build: a chunked scan of its live nodes into per-key
//! accumulators, then a publish under the budget.

use std::collections::HashMap;
use std::sync::Arc;

use relate_core::Value;
use relate_planner::KeyStats;

use super::accum::Accum;
use super::gate::{admission, Admission, KeyClass};
use super::sketch::identity_hash;
use super::{Phase, STALE_DIVISOR};
use crate::graph::Graph;
use crate::hash_seed::FxBuildHasher;

/// Nodes read between two points where no store lock is held.
const CHUNK_NODES: usize = 4096;

/// Most keys one label's scan accumulates (about 1 KB each while
/// scanning). The first keys seen keep their accumulators for the whole
/// scan and a later key is refused for the whole scan: an accumulator is
/// never evicted or restarted, so a published key was read from its first
/// value to its last. The byte budget's priority (structured over text) is
/// applied once, at publish.
const MAX_SCAN_KEYS: usize = 2048;

/// The per-key accumulators of one label's scan.
pub(super) struct LabelScan {
    accums: HashMap<u32, Accum, FxBuildHasher>,
    declined: HashMap<u32, u64, FxBuildHasher>,
    max_keys: usize,
    rows: u64,
}

impl LabelScan {
    pub(super) fn new() -> Self {
        Self::with_cap(MAX_SCAN_KEYS)
    }

    pub(super) fn with_cap(max_keys: usize) -> Self {
        Self {
            accums: HashMap::default(),
            declined: HashMap::default(),
            max_keys,
            rows: 0,
        }
    }

    /// Read one value of `key`: admitted into the key's accumulator,
    /// counted as declined, or, for a key past the scan's cap, skipped (the
    /// key is never published, so no figure about it exists to be wrong).
    pub(super) fn observe(&mut self, key: u32, value: &Value) {
        let admission = admission(value);
        let Some(hash) = (admission != Admission::Declined)
            .then(|| identity_hash(value))
            .flatten()
        else {
            *self.declined.entry(key).or_default() += 1;
            return;
        };
        if !self.accums.contains_key(&key) && self.accums.len() >= self.max_keys {
            return;
        }
        self.accums
            .entry(key)
            .or_insert_with(Accum::new)
            .observe(value, admission, hash);
    }

    /// The finished statistics, keys that are mostly declined dropped:
    /// figures about a minority of a key's values are worse than none.
    pub(super) fn finish(self) -> Vec<(u32, KeyClass, KeyStats)> {
        let Self {
            accums, declined, ..
        } = self;
        let mut finished: Vec<(u32, KeyClass, KeyStats)> = accums
            .into_iter()
            .filter_map(|(key, acc)| {
                let declined = declined.get(&key).copied().unwrap_or(0);
                (declined <= acc.admitted()).then(|| (key, acc.class(), acc.finish(declined)))
            })
            .collect();
        // Structured keys admit first, so they are never the ones refused.
        finished.sort_by_key(|(key, class, _)| (*class == KeyClass::Text, *key));
        finished
    }
}

impl Graph {
    /// Scan `label`'s live nodes in chunks and publish its keys' statistics.
    /// `keep_going` runs between chunks, where no store lock is held (tests
    /// prove it by writing there from another thread); answering `false`
    /// abandons the scan and returns the label to the pump's queue. The
    /// star bucket is never scanned: writes do not count against it, so it
    /// could never turn stale (a consult for it answers `None`). Whether the
    /// scan ran to its publication.
    pub(super) fn build_key_stats_for(
        &self,
        label: u32,
        mut keep_going: impl FnMut() -> bool,
    ) -> bool {
        debug_assert_ne!(label, self.indexes.star_label_id, "the star is unsupported");
        let ids = self.pair_candidate_ids(label);
        let mut scan = LabelScan::new();
        for chunk in ids.chunks(CHUNK_NODES) {
            for &id in chunk {
                let Some(chain) = self.nodes.get(&id) else {
                    continue;
                };
                let Some(version) = chain.live() else {
                    continue;
                };
                let live = version.live;
                if !live.has_label(label) {
                    continue;
                }
                scan.rows += 1;
                for (key, value) in live.props.iter() {
                    scan.observe(key, value);
                }
            }
            if !keep_going() {
                self.indexes.key_stats.unclaim(label);
                return false;
            }
        }
        self.publish_key_stats(label, scan);
        true
    }

    /// Finish the accumulators and publish them under the budget. A label
    /// that saw more than its stale threshold of writes while it scanned
    /// still publishes (best-effort statistics beat rescanning a label that
    /// is written faster than it can be read); in that case its write count
    /// restarts, so it goes stale again only after another full threshold of
    /// writes lands behind the publication. Below the threshold the writes
    /// that landed during the scan stay counted: the scan may have missed
    /// them, so they still count toward the label going stale.
    fn publish_key_stats(&self, label: u32, scan: LabelScan) {
        let state = &self.indexes.key_stats;
        let rows = scan.rows;
        let finished = scan.finish();

        let Some(mut label_state) = state.labels.get_mut(&label) else {
            return;
        };
        let stale_after = (rows / STALE_DIVISOR).max(super::MIN_STALE_WRITES);
        if label_state.writes > stale_after {
            label_state.writes = 0;
        }
        // Lock order: the label's state, then the ledger, then the consult
        // map. The ledger alone records which keys are published, so an
        // eviction needs no other label's state.
        let mut ledger = state.ledger.lock().expect("ledger lock");
        // Whether a consult can now answer differently: an admitted key is
        // published (evicting another label's keys to make room, if need be).
        let mut changed = false;
        for (key, class, stats) in finished {
            let bytes = super::approx_bytes(&stats);
            if let Some(evicted) = ledger.admit((label, key), bytes, class) {
                for victim in evicted {
                    state.published.remove(&victim);
                }
                state.published.insert((label, key), Arc::new(stats));
                changed = true;
            }
        }
        drop(ledger);
        label_state.stale_after = stale_after;
        label_state.phase = Phase::Built;
        // After the published map changed (the epoch's contract); a label
        // that published nothing leaves every answer as it was.
        if changed {
            state.bump_epoch();
        }
    }
}
