//! LABS-851 (the structure round): the graph's exact count state —
//! the one bundle behind the Graph's `count_stats` mutex (the
//! `indexes: Indexes` precedent) — and its whole policy family: the
//! delta vocabulary, the bundle type, the counting operator's probes,
//! the bump/rebuild seams every mutation path rides, the public
//! readers, and the windowed consistency read (ADR-0387's Decision 1
//! put the count family in one module; the degree histograms stay in
//! `degree_stats.rs` — estimate state the counting operator never
//! reads, charged and rebuilt on its own seams).

use std::collections::{HashMap, HashSet};

use relate_core::NodeId;

use super::shard;
use super::{Graph, LabelCache, RelRow};

/// One count-map mutation's direction: every caller
/// mutates one entry at a time, so the delta is ±1 by construction — the
/// open-ended `i64` delta let an out-of-contract arm hide (a negative
/// delta on a vacant entry returned 0 as a no-op); the enum makes the
/// direction the only contract, and each arm is total on its own side.
#[derive(Clone, Copy)]
pub(super) enum CountDelta {
    Add,
    Remove,
}

/// One distinct label set (its label names in label order) with the
/// number of live nodes carrying it — the unit the windowed label-set read
/// hands the grouped-aggregate answer.
pub(crate) type LabelSetCount = (std::sync::Arc<Vec<String>>, u64);

impl CountDelta {
    /// The one drain-on-zero state machine every exact count map rides
    /// (the apply's `count_entry` and the admission estimate's overlay
    /// fold): the entry's count after the paired ±1, and the charge it
    /// carries — `entry_bytes` when an Add creates the entry (count 0 to
    /// 1), the refund when a Remove drains it back to zero, 0 otherwise.
    /// A Remove at zero has no paired Add: `None`, which the apply treats
    /// as a broken store and the estimate as nothing to price. An Add past
    /// u64 is a broken store here, loudly.
    fn step(self, count: u64, entry_bytes: u64) -> Option<(u64, i64)> {
        match self {
            CountDelta::Add => {
                let next = count
                    .checked_add(1)
                    .expect("count overflow: an Add past u64 is a broken store");
                Some((next, if count == 0 { entry_bytes as i64 } else { 0 }))
            }
            CountDelta::Remove => match count {
                0 => None,
                1 => Some((0, -(entry_bytes as i64))),
                _ => Some((count - 1, 0)),
            },
        }
    }
}

/// One label mutation as the count bundle sees it (LABS-370, ADR-0589):
/// the labels that joined or left, the direction, and the node's whole
/// label set on either side of the edit.
pub(super) struct LabelMutation<'a> {
    /// The node's label set before the mutation.
    pub(super) before: &'a [u32],
    /// The node's label set once the edit is installed.
    pub(super) after: &'a [u32],
    /// The labels that joined (Add) or left (Remove).
    pub(super) labels: &'a [u32],
    pub(super) delta: CountDelta,
}

/// LABS-851 (the structure round): the counting operator's count state —
/// one bundle behind the Graph's one `count_stats` mutex (the
/// `indexes: Indexes` precedent), so the windowed-consistency invariant
/// is structural: the count-only reader holds the single guard across
/// its exactness check and its reads, every bump takes the same guard,
/// and a future arm's state (the deferred (label, type) side counts)
/// lands inside the bundle instead of as another sibling lock. The
/// degree histograms stay beside it, not inside it — estimate state the
/// counting operator never reads, charged and rebuilt on its own seams.
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct CountStats {
    /// The bundle's rel side — the two maps one rel fold fills, grouped so
    /// a rebuild shard can carry exactly them and nothing else.
    pub(super) rel: RelCounts,
    /// LABS-370 (v1 follow-up): the exact per-label live node counts —
    /// the counting operator's single-node answer. Unlike the label
    /// index (an upper bound that keeps deleted ids until checkpoint
    /// GC), this map is bumped at every label-set mutation seam and
    /// drained on zero, charged to `used_bytes` per entry like the pair
    /// counts.
    pub(super) label_counts: HashMap<u32, u64>,
    /// ADR-0589: the exact live node count per distinct label SET, keyed
    /// by the node's label ids in label order (the order `labels(n)`
    /// reports), the empty set included. The grouped label aggregates'
    /// answer: unlike `label_counts` (a multi-label node counts once per
    /// label) each live node counts in exactly one entry, so the values
    /// sum to `node_total`. Bumped at the node-liveness seam and moved
    /// between entries by the label-mutation seam, drained on zero, and
    /// charged per entry like the other query-controlled maps.
    pub(super) label_set_counts: HashMap<Vec<u32>, u64>,
    /// LABS-851: the exact total live node count — the counting
    /// operator's unlabeled single-node answer (`MATCH (n) RETURN
    /// count(n)`), which the per-label map cannot answer (a label-less
    /// node counts nowhere in it). Bumped at the same create/delete
    /// seams as the label counts; label mutations never touch it (the
    /// node stays live). A single counter, not a query-controlled map —
    /// it rides no `used_bytes` charge (the `rel_type_counts`
    /// precedent).
    pub(super) node_total: u64,
}

/// The count bundle's rel side: the per-type live counts and the exact
/// (start-label, rel-type, end-label) pair counts, which
/// [`Graph::fold_rel_stats`] fills together and which the wholesale
/// rebuild's shards carry as their whole result. A named pair rather than
/// a partly-populated [`CountStats`], so the caller's "merge exactly these
/// two maps" is the type instead of a comment — and a field later added to
/// the node side cannot be silently neither filled by a shard nor merged.
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct RelCounts {
    /// Live relationships per interned relationship type, maintained
    /// beside install and delete and rebuilt with adjacency after abort
    /// or restore. Planning uses it as the numerator of average degree;
    /// the counting operator's `RelType`/`AllRels` arms read it. It
    /// rides no `used_bytes` charge — bounded by the type vocabulary,
    /// not the data (nothing a query can grow without bound).
    pub(super) rel_type_counts: HashMap<u32, u64>,
    /// LABS-370: exact (start-label, rel-type, end-label) rel counts —
    /// the directional pair the counting operator answers from, and the
    /// estimate the ADR-0218 ladder and the LABS-348 gate can consult.
    /// Charged to `used_bytes` on entry creation (the LABS-351 security
    /// discipline: an uncharged query-controlled map is a memory-limit
    /// bypass).
    pub(super) pair_counts: HashMap<(u32, u32, u32), u64>,
}

impl RelCounts {
    /// Merge a wholesale-rebuild shard's counts into this one by
    /// summing. Owned by the type whose two maps it merges, so a third
    /// map added to the rel side lands in this one method rather than
    /// as another caller loop — the same reason the type exists at
    /// all. The sum itself is [`sum_counts`], the module's loud-sum
    /// contract in one spelling.
    fn merge(&mut self, part: RelCounts) {
        sum_counts(&mut self.rel_type_counts, part.rel_type_counts);
        sum_counts(&mut self.pair_counts, part.pair_counts);
    }
}

/// The count maps' one loud sum: fold `part` into `into` by checked
/// addition. The module's contract at every summing site (`count_entry`,
/// `fold_node_total`): a wrapped sum means the store is broken, and the
/// failure says so. One spelling so the third map the bundle later
/// grows is a one-line call, not a third copy of the loop.
fn sum_counts<K: std::hash::Hash + Eq>(into: &mut HashMap<K, u64>, part: HashMap<K, u64>) {
    for (key, count) in part {
        let slot = into.entry(key).or_insert(0);
        *slot = slot
            .checked_add(count)
            .expect("count overflow: a merge sum past u64 is a broken store");
    }
}

/// The count probe the counting operator answers from: the exact per-label
/// live count, the exact (start-label, type, end-label) pair count, or one
/// of the LABS-851 whole-population arms — the total live node count, the
/// per-type live rel count over unlabeled endpoints, and the total live rel
/// count over unlabeled endpoints — read under the stats locks by
/// [`Graph::windowed_stats_count`].
pub(crate) enum CountProbe {
    Label(String),
    Pair(String, String, String),
    /// The unlabeled single node (`MATCH (n) RETURN count(n)`): the total
    /// live node count, which the per-label map cannot answer — a node
    /// with no label counts nowhere in it.
    AllNodes,
    /// One rel type over unlabeled endpoints (`MATCH ()-[r:T]->()`): the
    /// per-type live count (`rel_type_counts`).
    RelType(String),
    /// [`CountProbe::RelType`] for an already-resolved token id: the one
    /// by-id read both spellings end in.
    RelTypeId(u32),
    /// Every rel type over unlabeled endpoints, directed
    /// (`MATCH ()-->() RETURN count(*)`): the sum of the per-type counts.
    /// Undirected declines at the recognizer (each rel matches twice minus
    /// self-loops, which no maintained count expresses).
    AllRels,
}

impl Graph {
    /// One pair-count entry's charge (the LABS-351 discipline, same
    /// floor-estimate style).
    pub(super) const PAIR_COUNT_ENTRY_BYTES: u64 =
        (std::mem::size_of::<(u32, u32, u32)>() + std::mem::size_of::<u64>() + 48 + 64) as u64;
    /// One exact label-count entry's charge (the LABS-351 discipline, same
    /// floor-estimate style): charged to `used_bytes` on entry creation,
    /// refunded when a delete or label removal drains the count to zero
    /// and removes the entry.
    pub(super) const LABEL_COUNT_ENTRY_BYTES: u64 =
        (std::mem::size_of::<u32>() + std::mem::size_of::<u64>() + 48 + 64) as u64;

    /// One label-set entry's fixed charge (ADR-0589): the key vector's
    /// header, the count, and the map's slot estimate; the key's
    /// elements add [`Self::label_set_entry_bytes`]'s per-id width.
    const LABEL_SET_ENTRY_BYTES: u64 =
        (std::mem::size_of::<Vec<u32>>() + std::mem::size_of::<u64>() + 48 + 64) as u64;

    /// One label-set entry's whole charge — the one spelling the bump
    /// folds and the structure recount both read, so the refund of a
    /// drained entry equals its creation charge by construction.
    pub(super) fn label_set_entry_bytes(set: &[u32]) -> u64 {
        Self::LABEL_SET_ENTRY_BYTES + std::mem::size_of_val(set) as u64
    }

    /// The committed label-count map's keys — the admission estimate's
    /// charged-tier seed (the family owns every lock-and-read site for
    /// its maps, the LABS-515 reader: the estimate file
    /// took the locks directly before). Each reader holds its stats
    /// lock only for its own read — serial holds, matching
    /// `degree_stats_structure_bytes`' shape.
    pub(super) fn label_count_keys(&self) -> HashSet<u32> {
        self.count_stats
            .lock()
            .unwrap()
            .label_counts
            .keys()
            .copied()
            .collect()
    }

    /// The committed node count of one label set (ADR-0589), 0 when no
    /// live node carries it. A point read under the bundle's guard: the
    /// admission estimate seeds its per-set overlay one probe per distinct
    /// set a batch touches, never a copy of the map.
    fn committed_set_count(&self, set: &[u32]) -> u64 {
        self.count_stats
            .lock()
            .unwrap()
            .label_set_counts
            .get(set)
            .copied()
            .unwrap_or(0)
    }

    /// The admission estimate's fold over its label-set overlay (the
    /// batch's net counts): the apply's `count_entry` policy and charge
    /// through the one shared [`CountDelta::step`], but tolerant where the
    /// apply is loud — the estimate simulates a batch that may still be
    /// refused, and a simulated removal with no paired add prices
    /// nothing (an over-estimate) instead of failing the admission. A set
    /// the overlay has never held seeds from the committed count in the
    /// same one probe of the overlay; a drained set stays in it at zero
    /// (present, not absent), so a drained committed set is never
    /// re-seeded from the store.
    pub(super) fn estimate_set_move(
        &self,
        overlay: &mut HashMap<Vec<u32>, u64>,
        set: &[u32],
        delta: CountDelta,
    ) -> i64 {
        let count = overlay
            .entry(set.to_vec())
            .or_insert_with(|| self.committed_set_count(set));
        match delta.step(*count, Self::label_set_entry_bytes(set)) {
            Some((next, charge)) => {
                *count = next;
                charge
            }
            None => 0,
        }
    }

    /// The committed pair counts, cloned under the one stats lock — the
    /// admission estimate's pair overlay seeds itself from this snapshot
    /// (the old keys-only seed priced charges but could not price a
    /// delete's drain-to-zero refund).
    pub(super) fn pair_counts(&self) -> HashMap<(u32, u32, u32), u64> {
        self.count_stats.lock().unwrap().rel.pair_counts.clone()
    }

    /// The endpoint cross-product charge loop the pair-count family
    /// shares: one start-set × end-set walk over the
    /// pair map, `count_entry` per triple, the charge batched for the
    /// caller's [`Graph::charge`] — the create path's whole-endpoint
    /// cross product and the label mutation's per-incident-rel cross
    /// product are the same loop with different sets, hand-rolled twice
    /// before.
    pub(super) fn charge_pair_cross(
        pairs: &mut HashMap<(u32, u32, u32), u64>,
        start_set: &[u32],
        type_id: u32,
        end_set: &[u32],
        delta: CountDelta,
    ) -> i64 {
        let mut charge = 0i64;
        for s in start_set {
            for e in end_set {
                charge += Self::count_entry(
                    pairs,
                    &(*s, type_id, *e),
                    delta,
                    Self::PAIR_COUNT_ENTRY_BYTES,
                );
            }
        }
        charge
    }

    /// One count-map entry mutation with its entry-creation charge — the
    /// one policy every exact count map rides (the pair counts, the label
    /// counts): the delta is a paired ±1 — [`CountDelta::Add`] on the
    /// create side of a seam, [`CountDelta::Remove`] on its delete side,
    /// and a Remove without its paired Add is a broken store that fails
    /// loudly here; the return is the charge to apply (`entry_bytes` on
    /// the Add that creates the entry, the refund when a Remove drains
    /// it to zero and removes it, 0 otherwise) — the caller batches the
    /// `used_bytes` application outside the lock through
    /// [`Graph::charge`] so no caller can forget the refund (a
    /// label-churn delete that drains an entry must give its creation
    /// bytes back or `used_bytes` drifts upward while the recount stays
    /// true).
    fn count_entry<K, Q>(
        counts: &mut HashMap<K, u64>,
        key: &Q,
        delta: CountDelta,
        entry_bytes: u64,
    ) -> i64
    where
        K: std::hash::Hash + Eq + std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ToOwned<Owned = K> + ?Sized,
    {
        // The pairing invariant is loud in both directions, matching
        // `bump_node_population_stats`: an Add past u64 or a Remove
        // without its paired Add is a broken store, not a state to clip
        // silently (LABS-311: no fabricated defaults over
        // invariant-bound values). The key is borrowed so the dominant
        // occupied-entry Add and every Remove allocate nothing (the
        // label-set map's keys are vectors); only the Add that creates
        // the entry owns its key. Every Remove is paired with the Add
        // that created its entry (the create/delete and label-mutation
        // seams pair 1:1; the wholesale rebuild folds live rels only), so
        // a vacant key on the Remove side is unrepresentable in the
        // paired contract — a broken pairing seam fails here, at the seam
        // that broke it, not as a silently drifting count map. A drained
        // entry leaves the map — label churn must not leak zero-count
        // entries — and refunds its creation charge (the accounting
        // oracle tracks both directions).
        const UNPAIRED: &str = "count Remove without its paired Add";
        match counts.get_mut(key) {
            Some(count) => {
                let (next, charge) = delta.step(*count, entry_bytes).expect(UNPAIRED);
                if next == 0 {
                    counts.remove(key);
                } else {
                    *count = next;
                }
                charge
            }
            None => {
                let (next, charge) = delta.step(0, entry_bytes).expect(UNPAIRED);
                counts.insert(key.to_owned(), next);
                charge
            }
        }
    }

    /// LABS-851 (the round-6 seam merge): the rel-liveness seam's one
    /// call — the per-type count and the endpoint-label pair counts
    /// fold together at create and delete under the bundle's one
    /// guard (the pairing is structure, mirroring
    /// `bump_node_population_stats`); the endpoints' live labels are
    /// read outside the lock. The type map rides no entry charge
    /// (vocabulary-bounded), the pair map's cross product carries the
    /// LABS-351 charge — both flow the one charge path.
    pub(super) fn bump_rel_stats(
        &self,
        start: NodeId,
        type_id: u32,
        end: NodeId,
        delta: CountDelta,
    ) {
        let start_labels = self.live_label_ids(start);
        let end_labels = self.live_label_ids(end);
        self.bump_counts(|stats| {
            Self::fold_rel_stats(&mut stats.rel, &start_labels, type_id, &end_labels, delta)
        });
    }

    /// The bump seams' one scaffold — every incremental count-bundle
    /// mutation rides it (the three seams here and the incident walk's
    /// `apply_incident_pair_counts`): the fold runs under the bundle's
    /// one guard and its charge applies after the guard drops — so
    /// the fold/charge pairing is structure for both halves (a seam
    /// cannot run a fold and drop its `used_bytes` charge).
    pub(super) fn bump_counts(&self, fold: impl FnOnce(&mut CountStats) -> i64) {
        let charge = fold(&mut self.count_stats.lock().unwrap());
        self.charge(charge);
    }

    /// One label-set's charge over the count bundle's label map — the
    /// one fold every label-count bump rides (the `count_entry` walk
    /// with the entry-charge constant), so the label-mutation seam and
    /// the node-liveness seam share the policy, never a second copy.
    fn fold_label_counts(stats: &mut CountStats, labels: &[u32], delta: CountDelta) -> i64 {
        let mut charge = 0i64;
        for &label in labels {
            charge += Self::count_entry(
                &mut stats.label_counts,
                &label,
                delta,
                Self::LABEL_COUNT_ENTRY_BYTES,
            );
        }
        charge
    }

    /// The node total's one loud fold — the pairing invariant stated
    /// once for the bump seam and the wholesale rebuild: an Add past
    /// u64 or a Remove without its paired Add is a broken store, not
    /// a state to clip silently.
    fn fold_node_total(stats: &mut CountStats, delta: CountDelta) {
        stats.node_total = match delta {
            CountDelta::Add => stats
                .node_total
                .checked_add(1)
                .expect("node_total overflow: an Add past u64 is a broken store"),
            CountDelta::Remove => stats
                .node_total
                .checked_sub(1)
                .expect("node_total underflow: a Remove without its paired Add"),
        };
    }

    /// One node's label SET in or out of the set-count map (ADR-0589):
    /// the `count_entry` policy with the key's width in the entry charge.
    fn fold_label_set(stats: &mut CountStats, labels: &[u32], delta: CountDelta) -> i64 {
        Self::count_entry(
            &mut stats.label_set_counts,
            labels,
            delta,
            Self::label_set_entry_bytes(labels),
        )
    }

    /// The node side's one fold — the label counts, the label-set
    /// count and the node total together (the pairing the node-liveness
    /// seam and the wholesale rebuild both ride, mirroring
    /// `fold_rel_stats`). Returns the two maps' entry charges.
    fn fold_node_stats(stats: &mut CountStats, labels: &[u32], delta: CountDelta) -> i64 {
        let charge = Self::fold_label_counts(stats, labels, delta)
            + Self::fold_label_set(stats, labels, delta);
        Self::fold_node_total(stats, delta);
        charge
    }

    /// The rel side's one fold — the per-type count and the
    /// endpoint-label cross product together (the pairing the
    /// rel-liveness seam and the wholesale rebuild both ride, like
    /// `fold_node_stats` on the node side). The
    /// type map rides no entry charge (vocabulary-bounded); the pair
    /// map's cross product carries the LABS-351 charge.
    fn fold_rel_stats(
        counts: &mut RelCounts,
        start_labels: &[u32],
        type_id: u32,
        end_labels: &[u32],
        delta: CountDelta,
    ) -> i64 {
        let type_charge = Self::count_entry(&mut counts.rel_type_counts, &type_id, delta, 0);
        let pair_charge = Self::charge_pair_cross(
            &mut counts.pair_counts,
            start_labels,
            type_id,
            end_labels,
            delta,
        );
        type_charge + pair_charge
    }

    /// LABS-370 (v1 follow-up): the label-mutation seam's label half —
    /// `apply_label_delta_stats` is the only caller (liveness is
    /// unchanged there, so the node total does not move; create and
    /// delete ride `bump_node_population_stats`; the wholesale rebuild
    /// is `rebuild_count_stats`).
    ///
    /// ADR-0589: the node's label set moves from `before` to `after`
    /// in the same guarded step as the per-label counts, so a reader
    /// never sees the node in neither set or in both.
    pub(super) fn bump_label_counts(&self, mutation: LabelMutation<'_>) {
        self.bump_counts(|stats| {
            Self::fold_label_counts(stats, mutation.labels, mutation.delta)
                + Self::fold_label_set(stats, mutation.before, CountDelta::Remove)
                + Self::fold_label_set(stats, mutation.after, CountDelta::Add)
        });
    }

    /// LABS-851: the node-liveness seam's one call — the label counts
    /// and the total node count bump together at create and delete
    /// under the bundle's one guard (the pairing is structure, not a
    /// comment: a future seam cannot apply one half of it). The
    /// pairing invariant is loud in both directions: an Add overflow
    /// or a Remove underflow is a broken pairing, not a state to clip
    /// silently (the `count_entry` contract).
    pub(super) fn bump_node_population_stats(&self, labels: &[u32], delta: CountDelta) {
        self.bump_counts(|stats| Self::fold_node_stats(stats, labels, delta));
    }

    /// The count bundle's share of the retained-structure recount (the
    /// `structure_bytes` oracle): the charged maps' entry counts at
    /// the same constants the incremental bumps charge (the #398
    /// lesson — the recount and the charge must read one constant).
    /// The histograms' share stays in `degree_stats_structure_bytes`
    /// beside the histogram family.
    pub(super) fn count_state_structure_bytes(&self) -> u64 {
        let stats = self.count_stats.lock().unwrap();
        (stats.rel.pair_counts.len() as u64)
            .saturating_mul(Self::PAIR_COUNT_ENTRY_BYTES)
            .saturating_add(
                (stats.label_counts.len() as u64).saturating_mul(Self::LABEL_COUNT_ENTRY_BYTES),
            )
            .saturating_add(
                stats
                    .label_set_counts
                    .keys()
                    .map(|set| Self::label_set_entry_bytes(set))
                    .fold(0u64, u64::saturating_add),
            )
    }

    /// Rebuild the exact count state wholesale from the live graph —
    /// abort and restore recompute it (the abort's install-time bumps
    /// die with the reverted versions). The count state's one rebuild
    /// home: `rebuild_adj` feeds it the same materialized `RelRow`s it
    /// hands `adj_build`, beside the histogram fold (both paired
    /// rebuild sites run the two together). The node fold reads only liveness and the live
    /// versions' already-interned label ids — never `node_at`'s full
    /// property materialization — and every live node counts toward
    /// the total, the label-less ones included. Both sides ride the
    /// bump seams' own folds with their charges discarded — the
    /// wholesale rebuild's one exception to the no-discarded-return
    /// rule: its caller runs `recompute_used`, which re-tallies
    /// wholesale. The rel side rides the start-keyed partition
    /// `rebuild_adj` computes once and is merged by summing: a shard's
    /// nodes are exclusive to it, but a (start-label, type, end-label)
    /// triple spans nodes, so its two maps accumulate rather than
    /// extend. Both are bounded by the label and type vocabulary, not
    /// the data, so the merge is small however many shards there are.
    pub(super) fn rebuild_count_stats(
        &self,
        rows: &[RelRow],
        labels: &LabelCache,
        shards: &[Vec<usize>],
    ) {
        // Built whole outside the guard, published in one store: the
        // bundle's wholesale rebuild owns all four fields — one
        // bundle, one guard, one publish, so a reader can never see
        // one rebuild's fields mixed with the previous rebuild's (the
        // structural claim the bundle exists for). One liveness
        // convention through the whole fold: `live_version`, the seam
        // the bump paths ride — never a snapshot-ts probe beside it.
        let mut stats = CountStats::default();
        self.nodes.for_each(|_id, vers| {
            let Some(v) = super::live_version(vers) else {
                return;
            };
            // Charge discarded — see the doc's rebuild note.
            let _ = Self::fold_node_stats(&mut stats, &v.live.labels, CountDelta::Add);
        });
        // The rel side rides `rebuild_adj`'s materialized `RelRow`s —
        // the same rows `adj_build` installs (both paired rebuild sites
        // pass them on); charges discarded — see the doc's rebuild note.
        let parts = shard::run_shards(shards, |shard| Self::count_rel_part(rows, labels, shard));
        // Only the rel side rides the shards, so only its two maps merge —
        // the node walk above owns the label counts and the node total.
        for part in parts {
            stats.rel.merge(part);
        }
        *self.count_stats.lock().unwrap() = stats;
    }

    /// One shard's share of the count bundle's rel side: its rows' per-type
    /// counts and endpoint-label pair counts, which the caller merges by
    /// summing. The shard reuses [`Graph::fold_rel_stats`] rather than
    /// re-spelling it, which is why its result is that fold's own two-map
    /// [`RelCounts`] and not a bundle.
    fn count_rel_part(rows: &[RelRow], labels: &LabelCache, shard: &[usize]) -> RelCounts {
        let mut counts = RelCounts::default();
        for &index in shard {
            let row = &rows[index];
            let _ = Self::fold_rel_stats(
                &mut counts,
                labels.get(row.start),
                row.type_id,
                labels.get(row.end),
                CountDelta::Add,
            );
        }
        counts
    }

    /// LABS-370: the exact count of rels between a (start-label,
    /// rel-type, end-label) triple — the counting operator's O(1)
    /// answer. `None` when nothing of that triple was ever written.
    /// The pair triple's one resolution: the three names through the
    /// interner, then the one map read — [`Self::pattern_pair_rows`]'s
    /// Option contract (an unknown name or never-written pair is
    /// `None`) and the probe's Pair arm (the same facts as a hard 0)
    /// both consume this spelling, never a second copy.
    fn pair_rows_under(
        &self,
        stats: &CountStats,
        start_label: &str,
        rel_type: &str,
        end_label: &str,
    ) -> Option<u64> {
        let s = self.interner.get_id(start_label)?;
        let t = self.interner.get_id(rel_type)?;
        let e = self.interner.get_id(end_label)?;
        stats.rel.pair_counts.get(&(s, t, e)).copied()
    }

    /// Test-seam reader (the production count path is
    /// [`Graph::windowed_stats_count`]; the pair triple's resolution
    /// lives in [`Self::pair_rows_under`], shared with the probe).
    #[cfg(test)]
    pub(crate) fn pattern_pair_rows(
        &self,
        start_label: &str,
        rel_type: &str,
        end_label: &str,
    ) -> Option<u64> {
        self.pair_rows_under(
            &self.count_stats.lock().unwrap(),
            start_label,
            rel_type,
            end_label,
        )
    }

    /// Live relationships of `rel_type` — the one read path
    /// [`probe_count_under`](Self::probe_count_under)'s `RelType` arm
    /// owns (through [`Self::interned_count`]), consulted under the
    /// bundle's guard; the planner's `PatternStats` adapter and the
    /// constraint-key estimate read this boundary.
    pub(crate) fn pattern_rel_type_rows(&self, rel_type: &str) -> u64 {
        let stats = self.count_stats.lock().unwrap();
        self.interned_count(&stats.rel.rel_type_counts, rel_type)
    }

    /// [`Graph::pattern_rel_type_rows`] for an already-resolved token id.
    pub(crate) fn pattern_rel_type_rows_by_id(&self, type_id: u32) -> u64 {
        let stats = self.count_stats.lock().unwrap();
        self.probe_count_under(CountProbe::RelTypeId(type_id), &stats)
    }

    /// LABS-370 (v1 follow-up): the exact count of live nodes carrying
    /// `label` — the one read path [`probe_count_under`](Self::
    /// probe_count_under)'s `Label` arm owns, consulted under the
    /// bundle's guard. Unlike the label index (an upper bound that
    /// keeps deleted ids until checkpoint GC), the count state follows
    /// every label-set mutation seam exactly.
    pub(crate) fn pattern_label_count_exact(&self, label: &str) -> u64 {
        let stats = self.count_stats.lock().unwrap();
        self.probe_count_under(CountProbe::Label(label.to_string()), &stats)
    }

    /// LABS-370: the count-only reader's consistency
    /// window. A plain exactness check followed by unlocked count reads
    /// is a check-then-read a concurrent `apply_redo` can interleave
    /// between — leaking out-of-snapshot writes into the count. Here
    /// the reader holds the count bundle's ONE guard across the
    /// exactness check and the reads (LABS-851's bundle made the
    /// invariant structural: every stats mutation path —
    /// `bump_rel_stats`, `bump_label_counts`, the incident walk,
    /// `bump_node_population_stats`, the rebuilds — takes the same
    /// guard, so a writer cannot mutate any count state while it is
    /// held), and `apply_redo_inner` opens `in_write` BEFORE its first
    /// mutation and bumps `commit_ts` AFTER its last, so the under-lock
    /// check reads a window the mutation discipline already covers. A
    /// failed check declines (`None` — the scan answers). The degree
    /// histograms need no lock here: the count-only shapes never read
    /// them. The `exact` closure runs UNDER the bundle's one guard —
    /// it must not take `count_stats` again and must not call the
    /// `pattern_*` readers (they lock); the one caller passes
    /// lock-free atomic reads (`sees_exact_base_stats`), and this
    /// seam is where that obligation is stated.
    pub(crate) fn windowed_stats_count(
        &self,
        exact: impl FnOnce() -> bool,
        probe: CountProbe,
    ) -> Option<u64> {
        self.windowed_read(exact, |stats| Some(self.probe_count_under(probe, stats)))
    }

    /// The consistency window both windowed readers share: the exactness
    /// check and `read` run under the bundle's one guard, and a failed
    /// check — or a `read` that declines — is `None` (the scan answers).
    /// `exact` carries the lock-free obligation [`Self::windowed_stats_count`]
    /// states; `read` runs under the guard and must not lock the bundle.
    fn windowed_read<R>(
        &self,
        exact: impl FnOnce() -> bool,
        read: impl FnOnce(&CountStats) -> Option<R>,
    ) -> Option<R> {
        let stats = self.count_stats.lock().unwrap();
        if !exact() {
            return None;
        }
        read(&stats)
    }

    /// The set map answers a grouped aggregate in time proportional to
    /// its entries, so it only beats the node scan while nodes share
    /// sets: below [`Self::LABEL_SET_FLOOR`] entries it always pays, above
    /// it only while the average set holds at least
    /// [`Self::LABEL_SET_MIN_SHARING`] nodes.
    fn label_set_map_pays(stats: &CountStats) -> bool {
        let sets = stats.label_set_counts.len() as u64;
        sets <= Self::LABEL_SET_FLOOR
            || stats.node_total >= sets.saturating_mul(Self::LABEL_SET_MIN_SHARING)
    }

    /// The set-map answer's entry count below which it always answers.
    const LABEL_SET_FLOOR: u64 = 4096;
    /// The average nodes per set the map needs above the floor to answer.
    const LABEL_SET_MIN_SHARING: u64 = 4;

    /// ADR-0589: the label-set distribution under the same consistency
    /// window as [`Self::windowed_stats_count`] — the exactness check and
    /// the read share the bundle's one guard, a failed check declines
    /// (`None`, the scan answers), and the `exact` closure carries the
    /// same lock-free obligation. Returns each distinct live label set as
    /// its label NAMES in label order with its node count, sorted by label
    /// id so the answer's order is deterministic. The ids are cloned under
    /// the guard (bounded by the number of distinct sets); the names
    /// resolve after it drops. A graph whose sets approach its node count
    /// ([`Self::label_set_map_pays`]) declines too: the map answers in time
    /// proportional to the sets, so it has no speed to offer there.
    pub(crate) fn windowed_label_sets(
        &self,
        exact: impl FnOnce() -> bool,
    ) -> Option<Vec<LabelSetCount>> {
        let mut sets: Vec<(Vec<u32>, u64)> = self.windowed_read(exact, |stats| {
            Self::label_set_map_pays(stats).then(|| {
                stats
                    .label_set_counts
                    .iter()
                    .map(|(set, count)| (set.clone(), *count))
                    .collect()
            })
        })?;
        sets.sort_unstable();
        Some(
            sets.into_iter()
                .map(|(set, count)| (self.interner.cached_labels(&set), count))
                .collect(),
        )
    }

    /// One name→count resolution the single-name probe arms share —
    /// intern the name, a missing name or a drained entry is a hard 0
    /// (exactness makes them equivalent), the one spelling of the
    /// family's unknown-name policy.
    fn interned_count(&self, counts: &HashMap<u32, u64>, name: &str) -> u64 {
        self.interner
            .get_id(name)
            .and_then(|id| counts.get(&id).copied())
            .unwrap_or(0)
    }

    /// The probe read under the held bundle guard — no re-locking: the
    /// public `pattern_*` readers lock again and must not be called
    /// from inside the window.
    fn probe_count_under(&self, probe: CountProbe, stats: &CountStats) -> u64 {
        match probe {
            CountProbe::Label(label) => self.interned_count(&stats.label_counts, label.as_str()),
            CountProbe::Pair(s, t, e) => {
                // None ≡ 0 by exactness (a drained entry leaves the
                // map); an unknown name is a hard 0 the same way —
                // `pair_rows_under` is the one resolution spelling.
                self.pair_rows_under(stats, s.as_str(), t.as_str(), e.as_str())
                    .unwrap_or(0)
            }
            // LABS-851: the whole-population arms. The total node count
            // is a maintained counter (label-less nodes included); the
            // rel-type arms read the same exactness-drained map the
            // planner's `pattern_rel_type_rows` reads — a missing
            // (or unknown-name) type is a hard 0, and every type summed
            // is the directed total.
            CountProbe::AllNodes => stats.node_total,
            CountProbe::RelType(t) => self.interned_count(&stats.rel.rel_type_counts, t.as_str()),
            CountProbe::RelTypeId(id) => stats.rel.rel_type_counts.get(&id).copied().unwrap_or(0),
            CountProbe::AllRels => stats
                .rel
                .rel_type_counts
                .values()
                .copied()
                // The read-side total rides the family's loudness
                // contract: a sum past u64 is a broken store.
                .fold(0u64, |acc, n| {
                    acc.checked_add(n)
                        .expect("rel-type total overflow: a sum past u64 is a broken store")
                }),
        }
    }
}
