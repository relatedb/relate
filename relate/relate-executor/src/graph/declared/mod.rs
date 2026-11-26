//! `CREATE TEXT INDEX`, `CREATE RANGE INDEX`, and `CREATE POINT INDEX`
//! access methods. Each structure exists only for a catalog pair and is
//! built on create, on startup (`rebuild_indexes`), and incrementally on
//! later writes. A seek with no matching index returns `None` so the
//! caller scans the label and evaluates the predicate.

use std::collections::HashSet;

use relate_core::{NodeId, Value};

use crate::redo::{IndexDetailSpec, IndexKindSpec};

use super::index_postings::PairVersion;
use super::*;

mod aggregate;
mod order;
mod point;
mod seek;
mod text;

pub(super) use order::RangeIndex;
pub(in crate::graph) use order::{order_key, OrderKey};
pub(crate) use order::{Extremum, RangeSeek, SpatialSeek};
pub(crate) use point::spatial_holds;
pub(super) use point::PointIndex;
/// Profile-only: the bench harness drives the point backend directly
/// (the R-tree vs. grid comparison of record lives there).
#[cfg(test)]
pub(super) use point::{point_insert, spatial_ids};
pub(crate) use text::trigram_count;
pub(super) use text::GramIndex;

/// One declared index's postings for a `(label, property)` pair. The
/// graph wiring below is written once against this trait.
pub(super) trait DrainState: Default {
    fn dirty_mut(&mut self) -> &mut HashSet<NodeId>;
    /// Post one version's value for `id`. A value of another type posts
    /// nothing.
    fn insert(&mut self, id: NodeId, value: &Value);
    /// Drop every posting of `id`.
    fn remove_node(&mut self, id: NodeId);
    /// Reconcile per-node trust state after a re-post — [`Self::insert`]
    /// sees every version, so a set that must reflect the current
    /// committed value corrects here from the LIVE value: `Some` when
    /// the live version carries the pair's value, `None` when it does
    /// not (the node contributes nothing at the latest state). The
    /// default is nothing to do.
    fn reconcile_live(&mut self, _id: NodeId, _live: Option<&Value>) {}
    /// Finalize a full local build (create, reopen rebuild) before the
    /// pair is published: a seek never observes an unsealed pair. The
    /// default is nothing to do.
    fn seal(&mut self) {}
}

impl Graph {
    pub(super) fn declared_dirty(&self, lid: u32, kid: u32, id: NodeId) {
        Self::mark_dirty(&self.indexes.gram_pairs, lid, kid, id);
        Self::mark_dirty(&self.indexes.range_pairs, lid, kid, id);
        Self::mark_dirty(&self.indexes.point_pairs, lid, kid, id);
    }

    fn mark_dirty<St: DrainState>(pairs: &CoreMap<(u32, u32), St>, lid: u32, kid: u32, id: NodeId) {
        if let Some(mut st) = pairs.get_mut(&(lid, kid)) {
            st.dirty_mut().insert(id);
        }
    }

    /// `(gram entries, dirty ids)` when a text index has materialized this pair.
    #[cfg(test)]
    pub(super) fn gram_pair_state(&self, label: &str, key: &str) -> Option<(usize, usize)> {
        let pk = self.string_pair_id(label, key)?;
        self.indexes
            .gram_pairs
            .get(&pk)
            .map(|st| (st.grams.len(), st.dirty.len()))
    }

    pub(super) fn materialize_detail(&self, detail: &IndexDetailSpec) {
        for (label, property, kind) in detail.covered_pairs() {
            match kind {
                IndexKindSpec::Text => self.materialize_text_index(label, property),
                IndexKindSpec::Range => self.materialize_range_index(label, property),
                IndexKindSpec::Point => self.materialize_point_index(label, property),
                _ => {}
            }
        }
    }

    pub(super) fn drop_detail(&self, detail: &IndexDetailSpec) {
        let remaining: Vec<IndexDetailSpec> = self
            .theater
            .lock()
            .unwrap()
            .index_details
            .values()
            .cloned()
            .collect();
        for (label, property, kind) in detail.covered_pairs() {
            let still = remaining.iter().any(|other| {
                other
                    .covered_pairs()
                    .iter()
                    .any(|(l, p, k)| *l == label && *p == property && *k == kind)
            });
            if still {
                continue;
            }
            let Some(pk) = self.string_pair_id(label, property) else {
                continue;
            };
            match kind {
                IndexKindSpec::Text => {
                    self.indexes.gram_pairs.remove(&pk);
                }
                IndexKindSpec::Range => {
                    self.indexes.range_pairs.remove(&pk);
                }
                IndexKindSpec::Point => {
                    self.indexes.point_pairs.remove(&pk);
                }
                _ => {}
            }
        }
    }

    pub(super) fn rebuild_declared_indexes(&self) {
        self.indexes.gram_pairs.clear();
        self.indexes.range_pairs.clear();
        self.indexes.point_pairs.clear();
        let details: Vec<IndexDetailSpec> = self
            .theater
            .lock()
            .unwrap()
            .index_details
            .values()
            .cloned()
            .collect();
        for detail in details {
            self.materialize_detail(&detail);
        }
    }

    /// The node `(label, property)` pairs the store's RANGE indexes serve an
    /// equality seek on, read from the typed catalog details
    /// ([`IndexDetailSpec::range_seek_pairs`]) rather than re-parsed from the
    /// rendered `:Label(property)` text: every component of a composite range
    /// index is covered, a relationship index contributes no node pair, and a
    /// comma-bearing property name stays one name.
    pub fn range_indexed_node_properties(&self) -> Vec<(String, String)> {
        self.theater
            .lock()
            .unwrap()
            .index_details
            .values()
            .flat_map(IndexDetailSpec::range_seek_pairs)
            .map(|(label, property)| (label.to_string(), property.to_string()))
            .collect()
    }

    fn intern_pair(&self, label: &str, key: &str) -> (u32, u32) {
        (self.interner.intern(label), self.interner.intern(key))
    }

    /// `key`'s interned id and the labels among `labels` whose pair for it
    /// exists in `pairs`. `None` when none do (the seek must scan). An empty
    /// pattern label list cannot be served: an index on one label would miss
    /// the others.
    fn indexed_labels<St>(
        &self,
        labels: &[String],
        key: &str,
        pairs: &CoreMap<(u32, u32), St>,
    ) -> Option<(u32, Vec<u32>)> {
        if labels.is_empty() {
            return None;
        }
        let kid = self.interner.get_id(key)?;
        let ids: Vec<u32> = labels
            .iter()
            .filter_map(|l| self.interner.get_id(l))
            .filter(|lid| pairs.contains_key(&(*lid, kid)))
            .collect();
        (!ids.is_empty()).then_some((kid, ids))
    }

    /// Feed one node to a pair's [`DrainState`]: drop its earlier
    /// postings (a no-op on a freshly built pair), post every version's
    /// value (the superset contract, ADR-0018), and reconcile the
    /// per-node trust state from the LIVE value the same pass visits —
    /// or from its absence, when no live version carries the pair's
    /// value. The ordered protocol every build and drain path keeps, in
    /// its one home, so a new path cannot post without the reconcile
    /// that keeps the range index's trust marks honest.
    pub(super) fn post_pair_node<St: DrainState>(
        &self,
        st: &mut St,
        id: NodeId,
        lid: u32,
        kid: u32,
    ) {
        st.remove_node(id);
        // The reconcile runs inside the walk (the value reference cannot
        // leave it — the walk's closure is higher-ranked over the value
        // borrow), tracked by a seen flag rather than a held `Option`.
        // Exactly one version is THE live one (the walk's newest-
        // open-ended rule), so exactly one reconcile call decides the
        // marks.
        let mut live_seen = false;
        self.for_each_node_pair_value(id, lid, kid, |value, version| {
            st.insert(id, value);
            if version == PairVersion::Live {
                live_seen = true;
                st.reconcile_live(id, Some(value));
            }
        });
        if !live_seen {
            st.reconcile_live(id, None);
        }
    }

    /// Build one pair from every version of every node in the label index
    /// (a superset; the seek re-checks the snapshot, ADR-0018).
    fn materialize_pair<St: DrainState>(
        &self,
        pairs: &CoreMap<(u32, u32), St>,
        label: &str,
        property: &str,
    ) {
        let (lid, kid) = self.intern_pair(label, property);
        let mut st = St::default();
        for id in self.pair_candidate_ids(lid) {
            self.post_pair_node(&mut st, id, lid, kid);
        }
        // Sealed before publish: a concurrent seek either sees the fully
        // built pair or none of it.
        st.seal();
        pairs.insert((lid, kid), st);
    }

    pub(super) fn materialize_text_index(&self, label: &str, property: &str) {
        self.materialize_pair(&self.indexes.gram_pairs, label, property);
    }

    pub(super) fn materialize_range_index(&self, label: &str, property: &str) {
        self.materialize_pair(&self.indexes.range_pairs, label, property);
    }

    pub(super) fn materialize_point_index(&self, label: &str, property: &str) {
        self.materialize_pair(&self.indexes.point_pairs, label, property);
    }

    /// Repost every dirty node of one pair. The write guard is held across
    /// the whole drain, so a concurrent `declared_dirty` lands either before
    /// the take (reposted here) or after it (kept for the next drain).
    /// Drains the pair's dirty postings and reports whether the pair
    /// exists — the one presence spelling, so a caller asking "is this
    /// pair declared, and current?" never restates the guard beside it.
    fn drain_pair<St: DrainState>(
        &self,
        pairs: &CoreMap<(u32, u32), St>,
        lid: u32,
        kid: u32,
    ) -> bool {
        let Some(mut st) = pairs.get_mut(&(lid, kid)) else {
            return false;
        };
        let ids = std::mem::take(st.dirty_mut());
        for id in ids {
            self.post_pair_node(&mut *st, id, lid, kid);
        }
        true
    }
}

/// Intersect `next` into the accumulator: the first set becomes the
/// accumulator, every later one narrows it. The canonical accumulator fold
/// for every multi-set seek (declared indexes and equality lookups).
pub(super) fn intersect_acc(acc: &mut Option<HashSet<NodeId>>, next: HashSet<NodeId>) {
    *acc = Some(match acc.take() {
        None => next,
        Some(prev) => prev.intersection(&next).copied().collect(),
    });
}

/// Per-kind byte estimate of the lazily built declared index pairs — the
/// research-gated census harness's view (ADR-0476 quarantine). Only
/// the order/by-node arms are counted; the eq-only fallback arms are not.
/// [`Self::text_bytes`] and [`Self::point_bytes`] are per-posting
/// estimates (no byte walk); every printed census row is one of these
/// fields.
#[cfg(feature = "research")]
#[derive(Debug, Default, Clone)]
pub struct DeclaredCensus {
    pub range_pairs: usize,
    pub range_order_keys: u64,
    pub range_posted_ids: u64,
    pub range_bytes: u64,
    pub text_pairs: usize,
    pub text_grams: u64,
    /// 8 bytes per gram posting.
    pub text_bytes: u64,
    pub point_pairs: usize,
    pub point_entries: u64,
    /// 40 bytes per posted point.
    pub point_bytes: u64,
}

impl Graph {
    #[cfg(feature = "research")]
    pub(super) fn declared_index_census(&self) -> DeclaredCensus {
        use std::mem::size_of;

        use crate::memory::{hashset_i64_bytes, vec_heap, OVERHEAD_MAP_ENTRY};

        let mut c = DeclaredCensus::default();
        for pair in self.indexes.range_pairs.iter() {
            c.range_pairs += 1;
            let st = pair.value();
            for postings in st.order.values() {
                c.range_order_keys += 1;
                c.range_bytes += size_of::<order::OrderKey>() as u64
                    + size_of::<order::Postings>() as u64
                    + OVERHEAD_MAP_ENTRY;
                match postings {
                    order::Postings::One(_) => c.range_posted_ids += 1,
                    order::Postings::Many(set) => {
                        c.range_posted_ids += set.len() as u64;
                        c.range_bytes += hashset_i64_bytes(set.capacity());
                    }
                }
            }
            for keys in st.by_node.values() {
                c.range_bytes +=
                    OVERHEAD_MAP_ENTRY + size_of::<order::RevKeys<order::OrderKey>>() as u64;
                if let order::RevKeys::Many(keys) = keys {
                    c.range_bytes += vec_heap(keys.len(), size_of::<order::OrderKey>() as u64);
                }
            }
            // The per-id sets are charged by element count: the transient
            // dirty queue, and the retained `unbounded` temporals and
            // `unposted` NaN values — small next to the postings above.
            c.range_bytes +=
                hashset_i64_bytes(st.dirty.len() + st.unbounded.len() + st.unposted.len());
        }
        for pair in self.indexes.gram_pairs.iter() {
            c.text_pairs += 1;
            let grams = pair.value().grams.len() as u64;
            c.text_grams += grams;
            c.text_bytes += grams * 8;
        }
        for pair in self.indexes.point_pairs.iter() {
            c.point_pairs += 1;
            let entries = pair.value().pt_count() as u64;
            c.point_entries += entries;
            c.point_bytes += entries * 40;
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_remove_drops_only_that_node() {
        let mut st = RangeIndex::default();
        order::insert_order(&mut st, &Value::Int(1), 10);
        order::insert_order(&mut st, &Value::Int(2), 11);
        order::order_remove_node(&mut st, 10);
        assert!(!st
            .order
            .contains_key(&order::order_key(&Value::Int(1)).unwrap()));
        assert!(st
            .order
            .get(&order::order_key(&Value::Int(2)).unwrap())
            .unwrap()
            .contains(11));
    }

    #[test]
    fn string_order_key_shares_the_value_payload() {
        let payload: Arc<str> = "Ada".into();
        let value = Value::String(Arc::clone(&payload));
        let mut st = RangeIndex::default();
        order::insert_order(&mut st, &value, 10);
        order::insert_order(&mut st, &Value::String("Bob".into()), 11);
        let posted = st.by_node[&10].first().unwrap();
        assert!(matches!(posted, order::OrderKey::String(s) if Arc::ptr_eq(s, &payload)));
        assert!(st
            .order
            .keys()
            .any(|k| matches!(k, order::OrderKey::String(s) if Arc::ptr_eq(s, &payload))));
        assert_eq!(order::prefix_ids(&st.order, "A"), HashSet::from([10]));
    }

    #[test]
    fn gram_remove_drops_only_that_node() {
        let mut st = GramIndex::default();
        text::gram_insert(&mut st, &Arc::from("abc"), 10);
        text::gram_insert(&mut st, &Arc::from("zzz"), 11);
        text::gram_remove_node(&mut st, 10);
        assert!(st.grams.values().all(|ids| !ids.holds(10)));
        assert!(st.grams.values().any(|ids| ids.holds(11)));
    }

    /// `finish_ids` re-checks posted candidates on the raw version (LABS-853):
    /// a tombstoned node, one missing a pattern label, and one whose visible
    /// value fails the seek are each dropped exactly as the eager `node_at`
    /// re-check dropped them — compared against that oracle at both
    /// snapshots.
    #[test]
    fn finish_ids_filters_like_the_eager_node_at_recheck() {
        let g = Graph::default();
        let node = |labels: &[&str], x: i64| {
            g.create_node(
                labels.iter().map(|l| l.to_string()).collect(),
                BTreeMap::from([("x".into(), Value::Int(x))]),
            )
            .unwrap()
        };
        g.begin_write().unwrap();
        let kept = node(&["N", "M"], 5);
        let dead = node(&["N", "M"], 6);
        let unlabeled = node(&["N"], 7);
        let failing = node(&["N", "M"], 8);
        g.commit_write();
        g.materialize_range_index("N", "x");
        let before = g.commit_ts();
        g.begin_write().unwrap();
        g.delete_node(dead, true).unwrap();
        g.set_property_node(failing, "x", Value::Int(0));
        g.commit_write();
        let after = g.commit_ts();

        let op = relate_ir::BinaryOp::Gt;
        let bound = Value::Int(1);
        let seek = RangeSeek::Cmp {
            op,
            bound: bound.clone(),
        };
        let labels = vec!["N".to_string(), "M".to_string()];
        let eager = |ts: u64| {
            let mut ids: Vec<_> = [kept, dead, unlabeled, failing]
                .into_iter()
                .filter(|id| {
                    g.node_at(*id, ts).is_some_and(|n| {
                        Graph::node_matches(&n, &labels, &BTreeMap::new())
                            && n.properties
                                .get("x")
                                .is_some_and(|v| order::cmp_holds(v, op, &bound))
                    })
                })
                .collect();
            ids.sort();
            ids
        };
        let lazy = |ts: u64| g.lookup_range_seek(&labels, "x", &seek, ts).unwrap();

        assert_eq!(lazy(before), vec![kept, dead, failing]);
        assert_eq!(lazy(before), eager(before));
        assert_eq!(lazy(after), vec![kept]);
        assert_eq!(lazy(after), eager(after));
        // A pattern label the interner never saw is carried by no node.
        let ghost = vec!["N".to_string(), "Ghost".to_string()];
        assert_eq!(g.lookup_range_seek(&ghost, "x", &seek, after), Some(vec![]));
    }
}
