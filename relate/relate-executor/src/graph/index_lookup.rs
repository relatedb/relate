//! The index-backed lookup and scan planning (ADR-0218, ADR-0063): the
//! label/equality candidate sets, the residual scan, and the plan-time
//! cardinality reads. A sibling of `index.rs`.

use super::declared::intersect_acc;
use super::*;

enum NodeCandidates {
    /// No labels and no properties: the caller scans.
    Unindexed,
    /// Index postings that still need `node_matches`.
    Indexed(HashSet<NodeId>),
    /// A finished id list. Empty means the predicate cannot match.
    Final(Vec<NodeId>),
}

impl Graph {
    /// Approximate number of nodes carrying `label`: the label-index set
    /// size (ADR-0218). Plan-time estimate only — it is O(1), ignores
    /// snapshot visibility, and counts every id the index holds; a label
    /// the store has never interned is 0.
    pub fn label_count(&self, label: &str) -> u64 {
        self.interner
            .get_id(label)
            .map_or(0, |lid| self.label_count_by_id(lid))
    }

    /// [`Graph::label_count`] for an already-resolved label token id.
    pub fn label_count_by_id(&self, lid: u32) -> u64 {
        self.indexes
            .label_index
            .get(&lid)
            .map_or(0, |ids| ids.len() as u64)
    }

    pub fn lookup_nodes(
        &self,
        labels: &[String],
        props: &BTreeMap<String, Value>,
    ) -> Option<Vec<NodeId>> {
        self.lookup_nodes_at(labels, props, self.read_ts())
    }

    /// The label/property candidate lookup at `ts`, ascending and
    /// deduplicated: `None` only for the both-empty `Unindexed` arm —
    /// the shape the caller scans for instead — a non-empty seek is
    /// always served.
    pub fn lookup_nodes_at(
        &self,
        labels: &[String],
        props: &BTreeMap<String, Value>,
        ts: u64,
    ) -> Option<Vec<NodeId>> {
        self.lookup_nodes_upto_at(labels, props, ts, None)
    }

    /// [`Graph::lookup_nodes_at`] for a caller that only needs to know
    /// whether more than `cap` nodes match: a residual scan stops once it
    /// has `cap + 1` matches. An answer of at most `cap` ids is complete,
    /// ascending and deduplicated; a longer one is an arbitrary
    /// `cap + 1`-id subset of the matches, meaningful only by its length.
    /// Index-served arms answer in full (their postings already exist);
    /// `None` is the both-empty `Unindexed` arm, as for the uncapped form.
    pub fn lookup_nodes_upto_at(
        &self,
        labels: &[String],
        props: &BTreeMap<String, Value>,
        ts: u64,
        cap: Option<usize>,
    ) -> Option<Vec<NodeId>> {
        // The label scan's hot shape — one label, no property equality —
        // is the one canonical label scan ([`node_ids_with_label_at`]):
        // the bucket's ids copied once under its guard, each probed for
        // visibility and the interned label id in place. No node is
        // materialized (the profile-hot `node_at` path builds every
        // candidate's full property map just to check a label).
        if labels.len() == 1 && props.is_empty() {
            return Some(self.node_ids_with_label_at(&labels[0], ts));
        }
        match self.node_candidates(labels, props, ts, cap) {
            NodeCandidates::Unindexed => None,
            NodeCandidates::Final(ids) => Some(ids),
            NodeCandidates::Indexed(acc) => {
                // A label-only lookup needs no materialized node: the
                // id-based label probes answer it, the labels interned
                // once here instead of per candidate. (The materialized
                // form built every candidate's property map just to check
                // labels — ADR-0473's q30 measurement.) An uninterned
                // label can match nothing (the interner is append-only),
                // so a miss empties the answer — exactly what a
                // `node_matches` against no node's label list would do.
                let mut ids: Vec<_> = if props.is_empty() {
                    let lids: Option<Vec<u32>> =
                        labels.iter().map(|l| self.interner.get_id(l)).collect();
                    match lids {
                        Some(lids) => acc
                            .into_iter()
                            .filter(|&id| {
                                lids.iter()
                                    .all(|lid| self.node_carries_label_at(id, *lid, ts))
                            })
                            .collect(),
                        None => Vec::new(),
                    }
                } else {
                    acc.into_iter()
                        .filter(|id| {
                            self.node_at(*id, ts)
                                .is_some_and(|n| Self::node_matches(&n, labels, props))
                        })
                        .collect()
                };
                ids.sort();
                Some(ids)
            }
        }
    }

    /// The ids posted under every label in `labels`: the label-index
    /// buckets intersected through `intersect_acc`, the fold every
    /// multi-set seek uses. A label the store never interned can match
    /// nothing, so its empty bucket empties the result. Callers route a
    /// label-less pattern to the full scan — this is only reached with a
    /// non-empty `labels`.
    fn intersect_label_ids(&self, labels: &[String]) -> HashSet<NodeId> {
        let mut acc: Option<HashSet<NodeId>> = None;
        for label in labels {
            // An uninterned label can match nothing.
            let ids = self
                .interner
                .get_id(label)
                .and_then(|lid| self.indexes.label_index.get(&lid).map(|r| r.clone()))
                .unwrap_or_default();
            intersect_acc(&mut acc, ids);
        }
        acc.unwrap_or_default()
    }

    /// Index postings for a node lookup, or a finished answer when the
    /// index cannot serve the predicate.
    fn node_candidates(
        &self,
        labels: &[String],
        props: &BTreeMap<String, Value>,
        ts: u64,
        cap: Option<usize>,
    ) -> NodeCandidates {
        if labels.is_empty() && props.is_empty() {
            return NodeCandidates::Unindexed;
        }
        if !labels.is_empty() && props.is_empty() {
            return NodeCandidates::Indexed(self.intersect_label_ids(labels));
        }
        if props.is_empty() || !self.range_serves_equality(labels, props) {
            return NodeCandidates::Final(self.scan_match(labels, props, ts, cap));
        }
        let mut acc: Option<HashSet<NodeId>> = None;
        for (key, value) in props {
            if GroupKey::for_property_index(value).is_none() {
                return NodeCandidates::Final(Vec::new());
            }
            let Some(kid) = self.interner.get_id(key) else {
                return NodeCandidates::Final(Vec::new());
            };
            let mut next: Option<HashSet<NodeId>> = None;
            for label in labels {
                let Some(lid) = self.interner.get_id(label) else {
                    continue;
                };
                if !self.indexes.range_pairs.contains_key(&(lid, kid)) {
                    continue;
                }
                let Some(ids) = self.range_equality_ids(lid, kid, value) else {
                    return NodeCandidates::Final(self.scan_match(labels, props, ts, cap));
                };
                intersect_acc(&mut next, ids);
            }
            intersect_acc(&mut acc, next.unwrap_or_default());
        }
        NodeCandidates::Indexed(acc.unwrap_or_default())
    }

    /// The canonical node-predicate for a label/property seek: every
    /// sought label carried, every sought property equal under Cypher
    /// equality (`1 = 1.0`) — the predicate the base's residual scan
    /// (`scan_match` and the `Indexed` arm's re-check), the declared
    /// path's `finish_ids`, and the overlay's scratch merge all apply.
    /// Hosted here, the lookup/scan file whose module doc claims this
    /// family — the one home every consumer reads downward.
    /// Whether a keyed node lookup on `(labels, props)` is served by the
    /// declared range index — the base half's identity-keyed mode, and
    /// the mode the overlay's scratch half must match so one lookup's
    /// two halves answer with one equality relation (LABS-1085 review).
    /// A scan-served lookup keeps `=` semantics in both halves.
    pub(crate) fn equality_served_by_index(
        &self,
        labels: &[String],
        props: &BTreeMap<String, Value>,
    ) -> bool {
        self.range_serves_equality(labels, props)
    }

    pub(crate) fn node_matches(
        n: &Node,
        labels: &[String],
        props: &BTreeMap<String, Value>,
    ) -> bool {
        // Property comparison is Cypher equality (`1 = 1.0`), matching the
        // normalized index key that produced the candidate.
        labels.iter().all(|l| n.labels.iter().any(|x| x == l))
            && props
                .iter()
                .all(|(k, v)| n.properties.get(k).is_some_and(|have| have.eq_non_null(v)))
    }

    /// Label scan (or a full scan when the pattern has no label) filtered by
    /// equality. Used when no `RANGE` index serves the properties. With
    /// `cap`, the scan stops at the first `cap + 1` matches (see
    /// [`Graph::lookup_nodes_upto_at`]).
    fn scan_match(
        &self,
        labels: &[String],
        props: &BTreeMap<String, Value>,
        ts: u64,
        cap: Option<usize>,
    ) -> Vec<NodeId> {
        let ids: Vec<NodeId> = if labels.is_empty() {
            self.all_node_ids_at(ts)
        } else {
            self.intersect_label_ids(labels).into_iter().collect()
        };
        let mut matched = Vec::new();
        for id in ids {
            let hit = self
                .node_at(id, ts)
                .is_some_and(|n| Self::node_matches(&n, labels, props));
            if hit {
                matched.push(id);
                if cap.is_some_and(|cap| matched.len() > cap) {
                    break;
                }
            }
        }
        matched.sort();
        matched
    }

    pub fn lookup_string_contains_at(
        &self,
        labels: &[String],
        key: &str,
        needle: &str,
        ts: u64,
    ) -> Option<Vec<NodeId>> {
        self.lookup_text_contains(labels, key, needle, ts)
    }

    /// `None` unless a `RANGE` index names the `(label, property)`.
    /// `STARTS WITH` reads that ordered map; without one the caller scans.
    pub fn lookup_string_prefix_at(
        &self,
        labels: &[String],
        key: &str,
        prefix: &str,
        ts: u64,
    ) -> Option<Vec<NodeId>> {
        self.lookup_text_prefix(labels, key, prefix, ts)
    }
}
