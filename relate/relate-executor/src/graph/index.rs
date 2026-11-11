use relate_ir::BinaryOp;

use super::adjacency::adj_remove;
use super::*;

/// Lazily materialized equality postings for one `(class, key)` pair
/// (ADR-0063): the postings are a **superset** of true matches — stale ids
/// from overwritten/deleted versions are filtered by the seek's residual
/// snapshot re-verification (ADR-0018). A pair exists only after an
/// equality seek materialized it; writes to unmaterialized pairs cost
/// nothing, writes after materialization dirty-mark the entity. A node
/// pair's class is a label id; a relationship pair's class is a type id
/// (ADR-0185); the ids share `NodeId`'s/`RelId`'s `i64` shape.
#[derive(Debug, Default)]
pub(super) struct PropPairIndex {
    /// True once the pair has been built from the label index at least once.
    pub(super) built: bool,
    /// Property value → ids that have carried it for this pair.
    pub(super) postings: HashMap<GroupKey, HashSet<NodeId>>,
    /// Ids written since `postings` was last brought current for this pair.
    pub(super) dirty: HashSet<NodeId>,
}

/// All lazily maintained index state of a [`Graph`] (ADR-0063 lazy
/// postings, ADR-0185 relationship pairs): the label index, the equality
/// and declared pair maps, the interned `*`/empty-label sentinel ids, and
/// the committed index memory charge. Bundled so `Graph`'s fields name
/// what the graph is, and the index lifecycle lives beside the index code.
#[derive(Debug)]
pub(super) struct Indexes {
    pub(super) label_index: CoreMap<u32, HashSet<NodeId>>,
    /// `(label_id, key_id)` → lazily materialized equality postings
    /// (ADR-0063 contract applied to equality seeks). A pair exists only
    /// after an equality seek built it; writes after the build dirty-mark.
    pub(super) prop_pairs: CoreMap<(u32, u32), PropPairIndex>,
    /// `(rel_type_id, key_id)` → lazily materialized equality postings, the
    /// relationship twin of `prop_pairs` (ADR-0185).
    pub(super) rel_prop_pairs: CoreMap<(u32, u32), PropPairIndex>,
    /// `(label_id, key_id)` → trigram postings for a `CREATE TEXT INDEX`.
    pub(super) gram_pairs: CoreMap<(u32, u32), GramIndex>,
    /// `(label_id, key_id)` → ordered postings for a `CREATE RANGE INDEX`.
    pub(super) range_pairs: CoreMap<(u32, u32), RangeIndex>,
    /// `(label_id, key_id)` → spatial postings for a `CREATE POINT INDEX`.
    pub(super) point_pairs: CoreMap<(u32, u32), PointIndex>,
    /// Interned id of the `*` any-label bucket used when a seek has no label.
    pub(super) star_label_id: u32,
    /// Interned id of the empty-string bucket for label-less nodes.
    pub(super) empty_label_id: u32,
    /// Committed [`crate::memory::IndexCharge`]. Rebuilt when `index_details`
    /// changes so each node write clones an `Arc` instead of rescanning the
    /// catalog.
    pub(super) charge_cache: Mutex<Arc<crate::memory::IndexCharge>>,
}

impl Indexes {
    pub(super) fn new(interner: &Interner) -> Self {
        Self {
            label_index: CoreMap::default(),
            prop_pairs: CoreMap::default(),
            rel_prop_pairs: CoreMap::default(),
            gram_pairs: CoreMap::default(),
            range_pairs: CoreMap::default(),
            point_pairs: CoreMap::default(),
            star_label_id: interner.intern("*"),
            empty_label_id: interner.intern(""),
            charge_cache: Mutex::new(Arc::new(crate::memory::IndexCharge::default())),
        }
    }

    /// Drop every lazily built structure (ADR-0063): the next seek or probe
    /// rebuilds from the live graph on demand. `rebuild_indexes` re-fills
    /// the label index and the declared pairs afterwards.
    fn clear(&self) {
        self.label_index.clear();
        self.prop_pairs.clear();
        self.rel_prop_pairs.clear();
        self.gram_pairs.clear();
        self.range_pairs.clear();
        self.point_pairs.clear();
    }
}

impl Graph {
    /// Insert `id` into the label index: the empty-label bucket when it
    /// carries no labels, then one entry per label. The one spelling the
    /// write path ([`index_add`](Self::index_add)) and the wholesale rebuild
    /// ([`rebuild_indexes`](Self::rebuild_indexes)) share.
    fn label_index_insert(&self, id: NodeId, label_ids: &[u32]) {
        // One insert policy: a label-less node lands in the empty-label
        // bucket, a labelled one in each of its label buckets.
        let lids: &[u32] = if label_ids.is_empty() {
            &[self.indexes.empty_label_id]
        } else {
            label_ids
        };
        for &lid in lids {
            self.indexes.label_index.entry(lid).or_default().insert(id);
        }
    }

    /// Add `id`'s label/equality postings, read straight from the live
    /// record — no `Node` materialization on the bulk path (ADR-0063).
    /// Same visibility rule as `node()`: the version visible at `read_ts`.
    pub(super) fn index_add(&self, id: NodeId) {
        let ts = self.read_ts();
        let Some(vers) = self.nodes.get(&id) else {
            return;
        };
        let Some(v) = vers.visible_at(ts) else {
            return;
        };
        let live = &v.live;
        self.label_index_insert(id, &live.labels);
        // Equality postings are lazy (ADR-0063 contract): dirty-mark only the
        // `(label, key)` pairs already materialized by a seek. A pair nobody
        // seeks costs nothing on the write path.
        for (lid, kid) in self.declared_dirty_pairs(live) {
            if let Some(mut st) = self.indexes.prop_pairs.get_mut(&(lid, kid)) {
                if st.built {
                    st.dirty.insert(id);
                }
            }
            self.declared_dirty(lid, kid, id);
        }
    }

    /// The `(label id, key id)` pairs a live node's declared postings cover:
    /// its labels plus the any-label bucket, crossed with every property key.
    /// The one walk `index_add` and `string_dirty_mark_node` share.
    fn declared_dirty_pairs<'a>(
        &self,
        live: &'a LiveNode,
    ) -> impl Iterator<Item = (u32, u32)> + 'a {
        let star = self.indexes.star_label_id;
        live.labels
            .iter()
            .copied()
            .chain(std::iter::once(star))
            .flat_map(move |lid| live.props.iter().map(move |(kid, _)| (lid, kid)))
    }

    /// Dirty-mark the built string pairs of `id`'s installed live version —
    /// the id-taking shape the other lazy-posting helpers use; the caller has
    /// just installed the version (apply-before-dirty ordering).
    pub(super) fn string_dirty_mark_node(&self, id: NodeId) {
        let vers = self
            .nodes
            .get(&id)
            .expect("string dirty-mark follows an installed node version");
        let live = vers.last().live;
        for (lid, kid) in self.declared_dirty_pairs(live) {
            self.declared_dirty(lid, kid, id);
        }
    }
}

#[cfg(feature = "test-hooks")]
thread_local! {
    /// Whole-graph index rebuilds (`rebuild_indexes`) on this thread. Behind
    /// the off-by-default `test-hooks` feature, unlike the cfg(test)-only
    /// `ADJ_BUILDS` seam in `rebuild.rs`: the facade's reopen tests read it
    /// across the
    /// crate boundary to pin that a clean close's open skips the wholesale
    /// rebuild (LABS-1112) — behavior with no other observable surface.
    static INDEX_REBUILDS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Take this thread's whole-graph index-rebuild count, resetting it.
#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub fn take_index_rebuilds() -> u64 {
    INDEX_REBUILDS.with(|n| n.replace(0))
}

impl Graph {
    /// Rebuild the indexes from the live versions. Adjacency is left as it
    /// is: writes and replayed redo keep it current, and
    /// `Graph::finish_restore` builds it after a checkpoint load. The open
    /// flow's single post-replay site owns this rebuild — exactly once per
    /// open, after the graph is final (snapshot plus replayed tail).
    pub fn rebuild_indexes(&self) {
        #[cfg(feature = "test-hooks")]
        INDEX_REBUILDS.with(|n| n.set(n.get() + 1));
        // Equality and string postings are both lazy (ADR-0063), and
        // relationship equality pairs are lazy the same way (ADR-0185):
        // drop them; the next seek rebuilds from the live graph on demand.
        self.indexes.clear();
        let ts = self.commit_ts();
        for id in self.all_node_ids_at(ts) {
            if let Some(n) = self.node_at(id, ts) {
                let label_ids: Vec<u32> =
                    n.labels.iter().map(|l| self.interner.intern(l)).collect();
                self.label_index_insert(id, &label_ids);
            }
        }
        // ADR-0439: the committed constraint-key map rebuilds wholesale from
        // the live graph on open, checkpoint restore, and abort — like
        // `label_counts`, and with replay-installed duplicates carried
        // forward. Its charge is re-tallied by `recompute_used` below.
        self.rebuild_constraint_keys();
        // Equality, prefix, trigram, and point postings follow catalog
        // indexes. `rebuild_declared_indexes` refills the pairs named by
        // TEXT / RANGE / POINT; anything else stays a scan.
        self.rebuild_declared_indexes();
        // Checkpoint restore installs `index_details` without going through
        // `apply_redo`, so the charge cache is still empty until here.
        self.refresh_index_charge();
        self.recompute_used();
    }

    pub fn gc_versions(&self) {
        let horizon = self.gc_horizon();
        // ADR-0551: the checkpoint's GC pass is the one moment the store may
        // pay the O(chunk) re-encode; a chunk below the watermark stays delta.
        self.compact_adjacency_at_watermark(horizon);
        let nids: Vec<NodeId> = self.nodes.keys();
        for id in nids {
            self.gc_node_chain(id, horizon);
        }
        let rids: Vec<RelId> = self.rels.keys();
        for id in rids {
            self.gc_rel_chain(id, horizon);
        }
        self.recompute_used();
    }

    pub(super) fn gc_node_chain(&self, id: NodeId, horizon: u64) {
        let Some(mut vers) = self.nodes.get_mut(&id) else {
            return;
        };
        if !vers.any_dead(horizon) {
            return;
        }
        let dead: Vec<Node> = vers
            .iter()
            .filter(|v| v.dead_at(horizon))
            .map(|v| v.live.to_node(id, &self.interner))
            .collect();
        let empty = !vers.collect(horizon);
        let remaining: Vec<Node> = if empty {
            Vec::new()
        } else {
            vers.iter()
                .map(|v| v.live.to_node(id, &self.interner))
                .collect()
        };
        drop(vers);
        if empty {
            self.nodes.remove(&id);
        }
        self.index_drop_unused(id, &dead, &remaining);
    }

    pub(super) fn gc_rel_chain(&self, id: RelId, horizon: u64) {
        // Pairs are lazy (ADR-0185): when none was ever built there is
        // nothing to prune, so the dead/remaining collection — two
        // `LiveRel` clones per collected version — is skipped. Checked
        // before the `rels` guard is taken: the pair guard is never taken
        // while a `rels` guard is held. A pair built racing this check
        // keeps the collected ids as stale postings: a superset leak,
        // never a correctness one.
        let prune = !self.indexes.rel_prop_pairs.is_empty();
        let Some(mut vers) = self.rels.get_mut(&id) else {
            return;
        };
        if !vers.any_dead(horizon) {
            return;
        }
        // LABS-1116: the chain's endpoints and type, needed to prune its
        // adjacency hops when the chain empties. The `any(dead_at)` guard
        // above proves the chain non-empty, and every version of one rel
        // id shares its endpoints (`cow` clones the previous version's
        // endpoints; no operation moves a rel) — and `Chain::last` fails
        // loud on an empty chain, a broken invariant that must not
        // silently skip the prune (F-1-11).
        let newest = vers.last();
        let (start, end, type_id) = (newest.live.start, newest.live.end, newest.live.type_id);
        let dead: Vec<LiveRel> = if prune {
            vers.iter()
                .filter(|v| v.dead_at(horizon))
                .map(|v| v.live.clone())
                .collect()
        } else {
            Vec::new()
        };
        let empty = !vers.collect(horizon);
        let remaining: Vec<LiveRel> = if prune && !empty {
            vers.iter().map(|v| v.live.clone()).collect()
        } else {
            Vec::new()
        };
        drop(vers);
        if empty {
            self.rels.remove(&id);
            // LABS-1116: prune the orphaned hops — without this, a chain
            // the GC collected leaves its hops in adjacency forever, and
            // every later scan pays a `rels` map miss per ghost hop (the
            // per-hop visibility probe stays the authority; this only
            // stops offering it a rel it cannot resolve).
            adj_remove(&self.out_adj, &self.sealed_out, start, type_id, id);
            adj_remove(&self.in_adj, &self.sealed_in, end, type_id, id);
        }
        // Prune after the `rels` guard is released — the pair guard is never
        // taken while a `rels` guard is held. When `prune` is false both
        // slices are empty and this is a no-op.
        self.rel_index_drop_unused(id, &dead, &remaining);
    }

    pub(super) fn index_drop_unused(&self, id: NodeId, dead: &[Node], remaining: &[Node]) {
        let mut keep_labels = HashSet::new();
        let mut keep_props = HashSet::new();
        for n in remaining {
            collect_index_keys(n, &mut keep_labels, &mut keep_props);
        }
        let mut drop_labels = HashSet::new();
        let mut drop_props = HashSet::new();
        for n in dead {
            collect_index_keys(n, &mut drop_labels, &mut drop_props);
        }
        for l in drop_labels {
            if keep_labels.contains(&l) {
                continue;
            }
            let lid = if l.is_empty() {
                self.indexes.empty_label_id
            } else {
                match self.interner.get_id(&l) {
                    Some(lid) => lid,
                    None => continue,
                }
            };
            if let Some(mut set) = self.indexes.label_index.get_mut(&lid) {
                set.remove(&id);
            }
        }
        // The pair-side prune shares `drop_unused_pairs` with the
        // relationship twin; the string (label, key) triples resolve to
        // interned ids here through `string_pair_id`, the one pair-id
        // resolver (`*` bucket included).
        let drop_props: Vec<(String, String, GroupKey)> = drop_props.into_iter().collect();
        let keep_props: Vec<(String, String, GroupKey)> = keep_props.into_iter().collect();
        self.drop_unused_pairs(
            &self.indexes.prop_pairs,
            id,
            &drop_props,
            &keep_props,
            |(label, key, gk), f| {
                let Some((lid, kid)) = self.string_pair_id(label, key) else {
                    return;
                };
                f(lid, kid, gk.clone());
            },
        );
    }
}

fn collect_index_keys(
    n: &Node,
    labels: &mut HashSet<String>,
    props: &mut HashSet<(String, String, GroupKey)>,
) {
    if n.labels.is_empty() {
        labels.insert(String::new());
    }
    labels.extend(n.labels.iter().cloned());
    for (k, v) in &*n.properties {
        if let Some(gk) = GroupKey::for_property_index(v) {
            for l in &*n.labels {
                props.insert((l.clone(), k.clone(), gk.clone()));
            }
            props.insert(("*".into(), k.clone(), gk));
        }
    }
}

/// How many of a label's nodes a sampled `CONTAINS` estimate reads before
/// it extrapolates (LABS-643): enough that a rare needle still surfaces in
/// the sample of a million-row label, few enough that the plan-time read
/// stays a bounded microsecond cost, never a scan. `pub(crate)` for the
/// pattern-stats tests, which size labels around it.
pub(crate) const CONTAINS_SAMPLE_BOUND: usize = 1024;

/// The floor on a sampled estimate, as a divisor of the label's rows
/// (LABS-643): an unindexed `CONTAINS` filter still pays the whole-label
/// scan, so its estimate never drops below 1% of the label. Low enough
/// that the survivor count still beats a competing label scan (GBL SF0.1
/// q19: a 160-row floor on the 16k Tag label still seeds ahead of
/// Place's 1,460 whole-label rows), high enough that it never undercuts
/// an index-served seed (GBL q2/q16: the 1%-of-Post floor loses to the
/// RANGE-equality person/place seed, where anchoring on a whole-post
/// content scan is the 200 ms plan).
const CONTAINS_SAMPLE_FLOOR_DIVISOR: u64 = 100;

/// How many needle estimates one committed epoch's
/// [`ContainsMemo`](struct@ContainsMemo) keeps. The memo is keyed by a
/// query-supplied needle, so it is a query-controlled map — the memory
/// discipline the charged stats maps (`pair_counts`, `label_counts`)
/// solve with `used_bytes`, this one solves with a hard cap: past it the
/// memo clears and the next caller resamples.
const CONTAINS_MEMO_CAP: usize = 64;

/// One interned `(label, key)` pair's needle estimates: the needle texts
/// and their sampled counts, few enough per pair that a borrowed linear
/// scan beats hashing an owned triple.
type NeedleEstimates = Vec<(Box<str>, ContainsCount)>;

/// The committed epoch's memoized sampled `CONTAINS` estimates
/// (LABS-643): the executor's per-row start-selection estimate re-asks
/// one `(label, property, needle)` question for every row of a clause,
/// and the sample scan must not ride that loop. Needles nest per interned
/// `(label, key)` pair, not one flat `(u32, u32, Box<str>)` key, so the
/// hot lookup borrows the caller's needle text without allocating an
/// owned key per row.
#[derive(Default, Debug)]
pub(super) struct ContainsMemo {
    /// The `commit_ts` the entries sampled: a commit moves the epoch and
    /// the next reader resets the map, so no entry can outlive the
    /// committed data it sampled.
    ts: u64,
    /// `(label_id, key_id)` → the epoch's needle estimates.
    entries: HashMap<(u32, u32), NeedleEstimates>,
}

impl Graph {
    /// Nodes stored under `label`. `0` when the label is absent.
    /// The label index can hold stale ids, so this is an upper bound used
    /// only to compare pattern costs — the same label-index estimate as
    /// [`Self::label_count`], which owns the one implementation, so this
    /// pattern-stats spelling delegates to it.
    pub fn pattern_label_rows(&self, label: &str) -> u64 {
        self.label_count(label)
    }

    /// Equality hits for a RANGE index on `(label, property)`. `None` when
    /// that index does not exist.
    pub fn pattern_equality_rows(&self, label: &str, property: &str, value: &Value) -> Option<u64> {
        let lid = self.interner.get_id(label)?;
        let kid = self.interner.get_id(property)?;
        self.pattern_equality_rows_by_id(lid, kid, value)
    }

    /// [`Graph::pattern_equality_rows`] for already-resolved token ids.
    pub fn pattern_equality_rows_by_id(&self, lid: u32, kid: u32, value: &Value) -> Option<u64> {
        let ids = self.range_equality_ids(lid, kid, value)?;
        Some(ids.len() as u64)
    }

    /// Range-window hits for one comparison predicate on a RANGE index
    /// over `(label, property)` (ADR-0486): the posting-length count the
    /// one [`Graph::range_seek_cardinality`] answers — the same count the
    /// executor's seek cardinality reads, so the ladder's price and the
    /// runtime seek cannot drift, and the decline conditions are exactly
    /// the seek's (no index on the pair, no order key on the bound, or
    /// unbounded temporal postings). `op` is one of the four comparison
    /// ops — the planner's recognizer gates the domain, `=` is
    /// [`Graph::pattern_equality_rows`]'s. The count is an upper bound,
    /// not snapshot-filtered. `None` is not zero hits.
    pub fn pattern_range_rows(
        &self,
        label: &str,
        property: &str,
        op: BinaryOp,
        bound: &Value,
    ) -> Option<u64> {
        // The cheap decline first, the `pattern_equality_rows` way: the
        // pair's index existence answers "cannot serve" before the
        // seek's owned forms are built — the ladder prices every
        // comparison conjunct once per row, and the common no-index
        // case must pay no label String and no bound clone for its
        // `None`. This is `indexed_labels`' own pair predicate
        // (interner ids, `range_pairs` membership) spelled at the
        // front; the bound's `order_key` gate stays the count's, where
        // the built seek already carries it.
        let lid = self.interner.get_id(label)?;
        let kid = self.interner.get_id(property)?;
        if !self.indexes.range_pairs.contains_key(&(lid, kid)) {
            return None;
        }
        let seek = RangeSeek::Cmp {
            op,
            bound: bound.clone(),
        };
        self.range_seek_cardinality(&[label.to_string()], property, &seek)
            .map(|count| count as u64)
    }

    /// `CONTAINS` candidates for the pattern-stats ladder (ADR-0218):
    /// the exact count a TEXT index on `(label, property)` answers, and —
    /// LABS-643 — without that index the bounded sampled estimate
    /// carrying its whole-label scan rows, never `None`. The planner's
    /// CONTAINS level used to be measurable only through a TEXT index, so
    /// a RANGE-only store fell back to whole-label rows (GBL SF0.1 q19:
    /// Tag priced at its 16k label rows lost the seed to Place's 1.4k,
    /// and the walk rode 342k neighbor hops for a query whose needle
    /// selects ~35 tags). The sampled form's `scan` field is the one the
    /// compound start estimate (ADR-0474) floors the seed's walk with.
    pub fn pattern_contains_rows(
        &self,
        label: &str,
        property: &str,
        needle: &str,
    ) -> ContainsCount {
        // One epoch read threaded through both provenance paths (the TEXT
        // lookup and the sample): the two could observe different commits
        // if each took its own `commit_ts`.
        let ts = self.commit_ts();
        if let Some(ids) = self.lookup_text_contains(&[label.to_string()], property, needle, ts) {
            return ContainsCount::TextIndex(ids.len() as u64);
        }
        self.sampled_contains_rows(label, property, needle, ts)
    }

    /// The LABS-643 sampled estimate, memoized per committed epoch: the
    /// first caller of an epoch scans, every later one reads the memo —
    /// the executor's per-row start-selection estimate re-asks one
    /// `(label, property, needle)` question per row and must not rescan
    /// per row. `ts` is the epoch the caller's provenance paths already
    /// read ([`Self::pattern_contains_rows`]).
    fn sampled_contains_rows(
        &self,
        label: &str,
        property: &str,
        needle: &str,
        ts: u64,
    ) -> ContainsCount {
        let pair = match (self.interner.get_id(label), self.interner.get_id(property)) {
            (Some(lid), Some(kid)) => (lid, kid),
            // A key the store never interned can match nothing, but the
            // label's bucket is still the scan a seed there would pay; a
            // label never interned has no bucket at all. Both stay
            // measurable zeros.
            (Some(lid), None) => {
                let scan = self
                    .indexes
                    .label_index
                    .get(&lid)
                    .map_or(0, |bucket| bucket.len() as u64);
                return ContainsCount::SampleScan { rows: 0, scan };
            }
            _ => return ContainsCount::SampleScan { rows: 0, scan: 0 },
        };
        if let Some(count) = self.contains_memo_hit(ts, pair, needle) {
            return count;
        }
        let count = self.scan_contains_sample(pair, needle, ts);
        self.contains_memo_store(ts, pair, needle, count);
        count
    }

    /// One bounded sample scan: the first `CONTAINS_SAMPLE_BOUND` ids of
    /// the label's index bucket, each probed for its live string at `ts`.
    /// The bucket's ids are copied under one short `label_index` guard
    /// that is dropped before the first `nodes` guard — the lock order of
    /// [`label_probe`](super::label_probe)'s copies, never reversing
    /// [`Graph::index_add`]'s `nodes` -> `label_index` order. A stale id
    /// (deleted since, bucketed until GC) counts as a scanned non-match,
    /// the same upper-bound stance [`Graph::label_count`] takes, so the
    /// extrapolation stays consistent with the label-row level above it.
    /// The bucket's whole row count rides along as the count's `scan`:
    /// the unseekable filter pays that scan before any match, the cost
    /// the compound start estimate (ADR-0474) floors the seed's walk
    /// with.
    fn scan_contains_sample(&self, pair: (u32, u32), needle: &str, ts: u64) -> ContainsCount {
        let (lid, kid) = pair;
        // (sample, bucket rows): the copy takes at most the bound, and the
        // bucket's whole size is the extrapolation's denominator base.
        let (sample, total): (Vec<NodeId>, u64) = match self.indexes.label_index.get(&lid) {
            Some(bucket) => (
                bucket.iter().take(CONTAINS_SAMPLE_BOUND).copied().collect(),
                bucket.len() as u64,
            ),
            None => return ContainsCount::SampleScan { rows: 0, scan: 0 },
        };
        let scanned = sample.len() as u64;
        if scanned == 0 {
            return ContainsCount::SampleScan {
                rows: 0,
                scan: total,
            };
        }
        let matches = sample
            .iter()
            .filter(|id| {
                // The raw version's one-slot `CompactProps::get`, not
                // `node_at`: the sample must not materialize a property
                // map per node (the profile-hot `to_map` path, ADR-0033),
                // and the visible version's probe stays inside the one
                // `nodes` guard it borrows.
                self.nodes.get(id).is_some_and(|vers| {
                    visible_version(&vers, ts).is_some_and(|ver| {
                        ver.live.props.get(kid).is_some_and(|value| {
                            value.as_string().is_some_and(|s| s.contains(needle))
                        })
                    })
                })
            })
            .count() as u64;
        // The count: exact when the whole bucket was scanned (`scanned ==
        // total` collapses the formula to `matches`), extrapolated beyond
        // it. The u128 product keeps a full-bucket match count from
        // overflowing before the divide. The floor keeps the unseekable
        // filter from pricing itself below an index-served seed
        // (`CONTAINS_SAMPLE_FLOOR_DIVISOR` owns the one rationale).
        let counted =
            ((matches as u128 * total as u128) / u128::from(scanned)).min(u64::MAX as u128) as u64;
        ContainsCount::SampleScan {
            rows: counted.max(total / CONTAINS_SAMPLE_FLOOR_DIVISOR),
            scan: total,
        }
    }

    /// The memo read of the current epoch: `None` on a miss or when the
    /// epoch moved (the caller rescans and stores under the new epoch).
    fn contains_memo_hit(&self, ts: u64, pair: (u32, u32), needle: &str) -> Option<ContainsCount> {
        let memo = self.contains_memo.lock().unwrap();
        if memo.ts != ts {
            return None;
        }
        memo.entries
            .get(&pair)?
            .iter()
            .find(|(text, _)| text.as_ref() == needle)
            .map(|(_, count)| *count)
    }

    /// The memo write: a moved epoch resets first, and a map at the cap
    /// clears instead of growing (the query-controlled-map bound, see
    /// [`ContainsMemo`](struct@ContainsMemo)). Concurrent samplers of one
    /// needle update the slot in place rather than stacking duplicates.
    fn contains_memo_store(&self, ts: u64, pair: (u32, u32), needle: &str, count: ContainsCount) {
        let mut memo = self.contains_memo.lock().unwrap();
        if memo.ts != ts {
            memo.ts = ts;
            memo.entries.clear();
        }
        if memo.entries.values().map(Vec::len).sum::<usize>() >= CONTAINS_MEMO_CAP {
            memo.entries.clear();
        }
        let bucket = memo.entries.entry(pair).or_default();
        match bucket.iter_mut().find(|(text, _)| text.as_ref() == needle) {
            Some(slot) => slot.1 = count,
            None => bucket.push((Box::from(needle), count)),
        }
    }

    /// Record a DDL-named graph catalog name in the namespace its entity
    /// kind selects (ADR-0363 decision 5): `names` are node labels when
    /// `relationship` is false, relationship types when true. This is the
    /// committed twin of the write overlay's scratch facts: an index or
    /// constraint DDL records its subject through the same catalog sets, so
    /// a name a transaction mentions stays known after COMMIT publishes it.
    /// Monotone like the data-write feeds: a later drop never removes a name.
    pub(crate) fn record_schema_names(
        &self,
        relationship: bool,
        names: impl IntoIterator<Item = String>,
    ) {
        let mut catalog = self.catalog.lock().unwrap();
        for name in names {
            catalog.record_schema_name(relationship, name);
        }
    }

    /// Whether the store catalog knows `label` — the same label set
    /// `Graph::create_node` / `Graph::add_labels` record on first sight and
    /// the restore paths (checkpoint rows, WAL redo) record on reload. The
    /// catalog never removes a label, so a miss proves the label has never
    /// existed in this store (no false positive for the plan-time warning);
    /// a hit can outlive the last node carrying it.
    pub(crate) fn has_label(&self, label: &str) -> bool {
        self.catalog.lock().unwrap().labels.contains(label)
    }

    /// Whether the store catalog knows relationship type `rel_type` — the
    /// one rel-type set `Graph::create_rel` / the restore paths record.
    /// The same monotone contract as [`Graph::has_label`]: a miss proves
    /// the type has never existed, a hit may outlive the last relationship.
    pub(crate) fn has_rel_type(&self, rel_type: &str) -> bool {
        self.catalog.lock().unwrap().rel_types.contains(rel_type)
    }

    /// Whether property key `key` has ever been interned by this store —
    /// a **superset** of the keys `db.propertyKeys` reports.
    ///
    /// Every stored property key is interned: `CompactProps::from_parts`
    /// interns each key a CREATE/SET/MERGE writes, and the checkpoint
    /// restore and WAL redo paths build their properties through the same
    /// interner. A key the store has never held is therefore absent, and a
    /// miss is a true miss. The converse is not true — labels, relationship
    /// types and the internal relationship `ts` key share the table, and a
    /// key removed from every entity stays interned — so a hit can only
    /// suppress a warning, never invent one. That one-sided guarantee is
    /// what plan-time unknown-key diagnostics need: never warn about a key
    /// the store holds, at the cost of a false negative for a stale name.
    pub(crate) fn has_property_key(&self, key: &str) -> bool {
        self.interner.get_id(key).is_some()
    }
}

#[cfg(test)]
mod tests;
