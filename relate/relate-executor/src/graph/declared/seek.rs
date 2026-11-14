//! The declared indexes' seek family: the shared drain-scan-intersect-recheck
//! driver ([`Graph::lookup_indexed`]), the text, range and spatial seeks over
//! it, and the posting-length cardinalities the planner prices them with.
//! The pair wiring (build, drain, drop) these read stays in the parent.

use std::collections::{BTreeMap, HashSet};
use std::ops::ControlFlow;

use relate_core::{NodeId, Value};

use super::order::{self, RangeSeek, SpatialSeek};
use super::{intersect_acc, point, text, DrainState};
use crate::graph::*;

impl Graph {
    /// The shared seek: drain each indexed label's pair, scan it, intersect
    /// across labels, and re-check the snapshot. `scan` returns `None` to
    /// decline (the caller scans), as does a pair that vanished between
    /// `indexed_labels` and here — a concurrent `DROP INDEX` must not be
    /// reported as zero rows.
    fn lookup_indexed<St: DrainState>(
        &self,
        pairs: &CoreMap<(u32, u32), St>,
        labels: &[String],
        key: &str,
        ts: u64,
        scan: impl Fn(&St) -> Option<HashSet<NodeId>>,
        pred: impl Fn(&Value) -> bool,
    ) -> Option<Vec<NodeId>> {
        let (kid, lids) = self.indexed_labels(labels, key, pairs)?;
        let mut acc: Option<HashSet<NodeId>> = None;
        for lid in lids {
            self.drain_pair(pairs, lid, kid);
            let st = pairs.get(&(lid, kid))?;
            let next = scan(&st)?;
            drop(st);
            intersect_acc(&mut acc, next);
        }
        Some(self.finish_ids(acc.into_iter().flatten(), labels, key, ts, pred))
    }

    pub(in crate::graph) fn lookup_text_contains(
        &self,
        labels: &[String],
        key: &str,
        needle: &str,
        ts: u64,
    ) -> Option<Vec<NodeId>> {
        let grams = text::query_grams(needle)?;
        self.lookup_indexed(
            &self.indexes.gram_pairs,
            labels,
            key,
            ts,
            |st| Some(text::intersect_grams(st, &grams)),
            |v| v.as_string().is_some_and(|s| s.contains(needle)),
        )
    }

    pub(in crate::graph) fn lookup_text_prefix(
        &self,
        labels: &[String],
        key: &str,
        prefix: &str,
        ts: u64,
    ) -> Option<Vec<NodeId>> {
        self.lookup_indexed(
            &self.indexes.range_pairs,
            labels,
            key,
            ts,
            |st| Some(order::prefix_ids(&st.order, prefix)),
            |v| v.as_string().is_some_and(|s| s.starts_with(prefix)),
        )
    }

    pub(crate) fn lookup_range_seek(
        &self,
        labels: &[String],
        key: &str,
        seek: &RangeSeek,
        ts: u64,
    ) -> Option<Vec<NodeId>> {
        match seek {
            RangeSeek::Eq(v) => self.lookup_range_values(labels, key, std::slice::from_ref(v), ts),
            RangeSeek::In(vs) => self.lookup_range_values(labels, key, vs, ts),
            RangeSeek::Cmp { op, bound } => {
                order::order_key(bound)?;
                let op = *op;
                self.lookup_indexed(
                    &self.indexes.range_pairs,
                    labels,
                    key,
                    ts,
                    // A stored temporal outside the order-key window is not
                    // posted; the index cannot answer a comparison then.
                    |st| {
                        st.unbounded
                            .is_empty()
                            .then(|| order::range_ids(&st.order, op, bound))
                    },
                    |v| order::cmp_holds(v, op, bound),
                )
            }
        }
    }

    /// Upper bound on the ids [`Self::lookup_range_seek`] would collect from
    /// the index, counted from posting lengths. `None` when the index cannot
    /// serve the predicate. A comparison with unbounded temporal postings
    /// declines, matching the id seek. The bound is not snapshot-filtered.
    pub(crate) fn range_seek_cardinality(
        &self,
        labels: &[String],
        key: &str,
        seek: &RangeSeek,
    ) -> Option<usize> {
        match seek {
            RangeSeek::Eq(v) => self.range_values_cardinality(labels, key, std::slice::from_ref(v)),
            RangeSeek::In(vs) => self.range_values_cardinality(labels, key, vs),
            RangeSeek::Cmp { op, bound } => {
                order::order_key(bound)?;
                let op = *op;
                self.range_pair_cost(labels, key, |st| {
                    st.unbounded
                        .is_empty()
                        .then(|| order::range_count(&st.order, op, bound))
                })
            }
        }
    }

    /// Work upper bound for one range seek across the indexed labels. `Some(0)`
    /// when any label posts nothing, because the label intersection is empty.
    fn range_pair_cost(
        &self,
        labels: &[String],
        key: &str,
        count: impl Fn(&order::RangeIndex) -> Option<usize>,
    ) -> Option<usize> {
        let (kid, lids) = self.indexed_labels(labels, key, &self.indexes.range_pairs)?;
        let mut cost = 0usize;
        for lid in lids {
            self.drain_pair(&self.indexes.range_pairs, lid, kid);
            let st = self.indexes.range_pairs.get(&(lid, kid))?;
            let n = count(&st)?;
            if n == 0 {
                return Some(0);
            }
            cost = cost.saturating_add(n);
        }
        Some(cost)
    }

    fn range_values_cardinality(
        &self,
        labels: &[String],
        key: &str,
        values: &[Value],
    ) -> Option<usize> {
        let mut total = 0usize;
        let mut served = false;
        for value in values {
            if GroupKey::for_property_index(value).is_none() {
                continue;
            }
            let n =
                self.range_pair_cost(labels, key, |st| Some(order::equality_count(st, value)))?;
            served = true;
            total = total.saturating_add(n);
        }
        served.then_some(total)
    }

    fn lookup_range_values(
        &self,
        labels: &[String],
        key: &str,
        values: &[Value],
        ts: u64,
    ) -> Option<Vec<NodeId>> {
        let (kid, lids) = self.indexed_labels(labels, key, &self.indexes.range_pairs)?;
        let mut matched = HashSet::new();
        for v in values {
            if GroupKey::for_property_index(v).is_none() {
                continue;
            }
            let mut acc: Option<HashSet<NodeId>> = None;
            for lid in &lids {
                let ids = self.range_equality_ids(*lid, kid, v)?;
                intersect_acc(&mut acc, ids);
            }
            if let Some(ids) = acc {
                matched.extend(ids);
            }
        }
        Some(self.finish_ids(matched, labels, key, ts, |v| {
            values.iter().any(|b| v.eq_non_null(b))
        }))
    }

    /// The capped form of the declared equality seek: the matches of
    /// `labels` and `props` among the postings of the shortest-posting
    /// `(label, property)` pair, stopping at `cap + 1` matches. No id set
    /// is built and no posting is read past the stop; each posting read
    /// is one node read, counted as examined. Every candidate is checked
    /// against the snapshot at `ts` with [`Graph::node_matches`], the
    /// predicate the unbounded seek's re-check applies. `None` when a pair
    /// vanished (a concurrent `DROP INDEX`): the caller scans instead.
    pub(in crate::graph) fn range_equality_upto(
        &self,
        labels: &[String],
        props: &BTreeMap<String, Value>,
        ts: u64,
        cap: usize,
    ) -> Option<Vec<NodeId>> {
        let mut best: Option<(usize, u32, u32, &Value)> = None;
        for (key, value) in props {
            if GroupKey::for_property_index(value).is_none() {
                return Some(Vec::new());
            }
            let Some(kid) = self.interner.get_id(key) else {
                return Some(Vec::new());
            };
            for label in labels {
                let Some(lid) = self.interner.get_id(label) else {
                    continue;
                };
                if !self.indexes.range_pairs.contains_key(&(lid, kid)) {
                    continue;
                }
                self.drain_pair(&self.indexes.range_pairs, lid, kid);
                let st = self.indexes.range_pairs.get(&(lid, kid))?;
                let count = order::equality_count(&st, value);
                if best.is_none_or(|(least, ..)| count < least) {
                    best = Some((count, lid, kid, value));
                }
            }
        }
        let (_, lid, kid, value) = best?;
        let st = self.indexes.range_pairs.get(&(lid, kid))?;
        let (mut matched, mut examined) = (Vec::new(), 0u64);
        order::equality_each(&st, value, |id| {
            examined += 1;
            let hit = self
                .node_at(id, ts)
                .is_some_and(|n| Self::node_matches(&n, labels, props));
            if hit {
                matched.push(id);
            }
            if matched.len() > cap {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        crate::ctx::count_node_reads(examined);
        matched.sort_unstable();
        Some(matched)
    }

    /// Drain the range pair and read its equality postings. `None` when the
    /// pair vanished (a concurrent `DROP INDEX`), so the caller scans
    /// instead of reporting zero rows.
    pub(in crate::graph) fn range_equality_ids(
        &self,
        lid: u32,
        kid: u32,
        value: &Value,
    ) -> Option<HashSet<NodeId>> {
        self.drain_pair(&self.indexes.range_pairs, lid, kid);
        let st = self.indexes.range_pairs.get(&(lid, kid))?;
        Some(order::equality_ids(&st, value))
    }

    pub(crate) fn lookup_spatial_seek(
        &self,
        labels: &[String],
        key: &str,
        seek: &SpatialSeek,
        ts: u64,
    ) -> Option<Vec<NodeId>> {
        self.lookup_indexed(
            &self.indexes.point_pairs,
            labels,
            key,
            ts,
            |st| Some(point::spatial_ids(st, seek)),
            |v| point::spatial_holds(v, seek),
        )
    }

    /// Re-check each posted candidate against the snapshot at `ts`: the
    /// visible version must carry every pattern label and a `key` value
    /// that satisfies `pred`. Reads the raw version — interned label ids
    /// and the one-slot `CompactProps::get` — never `node_at`, which
    /// materializes the full property map per candidate (the profile-hot
    /// `to_map` path, ADR-0033; LABS-853).
    fn finish_ids(
        &self,
        acc: impl IntoIterator<Item = NodeId>,
        labels: &[String],
        key: &str,
        ts: u64,
        pred: impl Fn(&Value) -> bool,
    ) -> Vec<NodeId> {
        // Every stored label and key was interned before its version
        // committed, so a pattern label or key the interner never saw is
        // carried by no node: nothing survives the re-check.
        let lids: Option<Vec<u32>> = labels.iter().map(|l| self.interner.get_id(l)).collect();
        let (Some(lids), Some(kid)) = (lids, self.interner.get_id(key)) else {
            return Vec::new();
        };
        let mut ids: Vec<_> = acc
            .into_iter()
            .filter(|id| {
                self.nodes.get(id).is_some_and(|vers| {
                    visible_version(&vers, ts).is_some_and(|ver| {
                        ver.live.has_labels(&lids) && ver.live.props.get(kid).is_some_and(&pred)
                    })
                })
            })
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// True when every property has a range index on at least one pattern label.
    pub(in crate::graph) fn range_serves_equality(
        &self,
        labels: &[String],
        props: &BTreeMap<String, Value>,
    ) -> bool {
        if labels.is_empty() || props.is_empty() {
            return false;
        }
        props.keys().all(|k| {
            labels.iter().any(|l| {
                self.string_pair_id(l, k)
                    .is_some_and(|pk| self.indexes.range_pairs.contains_key(&pk))
            })
        })
    }
}
