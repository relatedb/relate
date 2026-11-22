//! The graph's logical-byte accounting (ADR-0042, ADR-0202): the
//! live-size oracle, the redo-batch admission estimates, and the charge
//! helpers every mutation seam rides. Split out of `graph/mod.rs` (LABS-677).

use std::sync::Arc;

use relate_core::{Node, NodeId, RelId};

use super::*;

pub(super) mod adjacency;

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

    /// The committed dense id spans — a group admission walk's seed
    /// ([`DenseSpans`](crate::memory::DenseSpans)): its first slot
    /// estimates against these and the walk carries the values each
    /// admitted slot advanced them to. Every dense span the admission
    /// estimate prices rides here: the node and relationship slot arrays,
    /// the adjacency tier's two directions, and the sealed span table's
    /// two.
    #[doc(hidden)]
    pub fn dense_spans(&self) -> crate::memory::DenseSpans {
        crate::memory::DenseSpans {
            node: self.nodes.span(),
            rel: self.rels.span(),
            adj_out: self.out_side().delta().span(),
            adj_in: self.in_side().delta().span(),
            sealed_out: self.out_side().sealed().span(),
            sealed_in: self.in_side().sealed().span(),
        }
    }

    /// Live-size delta of `ops`, its spill reported separately
    /// ([`RedoDelta::spill`](crate::memory::RedoDelta::spill)). Nodes are
    /// accounted under the charge in force after `ops`; when that differs
    /// from the current charge (DDL in the batch), every retained version —
    /// including ones `ops` tombstone — is moved from the old charge to the
    /// new one. A delete keeps its record until GC, so it adds nothing. A
    /// copy-on-write set adds the new version.
    ///
    /// The one single-record entry point: it seeds from the committed spans
    /// and derives the ADR-0439 term itself, so no caller re-spells that
    /// order — a caller holding a seed (the group admission walk) instead
    /// passes its own term to [`Graph::estimate_redo_delta_with_constraint_growth`].
    #[doc(hidden)]
    pub fn estimate_redo_delta(&self, ops: &[RedoOp]) -> crate::memory::RedoDelta {
        let growth = self.estimate_constraint_key_entry_growth(ops);
        self.estimate_redo_delta_with_constraint_growth(ops, growth, self.dense_spans())
            .0
    }

    /// [`Graph::estimate_redo_delta`] with the constraint-key entry-growth
    /// term and the dense-span seed supplied by the caller. The group-commit
    /// admission loop classifies each slot once and passes the delta its walk
    /// produced, so the slot's ops are not re-classified here (ADR-0439).
    /// `spans` is the seed — the walk's advanced dense spans for a group (its
    /// slots price against what the group already reserved), the committed
    /// ones for a single-record caller — and the advanced spans come back OUT
    /// beside the delta, never written through the seed: the caller keeps them
    /// only for an accepted slot, so a refused estimate leaves no phantom rung
    /// behind.
    #[doc(hidden)]
    pub fn estimate_redo_delta_with_constraint_growth(
        &self,
        ops: &[RedoOp],
        constraint_growth: i64,
        spans: crate::memory::DenseSpans,
    ) -> (crate::memory::RedoDelta, crate::memory::DenseSpans) {
        let before = self.index_charge();
        let after = self.charge_after_ops(ops);
        let (
            crate::memory::RedoDelta {
                total: mut delta,
                mut spill,
            },
            chain,
        ) = crate::memory::estimate_redo_delta(
            &self.interner,
            ops,
            |id| self.node(id),
            |id| self.rel_form(id),
            |id| self.node_chain_shape(id),
            |id| self.rel_chain_shape(id),
            |s| crate::memory::chain_bytes(s, node_version_stride()),
            |s| crate::memory::chain_bytes(s, rel_version_stride()),
            spans,
            &after,
        );
        let (adj, adj_spans) = self.estimate_adj_delta(ops, &spans);
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
        // caller supplies the term: derived by
        // [`Graph::estimate_constraint_key_entry_growth`] for a single-record
        // caller,
        // or the delta the group admission walk already computed.
        delta += constraint_growth;
        if before.as_ref() != &after {
            self.nodes.for_each(|id, vers| {
                for v in vers.iter() {
                    let n = v.live.to_node(id, &self.interner);
                    delta += crate::memory::live_node_account(&n, &after) as i64
                        - crate::memory::live_node_account(&n, &before) as i64;
                }
            });
        }
        (
            crate::memory::RedoDelta {
                total: delta,
                spill,
            },
            // The one merge of the two advances: the chain spans from the
            // memory walk, the adjacency and sealed spans from this one — a
            // seed advanced by every dense-span family the estimate prices,
            // each walk owning exactly its fields.
            crate::memory::DenseSpans {
                node: chain.node,
                rel: chain.rel,
                adj_out: adj_spans.adj_out,
                adj_in: adj_spans.adj_in,
                sealed_out: adj_spans.sealed_out,
                sealed_in: adj_spans.sealed_in,
            },
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
