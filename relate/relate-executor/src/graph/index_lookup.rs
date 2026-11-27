//! The index-backed lookup and scan planning (ADR-0218, ADR-0063): the
//! label/equality candidate sets, the residual scan, and the plan-time
//! cardinality reads. A sibling of `index.rs`.

use std::ops::ControlFlow;

use super::declared::intersect_acc;
use super::index_postings::EqualityRoute;
use super::*;

/// The fixed half of an equality re-check walk — what every examined
/// candidate is re-verified against, and the cap contract — bundled so
/// the walk ([`RecheckWalk`]) reads one context per candidate.
pub(in crate::graph) struct RecheckSpec<'a> {
    /// Every pattern label and property, re-applied per candidate by
    /// [`Graph::node_matches`].
    pub(in crate::graph) labels: &'a [String],
    pub(in crate::graph) props: &'a BTreeMap<String, Value>,
    /// The snapshot the re-check verifies against.
    pub(in crate::graph) ts: u64,
    /// The capped contract's stop: `Break` at the first `cap + 1`
    /// matches ([`Graph::lookup_nodes_upto_at`]); `None` never breaks.
    pub(in crate::graph) cap: Option<usize>,
}

/// One equality re-check walk over an id stream: the hits and the
/// examined count, carrying the shared per-candidate body (count the
/// read, re-verify the snapshot with the canonical node predicate,
/// record the hit, stop at the cap) and the closing count-and-sort.
/// The caller supplies only the stream — a label bucket, a declared
/// posting walk, or a lazy posting bucket — visiting each id through
/// [`Self::visit`] and taking the answer from [`Self::finish`].
pub(in crate::graph) struct RecheckWalk<'a, 'g> {
    graph: &'g Graph,
    spec: &'a RecheckSpec<'a>,
    matched: Vec<NodeId>,
    examined: u64,
}

impl RecheckWalk<'_, '_> {
    /// One examined candidate — the shared body; `Break` at the cap.
    pub(in crate::graph) fn visit(&mut self, id: NodeId) -> ControlFlow<()> {
        self.examined += 1;
        let hit = self
            .graph
            .node_at(id, self.spec.ts)
            .is_some_and(|n| Graph::node_matches(&n, self.spec.labels, self.spec.props));
        if hit {
            self.matched.push(id);
        }
        if self.spec.cap.is_some_and(|cap| self.matched.len() > cap) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }

    /// The closing half: count the reads, sort the hits.
    pub(in crate::graph) fn finish(self) -> Vec<NodeId> {
        crate::ctx::count_node_reads(self.examined);
        let mut matched = self.matched;
        matched.sort_unstable();
        matched
    }
}

/// Whether a candidate resolution consults the attached store: the
/// ordinary path consults ([`StoreConsult::Consult`]); the equality
/// serve's per-value fallback rides [`StoreConsult::Bypass`] because its
/// one whole-list consult already asked and was declined — a typed
/// policy, not a caller's bare bool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreConsult {
    Consult,
    Bypass,
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
        self.lookup_nodes_at_with(labels, props, ts, cap, StoreConsult::Consult)
    }

    fn lookup_nodes_at_with(
        &self,
        labels: &[String],
        props: &BTreeMap<String, Value>,
        ts: u64,
        cap: Option<usize>,
        consult: StoreConsult,
    ) -> Option<Vec<NodeId>> {
        // The label scan's hot shape — one label, no property equality —
        // is the one canonical label scan ([`node_ids_with_label_at`]):
        // the bucket's ids copied once under its guard, each probed for
        // visibility and the interned label id in place. No node is
        // materialized (the profile-hot `node_at` path builds every
        // candidate's full property map just to check a label). ADR-0665
        // A/B: the store's label arm does not preempt this — the CI
        // interleave measured the bitset arm at 1.5-2.2x on the pure
        // label-scan pattern queries (q8/q30) while helping none.
        if labels.len() == 1 && props.is_empty() {
            return Some(self.node_ids_with_label_at(&labels[0], ts));
        }
        self.node_candidates(labels, props, ts, cap, consult)
    }

    /// The capped `=` / `IN` point seek over one property: the ids whose
    /// `key` equals any of `values` and that carry every label, stopping
    /// once more than `cap` distinct ids match. Each value rides the
    /// capped keyed lookup ([`Graph::lookup_nodes_upto_at`]) — the
    /// shortest posting streamed through the shared re-check walk, never
    /// the whole posting list built into an id set — so a non-unique
    /// value matching a large store reads `cap + 1` postings, not every
    /// match. `None` only when a per-value keyed lookup declines
    /// (a vanished pair under a concurrent `DROP INDEX`). An
    /// answer of at most `cap` ids is complete, ascending and
    /// deduplicated; a longer one is an arbitrary subset meaningful only
    /// by its length. A value with no property-index key (a NaN)
    /// matches nothing.
    pub(crate) fn lookup_equality_upto(
        &self,
        labels: &[String],
        key: &str,
        values: &[Value],
        ts: u64,
        cap: usize,
    ) -> Option<Vec<NodeId>> {
        let probe = |value: &Value| BTreeMap::from([(key.to_string(), value.clone())]);
        // The one route decision the uncapped serve reads
        // ([`Graph::lookup_range_seek`]). ADR-0665 A/B (measured
        // warm/cold ordering): behind the declared arm the attached
        // experimental store's sorted `(key,row)` side-file serves the
        // capped seek ahead of a COLD lazy pair only — a warm pair's
        // probe beats the side-file seek, so it keeps the engine arm.
        // The store is consulted ONCE for the whole list (the `IN` shape
        // its sorted pass answers in one seek); the fallback loop below
        // rides a store-free keyed lookup, so a cold-pair decline is not
        // re-asked per value, and a Scan route the store declined answers
        // `None` for the CALLER's one scan — the per-value loop would
        // turn it into one full scan per list member (the regression the
        // store arm introduced; the pre-store pre-check declined exactly
        // here).
        // The one route match answers both questions — whether the store
        // is consulted for the whole list, and whether a store decline
        // hands the CALLER the single scan — never a bool beside a
        // re-test of the same enum.
        match self.values_equality_route(labels, key, values) {
            EqualityRoute::Scan => {
                if let Some(ids) = self.prop_store_equality(labels, key, values, ts, Some(cap)) {
                    return Some(ids);
                }
                // No index route and no store answer: the caller's single
                // scan is the shape.
                return None;
            }
            // A declared pair always outranks the store (ADR-0614's
            // declared-then-postings precedence, unchanged).
            EqualityRoute::Declared => {}
            EqualityRoute::Postings => {
                if !self.postings_pair_ready(labels, key) {
                    if let Some(ids) = self.prop_store_equality(labels, key, values, ts, Some(cap))
                    {
                        return Some(ids);
                    }
                }
            }
        }
        let mut found = std::collections::BTreeSet::new();
        for value in values
            .iter()
            .filter(|v| GroupKey::for_property_index(v).is_some())
        {
            // `None` is the keyed lookup's decline: propagated, so the
            // serve declines with it.
            found.extend(self.lookup_nodes_at_with(
                labels,
                &probe(value),
                ts,
                Some(cap),
                // Bypass: the whole-list consult above already asked and
                // the store declined — the per-value fallback builds the
                // route's own postings answer, never a second consult.
                StoreConsult::Bypass,
            )?);
            if found.len() > cap {
                break;
            }
        }
        Some(found.into_iter().collect())
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
        consult: StoreConsult,
    ) -> Option<Vec<NodeId>> {
        if labels.is_empty() && props.is_empty() {
            return None;
        }
        if !labels.is_empty() && props.is_empty() {
            // The label bucket intersection finished here: the id-based
            // label probes answer it, the labels interned once — no node
            // materialized (the materialized form built every candidate's
            // property map just to check labels, ADR-0473's q30
            // measurement). An uninterned label can match nothing (the
            // interner is append-only), so a miss empties the answer.
            let acc = self.intersect_label_ids(labels);
            // Each posting is examined by the probe below.
            crate::ctx::count_node_reads(acc.len() as u64);
            // The probe filters in HashSet order; the arm's ascending
            // contract (which `union_ids` merges on and `far_probe`
            // assumes) needs the closing sort — the walk branch's
            // `finish` sorts itself.
            let mut probed: Vec<_> = match self.intern_labels(labels) {
                Some(lids) => acc
                    .into_iter()
                    .filter(|&id| {
                        lids.iter()
                            .all(|lid| self.node_carries_label_at(id, *lid, ts))
                    })
                    .collect(),
                None => Vec::new(),
            };
            probed.sort_unstable();
            return Some(probed);
        }
        // The props-non-empty shapes from here (the two guards above own
        // the empty-props ones). The route decides once (ADR-0614): a
        // declared RANGE pair, the lazy equality postings, or the scan —
        // the one three-way decision the id seek, the cardinality, this
        // arm, and the overlay's scratch-mode flag all read.
        let route = self.map_equality_route(labels, props);
        // No declared pair covers the lookup. ADR-0665 A/B (measured
        // warm/cold ordering): the attached experimental store's sorted
        // side-file preempts a COLD lazy pair — its build was paid at
        // attach, the postings' build is paid at first serve — but a WARM
        // pair's probe beats the side-file seek (the CI interleave measured
        // the store ahead of warm postings as a regression on repeated
        // serves), so the engine arm keeps it. The route's decline cases
        // (a label-less pattern, a value whose identity is narrower than
        // its `=` class) are `Scan` here — the store arm serves them
        // unconditionally (exact side-file match), with the full scan
        // behind it. A declared pair always outranks the store
        // (ADR-0614's declared-then-postings precedence, unchanged).
        let store_first = match route {
            EqualityRoute::Declared => false,
            EqualityRoute::Scan => true,
            EqualityRoute::Postings => !self.postings_pair_ready(
                labels,
                props
                    .first_key_value()
                    .expect("a props-non-empty candidate carries a first key")
                    .0,
            ),
        };
        if consult == StoreConsult::Consult && store_first && props.len() == 1 {
            // One-property shapes only (the exact side-file match);
            // multi-property patterns stay on the index routes, and the
            // label-only shape never reaches here (the canonical label
            // route above owns it).
            let (key, value) = props
                .first_key_value()
                .expect("a one-property candidate carries a first key");
            if let Some(ids) =
                self.prop_store_equality(labels, key, std::slice::from_ref(value), ts, cap)
            {
                return Some(ids);
            }
        }
        match route {
            EqualityRoute::Postings => {
                return Some(self.equality_postings_upto(labels, props, ts, cap));
            }
            EqualityRoute::Scan => {
                return Some(self.scan_match(labels, props, ts, cap));
            }
            // A declared RANGE pair covers every sought property: the
            // capped stream or the intersect loop below answers it.
            EqualityRoute::Declared => {}
        }
        // A capped caller streams the shortest posting instead of
        // building every posting's id set; a vanished pair (a concurrent
        // `DROP INDEX`) falls to the scan below.
        if let Some(cap) = cap {
            return match self.range_equality_upto(labels, props, ts, cap) {
                Some(ids) => Some(ids),
                None => Some(self.scan_match(labels, props, ts, Some(cap))),
            };
        }
        let mut acc: Option<HashSet<NodeId>> = None;
        for (key, value) in props {
            if GroupKey::for_property_index(value).is_none() {
                return Some(Vec::new());
            }
            let Some(kid) = self.interner.get_id(key) else {
                return Some(Vec::new());
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
                    return Some(self.scan_match(labels, props, ts, cap));
                };
                intersect_acc(&mut next, ids);
            }
            intersect_acc(&mut acc, next.unwrap_or_default());
        }
        // The declared pairs' accumulated postings, finished here: the
        // shared re-check walk verifies each against the full predicate —
        // the walk counts each examined posting itself, no cap on this
        // arm (`cap: None` makes the walk's `Break` unreachable — the
        // discard states it).
        let spec = RecheckSpec {
            labels,
            props,
            ts,
            cap: None,
        };
        let mut walk = self.recheck_walk(&spec);
        for id in acc.unwrap_or_default() {
            let _ = walk.visit(id);
        }
        Some(walk.finish())
    }

    /// Whether a keyed node lookup on `(labels, props)` is answered by
    /// an identity-keyed structure — a declared range pair, or, when
    /// none covers the pair, the lazy equality postings (ADR-0614) —
    /// the base half's mode, and the mode the overlay's scratch half
    /// must match so one lookup's two halves answer with one equality
    /// relation (LABS-1085 review). A scan-served lookup — a declined
    /// value, a label-less pattern — keeps `=` semantics in both
    /// halves.
    pub(crate) fn equality_served_by_index(
        &self,
        labels: &[String],
        props: &BTreeMap<String, Value>,
    ) -> bool {
        !matches!(self.map_equality_route(labels, props), EqualityRoute::Scan)
    }

    /// The label half of [`Self::node_matches`]: the node carries every one
    /// of `labels`. The overlay's scratch merge asks it too, so one
    /// containment rule serves the committed and the scratch side.
    pub(crate) fn node_carries_labels(n: &Node, labels: &[String]) -> bool {
        labels.iter().all(|l| n.labels.iter().any(|x| x == l))
    }

    /// The canonical node-predicate for a label/property seek: every
    /// sought label carried, every sought property equal under Cypher
    /// equality (`1 = 1.0`) — the predicate the base's residual scan
    /// (`scan_match` and the `Indexed` arm's re-check), the declared
    /// path's `finish_ids`, the lazy postings' serve (ADR-0614), and
    /// the overlay's scratch merge all apply. Hosted here, the
    /// lookup/scan file whose module doc claims this family — the one
    /// home every consumer reads downward.
    pub(crate) fn node_matches(
        n: &Node,
        labels: &[String],
        props: &BTreeMap<String, Value>,
    ) -> bool {
        // Property comparison is Cypher equality (`1 = 1.0`), matching the
        // normalized index key that produced the candidate.
        Self::node_carries_labels(n, labels)
            && props
                .iter()
                .all(|(k, v)| n.properties.get(k).is_some_and(|have| have.eq_non_null(v)))
    }

    /// Begin one equality re-check walk over `spec` — the shared
    /// per-candidate body and the closing count-and-sort behind
    /// [`RecheckWalk`], the form every re-checking site (the scan tail,
    /// both equality serves, the `Indexed` arm's posting re-check)
    /// reads.
    pub(in crate::graph) fn recheck_walk<'a>(
        &self,
        spec: &'a RecheckSpec<'a>,
    ) -> RecheckWalk<'a, '_> {
        RecheckWalk {
            graph: self,
            spec,
            matched: Vec::new(),
            examined: 0,
        }
    }

    /// The interned pattern label with the smallest label-index bucket —
    /// the one bucket a label-conjunction scan or a postings serve
    /// streams, so a serve never examines more nodes than the scan it
    /// replaces. `None` when any pattern label was never interned (the
    /// fail-closed rule: no node carries it, so the whole label
    /// conjunction matches nothing) or `labels` is empty — the answer
    /// both shapes give then.
    pub(in crate::graph) fn smallest_interned_bucket(&self, labels: &[String]) -> Option<u32> {
        self.intern_labels(labels)?
            .into_iter()
            .min_by_key(|lid| self.label_count_by_id(*lid))
    }

    /// Every pattern label's interned id, fail-closed: `None` when any
    /// label was never interned — no node carries it, so the whole
    /// label conjunction matches nothing. The one spelling of the rule
    /// the keyed lookup's label-only arm, the shortest-bucket choice,
    /// and `finish_ids`' re-check all read.
    fn intern_labels(&self, labels: &[String]) -> Option<Vec<u32>> {
        labels.iter().map(|l| self.interner.get_id(l)).collect()
    }

    /// Re-check each posted candidate against the snapshot at `ts`: the
    /// visible version must carry every pattern label and a `key` value
    /// that satisfies `pred`. Reads the raw version — interned label ids
    /// and the one-slot `CompactProps::get` — never `node_at`, which
    /// materializes the full property map per candidate (the profile-hot
    /// `to_map` path, ADR-0033; LABS-853). The raw-version member of the
    /// recheck family (beside [`RecheckWalk`]'s materializing form): the
    /// declared seek family and the lazy
    /// postings' single-key serve (ADR-0614) re-check through this same
    /// filter, so both answer exactly the same candidates.
    pub(in crate::graph) fn finish_ids(
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
        let (Some(lids), Some(kid)) = (self.intern_labels(labels), self.interner.get_id(key))
        else {
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
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Label scan (or a full scan when the pattern has no label) filtered by
    /// equality. Used when no `RANGE` index serves the properties and the
    /// lazy equality postings declined (ADR-0614: a label-less pattern, or
    /// a value whose identity is narrower than its `=` class). With
    /// `cap`, the scan stops at the first `cap + 1` matches (see
    /// [`Graph::lookup_nodes_upto_at`]).
    fn scan_match(
        &self,
        labels: &[String],
        props: &BTreeMap<String, Value>,
        ts: u64,
        cap: Option<usize>,
    ) -> Vec<NodeId> {
        // One probe per examined node: counted, so a scan that finds few
        // matches in a large bucket reports the reads it performed —
        // the shared walk body ([`Self::recheck_walk`]).
        let spec = RecheckSpec {
            labels,
            props,
            ts,
            cap,
        };
        let mut walk = self.recheck_walk(&spec);
        // The id stream, one shape for both patterns: every node for a
        // label-less pattern, else the shortest interned label bucket —
        // its ids copied under one short guard and visited outside it,
        // so the label-index guard is never held across a node read (the
        // lock order every writer takes is nodes, then label index).
        // Fail-closed: no bucket when a pattern label was never interned,
        // so the conjunction matches nothing
        // ([`Self::smallest_interned_bucket`]'s rule).
        let ids = if labels.is_empty() {
            self.all_node_ids_at(ts)
        } else {
            self.smallest_interned_bucket(labels)
                .map_or_else(Vec::new, |lid| self.label_index_copy(lid, usize::MAX).0)
        };
        for id in ids {
            if walk.visit(id).is_break() {
                break;
            }
        }
        walk.finish()
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
