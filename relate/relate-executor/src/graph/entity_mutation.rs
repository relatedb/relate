//! The entity-mutation family every cross-cutting feature hooks: the create /
//! install / copy-on-write / property-edit / delete seams for nodes and
//! relationships, in their own impl-block file (the `label_mutation.rs` and
//! `degree_stats.rs` precedent, a sibling of the graph family). `Graph`
//! (graph/mod.rs) keeps the struct and the family-independent plumbing
//! (ids, pins, charging, catalog reads); the seams that install a version,
//! maintain the indexes/adjacency/counts, and push redo live here.

use std::ops::ControlFlow;

use super::id_vec::IdVec;
use super::*;
use crate::adjacency::Supersede;

impl Graph {
    pub fn create_node(
        &self,
        labels: Vec<String>,
        properties: BTreeMap<String, Value>,
    ) -> Result<NodeId> {
        let id = self.alloc_node_id()?;
        self.install_node(id, labels, properties)?;
        Ok(id)
    }

    pub(in crate::graph) fn install_node(
        &self,
        id: NodeId,
        labels: Vec<String>,
        properties: BTreeMap<String, Value>,
    ) -> Result<()> {
        let next = checked_persisted_id(id, "node")?;
        self.next_node_id.fetch_max(next, Ordering::Relaxed);
        self.catalog
            .lock()
            .unwrap()
            .record_labels(labels.iter().cloned());
        self.bump_stats(|s| {
            s.nodes_created += 1;
            s.labels_added += labels.len() as i64;
            s.properties_set += properties.len() as i64;
        });
        if self.recording() {
            self.push_redo(RedoOp::create_node(
                id,
                std::sync::Arc::new(labels.clone()),
                properties.clone(),
            ));
        }
        let xmin = self.xmin_for_install();
        let live = LiveNode::from_parts(&self.interner, labels, properties);
        // ADR-0687: a create's report rule — its label ids only, the
        // property keys covered by the label stamp (`token_changes`'s
        // "what a seam reports").
        self.note_changed_tokens(live.labels.iter().copied());
        let charge =
            crate::memory::live_node_account_live(&live, &self.interner, &self.index_charge());
        // LABS-370 (v1 follow-up) / LABS-851: the exact label counts
        // and the total node count follow every create, before the
        // version owns the live set — the node-liveness seam's one
        // call.
        self.bump_node_population_stats(&live.labels, CountDelta::Add);
        // ADR-0439: a created node introduces its constrained tuple(s); a
        // replay that re-creates an id frees the previous live tuple first.
        // The map's scope decides whether the node can change any tuple.
        let edit = self.tracked_node_edit(id, |scope| {
            scope.touches_node(&live.labels, live.props.iter().map(|(kid, _)| kid))
        });
        self.finish_node_edit(edit, id, Some(&live));
        let before_shape = self.node_chain_shape(id);
        let before_array = self.nodes.array_bytes();
        install(&self.nodes, id, xmin, live);
        self.index_add(id);
        self.mark_node_dirty(id);
        // ADR-0063: lazy string postings — just dirty-mark built pairs, and
        // only after the version is installed (apply-before-dirty ordering).
        self.string_dirty_mark_node(id);
        self.charge(
            charge as i64
                + crate::memory::chain_growth(
                    before_shape,
                    self.node_chain_shape(id),
                    node_version_stride(),
                )
                + self.nodes.array_bytes() as i64
                - before_array as i64,
        );
        Ok(())
    }

    pub fn create_rel(
        &self,
        start: NodeId,
        end: NodeId,
        rel_type: String,
        properties: BTreeMap<String, Value>,
    ) -> Result<RelId> {
        self.validate_rel_endpoints(start, end)?;
        let id = self.alloc_rel_id()?;
        self.install_rel(id, start, end, rel_type, properties)?;
        Ok(id)
    }

    pub(in crate::graph) fn validate_rel_endpoints(
        &self,
        start: NodeId,
        end: NodeId,
    ) -> Result<()> {
        checked_persisted_id(start, "relationship start")?;
        checked_persisted_id(end, "relationship end")?;
        // Endpoint existence without materializing the endpoint nodes.
        let ts_now = self.read_ts();
        if !self.node_visible_at(start, ts_now) || !self.node_visible_at(end, ts_now) {
            return Err(super::missing_rel_endpoint_error());
        }
        Ok(())
    }

    pub(in crate::graph) fn install_rel(
        &self,
        id: RelId,
        start: NodeId,
        end: NodeId,
        rel_type: String,
        properties: BTreeMap<String, Value>,
    ) -> Result<()> {
        let next = checked_persisted_id(id, "relationship")?;
        self.validate_rel_endpoints(start, end)?;
        self.next_rel_id.fetch_max(next, Ordering::Relaxed);
        self.catalog
            .lock()
            .unwrap()
            .record_rel_type(rel_type.clone());
        self.bump_stats(|s| {
            s.relationships_created += 1;
            s.properties_set += properties.len() as i64;
        });
        if self.recording() {
            let op = RedoOp::create_rel(id, start, end, rel_type.clone(), properties.clone());
            self.push_redo(op);
        }
        let xmin = self.xmin_for_install();
        let live = LiveRel::from_parts(&self.interner, start, end, &rel_type, properties);
        // ADR-0687: a create's report rule — its type id only, the
        // property keys covered by the type stamp (`token_changes`'s
        // "what a seam reports").
        self.note_changed_tokens(std::iter::once(live.type_id));
        let charge = crate::memory::live_rel_account_live(&live);
        let ts = live.payload.ts(&self.interner);
        let (out_side, in_side) = (self.out_side(), self.in_side());
        let out_before = out_side.floor(start);
        let in_before = in_side.floor(end);
        let adj_arrays = out_side.delta().array_bytes() + in_side.delta().array_bytes();
        let degree_before = self.out_chunk_len(start, live.type_id);
        out_side.insert(start, live.type_id, id, ts);
        in_side.insert(end, live.type_id, id, ts);
        self.bump_degree_histograms(start, live.type_id, degree_before);
        self.bump_rel_stats(start, live.type_id, end, CountDelta::Add);
        let adj = out_side.floor(start) as i64 - out_before as i64 + in_side.floor(end) as i64
            - in_before as i64
            + (out_side.delta().array_bytes() + in_side.delta().array_bytes()) as i64
            - adj_arrays as i64;
        // ADR-0439: a created relationship introduces its constrained
        // tuple(s); relationship constraints have no backing-index postings.
        // The map's scope decides whether the relationship can change one.
        // Its key ids resolve before the scope probe takes the map lock
        // (interner shard read, then map lock — never the reverse).
        let key_ids = live.payload.key_ids(&self.interner);
        let edit = self.tracked_rel_edit(id, |scope| scope.touches_rel(live.type_id, &key_ids));
        self.finish_rel_edit(edit, id, Some(&live));
        let before_shape = self.rel_chain_shape(id);
        let before_array = self.rels.array_bytes();
        install(&self.rels, id, xmin, live);
        self.mark_rel_dirty(id);
        // ADR-0185: lazy relationship postings — dirty-mark built pairs, and
        // only after the version is installed (apply-before-dirty ordering).
        self.rel_index_add(id);
        self.charge(
            charge as i64
                + crate::memory::chain_growth(
                    before_shape,
                    self.rel_chain_shape(id),
                    rel_version_stride(),
                )
                + self.rels.array_bytes() as i64
                - before_array as i64
                + adj,
        );
        Ok(())
    }

    pub(in crate::graph) fn cow_node(&self, id: NodeId) -> Option<LiveNode> {
        cow(&self.write_ts, &self.nodes, id)
    }

    pub(in crate::graph) fn put_node(&self, id: NodeId, live: LiveNode) {
        // The write-back follows a successful `cow_node`, so the version
        // chain exists by construction; a missing one is a broken invariant,
        // never a silent discard of the edit.
        *self
            .nodes
            .get_mut(&id)
            .expect("put_node follows a successful cow_node")
            .tail_live_mut() = live;
    }

    pub(in crate::graph) fn cow_rel(&self, id: RelId) -> Option<LiveRel> {
        cow(&self.write_ts, &self.rels, id)
    }

    pub(in crate::graph) fn put_rel(&self, id: RelId, live: LiveRel) {
        // The write-back follows a successful `cow_rel`, so the version chain
        // exists by construction; a missing one is a broken invariant, never
        // a silent discard of the edit.
        *self
            .rels
            .get_mut(&id)
            .expect("put_rel follows a successful cow_rel")
            .tail_live_mut() = live;
    }

    pub fn set_property_node(&self, id: NodeId, key: &str, value: Value) {
        let before = self.node(id);
        if before.is_none() {
            // No live entity: there is nothing to edit or to hand off.
            return;
        }
        let before_shape = self.node_chain_shape(id);
        // ADR-0439: capture the before-image only when a uniqueness
        // constraint reads this key; a SET of an unconstrained property
        // cannot change any tuple, so it skips the map entirely. The map's
        // scope is the only source of that answer.
        let keys = self.tracked_node_key_edit(id, key);
        let mut n = self
            .cow_node(id)
            .expect("a live node has a copy-on-write version");
        // ADR-0687's property-edit rule: report the key the edit touched
        // beside the node's labels — the (label, key) equality and range
        // cardinalities a seek consult read. A set interns its key; a removal
        // resolves the id without minting and reports it only when the live
        // version carried the key, so a `SET n.k = null` for a key the node
        // never had changes nothing and stamps nothing.
        let key_id = if value.is_null() {
            self.interner
                .get_id(key)
                .filter(|&kid| n.props.get(kid).is_some())
        } else {
            Some(self.interner.intern(key))
        };
        self.note_changed_tokens(key_id.into_iter().chain(self.live_label_ids(id)));
        let (op, props_set) = if value.is_null() {
            if let Some(kid) = key_id {
                n.props.remove(kid);
            }
            (RedoOp::remove_node_prop(id, key.to_string()), 0i64)
        } else {
            n.props
                .set(key_id.expect("a set interns its key id"), value.clone());
            (RedoOp::set_node_prop(id, key.to_string(), value.clone()), 1)
        };
        // The edited live view is in hand: hand it to the map before it
        // moves into the version chain, never re-read it from the map.
        self.finish_node_edit(keys, id, Some(&n));
        self.put_node(id, n);
        self.bump_stats(|s| s.properties_set += props_set);
        self.push_redo(op);
        self.mark_node_dirty(id);
        // Dirty-mark only after the new version is installed — the ADR-0185
        // invariant: `index_add` marks every (label, key) pair the installed
        // version carries; a removal leaves stale postings, which the
        // residual re-verification filters.
        self.index_add(id);
        self.charge_node_after_edit(id, before.as_ref(), before_shape);
    }

    pub fn set_property_rel(&self, id: RelId, key: &str, value: Value) {
        if !self.rel_exists(id) {
            // No live entity: there is nothing to edit or to hand off.
            return;
        }
        let before_bytes = self.visible_rel_account(id);
        let before_shape = self.rel_chain_shape(id);
        let keys = self.tracked_rel_key_edit(id, key);
        let mut r = self
            .cow_rel(id)
            .expect("a live relationship has a copy-on-write version");
        let old_ts = r.payload.ts(&self.interner);
        let start = r.start;
        let end = r.end;
        let type_id = r.type_id;
        let xmax = self.tombstone_ts();
        // ADR-0687's property-edit rule: the key the edit touched beside the
        // rel type. A set interns its key; a removal resolves the id without
        // minting and reports it only when the interner holds an id for a
        // key the payload carried. A Triple payload answers its three
        // structural field names (`id`, `amount`, `ts`) without the
        // interner — `from_map` stores the shape and interns nothing — so
        // removing one finds a carried value under a name with no id, and
        // the id-keyed stamp table has nothing to record for it. The
        // removal itself proceeds below either way.
        let key_id = if value.is_null() {
            match self.interner.get_id(key) {
                Some(kid) => r.payload.get(&self.interner, key).is_some().then_some(kid),
                None => None,
            }
        } else {
            Some(self.interner.intern(key))
        };
        self.note_changed_tokens(key_id.into_iter().chain(std::iter::once(type_id)));
        let (op, props_set) = if value.is_null() {
            r.payload.remove(&self.interner, key);
            (RedoOp::remove_rel_prop(id, key.to_string()), 0i64)
        } else {
            r.payload.set(&self.interner, key, value.clone());
            (RedoOp::set_rel_prop(id, key.to_string(), value.clone()), 1)
        };
        let new_ts = r.payload.ts(&self.interner);
        // The edited live view is in hand: hand it to the map before it
        // moves into the version chain, never re-read it from the map.
        self.finish_rel_edit(keys, id, Some(&r));
        self.put_rel(id, r);
        if old_ts != new_ts {
            // A sealed position is superseded at `xmax` (ADR-0551); the
            // new hop lands in the delta. The sealed charge is the event's
            // price_side terms — the one formula the admission estimate
            // prices batches with — never a whole-tier floor walk.
            let (out_side, in_side) = (self.out_side(), self.in_side());
            let adj_before = out_side.floor(start) + in_side.floor(end);
            let (sealed_refusable, sealed_spill) =
                self.price_sealed_event(start, end, type_id, id, Supersede::Repositioned);
            out_side.reposition(start, type_id, id, new_ts, xmax);
            in_side.reposition(end, type_id, id, new_ts, xmax);
            self.charge(
                (out_side.floor(start) + in_side.floor(end)) as i64 - adj_before as i64
                    + sealed_refusable
                    + sealed_spill,
            );
        }
        // Dirty-mark only after the new version is installed — the
        // ADR-0185 invariant: `rel_index_add` reads the installed version's
        // own type and keys; a removal's key is gone from the payload, and
        // its stale postings are filtered by the residual re-verification.
        self.rel_index_add(id);
        self.bump_stats(|s| s.properties_set += props_set);
        self.push_redo(op);
        self.mark_rel_dirty(id);
        self.charge_rel_after_edit(id, before_bytes, before_shape);
    }

    /// Tombstone timestamp for a delete: the write ts while in a write,
    /// else the commit ts clamped to ≥ 1 so the tombstone is never the
    /// "no tombstone" sentinel 0. Both MVCC delete paths share this rule.
    pub(in crate::graph) fn tombstone_ts(&self) -> u64 {
        if self.in_write.load(Ordering::Acquire) {
            self.write_ts.load(Ordering::Acquire)
        } else {
            self.commit_ts().max(1)
        }
    }

    pub fn delete_rel(&self, id: RelId) {
        let ts = self.tombstone_ts();
        // ADR-0439: free the constrained tuple from the live version before
        // the tombstone (entity guard first, map lock last).
        self.constraint_keys_delete_rel(id);
        let before_shape = self.rel_chain_shape(id);
        let deleted = {
            let Some(mut vers) = self.rels.get_mut(&id) else {
                return;
            };
            let Some(live) = vers.tombstone(ts) else {
                return;
            };
            let (start, end, type_id) = (live.start, live.end, live.type_id);
            // ADR-0687's delete rule: the dying version's type and
            // property-key ids while they still exist (`token_changes`'s
            // "what a seam reports") — no later walk recovers them.
            let mut changed = vec![type_id];
            // The walk is the full set: the visitor never breaks, so its
            // flow continues after every key id.
            let walk = live.payload.key_ids(&self.interner).for_each(|kid| {
                changed.push(kid);
                ControlFlow::Continue(())
            });
            debug_assert!(walk.is_continue());
            self.note_changed_tokens(changed);
            // LABS-370: the pair counts are exact (the counting operator
            // answers from them), so a delete leaves its (start-label, type,
            // end-label) triples — the mirror of the create-path bump.
            self.bump_rel_stats(start, type_id, end, CountDelta::Remove);
            (start, end, type_id)
        };
        let (start, end, type_id) = deleted;
        let (out_side, in_side) = (self.out_side(), self.in_side());
        let adj_before = out_side.floor(start) + in_side.floor(end);
        // The sealed charge is the event's price_side terms — the one
        // formula the admission estimate prices batches with — priced
        // before the tombstones land, never a whole-tier floor walk.
        let (sealed_refusable, sealed_spill) =
            self.price_sealed_event(start, end, type_id, id, Supersede::Deleted);
        // ADR-0551: the hop keeps its slot with a versioned tombstone so an
        // older snapshot still sees it — in the delta (visibility filters it)
        // or in the sealed stream. Recording it unconditionally keeps the
        // write path decode-free; the fold resolves a tombstone whose hop
        // only ever lived in the delta.
        out_side.tombstone_deleted(start, type_id, id, ts);
        in_side.tombstone_deleted(end, type_id, id, ts);
        let adj_after = out_side.floor(start) + in_side.floor(end);
        self.charge(
            crate::memory::chain_growth(
                before_shape,
                before_shape.after_delete(),
                rel_version_stride(),
            ) + adj_after as i64
                - adj_before as i64
                + sealed_refusable
                + sealed_spill,
        );
        self.bump_stats(|s| s.relationships_deleted += 1);
        self.push_redo(RedoOp::delete_rel(id));
        self.mark_rel_dirty(id);
    }

    pub fn delete_node(&self, id: NodeId, detach: bool) -> Result<()> {
        // The canonical incident scan: visibility-filtered at the read
        // timestamp and self-loop-deduplicated (graph/adjacency.rs).
        let incident = self.neighbors(id, relate_ir::Direction::Both);
        if !detach && !incident.is_empty() {
            return Err(delete_connected_node_error());
        }
        for rid in incident {
            self.delete_rel(rid);
        }
        // ADR-0439: free the node's constrained tuple(s) from the live
        // version before the tombstone (entity guard first, map lock last).
        self.constraint_keys_delete_node(id);
        let ts = self.tombstone_ts();
        // The tombstone's dying labels, carried out of the `nodes` guard
        // so the reclamation candidates (ADR-0595) are appended after it
        // is released — the lock contract `suspect_postings` states, and
        // the shape the label twins' sites already have. The one carrier of
        // "the live version was tombstoned": a missing entry is a missing
        // node, and every later step runs in one block off it.
        // The one carrier of "the live version was tombstoned": its label
        // ids and its property-key ids, both collected under the guard the
        // dying version is borrowed in. A missing entry is a missing node,
        // and every later step runs in one block off it.
        let mut dying: Option<(Vec<u32>, Vec<u32>)> = None;
        let before_shape = self.node_chain_shape(id);
        if let Some(mut vers) = self.nodes.get_mut(&id) {
            if let Some(live) = vers.tombstone(ts) {
                // LABS-370 (v1 follow-up) / LABS-851: the exact label
                // counts and the total node count follow every delete —
                // the count state does not. The node-liveness seam's
                // one call.
                self.bump_node_population_stats(&live.labels, CountDelta::Remove);
                let keys = live.props.iter().map(|(kid, _)| kid).collect();
                dying = Some((live.labels.clone(), keys));
            }
        }
        let Some((dying_labels, dying_keys)) = dying else {
            return Ok(());
        };
        // ADR-0687's delete rule: the dying version's label ids AND its
        // property-key ids — the tombstone is the last place the keys
        // exist, so no later walk could report them (`token_changes`'
        // "what a seam reports"; `delete_rel`'s report is the twin).
        self.note_changed_tokens(dying_labels.iter().copied().chain(dying_keys));
        // ADR-0595: the tombstone's dying buckets — the labels the
        // record carried, or the empty-label bucket when it carried
        // none — become reclamation candidates here, at the removal
        // site, for the apply's tail drain.
        self.suspect_postings(id, &dying_labels);
        self.charge(crate::memory::chain_growth(
            before_shape,
            before_shape.after_delete(),
            node_version_stride(),
        ));
        self.bump_stats(|s| s.nodes_deleted += 1);
        self.push_redo(RedoOp::delete_node(id));
        self.mark_node_dirty(id);
        Ok(())
    }
}

/// Install `live` as `id`'s newest version: a fresh id gets an inline slot;
/// an id that already has versions (a replayed re-create) keeps them.
fn install<Live>(map: &IdVec<Chain<Live>>, id: i64, xmin: u64, live: Live) {
    match map.entry(id) {
        super::id_vec::Entry::Occupied(mut slot) => slot.get_mut().push(Ver {
            xmin,
            xmax: 0,
            live,
        }),
        super::id_vec::Entry::Vacant(mut slot) => {
            slot.insert(Chain::inline(xmin, live));
        }
    }
}
