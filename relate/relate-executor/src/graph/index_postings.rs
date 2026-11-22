//! The lazy equality/string postings engine (ADR-0063, ADR-0185): the pair
//! build-and-drain, the per-value postings, and the dirty-mark read seam.
//! A sibling of `index.rs` (the `degree_stats` family precedent), so the
//! parent keeps the index state, the apply seams, and the rebuild
//! orchestration.

use super::declared::DrainState;
use super::index::PropPairIndex;
use super::index_lookup::RecheckSpec;
use super::*;

use crate::group_key::identity_narrower_than_eq;

/// How a keyed equality lookup is answered (ADR-0614): a declared
/// RANGE pair (ordered postings, no lazy build) always wins; else the
/// lazy equality postings when the values pass the decline predicate;
/// else the scan. The one three-way decision the id seek, the
/// cardinality, the keyed-lookup arm, and the overlay's scratch-mode
/// flag all read — precedence and the decline in one place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::graph) enum EqualityRoute {
    /// A declared RANGE pair covers the sought key(s).
    Declared,
    /// No declared pair; the values pass the decline predicate.
    Postings,
    /// Neither: the caller scans (ADR-0614's decline).
    Scan,
}

/// One walked version's recency — the live/superseded distinction the
/// pair walk hands its consumer alongside the value: named, not a
/// positional `bool` (the `Extremum` precedent).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PairVersion {
    /// The newest version with no `xmax` — the current committed value.
    Live,
    /// A version an older statement snapshot may still see.
    Superseded,
}

impl PairVersion {
    /// The classification at one walk position, against
    /// [`Chain::live_index`]'s position — stated once for the pair walk.
    fn at(idx: usize, live_at: Option<usize>) -> Self {
        if Some(idx) == live_at {
            PairVersion::Live
        } else {
            PairVersion::Superseded
        }
    }
}

impl Graph {
    /// `(label, key)` strings → interned pair key, honouring the `*`
    /// any-label bucket. `None` when either side was never interned.
    pub(super) fn string_pair_id(&self, label: &str, key: &str) -> Option<(u32, u32)> {
        let lid = if label == "*" {
            self.indexes.star_label_id
        } else {
            self.interner.get_id(label)?
        };
        let kid = self.interner.get_id(key)?;
        Some((lid, kid))
    }

    /// Build the pair from `candidates` on first use, drain the ids written
    /// since it was last current, and hand it to `read` while its write
    /// guard is held (ADR-0063, ADR-0185). The one committed-side pair
    /// engine for nodes and relationships: the entity seam is the three
    /// closures — `candidates` lists the pair's ids (the label index, or
    /// the whole `rels` map), `feed` posts one candidate's every version
    /// and, where the pair kind keeps a trust mark, reconciles it from the
    /// live version, and `read` consumes the pair under its guard. The two
    /// adapters are `with_prop_pair` (nodes: the label-plus-property walk,
    /// the live-reconciled mark) and `with_rel_prop_pair` (relationships:
    /// the type-plus-payload walk, post-only — its pairs carry no mark).
    ///
    /// Lock order: the pair's write guard is held across the build and the
    /// drain, which take `label_index`, `nodes` or `rels` read guards.
    /// `index_add`/`rel_index_add` take the same guard in the reverse
    /// order: they dirty-mark the pair after the installed version's
    /// `nodes`/`rels` guard is released. Holding the pair guard is what
    /// keeps a concurrent commit's entity out of the gap between the
    /// build's candidate copy and `built = true`: the write side waits,
    /// then finds the pair built and dirty-marks it. The inversion cannot
    /// deadlock while both sides take `nodes`/`rels` as readers: dashmap
    /// 6's shard lock blocks a reader only while a writer holds the shard,
    /// not while one waits, and no `nodes`/`rels` writer waits for a pair
    /// guard while it holds its shard. The `relate` tests
    /// `uniqueness_probes_in_a_transaction_complete_beside_concurrent_commits`
    /// and its relationship twin drive both sides at once.
    pub(super) fn with_pair<R>(
        &self,
        pairs: &CoreMap<(u32, u32), PropPairIndex>,
        pair: (u32, u32),
        candidates: impl Fn() -> Vec<i64>,
        // The pair kind's one feed: post one candidate's every version,
        // and — where the pair keeps a trust mark — reconcile it from the
        // live version. The node pairs do; the relationship pairs post
        // alone.
        feed: impl Fn(i64, &mut PropPairIndex),
        read: impl FnOnce(&PropPairIndex) -> R,
    ) -> R {
        let mut st = pairs.entry(pair).or_default();
        if !st.built {
            for id in candidates() {
                feed(id, &mut st);
            }
            st.built = true;
        }
        if !st.dirty.is_empty() {
            let ids = std::mem::take(&mut st.dirty);
            for id in ids {
                // The feed owns the remove-then-post order (the node
                // pairs' is `post_pair_node`'s): a live version that
                // carries no value for the pair leaves whatever a stale
                // version marked cleared, not trusted.
                feed(id, &mut st);
            }
        }
        read(&st)
    }

    /// Prune `id` from the built pairs of `pairs` (ADR-0063, ADR-0185): for
    /// every (class, key, value) triple a dead version carried that no
    /// remaining version carries, remove `id` from that pair's bucket. Lazy
    /// postings: only built pairs; unbuilt pairs have no stale ids to
    /// remove. The one prune for nodes and relationships; each side's
    /// `triples` closure enumerates its versions' (class, key, value key)
    /// triples.
    pub(super) fn drop_unused_pairs<V>(
        &self,
        pairs: &CoreMap<(u32, u32), PropPairIndex>,
        id: i64,
        dead: &[V],
        remaining: &[V],
        triples: impl Fn(&V, &mut dyn FnMut(u32, u32, GroupKey)),
    ) {
        let mut keep = HashSet::new();
        for v in remaining {
            triples(v, &mut |class, kid, gk| {
                keep.insert((class, kid, gk));
            });
        }
        let mut drop_set = HashSet::new();
        for v in dead {
            triples(v, &mut |class, kid, gk| {
                drop_set.insert((class, kid, gk));
            });
        }
        for (class, kid, gk) in drop_set.difference(&keep) {
            if let Some(mut st) = pairs.get_mut(&(*class, *kid)) {
                if st.built {
                    if let Some(set) = st.postings.get_mut(gk) {
                        set.remove(&id);
                    }
                }
            }
        }
    }

    /// Whether `(lid, kid)` has an equality pair an earlier seek already
    /// built — the lazy pair's own one answer for its readiness, so a
    /// reader outside this module never reaches into the pair map to ask.
    pub(super) fn prop_pair_built(&self, pair: (u32, u32)) -> bool {
        self.indexes
            .prop_pairs
            .get(&pair)
            .is_some_and(|st| st.built)
    }

    /// Build `(label_id, key_id)` from the label index on first use, drain
    /// the ids written since the pair was last current, and hand the pair to
    /// `read` while its write guard is held (ADR-0063) — the node adapter of
    /// [`Self::with_pair`], whose lock order applies here. `prop_postings`
    /// and `equality_postings_len` both read through this, so a bucket's
    /// contents and its size cannot drift.
    pub(super) fn with_prop_pair<R>(
        &self,
        lid: u32,
        kid: u32,
        read: impl FnOnce(&PropPairIndex) -> R,
    ) -> R {
        self.with_pair(
            &self.indexes.prop_pairs,
            (lid, kid),
            || self.pair_candidate_ids(lid),
            // The node pairs' feed is the declared pairs' one protocol
            // (`post_pair_node`): post every version, and on THE live
            // version restate `unposted` from it alone — a superseded NaN
            // must not poison the pair, the live NaN still declines, and a
            // live version carrying no value clears whatever a stale one
            // marked.
            |id, st| self.post_pair_node(st, id, lid, kid),
            read,
        )
    }

    /// Postings for `(label_id, key_id)` at value `gk`, built and drained
    /// through [`Self::with_prop_pair`]. A superset (ADR-0063: every version
    /// of every candidate), so callers re-verify every id; empty when no
    /// indexed version carries `gk`.
    pub(super) fn prop_postings(&self, lid: u32, kid: u32, gk: GroupKey) -> HashSet<NodeId> {
        self.with_prop_pair(lid, kid, |st| {
            st.postings.get(&gk).cloned().unwrap_or_default()
        })
    }

    /// The guard preamble `equality_postings` and `equality_postings_len`
    /// share: the interned `(label, key)` pair plus `value`'s
    /// property-index key. `None` when the label or key was never interned
    /// or `value` contains a NaN.
    fn equality_pair_key(
        &self,
        label: &str,
        key: &str,
        value: &Value,
    ) -> Option<(u32, u32, GroupKey)> {
        let gk = GroupKey::for_property_index(value)?;
        let (lid, kid) = self.string_pair_id(label, key)?;
        Some((lid, kid, gk))
    }

    /// Equality postings for `(label, key) = value` from the lazily built
    /// pair (ADR-0063 superset: every version of every candidate), for the
    /// constraint uniqueness probe (ADR-0172 §3). Unlike `lookup_nodes_at`
    /// it does not require a range index on the pair: a node constraint's
    /// backing index materializes seek postings (LABS-328), but relationship
    /// constraints have no backing-index postings at all, so this lazy pair
    /// is the one probe both entity kinds share. Empty when the label or key
    /// was never interned or `value` contains a NaN. Callers re-verify every
    /// id.
    pub(crate) fn equality_postings(&self, label: &str, key: &str, value: &Value) -> Vec<NodeId> {
        let Some((lid, kid, gk)) = self.equality_pair_key(label, key, value) else {
            return Vec::new();
        };
        self.prop_postings(lid, kid, gk).into_iter().collect()
    }

    /// The size of the `(label, key) = value` equality bucket from the same
    /// lazily built pair `equality_postings` reads, counted in place
    /// (ADR-0184). Empty when the label or key was never interned or
    /// `value` contains a NaN.
    pub(crate) fn equality_postings_len(&self, label: &str, key: &str, value: &Value) -> usize {
        let Some((lid, kid, gk)) = self.equality_pair_key(label, key, value) else {
            return 0;
        };
        self.with_prop_pair(lid, kid, |st| st.postings.get(&gk).map_or(0, HashSet::len))
    }

    /// Whether the lazy equality postings can answer a keyed equality
    /// lookup's values — a pattern map's or an `IN` list's (ADR-0614):
    /// the labels are non-empty (the pair engine keys on interned
    /// labels; the label-less shape scans) and no value's identity is
    /// narrower than its `=` class — a named-zone datetime, bare or
    /// nested, whose DST fall-back halves are `=`-equal but key apart,
    /// so an identity bucket would miss a partner the scan finds. The
    /// route deciders ([`Self::map_equality_route`],
    /// [`Self::values_equality_route`]) read this one predicate, so
    /// every route declines the same values by construction.
    /// Emptiness is each caller's own invariant — the map form always
    /// carries a first entry, and an empty `IN` list is the serve's
    /// empty answer — so the predicate states only what it decides. A
    /// NaN has no property-index key at all: the serve answers it
    /// empty (a NaN equals nothing), never by decline.
    pub(in crate::graph) fn equality_postings_serve<'a>(
        labels: &[String],
        mut values: impl Iterator<Item = &'a Value>,
    ) -> bool {
        !labels.is_empty() && !values.any(identity_narrower_than_eq)
    }

    /// The route for a keyed equality lookup over a property map
    /// (ADR-0614). The map form's own invariant is a non-empty map —
    /// an empty one has no pair to seek and never routes to the
    /// postings. Read by the keyed-lookup arm
    /// (`Graph::node_candidates`) and the overlay's scratch-mode
    /// decision (`Graph::equality_served_by_index`).
    pub(in crate::graph) fn map_equality_route(
        &self,
        labels: &[String],
        props: &BTreeMap<String, Value>,
    ) -> EqualityRoute {
        if self.range_serves_equality(labels, props) {
            EqualityRoute::Declared
        } else if !props.is_empty() && Self::equality_postings_serve(labels, props.values()) {
            EqualityRoute::Postings
        } else {
            EqualityRoute::Scan
        }
    }

    /// The route for a single-key `=`/`IN` seek (ADR-0614). Read by the
    /// WHERE-form id seek; the cardinality's postings arm is `None`
    /// (ADR-0614 §6: the lazy bucket has no count-without-build).
    pub(in crate::graph) fn values_equality_route(
        &self,
        labels: &[String],
        key: &str,
        values: &[Value],
    ) -> EqualityRoute {
        if self.range_serves_key(labels, key) {
            EqualityRoute::Declared
        } else if Self::equality_postings_serve(labels, values.iter()) {
            EqualityRoute::Postings
        } else {
            EqualityRoute::Scan
        }
    }

    /// The interned `(label, key)` pair a postings serve streams: the
    /// first property (a `BTreeMap`, so the choice is deterministic),
    /// on the pattern label with the smallest label-index bucket — the
    /// one shared shortest-bucket choice the scan tail also reads
    /// ([`Graph::smallest_interned_bucket`]), fail-closed on an
    /// uninterned label or key alike: no node carries either, so the
    /// serve answers empty, exactly what a scan against no bucket finds.
    fn smallest_serve_pair(&self, labels: &[String], key: &str) -> Option<(u32, u32)> {
        let kid = self.interner.get_id(key)?;
        let lid = self.smallest_interned_bucket(labels)?;
        Some((lid, kid))
    }

    /// The keyed lookup `labels × props` served from the lazy equality
    /// postings (ADR-0614) — call on [`EqualityRoute::Postings`]; the
    /// route owns the decline. The identity bucket of the smallest
    /// interned `(label, key)` pair, streamed under the pair's guard
    /// (the lock order the declared arm's capped serve already uses)
    /// through the shared re-check walk, so the capped path never
    /// materializes the id set — the ADR-0063 superset contract, a
    /// stale posting costs a read, never a wrong answer. With `cap`
    /// the walk stops at the first `cap + 1` matches
    /// ([`Graph::lookup_nodes_upto_at`]'s contract). A value with no
    /// property-index key (a NaN) or an uninterned name matches
    /// nothing: the empty answer, as the scan would give.
    pub(in crate::graph) fn equality_postings_upto(
        &self,
        labels: &[String],
        props: &BTreeMap<String, Value>,
        ts: u64,
        cap: Option<usize>,
    ) -> Vec<NodeId> {
        // The map form's own invariant: a keyed lookup carries a first
        // entry to seek (the route never sends an empty map here).
        let Some((key, value)) = props.first_key_value() else {
            return Vec::new();
        };
        let Some(gk) = GroupKey::for_property_index(value) else {
            return Vec::new();
        };
        let Some((lid, kid)) = self.smallest_serve_pair(labels, key) else {
            return Vec::new();
        };
        let spec = RecheckSpec {
            labels,
            props,
            ts,
            cap,
        };
        let mut walk = self.recheck_walk(&spec);
        self.with_prop_pair(lid, kid, |st| {
            for id in st.postings.get(&gk).into_iter().flat_map(HashSet::iter) {
                if walk.visit(*id).is_break() {
                    break;
                }
            }
        });
        walk.finish()
    }

    /// The single-key `=`/`IN` serve from the lazy equality postings
    /// (ADR-0614) — call on [`EqualityRoute::Postings`]; the route owns
    /// the decline. Each value's identity bucket on the smallest
    /// interned `(label, key)` pair, unioned, then re-verified through
    /// the declared seek family's snapshot filter — so a WHERE-form
    /// runtime seek answers exactly what the declared index's equality
    /// serve would. A NaN value contributes nothing (it equals
    /// nothing); an uninterned name answers empty.
    pub(in crate::graph) fn equality_postings_values(
        &self,
        labels: &[String],
        key: &str,
        values: &[Value],
        ts: u64,
    ) -> Vec<NodeId> {
        let Some((lid, kid)) = self.smallest_serve_pair(labels, key) else {
            return Vec::new();
        };
        let mut acc: HashSet<NodeId> = HashSet::new();
        for value in values {
            if let Some(gk) = GroupKey::for_property_index(value) {
                acc.extend(self.prop_postings(lid, kid, gk));
            }
        }
        self.finish_ids(acc, labels, key, ts, |v| {
            values.iter().any(|b| v.eq_non_null(b))
        })
    }

    /// Candidate ids of a `(label, key)` pair: the label index (the
    /// canonical bucket copy, uncapped), or every node for the `*`
    /// bucket.
    pub(super) fn pair_candidate_ids(&self, lid: u32) -> Vec<NodeId> {
        if lid == self.indexes.star_label_id {
            return self.nodes.keys();
        }
        self.label_index_copy(lid, usize::MAX).0
    }

    /// The value of `kid` on every version of `id` that carries `lid` (any
    /// label for the `*` bucket), each handed to `f` with whether it is
    /// THE live version (the newest with no `xmax`, [`Chain::live`]'s
    /// rule — a shadowed open-ended version is superseded).
    /// Every pair build and drain walks versions here, so all declared
    /// and equality postings share one superset contract (ADR-0018): the
    /// seek re-checks the snapshot. The [`PairVersion`] flag is the one
    /// pass's way out for per-node trust state that must reflect the
    /// current committed value (the range index's `unposted`): a second
    /// nodes-map read to recover it would walk what this already visited.
    pub(super) fn for_each_node_pair_value(
        &self,
        id: NodeId,
        lid: u32,
        kid: u32,
        mut f: impl FnMut(&Value, PairVersion),
    ) {
        let Some(vers) = self.nodes.get(&id) else {
            return;
        };
        let star = lid == self.indexes.star_label_id;
        // THE live version is the newest open-ended one —
        // [`Chain::live_index`]'s one statement of the rule, consulted
        // by position so the walk keeps its oldest-to-newest order: the
        // point drain's per-id posting is last-writer-wins, so a value
        // posted by a superseded version must not come after the live
        // one's.
        let live_at = vers.live_index();
        for (idx, v) in vers.iter().enumerate() {
            if !star && !v.live.has_label(lid) {
                continue;
            }
            if let Some(val) = v.live.props.get(kid) {
                f(val, PairVersion::at(idx, live_at));
            }
        }
    }
}

/// The equality pair as a [`DrainState`], so the node feed runs the one
/// post/reconcile protocol (`Graph::post_pair_node`) the declared pairs
/// run. Its posting policy is the lazy superset: `remove_node` does not
/// unpost (stale versions' buckets are pruned by `drop_unused_pairs`),
/// it only drops the trust mark the reconcile restates.
impl DrainState for PropPairIndex {
    fn dirty_mut(&mut self) -> &mut HashSet<NodeId> {
        &mut self.dirty
    }
    fn insert(&mut self, id: NodeId, value: &Value) {
        prop_pair_post(self, id, GroupKey::for_property_index(value).as_ref());
    }
    fn remove_node(&mut self, id: NodeId) {
        self.unposted.clear(id);
    }
    fn reconcile_live(&mut self, id: NodeId, live: Option<&Value>) {
        let unpostable = live.is_some_and(|v| GroupKey::for_property_index(v).is_none());
        self.unposted.restate(id, unpostable);
    }
}

/// The one posting step both pair feeds take — the identity computed by
/// the caller, which the node feed also needs for the trust mark. The
/// `unposted` mark is never decided here: only the live reconciliation
/// writes it (a superseded NaN must not poison the pair).
pub(super) fn prop_pair_post(st: &mut PropPairIndex, id: NodeId, gk: Option<&GroupKey>) {
    if let Some(gk) = gk {
        st.postings.entry(gk.clone()).or_default().insert(id);
    }
}
