//! The reads behind the index-backed aggregates: a range index's first or
//! last live key (min/max), and the live values an existing posting set
//! still holds (grouped counts). Postings are a superset of the truth (every
//! version of every candidate, deleted ids included), so each id is checked
//! against the statement's snapshot before it counts. Neither read builds
//! a pair; a missing or unsuitable one answers `None` and the caller scans.
//!
//! One guard and one memory policy for every read here: a pair's guard is
//! held for one bucket only, never across a node read, and what is copied
//! under it is that bucket's ids plus the key list the walk steps — so
//! transient memory is one key list (O(distinct keys)) and one bucket,
//! never the postings (ADR-0555).

use relate_core::{NodeId, Result, Value};

use super::order::{order_key, Extremum, OrderKey};
use crate::graph::*;

impl Graph {
    /// `id`'s value of `kid` at `ts`, when its visible version carries label
    /// `lid` and `still_keyed` holds for it (the value still hashes to the
    /// key it is posted under — postings are a superset of the truth): the
    /// one liveness check both aggregate reads make per posted id.
    fn labeled_prop_at(
        &self,
        id: NodeId,
        (lid, kid): (u32, u32),
        ts: u64,
        still_keyed: impl FnOnce(&Value) -> bool,
    ) -> Option<Value> {
        if !self.node_carries_label_at(id, lid, ts) {
            return None;
        }
        self.node_prop_id_at(id, ts, kid).filter(still_keyed)
    }

    /// Whether `(lid, kid)` has a declared range index; when it does its
    /// dirty postings are drained, so the caller reads current postings.
    /// The answer is [`Graph::drain_pair`]'s own presence check — no
    /// second `contains_key` beside it.
    fn drain_range_pair(&self, lid: u32, kid: u32) -> bool {
        self.drain_pair(&self.indexes.range_pairs, lid, kid)
    }

    /// The smallest or largest value of `(label, key)` at `ts`, read from
    /// the range index's end keys: a key whose every posted id is dead,
    /// unlabeled or overwritten at `ts` steps to its neighbor. `Some(Null)`
    /// when no live id holds a value; `None` when the store has no such
    /// index, it cannot answer (see `RangeIndex::answers_extremum`) or the
    /// live value at the end does not match its key's variant.
    pub(crate) fn range_extremum_at(
        &self,
        label: &str,
        key: &str,
        extremum: Extremum,
        ts: u64,
    ) -> Option<Value> {
        let pair = self.string_pair_id(label, key)?;
        if !self.drain_range_pair(pair.0, pair.1) {
            return None;
        }
        let pairs = &self.indexes.range_pairs;
        if !pairs.get(&pair)?.answers_extremum() {
            return None;
        }
        let mut at: Option<OrderKey> = None;
        loop {
            // The guard is held for the step only, never across node
            // reads: the key and the ids are copied out under it, the
            // walk runs outside it.
            let (k, ids): (OrderKey, Vec<NodeId>) = {
                let st = pairs.get(&pair)?;
                // No key left to step to: no live id holds a value — the
                // index's own answer is `null`, never a decline.
                let Some((k, postings)) = st.step_from(at.as_ref(), extremum) else {
                    return Some(Value::Null);
                };
                (k.clone(), postings.copy_ids())
            };
            // The first live value still posting under this key — the
            // walk's one spelling, no mutable beside the loop.
            match ids.iter().find_map(|&id| {
                self.labeled_prop_at(id, pair, ts, |v| order_key(v).as_ref() == Some(&k))
            }) {
                Some(v) => {
                    // The index ends in one Int or one String key variant
                    // (`answers_extremum`), so the key's variant decides
                    // which value types answer: a Float under an Int key
                    // is the scan's to give.
                    return match (&k, &v) {
                        (OrderKey::Int(_), Value::Int(_))
                        | (OrderKey::String(_), Value::String(_)) => Some(v),
                        _ => None,
                    };
                }
                // Every id under this key is stale: step to the neighbor.
                None => at = Some(k),
            }
        }
    }

    /// The one per-id step [`for_each_posted_value_at`]'s two arms share:
    /// the ids of one bucket, each handed `f` when it is live at `ts`,
    /// carries the label and still posts under the bucket's key. `Err(e)`
    /// is the walk's only stop — `f` stopping the walk is an error.
    fn step_posted_ids(
        &self,
        ids: &[NodeId],
        pair: (u32, u32),
        ts: u64,
        still_keyed: impl Fn(&Value) -> bool,
        f: &mut impl FnMut(&Value) -> Result<()>,
    ) -> Result<()> {
        for id in ids {
            if let Some(v) = self.labeled_prop_at(*id, pair, ts, &still_keyed) {
                f(&v)?;
            }
        }
        Ok(())
    }

    /// Calls `f` once per node that is live at `ts`, carries `label` and
    /// holds a posted value of `key` — its current value, from the
    /// declared range index's postings or, failing that, an equality pair
    /// an earlier seek already built. The visit order is a contract, not
    /// an artifact: buckets in ascending key order, ids within a bucket in
    /// ascending id order — the scan's grouping order, which the grouped
    /// answer's first-seen rows keep. One bucket is held under the pair's
    /// guard at a time and its ids copied out; the node reads run outside
    /// the guard, and transient memory is one bucket, never the index.
    /// `f` short-circuits: an `Err(e)` stops the walk and comes back as
    /// `Err(e)`. `Ok(None)` when neither pair exists (or the range index
    /// also posts values off its order, or the equality pair holds values
    /// it cannot post, so it undercounts): the caller scans. Never builds
    /// a pair.
    pub(crate) fn for_each_posted_value_at(
        &self,
        label: &str,
        key: &str,
        ts: u64,
        mut f: impl FnMut(&Value) -> Result<()>,
    ) -> Result<Option<()>> {
        let Some(pair) = self.string_pair_id(label, key) else {
            return Ok(None);
        };
        if self.drain_range_pair(pair.0, pair.1) {
            let pairs = &self.indexes.range_pairs;
            let keys: Vec<OrderKey> = {
                let Some(st) = pairs.get(&pair) else {
                    return Ok(None);
                };
                if !st.order_is_complete() {
                    return Ok(None);
                }
                st.order.keys().cloned().collect()
            };
            for k in keys {
                // One bucket per guard acquisition (the extremum step's
                // own shape): the copy under the guard is this bucket's
                // ids, never the index.
                let ids: Vec<NodeId> = {
                    let Some(st) = pairs.get(&pair) else {
                        return Ok(None);
                    };
                    let Some(postings) = st.order.get(&k) else {
                        return Ok(None);
                    };
                    postings.copy_ids()
                };
                self.step_posted_ids(
                    &ids,
                    pair,
                    ts,
                    |v| order_key(v).as_ref() == Some(&k),
                    &mut f,
                )?;
            }
        } else {
            if !self.prop_pair_built(pair) {
                return Ok(None);
            }
            // The keys alone are snapshotted (O(distinct values), sorted by
            // the typed key order); each bucket's ids are then copied one
            // guard acquisition at a time, so transient memory is a bucket.
            let Some(keys): Option<Vec<crate::group_key::GroupKey>> =
                self.with_prop_pair(pair.0, pair.1, |st| {
                    if !st.unposted.is_empty() {
                        return None;
                    }
                    Some(st.postings.keys_sorted())
                })
            else {
                return Ok(None);
            };
            for gk in keys {
                // The bucket was snapshotted under the same guard, so a
                // missing key is a torn window — decline to the scan, the
                // range arm's own policy, never a fabricated empty bucket.
                let ids: Option<Vec<NodeId>> = self.with_prop_pair(pair.0, pair.1, |st| {
                    st.postings.bucket(&gk).map(<[NodeId]>::to_vec)
                });
                let Some(ids) = ids else {
                    return Ok(None);
                };
                self.step_posted_ids(
                    &ids,
                    pair,
                    ts,
                    |v| crate::group_key::GroupKey::for_property_index(v).as_ref() == Some(&gk),
                    &mut f,
                )?;
            }
        }
        Ok(Some(()))
    }
}
