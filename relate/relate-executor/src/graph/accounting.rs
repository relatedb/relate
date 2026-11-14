//! The graph's logical-byte accounting (ADR-0042, ADR-0202): the
//! live-size oracle, the redo-batch admission estimates, and the charge
//! helpers every mutation seam rides. Split out of `graph/mod.rs` (LABS-677).

use std::collections::HashMap;
use std::sync::Arc;

use relate_core::{Node, NodeId, RelId};

use super::adjacency::{AdjSide, ChunkHops};
use super::*;

impl Graph {
    /// Live graph + index logical bytes (ADR-0042, ADR-0202).
    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
            .load(Ordering::Relaxed)
            .saturating_add(self.interner.bytes())
    }

    /// Full walk of retained versions, adjacency, and the interner
    /// (test / `rebuild_indexes` oracle).
    pub fn accounted_bytes(&self) -> u64 {
        self.structure_bytes().saturating_add(self.interner.bytes())
    }

    pub(super) fn structure_bytes(&self) -> u64 {
        let charge = self.index_charge();
        let mut n = 0u64;
        n = n.saturating_add(self.nodes.array_bytes());
        self.nodes.for_each(|_id, vers| {
            n = n.saturating_add(crate::memory::node_chain_bytes(vers.shape()));
            for v in vers.iter() {
                n = n.saturating_add(crate::memory::live_node_account_live(
                    v.live,
                    &self.interner,
                    &charge,
                ));
            }
        });
        n = n.saturating_add(self.rels.array_bytes());
        self.rels.for_each(|_id, vers| {
            n = n.saturating_add(crate::memory::rel_chain_bytes(vers.shape()));
            for v in vers.iter() {
                n = n.saturating_add(crate::memory::live_rel_account_live(v.live));
            }
        });
        // Each side's slot arrays and retained entries: the delta hops,
        // and the sealed side table (ADR-0551) — each entry's compressed
        // chunks and tombstones, the floor ADR-0551 prices, plus the slot
        // arrays that hold them, the term the heap census reads as
        // `slot_bytes`.
        for side in [self.out_side(), self.in_side()] {
            n = n.saturating_add(side.array_bytes());
            side.delta
                .for_each(|_id, adj| n = n.saturating_add(adj.floor_bytes()));
            side.sealed
                .for_each(|_id, region| n = n.saturating_add(region.floor_bytes()));
        }
        // LABS-351 / LABS-370: the degree histograms and the count
        // bundle's charged maps are retained structure — the recount
        // oracle must include them or the incremental entry charges
        // drift from it (the #398 lesson). Each family's term lives
        // with the family (graph/degree_stats.rs,
        // graph/count_state.rs) and composes here, at the oracle's
        // one site, beside the constraint-key term.
        n = n.saturating_add(self.degree_stats_structure_bytes());
        n = n.saturating_add(self.count_state_structure_bytes());
        // ADR-0439: the committed constraint-key map is retained structure
        // too; it carries the bytes each entry was charged, so the recount
        // and the incremental charge cannot drift.
        n = n.saturating_add(self.constraint_keys_structure_bytes());
        n
    }

    pub fn recompute_used(&self) {
        self.used_bytes
            .store(self.structure_bytes(), Ordering::Relaxed);
    }

    /// Live-size delta of `ops`. Nodes are accounted under the charge in
    /// force after `ops`; when that differs from the current charge (DDL in
    /// the batch), every retained version — including ones `ops` tombstone —
    /// is moved from the old charge to the new one. A delete keeps its
    /// record until GC, so it adds nothing. A copy-on-write set adds the
    /// new version.
    pub fn estimate_redo_delta(&self, ops: &[RedoOp]) -> i64 {
        self.estimate_redo_delta_with_constraint_growth(ops, None)
            .total
    }

    /// [`Graph::estimate_redo_delta`] with the constraint-key entry-growth
    /// term optionally supplied by the caller. The group-commit admission
    /// loop classifies each slot once and passes the delta its walk produced,
    /// so the slot's ops are not re-classified here (ADR-0439).
    #[doc(hidden)]
    pub fn estimate_redo_delta_with_constraint_growth(
        &self,
        ops: &[RedoOp],
        constraint_growth: Option<i64>,
    ) -> crate::memory::RedoDelta {
        let before = self.index_charge();
        let after = self.charge_after_ops(ops);
        let crate::memory::RedoDelta {
            total: mut delta,
            mut spill,
        } = crate::memory::estimate_redo_delta(
            &self.interner,
            ops,
            |id| self.node(id),
            |id| self.rel_form(id),
            |id| self.node_chain_shape(id),
            |id| self.rel_chain_shape(id),
            self.nodes.span(),
            self.rels.span(),
            &after,
        );
        let adj = self.estimate_adj_delta(ops);
        delta += adj.refusable + adj.spill;
        spill += adj.spill;
        delta += self.estimate_intern_delta(ops);
        // LABS-370 (security follow-up): the count family's entry growth
        // is committed structure too — an oversized-label write must be
        // rejected at admission, not after its entries land past the cap
        // (graph/degree_stats.rs owns the term).
        delta += self.estimate_stats_entry_growth(ops);
        // ADR-0439: a constrained write's new entries are committed
        // structure too — refused at admission, never after they land. The
        // group path supplies the delta its admission walk already computed.
        delta += match constraint_growth {
            Some(growth) => growth,
            None => self.estimate_constraint_key_entry_growth(ops),
        };
        if before.as_ref() != &after {
            self.nodes.for_each(|id, vers| {
                for v in vers.iter() {
                    let n = v.live.to_node(id, &self.interner);
                    delta += crate::memory::live_node_account(&n, &after) as i64
                        - crate::memory::live_node_account(&n, &before) as i64;
                }
            });
        }
        crate::memory::RedoDelta {
            total: delta,
            spill,
        }
    }

    /// The adjacency share of a batch's live-size delta, split into the
    /// refusable growth — a create's hop and slot growth (dense span or the
    /// side-map entry a wide id takes, as the store's own estimator charges
    /// it), plus a founding delete's dense side-table span — and the spill
    /// the checkpoint fold reclaims: a delete's region, chunk, and tombstone
    /// terms, and the side-map entry a founding delete at a wide id takes
    /// (ADR-0529).
    pub(super) fn estimate_adj_delta(&self, ops: &[RedoOp]) -> AdjDelta {
        let mut out = SideAdmission::new(self.out_side());
        let mut inn = SideAdmission::new(self.in_side());
        let mut fresh_ids = FreshIds::new();
        let mut delta = AdjDelta::default();
        // One traversal carries both halves: the create terms, and the
        // batch's own rel identities and pending delete tombstones the
        // delete half prices. A rel created earlier in this batch is
        // invisible to the committed store, so its delete resolves from
        // `created` — only creates already walked are in it, so a delete
        // that precedes its create tombstones nothing, as in the apply path;
        // a rel the store holds comes from its live version.
        let mut created: HashMap<RelId, (NodeId, NodeId, u32)> = HashMap::new();
        let mut pending_out: PendingTombstones = HashMap::new();
        let mut pending_in: PendingTombstones = HashMap::new();
        for op in ops {
            match op {
                RedoOp::CreateRel {
                    id,
                    start,
                    end,
                    rel_type,
                    ..
                } => {
                    let type_id = fresh_ids.resolve(self, rel_type);
                    created.insert(*id, (*start, *end, type_id));
                    delta.refusable += out.create(*start, type_id) + inn.create(*end, type_id);
                }
                RedoOp::DeleteRel { id } => {
                    let Some((start, end, type_id)) = created
                        .get(id)
                        .copied()
                        // The store's rels answer through the one live-identity
                        // spelling (a dead version's delete tombstones nothing,
                        // as in the apply path).
                        .or_else(|| self.rel_live_identity(*id))
                    else {
                        continue;
                    };
                    for (pending, node) in [(&mut pending_out, start), (&mut pending_in, end)] {
                        pending
                            .entry(node)
                            .or_default()
                            .entry(type_id)
                            .or_default()
                            .insert(*id);
                    }
                }
                _ => {}
            }
        }
        // Each delete records one tombstone per endpoint that does not hold
        // one yet (ADR-0551); the region, chunk, and tombstone terms are
        // reclaimed at the checkpoint fold, so they are spill and never
        // refuse a batch (ADR-0529: a delete at the cap commits). The
        // founding side-table slot is the exception — the dense span only
        // grows (ADR-0086) — so it stays refusable.
        delta + self.estimate_delete_spill(&pending_out, &pending_in)
    }

    /// The batch's delete terms: every delete
    /// records one tombstone per endpoint that does not hold one yet
    /// (ADR-0551) — a sealed chunk grows its tombstone vec, a never-sealed
    /// type adds its on-demand chunk to the endpoint's region, all reclaimed
    /// at the next checkpoint fold and so spill that never refuses a batch
    /// (ADR-0529) — while a founding delete's dense side-table span is
    /// permanent (ADR-0086) and stays refusable; its side-map entry,
    /// reclaimed with the region, is spill like the rest.
    fn estimate_delete_spill(
        &self,
        pending_out: &PendingTombstones,
        pending_in: &PendingTombstones,
    ) -> AdjDelta {
        let sealed_slot = sealed_slot();
        let sealed_overflow = crate::memory::overflow_entry_bytes(sealed_value());
        price_side(
            self.out_side().sealed,
            pending_out,
            sealed_slot,
            sealed_overflow,
        ) + price_side(
            self.in_side().sealed,
            pending_in,
            sealed_slot,
            sealed_overflow,
        )
    }

    fn estimate_intern_delta(&self, ops: &[RedoOp]) -> i64 {
        let mut seen = HashSet::new();
        let mut extra = 0i64;
        let mut consider = |s: &str| {
            if !seen.insert(s.to_string()) {
                return;
            }
            if self.interner.get_id(s).is_none() {
                extra += crate::memory::interned_string_bytes(s.len()) as i64;
            }
        };
        for op in ops {
            match op {
                RedoOp::CreateNode {
                    labels, properties, ..
                } => {
                    for label in labels.iter() {
                        consider(label);
                    }
                    for key in properties.keys() {
                        consider(key);
                    }
                }
                RedoOp::CreateRel {
                    rel_type,
                    properties,
                    ..
                } => {
                    consider(rel_type);
                    for key in properties.keys() {
                        consider(key);
                    }
                }
                RedoOp::SetNodeProp { key, .. }
                | RedoOp::RemoveNodeProp { key, .. }
                | RedoOp::SetRelProp { key, .. }
                | RedoOp::RemoveRelProp { key, .. } => consider(key),
                RedoOp::AddLabels { labels, .. } | RedoOp::RemoveLabels { labels, .. } => {
                    for label in labels {
                        consider(label);
                    }
                }
                _ => {}
            }
        }
        extra
    }

    pub(super) fn index_charge(&self) -> Arc<crate::memory::IndexCharge> {
        Arc::clone(&self.indexes.charge_cache.lock().unwrap())
    }

    pub(super) fn refresh_index_charge(&self) {
        let charge = {
            let theater = self.theater.lock().unwrap();
            Arc::new(crate::memory::IndexCharge::from_details(
                theater.index_details.values(),
            ))
        };
        *self.indexes.charge_cache.lock().unwrap() = charge;
    }

    fn charge_after_ops(&self, ops: &[RedoOp]) -> crate::memory::IndexCharge {
        let theater = self.theater.lock().unwrap();
        let mut details = theater.index_details.clone();
        drop(theater);
        crate::redo::project_index_details(&mut details, ops);
        crate::memory::IndexCharge::from_details(details.values())
    }

    pub(super) fn charge(&self, delta: i64) {
        if delta > 0 {
            self.used_bytes.fetch_add(delta as u64, Ordering::Relaxed);
        } else if delta < 0 {
            let sub = (-delta) as u64;
            atomic_fetch_update!(self.used_bytes, Ordering::Relaxed, Ordering::Relaxed, |u| {
                Some(u.saturating_sub(sub))
            })
            .expect("the saturating-sub closure never rejects");
        }
    }

    pub(super) fn charge_node_swap(&self, before: Option<&Node>, after: Option<&Node>) {
        let charge = self.index_charge();
        let b = before
            .map(|n| crate::memory::live_node_account(n, &charge))
            .unwrap_or(0);
        let a = after
            .map(|n| crate::memory::live_node_account(n, &charge))
            .unwrap_or(0);
        self.charge(a as i64 - b as i64);
    }

    pub(super) fn charge_rel_after_edit(
        &self,
        id: RelId,
        before_bytes: u64,
        before_shape: ChainShape,
    ) {
        let after_shape = self.rel_chain_shape(id);
        let after_bytes = self.visible_rel_account(id);
        if after_shape != before_shape {
            let growth = crate::memory::rel_chain_bytes(after_shape) as i64
                - crate::memory::rel_chain_bytes(before_shape) as i64;
            self.charge(after_bytes as i64 + growth);
        } else {
            self.charge(after_bytes as i64 - before_bytes as i64);
        }
    }

    #[cfg(test)]
    pub(super) fn node_chain_len(&self, id: NodeId) -> usize {
        self.nodes.get(&id).map_or(0, |v| v.len())
    }

    pub(super) fn node_chain_shape(&self, id: NodeId) -> ChainShape {
        self.nodes
            .get(&id)
            .map_or(ChainShape::Absent, |v| v.shape())
    }

    pub(super) fn rel_chain_shape(&self, id: RelId) -> ChainShape {
        self.rels.get(&id).map_or(ChainShape::Absent, |v| v.shape())
    }

    /// Copy-on-write keeps the previous version. An in-place edit replaces it.
    pub(super) fn charge_node_after_edit(
        &self,
        id: NodeId,
        before: Option<&Node>,
        before_shape: ChainShape,
    ) {
        let after = self.node(id);
        let after_shape = self.node_chain_shape(id);
        if after_shape != before_shape {
            let charge = self.index_charge();
            let added = after
                .as_ref()
                .map(|n| crate::memory::live_node_account(n, &charge))
                .unwrap_or(0);
            let growth = crate::memory::node_chain_bytes(after_shape) as i64
                - crate::memory::node_chain_bytes(before_shape) as i64;
            self.charge(added as i64 + growth);
        } else {
            self.charge_node_swap(before, after.as_ref());
        }
    }
}

/// The admission estimates' fresh-name resolver (LABS-425, stellar round
/// 1): a name the interner does not know — a label or rel type the
/// not-yet-applied batch mints — resolves to a descending id below the
/// committed range, one id per distinct name. Both estimate terms
/// ([`Graph::estimate_adj_delta`] here,
/// [`Graph::estimate_stats_entry_growth`] in
/// graph/degree_stats_estimate.rs) resolved this ladder by hand before;
/// the descending-id collision invariant (fresh ids never collide with
/// committed ids or each other) now lives in exactly one place.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FreshIds {
    fresh: HashMap<String, u32>,
    next: u32,
}

impl FreshIds {
    pub(super) fn new() -> Self {
        Self {
            fresh: HashMap::new(),
            next: u32::MAX,
        }
    }

    /// The interned id, else this batch's earlier mint, else a new
    /// descending id minted and memoized.
    pub(super) fn resolve(&mut self, g: &Graph, name: &str) -> u32 {
        g.interner
            .get_id(name)
            .or_else(|| self.fresh.get(name).copied())
            .unwrap_or_else(|| {
                let id = self.next;
                self.next = self.next.saturating_sub(1);
                self.fresh.insert(name.to_string(), id);
                id
            })
    }
}

/// A batch's pending delete tombstones on one side: per node, per rel
/// type, the rel ids that will record one.
type PendingTombstones = HashMap<NodeId, HashMap<u32, HashSet<RelId>>>;

/// An adjacency estimate's two shares (ADR-0529): `refusable` growth is
/// committed structure an over-cap batch is refused for; `spill` is what the
/// checkpoint fold reclaims, so it never refuses a batch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct AdjDelta {
    pub(super) refusable: i64,
    pub(super) spill: i64,
}

impl std::ops::Add for AdjDelta {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            refusable: self.refusable + other.refusable,
            spill: self.spill + other.spill,
        }
    }
}

/// What one [`place_in_span`](crate::memory::place_in_span) answer charges
/// — the one mapping from the placement policy's result to bytes, shared by
/// the create loop and the delete phase: `dense` growth is permanent
/// structure, `side_entry` the reclaimable entry a wide id takes. An id the
/// array cannot index is refused by the commit's own validation before
/// anything applies, so it prices nothing.
struct PlacementCharge {
    dense: i64,
    side_entry: bool,
}

fn place(span: &mut usize, node: NodeId, slot: u64) -> PlacementCharge {
    match crate::memory::place_in_span(span, node, slot) {
        None | Some(crate::memory::SpanPlacement::Inside) => PlacementCharge {
            dense: 0,
            side_entry: false,
        },
        Some(crate::memory::SpanPlacement::Grew(bytes)) => PlacementCharge {
            dense: bytes as i64,
            side_entry: false,
        },
        Some(crate::memory::SpanPlacement::SideEntry) => PlacementCharge {
            dense: 0,
            side_entry: true,
        },
    }
}

/// One side's create-admission state: the committed pair, the dense span the
/// batch has grown so far, the wide ids already priced, and the simulated
/// chunk lists the batch's creates have built. One op touches both sides by
/// the same rule, so the loop calls [`SideAdmission::create`] once per
/// endpoint.
struct SideAdmission<'a> {
    side: AdjSide<'a>,
    span: usize,
    wide: HashSet<NodeId>,
    pending: HashMap<NodeId, Vec<ChunkHops>>,
    slot: u64,
    overflow: u64,
}

impl<'a> SideAdmission<'a> {
    fn new(side: AdjSide<'a>) -> Self {
        Self {
            span: side.delta.span(),
            side,
            wide: HashSet::new(),
            pending: HashMap::new(),
            slot: adj_slot(),
            overflow: crate::memory::overflow_entry_bytes(adj_value()),
        }
    }

    /// The refusable bytes one created rel adds at `node` on this side: the
    /// node's slot when it has no adjacency entry yet, and the hop growth.
    fn create(&mut self, node: NodeId, type_id: u32) -> i64 {
        let mut bytes = 0;
        if !self.side.delta.contains_key(&node) {
            // A dense growth is committed structure (refusable); a
            // side-map entry is priced once per node per batch — the loop
            // can revisit a node across its ops, so the dedupe is here.
            let placed = place(&mut self.span, node, self.slot);
            bytes += placed.dense;
            if placed.side_entry && self.wide.insert(node) {
                bytes += self.overflow as i64;
            }
        }
        bytes
            + bump_adj(&mut self.pending, node, type_id, || {
                self.side.hop_counts(node)
            })
    }
}

/// One side's share of the delete terms
/// ([`Graph::estimate_delete_spill`](Graph::estimate_delete_spill)'s phase
/// 2): per node, the region-level terms —
/// the node's dense slot span (permanent, refusable) or its reclaimed
/// side-map entry (spill), and the chunks-vec entry growth across every
/// joining type, one
/// before-and-after term per region, never one per type, or the reserved
/// model's pow2 steps misprice a batch that joins several — then each key's
/// own terms (all fold-reclaimed, spill): a sealed chunk's tombstone-vec
/// growth, or a joining chunk's empty stream and tombstones.
fn price_side(
    sealed: &IdVec<SealedAdj>,
    pending: &PendingTombstones,
    sealed_slot: u64,
    sealed_overflow: u64,
) -> AdjDelta {
    let mut span = sealed.span();
    let mut priced = AdjDelta::default();
    for (node, keys) in pending {
        // One key loop prices every key's own term: a chunk the region
        // already holds grows its tombstone vec (a delete after a
        // reposition updates the existing slot in place), a type the
        // region does not hold joins it, bringing the empty stream and
        // its tombstones — priced once, here, never again in a
        // region-level arm.
        let region = sealed.get(node);
        let mut joining = 0;
        for (type_id, ids) in keys {
            match region.as_ref().and_then(|region| region.chunk(*type_id)) {
                Some(chunk) => {
                    let fresh = chunk.fresh_tombstone_count(ids);
                    priced.spill += chunk.tombstone_growth_bytes(fresh) as i64;
                }
                None => {
                    joining += 1;
                    priced.spill +=
                        crate::adjacency::fresh_sealed_chunk_floor_bytes(ids.len()) as i64;
                }
            }
        }
        // The region-level term, once per node: a founding delete grows the
        // node's slot in the side table — the dense span permanently
        // (refusable), a wide id's side-map entry only until the region
        // empties (spill) — and its chunks-vec entry growth is one
        // before-and-after term over every joining type. An existing region
        // prices the same entry growth (zero when nothing joined).
        match region {
            None => {
                // Each node reaches this arm once (`pending` is keyed by
                // node), so the entry prices unconditionally — the
                // per-batch dedupe is the create loop's, where a node can
                // recur.
                let placed = place(&mut span, *node, sealed_slot);
                priced.refusable += placed.dense;
                if placed.side_entry {
                    priced.spill += sealed_overflow as i64;
                }
                priced.spill += crate::adjacency::fresh_sealed_chunk_entry_bytes(joining) as i64;
            }
            Some(region) => {
                priced.spill += region.chunk_entry_growth_bytes(joining) as i64;
            }
        }
    }
    priced
}

fn bump_adj(
    sim: &mut HashMap<NodeId, Vec<ChunkHops>>,
    node: NodeId,
    type_id: u32,
    init: impl FnOnce() -> Vec<ChunkHops>,
) -> i64 {
    let chunks = sim.entry(node).or_insert_with(init);
    let bytes = |chunks: &[ChunkHops]| {
        crate::memory::adjacency_bytes(&chunks.iter().map(|c| c.hops).collect::<Vec<_>>())
    };
    let before = bytes(chunks);
    if let Some(slot) = chunks.iter_mut().find(|c| c.type_id == type_id) {
        slot.hops += 1;
    } else {
        chunks.push(ChunkHops { type_id, hops: 1 });
    }
    bytes(chunks) as i64 - before as i64
}
