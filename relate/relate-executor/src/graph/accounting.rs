//! The graph's logical-byte accounting (ADR-0042, ADR-0202): the
//! live-size oracle, the redo-batch admission estimates, and the charge
//! helpers every mutation seam rides. Split out of `graph/mod.rs` (LABS-677).

use std::collections::HashMap;
use std::mem::size_of;
use std::sync::Arc;

use relate_core::{Error, Node, NodeId, RelId};

use super::adjacency::{AdjSide, ChunkHops};
use super::*;
use crate::adjacency::{AdjHop, SealedRegion, Supersede, MAX_SPAN_TYPES};

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
            n = n.saturating_add(crate::memory::chain_bytes(
                vers.shape(),
                node_version_stride(),
            ));
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
            n = n.saturating_add(crate::memory::chain_bytes(
                vers.shape(),
                rel_version_stride(),
            ));
            for v in vers.iter() {
                n = n.saturating_add(crate::memory::live_rel_account_live(v.live));
            }
        });
        // Each side's slot arrays and retained delta entries, and the sealed
        // arenas (ADR-0551 / ADR-0599) — the arena bytes, the 12-byte
        // entries, the tombstones, and the dense span tables that index
        // them — the term the heap census reads as `slot_bytes`.
        for side in [self.out_side(), self.in_side()] {
            n = n.saturating_add(side.delta().array_bytes());
            side.delta()
                .for_each(|_id, adj| n = n.saturating_add(adj.floor_bytes()));
        }
        n = n.saturating_add(self.sealed_floor());
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
            |s| crate::memory::chain_bytes(s, node_version_stride()),
            |s| crate::memory::chain_bytes(s, rel_version_stride()),
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
        delta + self.estimate_delete_spill(&pending_out, &pending_in, Supersede::Deleted)
    }

    /// The adjacency tier's structural limits (ADR-0599), the ones its
    /// field widths state: a node holds at most [`MAX_SPAN_TYPES`] distinct
    /// rel types in one direction (the span length's `u16`), and one
    /// direction's arena at most `u32::MAX` stream bytes (the entry
    /// offset's `u32`). Enforced here, at admission, before the redo is
    /// logged — the tier's own mutation paths are infallible by design, so
    /// the wall they must never hit is refused upstream. A delete never
    /// breaches either: it founds a sealed entry only for a type the node
    /// already holds, and its tombstone terms reclaim at the fold.
    ///
    /// `earlier` are the ops of slots this group commit already accepted:
    /// the type wall is per group, not per slot — two slots that each add
    /// types under the bound must be refused when their combined types
    /// cross it, so this slot's creates are counted on top of the group's.
    /// Each earlier slot passed the same walk at its own admission, so the
    /// combined walk never fails on the earlier batches alone.
    pub fn enforce_adjacency_limits(&self, ops: &[RedoOp], earlier: &[&[RedoOp]]) -> Result<()> {
        let mut fresh_ids = FreshIds::new();
        let mut out = TypeWall::new(self.out_side());
        let mut inn = TypeWall::new(self.in_side());
        // A second accumulator beside `SideAdmission`, stated: the byte
        // estimate is per slot and only runs under a configured size limit,
        // while the wall is per group and unconditional — the two walks
        // cannot share a gate.
        let mut growth = 0u64;
        for batch in earlier.iter().copied().chain(std::iter::once(ops)) {
            for op in batch {
                if let RedoOp::CreateRel {
                    start,
                    end,
                    rel_type,
                    ..
                } = op
                {
                    let type_id = fresh_ids.resolve(self, rel_type);
                    out.record(*start, type_id)?;
                    inn.record(*end, type_id)?;
                    // One stored hop per endpoint reaches an arena at the
                    // fold. A delete adds none: its tombstones live in the
                    // entry column, never in the arena's byte buffer, so a
                    // delete is never refused for a bound it shrinks toward
                    // (ADR-0529's rule for the byte cap, on the field bound).
                    growth = growth.saturating_add(2 * size_of::<AdjHop>() as u64);
                }
            }
        }
        // The arena wall, per direction — each arena's own streams plus this
        // batch's sealed-tier growth against its own offset field. The
        // store's whole live charge is deliberately not compared: it prices
        // structures no arena holds, and refusing on it would freeze a store
        // whose live bytes sit above the bound — deletes included. A fold
        // the arena itself refuses degrades instead (see `put_chunk`), so
        // this wall is the early, loud spelling of the same bound.
        for arena_bytes in self
            .sides(relate_ir::Direction::Both)
            .map(|(side, _)| side.sealed().arena_bytes())
        {
            if sealed_arena_breaches(arena_bytes, growth) {
                return Err(Error::capacity_sealed_arena(
                    arena_bytes.saturating_add(growth),
                    u32::MAX as u64,
                ));
            }
        }
        Ok(())
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
        supersede: Supersede,
    ) -> AdjDelta {
        let sealed_slot = sealed_slot();
        let sealed_overflow = crate::memory::overflow_entry_bytes(sealed_slot);
        price_side(
            self.out_side().sealed(),
            pending_out,
            sealed_slot,
            sealed_overflow,
            supersede,
        ) + price_side(
            self.in_side().sealed(),
            pending_in,
            sealed_slot,
            sealed_overflow,
            supersede,
        )
    }

    /// One write event's sealed-tier price on both endpoints' sides — the
    /// per-event spelling of [`price_side`], the one formula the admission
    /// estimate prices whole batches with. A delete records its tombstone on
    /// demand, founding the entry when the region holds none (ADR-0551); a
    /// reposition supersedes only a position a sealed chunk holds, so its
    /// missing-chunk arm prices nothing. Priced before the event lands, so
    /// the counts the terms read are the pre-mutation ones.
    pub(super) fn price_sealed_event(
        &self,
        start: NodeId,
        end: NodeId,
        type_id: u32,
        id: RelId,
        supersede: Supersede,
    ) -> (i64, i64) {
        // The one-event spelling of [`price_side`]: the per-key and per-node
        // bodies called once per endpoint, no nested batch maps.
        let ids = HashSet::from([id]);
        let sealed_slot = sealed_slot();
        let sealed_overflow = crate::memory::overflow_entry_bytes(sealed_slot);
        let mut refusable = 0i64;
        let mut spill = 0i64;
        for (sealed, node) in [
            (self.out_side().sealed(), start),
            (self.in_side().sealed(), end),
        ] {
            let region = sealed.region(node);
            let (mut entries, mut tombstones) = sealed.entry_counts();
            spill += price_sealed_key(
                region,
                type_id,
                &ids,
                &mut entries,
                &mut tombstones,
                supersede,
            );
            let mut span = sealed.span();
            let (node_refusable, node_spill) = price_sealed_node(
                &mut span,
                region,
                node,
                sealed_slot,
                sealed_overflow,
                supersede,
            );
            refusable += node_refusable;
            spill += node_spill;
        }
        (refusable, spill)
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
            let growth = crate::memory::chain_bytes(after_shape, rel_version_stride()) as i64
                - crate::memory::chain_bytes(before_shape, rel_version_stride()) as i64;
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
            let growth = crate::memory::chain_bytes(after_shape, node_version_stride()) as i64
                - crate::memory::chain_bytes(before_shape, node_version_stride()) as i64;
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
            span: side.delta().span(),
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
        if !self.side.delta().contains_key(&node) {
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

/// One direction's share of the span-length limit
/// ([`Graph::enforce_adjacency_limits`]): per touched node, the distinct
/// types it already holds and the new types this batch would add. Held is
/// the sealed span's own entries unioned with the delta chunks — the same
/// vocabulary the span length counts, never the hop-bearing chunk list
/// alone (a type whose delta chunk a fold's fully-reclaimed arm removed
/// keeps its sealed entry, so `hop_counts` would undercount it). A type
/// the node holds is free; a fresh one counts against
/// [`MAX_SPAN_TYPES`].
struct TypeWall<'a> {
    side: AdjSide<'a>,
    nodes: HashMap<NodeId, (HashSet<u32>, HashSet<u32>)>,
}

impl<'a> TypeWall<'a> {
    fn new(side: AdjSide<'a>) -> Self {
        Self {
            side,
            nodes: HashMap::new(),
        }
    }

    /// One created rel's `(node, type)` pair. The held set is read once
    /// per touched node; the fresh set dedupes a batch that repeats a new
    /// type on the same node.
    fn record(&mut self, node: NodeId, type_id: u32) -> Result<()> {
        let (held, fresh) = self.nodes.entry(node).or_insert_with(|| {
            let mut held: HashSet<u32> = self
                .side
                .sealed()
                .region(node)
                .map(|r| r.type_ids().into_iter().collect())
                .unwrap_or_default();
            if let Some(adj) = self.side.delta().get(&node) {
                held.extend(adj.chunks.iter().map(|c| c.type_id));
            }
            (held, HashSet::new())
        });
        if !held.contains(&type_id) && fresh.insert(type_id) {
            let count = held.len() + fresh.len();
            if count > MAX_SPAN_TYPES {
                return Err(Error::capacity_adjacency_types(
                    count as u64,
                    MAX_SPAN_TYPES as u64,
                ));
            }
        }
        Ok(())
    }
}

/// The entry-offset bound ([`Graph::enforce_adjacency_limits`]): one
/// arena's own stream bytes plus the batch's growth stay under the `u32`
/// the entry offsets address. Saturating so an over-bound store refuses,
/// never wraps.
pub(super) fn sealed_arena_breaches(arena_bytes: u64, growth: u64) -> bool {
    arena_bytes.saturating_add(growth) > u32::MAX as u64
}

impl Graph {
    /// The sealed tier's whole reserved heap, both directions — the recount
    /// oracle's term ([`Graph::structure_bytes`]); the write path charges
    /// its events per region through [`Graph::price_sealed_event`] instead
    /// (ADR-0599).
    pub(super) fn sealed_floor(&self) -> u64 {
        let mut n = 0u64;
        for sealed in self
            .sides(relate_ir::Direction::Both)
            .map(|(side, _)| side.sealed())
        {
            n = n.saturating_add(sealed.array_bytes());
            n = n.saturating_add(sealed.floor_bytes());
        }
        n
    }
}

/// One side's share of the sealed-tier terms
/// ([`Graph::estimate_delete_spill`](Graph::estimate_delete_spill)'s phase
/// 2, and the write path's per-event spelling
/// [`Graph::price_sealed_event`]): per
/// node, the dense span-table growth (permanent, refusable), the entry-array
/// growth across every joining type — one before-and-after term per region,
/// never one per type, or the reserved model's pow2 steps misprice a batch
/// that joins several — then each key's own terms (all fold-reclaimed,
/// spill): a sealed chunk's tombstone-vec growth, or a joining chunk's
/// tombstones. `supersede` selects the missing-chunk arm: a delete records
/// its tombstone on demand and so founds the entry (ADR-0551); a
/// reposition only supersedes a position a sealed chunk holds
/// ([`AdjSide::reposition`]'s delta-only arm writes no tombstone), so a
/// missing chunk prices nothing.
fn price_side(
    sealed: &SealedArena,
    pending: &PendingTombstones,
    sealed_slot: u64,
    sealed_overflow: u64,
    supersede: Supersede,
) -> AdjDelta {
    let mut span = sealed.span();
    let (mut entries, mut tombstones) = sealed.entry_counts();
    let mut priced = AdjDelta::default();
    for (node, keys) in pending {
        // One key loop prices every key's own term: a chunk the region
        // already holds grows its tombstone vec (a delete after a
        // reposition updates the existing slot in place), a type the
        // region does not hold joins it, inserting its zero-hop entry and
        // bringing its tombstones — priced once, here, never again in a
        // region-level arm.
        let region = sealed.region(*node);
        for (type_id, ids) in keys {
            priced.spill += price_sealed_key(
                region,
                *type_id,
                ids,
                &mut entries,
                &mut tombstones,
                supersede,
            );
        }
        // The region-level term, once per node: a founding delete grows the
        // node's dense span permanently (refusable). Each node reaches this
        // arm once (`pending` is keyed by node), so the entry prices
        // unconditionally — the per-batch dedupe is the create loop's, where
        // a node can recur.
        let (node_refusable, node_spill) = price_sealed_node(
            &mut span,
            region,
            *node,
            sealed_slot,
            sealed_overflow,
            supersede,
        );
        priced.refusable += node_refusable;
        priced.spill += node_spill;
    }
    priced
}

/// One `(node, key)` entry's sealed-tier spill terms — the per-key body
/// [`price_side`] loops and [`Graph::price_sealed_event`] calls once per
/// endpoint. `entries`/`tombstones` are the running entry-column counts a
/// joining chunk grows.
fn price_sealed_key(
    region: Option<SealedRegion<'_>>,
    type_id: u32,
    ids: &HashSet<RelId>,
    entries: &mut usize,
    tombstones: &mut usize,
    supersede: Supersede,
) -> i64 {
    match region.filter(|r| r.has_chunk(type_id)) {
        Some(region) => {
            let fresh = region.fresh_tombstone_count(type_id, ids);
            region.tombstone_growth_bytes(type_id, fresh) as i64
        }
        None if supersede == Supersede::Deleted => {
            let mut spill = crate::adjacency::fresh_sealed_chunk_floor_bytes(ids.len()) as i64;
            spill += crate::adjacency::entry_column_growth(*entries, *tombstones, 1) as i64;
            *entries += 1;
            *tombstones += 1;
            spill
        }
        None => 0,
    }
}

/// One node's founding-delete span term — the per-node body [`price_side`]
/// and [`Graph::price_sealed_event`] share: the dense span a founding delete
/// grows (refusable) or the side-map entry it takes (spill, reclaimed with
/// the region it founds).
fn price_sealed_node(
    span: &mut usize,
    region: Option<SealedRegion<'_>>,
    node: NodeId,
    sealed_slot: u64,
    sealed_overflow: u64,
    supersede: Supersede,
) -> (i64, i64) {
    if region.is_none() && supersede == Supersede::Deleted {
        match crate::memory::place_in_span(span, node, sealed_slot) {
            None | Some(crate::memory::SpanPlacement::Inside) => {}
            Some(crate::memory::SpanPlacement::Grew(bytes)) => return (bytes as i64, 0),
            Some(crate::memory::SpanPlacement::SideEntry) => return (0, sealed_overflow as i64),
        }
    }
    (0, 0)
}

fn bump_adj(
    sim: &mut HashMap<NodeId, Vec<ChunkHops>>,
    node: NodeId,
    type_id: u32,
    init: impl FnOnce() -> Vec<ChunkHops>,
) -> i64 {
    let chunks = sim.entry(node).or_insert_with(init);
    let before = crate::adjacency::adjacency_bytes(chunks.iter().map(|c| c.hops));
    if let Some(slot) = chunks.iter_mut().find(|c| c.type_id == type_id) {
        slot.hops += 1;
    } else {
        chunks.push(ChunkHops { type_id, hops: 1 });
    }
    crate::adjacency::adjacency_bytes(chunks.iter().map(|c| c.hops)) as i64 - before as i64
}
