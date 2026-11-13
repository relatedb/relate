//! The graph's logical-byte accounting (ADR-0042, ADR-0202): the
//! live-size oracle, the redo-batch admission estimates, and the charge
//! helpers every mutation seam rides. Split out of `graph/mod.rs` (LABS-677).

use std::collections::HashMap;
use std::sync::Arc;

use relate_core::{Node, NodeId, RelId};

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
        n = n.saturating_add(self.out_adj.array_bytes());
        n = n.saturating_add(self.in_adj.array_bytes());
        self.out_adj
            .for_each(|_id, adj| n = n.saturating_add(adj.floor_bytes()));
        self.in_adj
            .for_each(|_id, adj| n = n.saturating_add(adj.floor_bytes()));
        // The sealed side tables (ADR-0551) are retained structure: each
        // entry's compressed chunks and tombstones, the floor ADR-0551
        // prices, plus the slot arrays that hold them — the term the heap
        // census reads as `slot_bytes`.
        n = n.saturating_add(self.sealed_out.array_bytes());
        n = n.saturating_add(self.sealed_in.array_bytes());
        for sealed in [&self.sealed_out, &self.sealed_in] {
            sealed.for_each(|_id, region| n = n.saturating_add(region.floor_bytes()));
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
        let (adj_refusable, adj_spill) = self.estimate_adj_delta(ops);
        delta += adj_refusable + adj_spill;
        spill += adj_spill;
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
    pub(super) fn estimate_adj_delta(&self, ops: &[RedoOp]) -> (i64, i64) {
        let mut out: HashMap<NodeId, Vec<(u32, usize)>> = HashMap::new();
        let mut inn: HashMap<NodeId, Vec<(u32, usize)>> = HashMap::new();
        let mut fresh_ids = FreshIds::new();
        let mut delta = 0i64;
        let slot = adj_slot();
        let overflow = crate::memory::overflow_entry_bytes(adj_value());
        let mut out_span = self.out_adj.span();
        let mut in_span = self.in_adj.span();
        let mut out_side = HashSet::new();
        let mut in_side = HashSet::new();
        // One traversal carries both halves: the create terms, and the
        // batch's own rel identities and pending delete tombstones the
        // delete half prices. A rel created earlier in this batch is
        // invisible to the committed store, so its delete resolves from
        // `created` — only creates already walked are in it, so a delete
        // that precedes its create tombstones nothing, as in the apply path;
        // a rel the store holds comes from its live version.
        let mut created: HashMap<RelId, (NodeId, NodeId, u32)> = HashMap::new();
        let mut pending_out: HashMap<NodeId, HashMap<u32, HashSet<RelId>>> = HashMap::new();
        let mut pending_in: HashMap<NodeId, HashMap<u32, HashSet<RelId>>> = HashMap::new();
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
                    // One op touches both sides by the same rule, so each
                    // side's store, sealed region, span, side-map set and
                    // pending map travel together as one named value and
                    // the loop runs once per endpoint — the pair the
                    // incident and detach walks already iterate as a
                    // slice. The fields name the side's slots because the
                    // two store refs and the three mutable slots have
                    // pairwise-identical types: a tuple position could
                    // swap an out/in pair silently, a field name cannot.
                    struct SideAdmission<'a, 'm> {
                        adj: &'a IdVec<TypedAdj>,
                        sealed: &'a IdVec<SealedAdj>,
                        node: NodeId,
                        span: &'m mut usize,
                        side: &'m mut HashSet<NodeId>,
                        pending: &'m mut HashMap<NodeId, Vec<(u32, usize)>>,
                    }
                    let mut sides = [
                        SideAdmission {
                            adj: &self.out_adj,
                            sealed: &self.sealed_out,
                            node: *start,
                            span: &mut out_span,
                            side: &mut out_side,
                            pending: &mut out,
                        },
                        SideAdmission {
                            adj: &self.in_adj,
                            sealed: &self.sealed_in,
                            node: *end,
                            span: &mut in_span,
                            side: &mut in_side,
                            pending: &mut inn,
                        },
                    ];
                    for side in &mut sides {
                        if !side.adj.contains_key(&side.node) {
                            // The one placement policy, priced here: a dense
                            // growth is committed structure (refusable); a
                            // side-map entry is priced once per node per
                            // batch — this loop can revisit a node across
                            // its ops, so the dedupe is this caller's.
                            match crate::memory::place_in_span(side.span, side.node, slot) {
                                // An id the array cannot index is refused by
                                // the commit's own validation before anything
                                // applies, so it prices nothing.
                                None | Some(crate::memory::SpanPlacement::Inside) => {}
                                Some(crate::memory::SpanPlacement::Grew(bytes)) => {
                                    delta += bytes as i64;
                                }
                                Some(crate::memory::SpanPlacement::SideEntry) => {
                                    if side.side.insert(side.node) {
                                        delta += overflow as i64;
                                    }
                                }
                            }
                        }
                        delta += bump_adj(side.pending, side.node, type_id, || {
                            hop_counts(side.adj, side.sealed, side.node)
                        });
                    }
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
                    pending_out
                        .entry(start)
                        .or_default()
                        .entry(type_id)
                        .or_default()
                        .insert(*id);
                    pending_in
                        .entry(end)
                        .or_default()
                        .entry(type_id)
                        .or_default()
                        .insert(*id);
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
        let (slot_growth, delete_spill) = self.estimate_delete_spill(&pending_out, &pending_in);
        (delta + slot_growth, delete_spill)
    }

    /// The batch's delete terms, as `(refusable, spill)`: every delete
    /// records one tombstone per endpoint that does not hold one yet
    /// (ADR-0551) — a sealed chunk grows its tombstone vec, a never-sealed
    /// type adds its on-demand chunk to the endpoint's region, all reclaimed
    /// at the next checkpoint fold and so spill that never refuses a batch
    /// (ADR-0529) — while a founding delete's dense side-table span is
    /// permanent (ADR-0086) and stays refusable; its side-map entry,
    /// reclaimed with the region, is spill like the rest.
    fn estimate_delete_spill(
        &self,
        pending_out: &HashMap<NodeId, HashMap<u32, HashSet<RelId>>>,
        pending_in: &HashMap<NodeId, HashMap<u32, HashSet<RelId>>>,
    ) -> (i64, i64) {
        let sealed_slot = sealed_slot();
        let sealed_overflow = crate::memory::overflow_entry_bytes(sealed_value());
        let (out_refusable, out_spill) =
            price_side(&self.sealed_out, pending_out, sealed_slot, sealed_overflow);
        let (in_refusable, in_spill) =
            price_side(&self.sealed_in, pending_in, sealed_slot, sealed_overflow);
        (out_refusable + in_refusable, out_spill + in_spill)
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

pub(super) fn hop_counts(
    map: &IdVec<TypedAdj>,
    sealed: &IdVec<SealedAdj>,
    node: NodeId,
) -> Vec<(u32, usize)> {
    map.get(&node)
        .map(|adj| {
            adj.chunk_lens(sealed.get(&node).as_deref())
                .map(|(type_id, delta, sealed)| (type_id, delta + sealed))
                .collect()
        })
        .unwrap_or_default()
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

/// One node's adjacency floor in one direction: the delta hop bytes plus its
/// sealed side-table entry's compressed chunks and tombstones.
pub(super) fn adj_floor(map: &IdVec<TypedAdj>, sealed: &IdVec<SealedAdj>, node: NodeId) -> u64 {
    map.get(&node).map(|adj| adj.floor_bytes()).unwrap_or(0)
        + sealed.get(&node).map_or(0, |region| region.floor_bytes())
}

/// One side's share of the delete terms
/// ([`Graph::estimate_delete_spill`](Graph::estimate_delete_spill)'s phase
/// 2), as the `(refusable, spill)` pair: per node, the region-level terms —
/// the node's dense slot span (permanent, refusable) or its reclaimed
/// side-map entry (spill), and the chunks-vec entry growth across every
/// joining type, one
/// before-and-after term per region, never one per type, or the reserved
/// model's pow2 steps misprice a batch that joins several — then each key's
/// own terms (all fold-reclaimed, spill): a sealed chunk's tombstone-vec
/// growth, or a joining chunk's empty stream and tombstones.
fn price_side(
    sealed: &IdVec<SealedAdj>,
    pending: &HashMap<NodeId, HashMap<u32, HashSet<RelId>>>,
    sealed_slot: u64,
    sealed_overflow: u64,
) -> (i64, i64) {
    let mut span = sealed.span();
    let mut refusable = 0i64;
    let mut spill = 0i64;
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
                    spill += chunk.tombstone_growth_bytes(fresh) as i64;
                }
                None => {
                    joining += 1;
                    spill += crate::adjacency::fresh_sealed_chunk_floor_bytes(ids.len()) as i64;
                }
            }
        }
        // The region-level term, once per node: a founding delete grows the
        // node's slot in the side table — the dense span permanently
        // (refusable), a wide id's side-map entry only until the region
        // empties (spill) — and its chunks-vec entry growth is one
        // before-and-after term over every joining type, never one per
        // type, or the reserved model's pow2 steps misprice a batch that
        // joins several. An existing region prices the same entry growth
        // (zero when nothing joined).
        match region {
            None => {
                // The one placement policy, priced here: a dense slot-span
                // growth is permanent (refusable), a wide id's side-map
                // entry is reclaimed with the region it founds (spill). Each
                // node reaches this arm once (`pending` is keyed by node), so
                // the entry prices unconditionally — the per-batch dedupe is
                // the create loop's, where a node can recur.
                match crate::memory::place_in_span(&mut span, *node, sealed_slot) {
                    None | Some(crate::memory::SpanPlacement::Inside) => {}
                    Some(crate::memory::SpanPlacement::Grew(bytes)) => refusable += bytes as i64,
                    Some(crate::memory::SpanPlacement::SideEntry) => {
                        spill += sealed_overflow as i64;
                    }
                }
                spill += crate::adjacency::fresh_sealed_chunk_entry_bytes(joining) as i64;
            }
            Some(region) => {
                spill += region.chunk_entry_growth_bytes(joining) as i64;
            }
        }
    }
    (refusable, spill)
}

fn bump_adj(
    sim: &mut HashMap<NodeId, Vec<(u32, usize)>>,
    node: NodeId,
    type_id: u32,
    init: impl FnOnce() -> Vec<(u32, usize)>,
) -> i64 {
    let chunks = sim.entry(node).or_insert_with(init);
    let before =
        crate::memory::adjacency_bytes(&chunks.iter().map(|(_, n)| *n).collect::<Vec<_>>());
    if let Some(slot) = chunks.iter_mut().find(|c| c.0 == type_id) {
        slot.1 += 1;
    } else {
        chunks.push((type_id, 1));
    }
    let after = crate::memory::adjacency_bytes(&chunks.iter().map(|(_, n)| *n).collect::<Vec<_>>());
    after as i64 - before as i64
}
