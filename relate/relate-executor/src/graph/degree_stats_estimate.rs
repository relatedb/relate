//! The admission-path estimate of the degree-statistics maps' entry
//! growth (LABS-370, ADR-0387 Decision 1): the batch simulator behind
//! [`Graph::estimate_stats_entry_growth`], in its own impl-block file —
//! the repo's methods-in-their-own-file precedent, a sibling of
//! `degree_stats.rs` (which owns the maps' mutation seams and readers).
//! This file owns the read-only simulation that prices a
//! not-yet-applied batch against the count family (the exact label
//! counts, the pair counts, the degree histograms); no mutation path
//! lives here. The simulation's accumulators are one named state —
//! [`BatchSim`], its steps the `RedoOp` arms (LABS-515: the accumulator
//! tangle the estimate threaded as function locals before) — and the
//! incident stream its AddLabels step prices is the one the apply path
//! walks ([`Graph::incident_rels`], LABS-515): apply and estimate
//! consult one enumeration form.

use std::collections::{HashMap, HashSet};

use relate_core::{NodeId, RelId};

use super::accounting::FreshIds;
use super::count_state::CountDelta;
use super::degree_stats_incident::IncidentRel;
use super::{fill_labels, Graph};
use crate::redo::RedoOp;

impl Graph {
    /// LABS-370 (security follow-up): the stats-map entry growth a
    /// committed batch would create — the admission estimate's term for
    /// the count family (the exact label counts, the pair counts, the
    /// degree histograms). Charged BEFORE the commit applies: an
    /// oversized-label write (one rel between two multi-label nodes
    /// materializes labels(start) × labels(end) pair entries) is
    /// rejected at admission, not after the entries land past the cap.
    ///
    /// Exact for the same batches the redo-delta oracle is exact for:
    /// the label sets are simulated through the batch (a create sets
    /// them, add edits them in op order, committed nodes start from
    /// their live sets), and a fresh name interns to an id no committed
    /// map can hold, so a fresh label's entry is always a creation. The
    /// removal direction is the stated exception — `BatchSim::step`
    /// prices no `RemoveLabels` drain, so such a batch is over-estimated
    /// (over-refused), never under. The AddLabels growth simulates the batch's in-batch
    /// rels too: the apply path's incident walk sees
    /// them (the live-version probe), so the estimate must count their
    /// pair entries — an estimate that walks committed adjacency only
    /// drifts from the applied `used_bytes` on mixed
    /// [CreateRel, AddLabels] batches.
    ///
    /// A two-sided incidence — a self-loop, or a reciprocal same-type
    /// pair — realizes its pair keys on BOTH sides (the apply path's
    /// own discipline, extension of it to the estimate): the incident walk prices every
    /// side of every incident rel, and repeated keys charge nothing
    /// twice through the seen-key sets (the `seen_far` early-out
    /// the first version carried skipped the second side's keys and
    /// under-priced admission).
    pub(super) fn estimate_stats_entry_growth(&self, ops: &[RedoOp]) -> i64 {
        // The simulation's phases as steps over one state (LABS-515):
        // the op loop drives the state in op order; the state's growth
        // is the estimate.
        let mut sim = BatchSim::new(self);
        for op in ops {
            sim.step(self, op);
        }
        sim.charges.growth
    }
}

/// The batch simulator's state (LABS-515): the accumulators the estimate
/// threaded as function locals before, as named fields. The op loop in
/// [`Graph::estimate_stats_entry_growth`] drives one state through the
/// batch, each `RedoOp` arm one step over it. Three field groups: the
/// label-set simulation (a create sets a node's labels, an add edits
/// them in op order, a committed node starts from its live set), the
/// entry-growth ledger ([`ChargeLedger`] — the keys already accounted
/// for and the total they charged, one accumulator), and the batch's
/// rels.
#[derive(Clone, Debug, PartialEq)]
struct BatchSim {
    /// The batch's simulated label sets, per node — the cache-fill half
    /// of the cache-fill-and-borrow pattern: the
    /// batch's simulated set, or the live set, cached once so no
    /// per-lookup clone of the live set rides the admission path.
    node_labels: HashMap<NodeId, Vec<u32>>,
    /// Fresh-name interning: a fresh name interns to an id no committed
    /// map can hold, so a fresh label's entry is always a creation.
    fresh_ids: FreshIds,
    /// The entry-growth ledger: two charge-only seen sets, one seeded
    /// pair-count map, and the total they charged net of refunds.
    charges: ChargeLedger,
    /// The batch's rel liveness, keyed by id ([`BatchRelState`]): the
    /// AddLabels growth must count every rel the batch created — live
    /// or deleted in the batch, because apply walks the adjacency after
    /// the batch's CreateRel ops land (rebucket and the incident walk
    /// see them) and a same-batch delete's pre-delete version stays
    /// live-looking inside the write window, its founded chunk priced
    /// chunk-granularly — so an estimate that walks committed adjacency
    /// only drifts from the applied `used_bytes` on mixed
    /// [CreateRel, DeleteRel, AddLabels] batches. The dead states are
    /// the delete arm's tombstones.
    batch_rels: HashMap<RelId, BatchRelState>,
}

/// A batch rel's liveness, by name and by founding: the AddLabels
/// phases price a rel exactly when this batch created it —
/// [`Live`](BatchRelState::Live) still holds and prices in both phases,
/// [`BatchDead`](BatchRelState::BatchDead) was created and then deleted
/// in the batch (the delete's tombstone founds the zero-hop sealed
/// chunk the histogram phase still prices, chunk-granularly, while the
/// pair phase walks live hops and the tombstone hides the rel's hop
/// from it) — while [`StoreDead`](BatchRelState::StoreDead) is
/// a committed rel the batch deleted: the committed incident stream and
/// chunk walk already price it, so no batch term may, and it is a bare
/// presence tombstone because nothing reads an identity off it (the
/// delete that resolved it owns the one refund, in the step that
/// resolved it). The dead states are the delete arm's terminal
/// tombstones, never removed, so a repeat delete resolves nothing (a
/// re-created id overwrites its entry and is live again).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BatchRelState {
    Live(BatchRel),
    BatchDead(BatchRel),
    StoreDead,
}

impl BatchRelState {
    /// The rel the AddLabels histogram phase prices, if this state is
    /// one: every rel the batch created, live or deleted in the batch —
    /// a same-batch delete still founds the zero-hop sealed chunk the
    /// chunk-granular charge rides — never a committed rel the batch
    /// deleted, which the committed terms own.
    fn priced_histogram(&self) -> Option<&BatchRel> {
        match self {
            BatchRelState::Live(rel) | BatchRelState::BatchDead(rel) => Some(rel),
            BatchRelState::StoreDead => None,
        }
    }

    /// The rel the AddLabels pair phase prices: only a still-live one —
    /// the pair charge walks live hops, and a batch-deleted rel's hops
    /// are tombstoned before the label mutation applies.
    fn priced_pair(&self) -> Option<&BatchRel> {
        match self {
            BatchRelState::Live(rel) => Some(rel),
            BatchRelState::BatchDead(_) | BatchRelState::StoreDead => None,
        }
    }
}

/// One of the batch's simulated rels, its direction as field names
/// (the LABS-515 naming: the rel's direction is the
/// fact `batch_incidents` translates into incident records and the
/// pair-key orientation rests on — a field, not a slot position).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BatchRel {
    start: NodeId,
    end: NodeId,
    type_id: u32,
}

impl BatchRel {
    /// The rel from the store's live-identity probe — the one spelling
    /// of the tuple-to-rel transition.
    fn from_identity((start, end, type_id): (NodeId, NodeId, u32)) -> Self {
        Self {
            start,
            end,
            type_id,
        }
    }
}

/// The entry-growth ledger (the LABS-515 grouping), two tiers by map:
/// the label and histogram tiers are charge-only seen sets — the
/// committed maps' own keys seeded at construction, then every key this
/// batch charges, one `insert` deciding the LABS-425 two-tier policy —
/// while the pair tier is a live count map through the apply's own
/// fold (`charge_pair_cross` over `count_entry`): the committed counts
/// seeded once, every arm's net delta after that, so a drain's refund
/// prices against the same counts the apply folds and charge, drain,
/// and refund can never drift between the two sides.
#[derive(Clone, Debug, PartialEq)]
struct ChargeLedger {
    /// The charge-only tiers' seen sets (label, histogram).
    labels: HashSet<u32>,
    hists: HashSet<(u32, u32)>,
    /// The label-set tier (ADR-0589): a live count overlay like the pair
    /// tier, seeded lazily — a set's committed count is probed on first
    /// touch instead of the map being copied, so pricing a batch never
    /// scales with the number of committed sets.
    sets: HashMap<Vec<u32>, u64>,
    /// The pair tier: the pair counts this batch rides, the committed
    /// counts seeded once and every arm's net delta after.
    pairs: HashMap<(u32, u32, u32), u64>,
    /// The total the accounted-for keys charged, net of the refunds a
    /// draining delete returns — the estimate's answer, counted in the
    /// entry-byte constants' own domain.
    growth: i64,
}

impl ChargeLedger {
    /// The ledger seeded from the committed maps (the entry-growth
    /// policy's committed tier): the charge-only tiers take the committed
    /// keys, the pair tier the committed counts. Both come through the
    /// family's own key readers — the locks live in degree_stats.rs, one
    /// reader per map, each held only for its own read.
    fn new(g: &Graph) -> Self {
        Self {
            labels: g.label_count_keys(),
            sets: HashMap::new(),
            pairs: g.pair_counts(),
            hists: g.degree_histogram_keys(),
            growth: 0,
        }
    }

    /// One label-count entry's charge: each charge binds its seen set
    /// to its own entry price — the association the open generic left
    /// as two independent inputs.
    fn charge_label(&mut self, key: u32) {
        Self::charge_new(
            &mut self.labels,
            key,
            Graph::LABEL_COUNT_ENTRY_BYTES,
            &mut self.growth,
        );
    }

    /// One node's label set entering or leaving the set map (ADR-0589),
    /// priced through the apply's own policy over the lazily seeded
    /// overlay ([`Graph::estimate_set_move`]).
    fn bump_set(&mut self, g: &Graph, set: &[u32], delta: CountDelta) {
        self.growth += g.estimate_set_move(&mut self.sets, set, delta);
    }

    /// One node moving between label sets (ADR-0589): the old set loses it,
    /// the new one gains it — the paired Remove/Add every label seam
    /// prices.
    fn move_set(&mut self, g: &Graph, from: &[u32], to: &[u32]) {
        self.bump_set(g, from, CountDelta::Remove);
        self.bump_set(g, to, CountDelta::Add);
    }

    /// One histogram entry's charge.
    fn charge_hist(&mut self, key: (u32, u32)) {
        Self::charge_new(
            &mut self.hists,
            key,
            Graph::HISTOGRAM_ENTRY_BYTES,
            &mut self.growth,
        );
    }

    /// One endpoint cross's net pair deltas, charged and refunded
    /// through the apply's own fold — the one entry point, so the
    /// growth bookkeeping stays here and the key enumeration is the
    /// canonical `charge_pair_cross`, never a re-derivation.
    fn bump_pair_cross(
        &mut self,
        start_ids: &[u32],
        type_id: u32,
        end_ids: &[u32],
        delta: CountDelta,
    ) {
        self.growth +=
            Graph::charge_pair_cross(&mut self.pairs, start_ids, type_id, end_ids, delta);
    }

    /// The entry-growth policy, named once (LABS-425;
    /// the two tiers folded into one seeded set in the LABS-515 stellar
    /// round): a key is a new entry iff neither the committed map nor
    /// an earlier op in this batch holds it — the estimate-side twin of
    /// `count_entry`'s apply-side policy.
    fn charge_new<K>(seen: &mut HashSet<K>, key: K, entry_bytes: u64, growth: &mut i64)
    where
        K: std::hash::Hash + Eq,
    {
        if seen.insert(key) {
            *growth += entry_bytes as i64;
        }
    }
}

impl BatchSim {
    /// The simulator's initial state: the ledger seeded with the
    /// committed maps' keys, everything else empty.
    fn new(g: &Graph) -> Self {
        Self {
            node_labels: HashMap::new(),
            fresh_ids: FreshIds::new(),
            charges: ChargeLedger::new(g),
            batch_rels: HashMap::new(),
        }
    }

    /// One op's step over the state, in op order: the four arms the
    /// count family's growth rides — creates charge their entries, an
    /// AddLabels prices its label and incident growth, a delete refunds
    /// the pair entries it drains. The removal direction beyond a rel
    /// delete is deliberately unpriced — a node delete's label refunds
    /// and the pair refunds of its detached incident rels, and a label
    /// removal's drains (the node-side sibling on the ticket): every
    /// one of them is a refund, so omitting them over-prices, which
    /// over-refuses but never admits past a cap. The no-op arms are
    /// enumerated so a new count-affecting op fails to compile here
    /// instead of silently pricing nothing.
    fn step(&mut self, g: &Graph, op: &RedoOp) {
        match op {
            RedoOp::CreateNode { id, labels, .. } => self.create_node(g, *id, labels),
            RedoOp::CreateRel {
                id,
                start,
                end,
                rel_type,
                ..
            } => self.create_rel(g, *id, *start, *end, rel_type),
            RedoOp::AddLabels { id, labels } => self.add_labels(g, *id, labels),
            RedoOp::RemoveLabels { id, labels } => self.remove_labels(g, *id, labels),
            RedoOp::DeleteRel { id } => self.delete_rel(g, *id),
            // Property and identity edits, index and constraint catalog
            // ops, and node deletion (the removal-direction omission
            // above) move no count entry this estimate prices.
            RedoOp::DeleteNode { .. }
            | RedoOp::SetNodeProp { .. }
            | RedoOp::RemoveNodeProp { .. }
            | RedoOp::SetRelProp { .. }
            | RedoOp::RemoveRelProp { .. }
            | RedoOp::CreateIndex { .. }
            | RedoOp::DropIndex { .. }
            | RedoOp::CatalogIndexDetail { .. }
            | RedoOp::CreateConstraint { .. }
            | RedoOp::DropConstraint { .. }
            | RedoOp::CatalogDerivedConstraint { .. }
            | RedoOp::PutDatabase { .. }
            | RedoOp::DropDatabase { .. }
            | RedoOp::PutGraphType { .. }
            | RedoOp::SetCurrentGraphType { .. }
            | RedoOp::PullIdentityPut { .. }
            | RedoOp::OntologySet { .. }
            | RedoOp::OntologyDrop { .. } => {}
        }
    }

    /// CreateNode: the node's labels become its simulated set, and each
    /// one prices its exact-count entry — a fresh name interns to an id
    /// no committed map can hold, so every one of them is a creation.
    fn create_node(&mut self, g: &Graph, id: NodeId, labels: &[String]) {
        let ids: Vec<u32> = labels
            .iter()
            .map(|l| self.fresh_ids.resolve(g, l))
            .collect();
        for &lid in &ids {
            self.charges.charge_label(lid);
        }
        self.charges.bump_set(g, &ids, CountDelta::Add);
        self.node_labels.insert(id, ids);
    }

    /// RemoveLabels: the removal's per-label and pair drains are refunds
    /// and stay unpriced, but the node moves between label sets (ADR-0589):
    /// its old set may drain (a refund) and its new one may be created, so
    /// the move is priced and the simulated set follows it.
    fn remove_labels(&mut self, g: &Graph, id: NodeId, labels: &[String]) {
        let drop: Vec<u32> = labels
            .iter()
            .map(|l| self.fresh_ids.resolve(g, l))
            .collect();
        let ids = fill_labels(&mut self.node_labels, g, id);
        let before = ids.clone();
        ids.retain(|l| !drop.contains(l));
        if ids.len() == before.len() {
            return;
        }
        let after = ids.clone();
        self.charges.move_set(g, &before, &after);
    }

    /// CreateRel: both endpoint label sets are ensured into the
    /// simulation, the rel's out-side histogram entry and its endpoint
    /// pair cross each price, and the rel joins the simulation for the
    /// AddLabels steps that follow.
    fn create_rel(&mut self, g: &Graph, id: RelId, start: NodeId, end: NodeId, rel_type: &str) {
        let t = self.fresh_ids.resolve(g, rel_type);
        fill_labels(&mut self.node_labels, g, start);
        fill_labels(&mut self.node_labels, g, end);
        // Both endpoint sets are ensured above — the index read is the
        // borrow, not a clone.
        let start_ids = &self.node_labels[&start];
        let end_ids = &self.node_labels[&end];
        for &s in start_ids {
            self.charges.charge_hist((s, t));
        }
        self.charges
            .bump_pair_cross(start_ids, t, end_ids, CountDelta::Add);
        // The rel joins the live set — a re-created id overwrites its
        // earlier entry, so a later delete resolves the live endpoints.
        self.batch_rels.insert(
            id,
            BatchRelState::Live(BatchRel {
                start,
                end,
                type_id: t,
            }),
        );
    }

    /// DeleteRel: each pair entry the rel's (start-label, type,
    /// end-label) cross holds loses one count, through the same
    /// [`Graph::count_entry`] the apply's `bump_rel_stats` rides — the
    /// entries this delete drains to zero are refunded, the ones that
    /// live on are not (the rel itself holds one count of every key in
    /// its cross, so the Remove is always paired). Which identity the
    /// refund prices is the liveness table's decision
    /// ([`BatchRelState`] owns the lifecycle); this step only reads it.
    fn delete_rel(&mut self, g: &Graph, id: RelId) {
        let rel = match self.batch_rels.get_mut(&id) {
            // One lookup, one transition: the state is deadened in
            // place, and a batch-created rel's identity comes back for
            // the refund. The terminal states yield none — a repeat
            // delete has nothing left to price.
            Some(state) => match *state {
                BatchRelState::Live(rel) => {
                    *state = BatchRelState::BatchDead(rel);
                    Some(rel)
                }
                BatchRelState::BatchDead(_) | BatchRelState::StoreDead => None,
            },
            // An unseen id: the store's live identity, priced once and
            // tombstoned so no later delete re-probes the pre-batch
            // store for it.
            None => g.rel_live_identity(id).map(|identity| {
                self.batch_rels.insert(id, BatchRelState::StoreDead);
                BatchRel::from_identity(identity)
            }),
        };
        let Some(rel) = rel else {
            return;
        };
        fill_labels(&mut self.node_labels, g, rel.start);
        fill_labels(&mut self.node_labels, g, rel.end);
        let start_ids = &self.node_labels[&rel.start];
        let end_ids = &self.node_labels[&rel.end];
        self.charges
            .bump_pair_cross(start_ids, rel.type_id, end_ids, CountDelta::Remove);
    }

    /// AddLabels: every requested label charges its exact-count entry
    /// through the [`ChargeLedger`] — a carried label's key already sits
    /// in the ledger's seed set, so it prices nothing — while the
    /// set-integrity guard keeps the simulation a set. The early-out keys
    /// on the ids that actually joined, the fact
    /// [`Graph::rebucket_added_labels`] and `apply_incident_pair_counts`
    /// guard on; the pricers then price the node's full simulated set (a
    /// carried label's keys are seeded or already charged, a fresh one's
    /// charge exactly what apply materializes). The committed
    /// chunk-granular histogram term prices against the out-adjacency
    /// chunks; the incident half prices the stream (the entries
    /// [`Graph::apply_incident_pair_counts`] and
    /// [`Graph::rebucket_added_labels`] materialize at apply time,
    /// estimated against the adjacency the apply path walks).
    fn add_labels(&mut self, g: &Graph, id: NodeId, labels: &[String]) {
        let ids = fill_labels(&mut self.node_labels, g, id);
        let before = ids.len();
        for l in labels {
            let lid = self.fresh_ids.resolve(g, l);
            self.charges.charge_label(lid);
            if !ids.contains(&lid) {
                ids.push(lid);
            }
        }
        // The apply-side guard's mirror (both apply phases skip an
        // empty mutated set, and no incident prices an empty one here):
        // a no-op AddLabels — no label actually joined — ends the step
        // before the incident walk. The joined ids are the set's own
        // growth, so no second collection tracks them.
        if ids.len() == before {
            return;
        }
        // The simulated set, snapshotted once and released from the
        // borrow before the pricing calls. It is the post-add set; the
        // pre-add set is its own prefix (`add_labels` only appends), so
        // both self-loop charge sets are slices of it — no second
        // collection.
        let mine = ids.clone();
        // ADR-0589: the node moves from its pre-add set (the prefix) to
        // the post-add one.
        self.charges.move_set(g, &mine[..before], &mine);
        // The pair phase prices only the ids that actually joined: apply
        // bumps just the mutated labels (`apply_incident_pair_counts`
        // walks `mutated_labels`), and the count overlay must mirror the
        // apply's counts exactly — a carried label's keys ride the
        // committed seed, priced nothing. The histogram phase below keeps
        // the full set (its charge is entry creation, carried keys are
        // seeded).
        // The batch's incident enumeration, priced per phase (LABS-515):
        // the histogram phase reads only the batch's created rels — its
        // committed out-side types come from `out_chunks` (delta and
        // sealed alike), so the committed incident stream would
        // contribute nothing — and a same-batch delete still founds the
        // zero-hop sealed chunk its tombstone lands on, a charge that is
        // chunk-granular. The pair phase owns the committed walk
        // ([`Graph::incident_rels`], the apply path's own) extended with
        // only the still-live batch rels: the pair charge walks live
        // hops, and a batch-deleted rel's hops are tombstoned before the
        // AddLabels applies.
        let batch: Vec<_> = self
            .batch_incidents(id, BatchRelState::priced_histogram)
            .collect();
        self.price_histogram_entries(g, id, &mine, &batch);
        // The near side is the joined ids, the far-side self-loop sets
        // the post-add set and its pre-add prefix — the apply's own
        // three sets.
        let mut live_incidents = g.incident_rels(id);
        live_incidents.extend(self.batch_incidents(id, BatchRelState::priced_pair));
        self.price_incidents(
            g,
            id,
            &mine[before..],
            &mine,
            &mine[..before],
            &live_incidents,
        );
    }

    /// The AddLabels step's histogram-entry pricing, one site (the
    /// LABS-515 consolidation: the committed term and
    /// the in-batch term were split across two methods before): a
    /// label mutation materializes its out-side histogram entries at
    /// apply time — [`Graph::rebucket_added_labels`] walks every
    /// out-adjacency CHUNK (CHUNK-granular: a type whose hops are all
    /// tombstoned still prices, because apply's rebucket charges per
    /// chunk, not per live hop), and the batch's created rels
    /// contribute their out-side types the committed chunks cannot hold
    ///. One type set covers both terms.
    fn price_histogram_entries(
        &mut self,
        g: &Graph,
        id: NodeId,
        mine: &[u32],
        incidents: &[IncidentRel],
    ) {
        // The type set is a set by construction: the term is
        // CHUNK-granular (per (label, type)), so the committed out-side
        // records' duplicates absorb at collection time and the ledger
        // probes only distinct keys. The types come through the incident
        // vocabulary, not endpoint equality over the batch's rels (the
        // LABS-515 one enumeration, one side spelling).
        let mut types: HashSet<u32> = g
            .out_chunks(id)
            .into_iter()
            .map(|(type_id, _)| type_id)
            .collect();
        for inc in incidents {
            if inc.is_out() {
                types.insert(inc.type_id);
            }
        }
        for &lid in mine {
            for &t in &types {
                self.charges.charge_hist((lid, t));
            }
        }
    }

    /// The incident half of the AddLabels step (LABS-515:
    /// one step method per phase, `create_node`/`create_rel`'s
    /// granularity): the batch's incident enumeration priced in one
    /// pass — every side of every incident rel prices its own pair keys
    ///: a two-sided incidence, a self-loop (far == id) or a
    /// reciprocal same-type pair, realizes (start-label, type,
    /// end-label) on both sides — and repeated keys charge nothing
    /// twice through the ledger's sets (the previous `seen_far`
    /// early-out keyed (type, far) and skipped the second side's keys
    /// entirely, under-pricing admission).
    ///
    /// A self-loop's far endpoint is the mutated node itself, so its far
    /// set is not a far node's but the node's own, mutation-aware on
    /// both sides — the apply's pass-2 policy, consulted here through
    /// the record's own [`IncidentRel::self_loop_sets`] so the
    /// (joined, type, joined) diagonal is charged once, exactly as
    /// `Graph::self_loop_charge_sets` decides it. Pricing the plain
    /// orientation instead would charge that diagonal on both sides.
    fn price_incidents(
        &mut self,
        g: &Graph,
        id: NodeId,
        mine: &[u32],
        own_out: &[u32],
        own_in: &[u32],
        incidents: &[IncidentRel],
    ) {
        // The pricing pass over the stream: the pair keys follow the
        // rel's own direction ([`IncidentRel::ordered`]). The
        // histogram entries are the step's other phase
        // (`price_histogram_entries`), already priced over the one
        // type set.
        for inc in incidents {
            let (start_ids, end_ids) = if inc.far == id {
                inc.self_loop_sets(mine, own_out, own_in)
            } else {
                let far_ids = &*fill_labels(&mut self.node_labels, g, inc.far);
                // The apply's own orientation — the pair keys follow the
                // rel's direction ([`IncidentRel::ordered`]), the cross is
                // the canonical fold.
                inc.ordered(mine, far_ids)
            };
            self.charges
                .bump_pair_cross(start_ids, inc.type_id, end_ids, CountDelta::Add);
        }
    }

    /// The batch's created rels incident to `id`, in the incident
    /// stream's own record shape and ordering — out side first, then in
    /// side, per rel, matching [`Graph::incident_rels`]: a
    /// batch self-loop (start == end == id) is the mutated
    /// node on BOTH ends, so it enumerates twice — once per side,
    /// exactly as a committed self-loop appears on both adjacency sides
    ///.
    ///
    /// Which rels price is the phase's own decision, stated on
    /// [`BatchRelState`] and passed as the accessor: the histogram
    /// phase prices every rel the batch created
    /// ([`BatchRelState::priced_histogram`]), the pair phase only the
    /// still-live ones ([`BatchRelState::priced_pair`]). A committed
    /// rel the batch deleted is absent in both — the committed stream
    /// ([`Graph::incident_rels`], the pre-batch
    /// store) already carries it, and a second copy here would
    /// double-price it.
    fn batch_incidents(
        &self,
        id: NodeId,
        priced: fn(&BatchRelState) -> Option<&BatchRel>,
    ) -> impl Iterator<Item = IncidentRel> + '_ {
        self.batch_rels
            .values()
            .filter_map(priced)
            .flat_map(move |rel| {
                let out = (id == rel.start).then(|| IncidentRel::outgoing(rel.type_id, rel.end));
                let inc = (id == rel.end).then(|| IncidentRel::incoming(rel.type_id, rel.start));
                out.into_iter().chain(inc)
            })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// LABS-515: the simulator's state transitions, pinned per step —
    /// each op's step moves exactly the fields it owns, and the
    /// dispatcher's enumerated no-op arms move nothing. The growth
    /// figures mirror `stats_entry_growth_estimates_mixed_batches` end
    /// to end.
    #[test]
    fn batch_sim_state_follows_each_step() {
        let g = Graph::default();
        g.begin_write().unwrap();
        let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
        let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
        g.commit_write();
        let a_id = g.interner.get_id("A").unwrap();
        let b_id = g.interner.get_id("B").unwrap();

        let mut sim = BatchSim::new(&g);
        // The initial state: the ledger's sets are seeded with the
        // committed maps' keys (A, B from the committed nodes); the simulated sets
        // and the rels start empty; growth is zero.
        assert!(sim.charges.labels.contains(&a_id) && sim.charges.labels.contains(&b_id));
        assert_eq!(sim.charges.labels.len(), 2);
        assert!(sim.charges.pairs.is_empty());
        assert!(sim.charges.hists.is_empty());
        assert!(sim.node_labels.is_empty());
        assert!(sim.batch_rels.is_empty());
        assert_eq!(sim.charges.growth, 0);

        // CreateRel: the rel joins the simulation, both endpoints'
        // label sets are ensured into it, and the (A, R) histogram
        // entry plus the (A, R, B) pair entry charge.
        sim.step(
            &g,
            &RedoOp::create_rel(1, a, b, "R".to_string(), BTreeMap::new()),
        );
        // "R" is a fresh name in this batch — the simulator's memoized
        // mint is the id the state's rels carry.
        let r_id = sim.fresh_ids.resolve(&g, "R");
        assert_eq!(
            sim.batch_rels,
            HashMap::from([(
                1,
                BatchRelState::Live(BatchRel {
                    start: a,
                    end: b,
                    type_id: r_id
                })
            )])
        );
        assert_eq!(sim.node_labels[&a], vec![a_id]);
        assert_eq!(sim.node_labels[&b], vec![b_id]);
        assert_eq!(
            sim.charges.growth,
            (Graph::HISTOGRAM_ENTRY_BYTES + Graph::PAIR_COUNT_ENTRY_BYTES) as i64
        );

        // AddLabels: the actually-added id joins the node's simulated
        // set and the seen set, and the incident stream prices the
        // in-batch rel's keys — its out side carries the (A2, R)
        // histogram term the chunk walk cannot see.
        sim.step(&g, &RedoOp::add_labels(a, vec!["A2".into()]));
        // "A2" is fresh too — resolve returns the same memoized mint the
        // step used.
        let a2_id = sim.fresh_ids.resolve(&g, "A2");
        assert_eq!(sim.node_labels[&a], vec![a_id, a2_id]);
        assert!(sim.charges.labels.contains(&a2_id));
        assert_eq!(
            sim.charges.growth,
            (2 * Graph::HISTOGRAM_ENTRY_BYTES
                + 2 * Graph::PAIR_COUNT_ENTRY_BYTES
                + Graph::LABEL_COUNT_ENTRY_BYTES) as i64
                // ADR-0589: the set A -> A,A2 is one id wider.
                + std::mem::size_of::<u32>() as i64
        );

        // A label removal prices only its label-set move (its per-label
        // and pair drains stay the deliberate over-estimating omission
        // documented at `BatchSim::step`).
        let before = sim.clone();
        sim.step(
            &g,
            &RedoOp::RemoveLabels {
                id: a,
                labels: vec!["A2".into()],
            },
        );
        // ADR-0589: a removal moves the node between label sets, so it
        // is no longer a no-op: the simulated set follows it, and the
        // set map's drain of the wider set and re-creation of the narrow
        // one price as the apply charges them.
        assert_eq!(sim.node_labels[&a], vec![a_id]);
        assert_eq!(
            sim.charges.growth,
            before.charges.growth - Graph::label_set_entry_bytes(&[a_id, a2_id]) as i64
                + Graph::label_set_entry_bytes(&[a_id]) as i64
        );
    }

    /// LABS-515: the AddLabels step's carried-label discipline — a
    /// requested label the node already carries moves neither the
    /// simulated set nor the pricing (its key already sits in the
    /// ledger's committed seed, so it charges nothing; a fully-carried
    /// request trips the growth guard and ends the step before any
    /// incident pricing).
    #[test]
    fn batch_sim_add_labels_skips_already_carried() {
        let g = Graph::default();
        g.begin_write().unwrap();
        let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
        let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
        // A committed rel incident to `a`, so the incident walk has a
        // far endpoint it would touch — the growth guard's skip stays
        // observable. Committed, not an in-batch CreateRel step: the
        // step pre-ensures both endpoints into the cache, which would
        // mask the footprint.
        g.create_rel(a, b, "R".to_string(), BTreeMap::new())
            .unwrap();
        g.commit_write();
        let a_id = g.interner.get_id("A").unwrap();

        let mut sim = BatchSim::new(&g);
        // The ledger's committed seed already holds every live label
        // key (both nodes'), so the carried label's charge inserts
        // nothing new.
        let seeded_labels = sim.charges.labels.clone();
        sim.step(&g, &RedoOp::add_labels(a, vec!["A".into()]));
        // The set is unchanged, the seed is unchanged (the insert
        // fails, no charge), and the growth guard ends the step before
        // any incident pricing — the far endpoint never enters the
        // simulation's label cache.
        assert_eq!(sim.node_labels[&a], vec![a_id]);
        assert_eq!(sim.charges.labels, seeded_labels);
        assert_eq!(sim.charges.growth, 0);
        assert_eq!(
            sim.node_labels.len(),
            1,
            "the incident walk never ran: the far endpoint never enters the cache"
        );
    }

    /// LABS-1386: the delete arm's two tombstones, by founding — a rel
    /// the batch created and then deleted keeps pricing at the AddLabels
    /// steps (the pre-delete version stays live-looking inside the write
    /// window, and the chunk the create founded prices the histogram
    /// chunk-granularly), so its incidents enumerate exactly as a live
    /// one's; a committed rel the batch deleted does not — the committed
    /// incident stream already carries it, and a batch copy would
    /// double-price it.
    #[test]
    fn batch_dead_rels_price_and_store_dead_rels_do_not() {
        let g = Graph::default();
        let (a, b) = crate::graph::test_support::endpoints(&g);
        g.begin_write().unwrap();
        let committed = g
            .create_rel(a, b, "C".to_string(), BTreeMap::new())
            .unwrap();
        g.commit_write();

        let mut sim = BatchSim::new(&g);
        // The batch creates and deletes its own rel: the tombstone keeps
        // the killed rel whole and priced.
        sim.step(
            &g,
            &RedoOp::create_rel(9, a, b, "R".to_string(), BTreeMap::new()),
        );
        sim.step(&g, &RedoOp::delete_rel(9));
        let r_id = sim.fresh_ids.resolve(&g, "R");
        assert_eq!(
            sim.batch_rels.get(&9),
            Some(&BatchRelState::BatchDead(BatchRel {
                start: a,
                end: b,
                type_id: r_id
            }))
        );
        let priced: Vec<_> = sim
            .batch_incidents(a, BatchRelState::priced_histogram)
            .collect();
        assert_eq!(priced.len(), 1, "one incident, the out side");
        assert!(priced[0].is_out());
        assert_eq!(priced[0].type_id, r_id);
        assert_eq!(priced[0].far, b);
        // The pair phase skips the same dead rel: its hops are
        // tombstoned before an AddLabels applies.
        assert!(
            sim.batch_incidents(a, BatchRelState::priced_pair)
                .next()
                .is_none(),
            "the dead rel prices no live incident"
        );

        // A committed rel the batch deletes: the StoreDead tombstone —
        // the delete arm resolves it (a repeat delete prices nothing),
        // but no incident of it prices through the batch terms.
        sim.step(&g, &RedoOp::delete_rel(committed));
        assert!(matches!(
            sim.batch_rels.get(&committed),
            Some(BatchRelState::StoreDead)
        ));
        let c_id = sim.fresh_ids.resolve(&g, "C");
        assert!(
            !sim.batch_incidents(a, BatchRelState::priced_histogram)
                .any(|inc| inc.type_id == c_id),
            "the committed rel's incident stays with the committed stream"
        );

        // A repeat delete of either tombstone resolves nothing: neither
        // the growth nor the states move.
        let before = sim.clone();
        sim.step(&g, &RedoOp::delete_rel(9));
        sim.step(&g, &RedoOp::delete_rel(committed));
        assert_eq!(sim, before);
    }
}
