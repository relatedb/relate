//! Index seeks over the overlay: the base graph's answer at the snapshot,
//! minus scratch deletions, plus scratch nodes that satisfy the predicate.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};

use relate_core::{NodeId, Value};

use crate::graph::{spatial_holds, Graph, RangeSeek, SpatialSeek};

use super::{union_ids, ExecGraph, WriteScratch};

impl<'a> ExecGraph<'a> {
    /// The label/property candidate lookup at the snapshot, ascending
    /// and deduplicated: `None` only when both `labels` and `props` are
    /// empty — the shape the caller scans for instead — a non-empty
    /// seek is always served.
    pub fn lookup_nodes(
        &self,
        labels: &[String],
        props: &BTreeMap<String, Value>,
    ) -> Option<Vec<NodeId>> {
        self.lookup_nodes_upto(labels, props, None)
    }

    /// [`ExecGraph::lookup_nodes`] for a caller that only needs to know
    /// whether more than `cap` nodes match ([`Graph::lookup_nodes_upto_at`]
    /// carries the contract: at most `cap` ids is the complete answer, a
    /// longer one only its length). A view with write scratch answers in
    /// full: a truncated base subset could lose members to the scratch's
    /// deletions and read as complete.
    pub fn lookup_nodes_upto(
        &self,
        labels: &[String],
        props: &BTreeMap<String, Value>,
        cap: Option<usize>,
    ) -> Option<Vec<NodeId>> {
        let base_cap = if self.scratch.is_some() { None } else { cap };
        // The base's own answer carries the both-empty policy (its only
        // `None` is the `Unindexed` arm), so the overlay propagates it
        // instead of re-checking the fact beside a handled form of it.
        let mut ids =
            self.base
                .lookup_nodes_upto_at(labels, props, self.snapshot_ts(), base_cap)?;
        // The base answer is already visibility-filtered at the snapshot;
        // only the scratch's deletions can hide more of it, so a
        // read-only view (no scratch) retains nothing — every candidate
        // it would re-check already passed the same visibility read.
        // Existence check is clone-free (node_exists, not node()).
        if let Some(cell) = self.scratch {
            ids.retain(|id| self.node_exists(*id));
            // This lookup's scratch additions merge through the
            // ascending-list algebra (LABS-803): [`scratch_lookup_adds`]
            // answers the family's ascending contract on its own, and
            // union_ids merges it with the ascending base — no duplicate
            // guard, no closing re-sort. (The string-lookup merges below
            // still spell the older push-and-sort idiom — their
            // migration is the review backlog's seek-family
            // unification.)
            //
            // The scratch half runs in the base half's own equality mode:
            // an index-served probe seeks the scratch's identity-keyed
            // index (ADR-0446 both halves), a scan-served probe scans
            // the scratch with `=` — one lookup, one relation, however
            // the base is served (LABS-1085 review).
            let by_index = self.base.equality_served_by_index(labels, props);
            ids = union_ids(ids, scratch_lookup_adds(cell, labels, props, by_index));
        }
        Some(ids)
    }

    /// Committed postings plus the scratch's, for one equality probe
    /// (ADR-0172 §3, ADR-0185): ascending, deduplicated. A superset — the
    /// caller re-verifies every id on this view.
    pub(super) fn merged_candidates<Id: Ord + Copy>(
        &self,
        committed: Vec<Id>,
        scratch: impl FnOnce(&mut WriteScratch) -> HashSet<Id>,
    ) -> Vec<Id> {
        let mut ids = committed;
        if let Some(cell) = self.scratch {
            let s = &mut *cell.borrow_mut();
            ids.extend(scratch(s));
        }
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Committed bucket length plus the scratch's, both counted in place
    /// (ADR-0184): a composite uniqueness probe compares these sizes
    /// without materializing any candidate list.
    pub(super) fn merged_len(
        &self,
        committed: usize,
        scratch: impl FnOnce(&mut WriteScratch) -> usize,
    ) -> usize {
        committed
            + self.scratch.map_or(0, |cell| {
                let s = &mut *cell.borrow_mut();
                scratch(s)
            })
    }

    /// Candidate ids for a node uniqueness probe on `(label, key) = value`
    /// (ADR-0172 §3), ascending and deduplicated: the committed graph's lazy
    /// equality postings for the pair plus the scratch's `prop_ix` seek.
    /// A superset — the caller re-verifies every id on this view. Empty when
    /// `value` has no property-index key (it contains a NaN).
    pub(crate) fn equality_candidates(&self, label: &str, key: &str, value: &Value) -> Vec<NodeId> {
        self.merged_candidates(self.base.equality_postings(label, key, value), |s| {
            s.batch.prop_ix.seek(&s.batch.nodes, &[label], key, value)
        })
    }

    /// The size of the committed plus scratch equality bucket for
    /// `(label, key) = value` (ADR-0184), counted in place: the pair
    /// selection of a composite uniqueness probe compares these sizes
    /// without materializing any candidate list. Zero when `value` has no
    /// property-index key (it contains a NaN).
    pub(crate) fn equality_bucket_len(&self, label: &str, key: &str, value: &Value) -> usize {
        self.merged_len(self.base.equality_postings_len(label, key, value), |s| {
            s.batch.prop_ix.seek_len(&s.batch.nodes, label, key, value)
        })
    }

    /// `None` when the base index cannot serve the needle (the gram index
    /// rejects empty needles): the caller falls back to a full scan, exactly
    /// like the `lookup_nodes` path, rather than matching nothing.
    pub fn lookup_string_contains(
        &self,
        labels: &[String],
        key: &str,
        needle: &str,
    ) -> Option<Vec<NodeId>> {
        self.merge_string_lookup(
            labels,
            key,
            |n| n.as_string().is_some_and(|s| s.contains(needle)),
            || {
                self.base
                    .lookup_string_contains_at(labels, key, needle, self.snapshot_ts())
            },
        )
    }

    /// `None` when no `RANGE` index serves the prefix: the caller scans.
    pub fn lookup_string_prefix(
        &self,
        labels: &[String],
        key: &str,
        prefix: &str,
    ) -> Option<Vec<NodeId>> {
        self.merge_string_lookup(
            labels,
            key,
            |n| n.as_string().is_some_and(|s| s.starts_with(prefix)),
            || {
                self.base
                    .lookup_string_prefix_at(labels, key, prefix, self.snapshot_ts())
            },
        )
    }

    pub(crate) fn lookup_range(
        &self,
        labels: &[String],
        key: &str,
        seek: &RangeSeek,
    ) -> Option<Vec<NodeId>> {
        self.merge_string_lookup(
            labels,
            key,
            |v| Self::range_overlay_pred(v, seek),
            || {
                self.base
                    .lookup_range_seek(labels, key, seek, self.snapshot_ts())
            },
        )
    }

    /// Upper bound on [`Self::lookup_range`], counted from posting lengths.
    /// `None` when the index cannot serve the predicate or the write overlay
    /// has created, updated, or deleted a node: the committed postings are
    /// then an incomplete description of this view, and the caller keeps the
    /// eager probe.
    pub(crate) fn lookup_range_cardinality(
        &self,
        labels: &[String],
        key: &str,
        seek: &RangeSeek,
    ) -> Option<usize> {
        if self.overlay_changes_node_membership() {
            return None;
        }
        self.base.range_seek_cardinality(labels, key, seek)
    }

    pub(crate) fn lookup_spatial(
        &self,
        labels: &[String],
        key: &str,
        seek: &SpatialSeek,
    ) -> Option<Vec<NodeId>> {
        self.merge_string_lookup(
            labels,
            key,
            |v| spatial_holds(v, seek),
            || {
                self.base
                    .lookup_spatial_seek(labels, key, seek, self.snapshot_ts())
            },
        )
    }

    fn range_overlay_pred(v: &Value, seek: &RangeSeek) -> bool {
        match seek {
            RangeSeek::Eq(b) => v.eq_non_null(b),
            RangeSeek::In(xs) => xs.iter().any(|b| v.eq_non_null(b)),
            RangeSeek::Cmp { op, bound } => {
                // Superset of the committed comparison. An incomparable value
                // or an unexpected operator stays in the candidate set; the
                // residual predicate in expand rechecks it.
                let Some(ord) = v.compare(bound) else {
                    return true;
                };
                match op {
                    relate_ir::BinaryOp::Lt => ord.is_lt(),
                    relate_ir::BinaryOp::Gt => ord.is_gt(),
                    relate_ir::BinaryOp::Lte => ord.is_le(),
                    relate_ir::BinaryOp::Gte => ord.is_ge(),
                    _ => true,
                }
            }
        }
    }

    fn merge_string_lookup(
        &self,
        labels: &[String],
        key: &str,
        pred: impl Fn(&Value) -> bool,
        base: impl FnOnce() -> Option<Vec<NodeId>>,
    ) -> Option<Vec<NodeId>> {
        let mut ids = base()?;
        ids.retain(|id| self.node_exists(*id));
        if let Some(cell) = self.scratch {
            // Read the scratch copy in place — `self.node` would clone its
            // whole property map per scratch node; `node_exists` keeps the
            // one visibility prologue (a scratch deletion hides the copy).
            let scratch = cell.borrow();
            for (id, n) in &scratch.batch.nodes {
                if ids.contains(id) || !self.node_exists(*id) {
                    continue;
                }
                if labels.iter().all(|l| n.labels.iter().any(|x| x == l))
                    && n.properties.get(key).is_some_and(&pred)
                {
                    ids.push(*id);
                }
            }
        }
        ids.sort();
        Some(ids)
    }
}

fn scratch_lookup_adds(
    cell: &RefCell<WriteScratch>,
    labels: &[String],
    props: &BTreeMap<String, Value>,
    by_index: bool,
) -> Vec<NodeId> {
    // One candidate resolution — `NodeId`s in both arms (the index seek
    // returns ids; the scan walks the map in place, never a per-node
    // clone) — then one application of the canonical `node_matches`. The
    // keyed arm needs the mutable borrow first (the seek builds its pair
    // lazily); the filter runs under the shared borrow after it.
    let keyed = match props.first_key_value() {
        Some((key, value)) if by_index => {
            let mut s = cell.borrow_mut();
            let batch = &mut s.batch;
            // One label narrows the candidates (every match carries all
            // of `labels`); none seeks the any-label bucket.
            let seek_labels = &labels[..labels.len().min(1)];
            Some(
                batch
                    .prop_ix
                    .seek(&batch.nodes, seek_labels, key, value)
                    .into_iter()
                    .collect::<Vec<_>>(),
            )
        }
        // A scan-served probe (no index serves the pair): the base half
        // scans with `=` — the scratch half scans the same way, so a
        // zoned-DateTime DST-overlap twin is found in either half.
        _ => None,
    };
    let mut adds: Vec<NodeId> = match keyed {
        Some(ids) => {
            let s = cell.borrow();
            ids.into_iter()
                .filter(|id| {
                    s.batch
                        .nodes
                        .get(id)
                        .is_some_and(|n| Graph::node_matches(n, labels, props))
                })
                .collect()
        }
        None => {
            let s = cell.borrow();
            s.batch
                .nodes
                .iter()
                .filter(|(_, n)| Graph::node_matches(n, labels, props))
                .map(|(id, _)| *id)
                .collect()
        }
    };
    adds.sort_unstable();
    adds
}
