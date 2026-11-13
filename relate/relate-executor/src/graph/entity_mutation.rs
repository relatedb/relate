//! The entity-mutation family every cross-cutting feature hooks: the create /
//! install / copy-on-write / property-edit / delete seams for nodes and
//! relationships, in their own impl-block file (the `label_mutation.rs` and
//! `degree_stats.rs` precedent, a sibling of the graph family). `Graph`
//! (graph/mod.rs) keeps the struct and the family-independent plumbing
//! (ids, pins, charging, catalog reads); the seams that install a version,
//! maintain the indexes/adjacency/counts, and push redo live here.

use super::accounting::adj_floor;
use super::adjacency::adj_tombstone_deleted;
use super::id_vec::IdVec;
use super::*;

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
            charge as i64 + crate::memory::node_chain_bytes(self.node_chain_shape(id)) as i64
                - crate::memory::node_chain_bytes(before_shape) as i64
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
        let charge = crate::memory::live_rel_account_live(&live);
        let ts = live.payload.ts(&self.interner);
        let out_before = adj_floor(&self.out_adj, &self.sealed_out, start);
        let in_before = adj_floor(&self.in_adj, &self.sealed_in, end);
        let adj_arrays = self.out_adj.array_bytes() + self.in_adj.array_bytes();
        let degree_before = self.out_chunk_len(start, live.type_id);
        adj_insert(&self.out_adj, start, live.type_id, id, ts);
        adj_insert(&self.in_adj, end, live.type_id, id, ts);
        self.bump_degree_histograms(start, live.type_id, degree_before);
        self.bump_rel_stats(start, live.type_id, end, CountDelta::Add);
        let adj = adj_floor(&self.out_adj, &self.sealed_out, start) as i64 - out_before as i64
            + adj_floor(&self.in_adj, &self.sealed_in, end) as i64
            - in_before as i64
            + (self.out_adj.array_bytes() + self.in_adj.array_bytes()) as i64
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
            charge as i64 + crate::memory::rel_chain_bytes(self.rel_chain_shape(id)) as i64
                - crate::memory::rel_chain_bytes(before_shape) as i64
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
        let kid = self.interner.intern(key);
        let (op, props_set) = if value.is_null() {
            n.props.remove(kid);
            (RedoOp::remove_node_prop(id, key.to_string()), 0i64)
        } else {
            n.props.set(kid, value.clone());
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
            // new hop lands in the delta.
            let adj_before = adj_floor(&self.out_adj, &self.sealed_out, start)
                + adj_floor(&self.in_adj, &self.sealed_in, end);
            adj_reposition(
                &self.out_adj,
                &self.sealed_out,
                start,
                type_id,
                id,
                new_ts,
                xmax,
            );
            adj_reposition(
                &self.in_adj,
                &self.sealed_in,
                end,
                type_id,
                id,
                new_ts,
                xmax,
            );
            self.charge(
                (adj_floor(&self.out_adj, &self.sealed_out, start)
                    + adj_floor(&self.in_adj, &self.sealed_in, end)) as i64
                    - adj_before as i64,
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

    pub fn merge_properties_node(&self, id: NodeId, map: &BTreeMap<String, Value>) {
        // One cycle for the whole map, mirroring how `install_node` takes a
        // map in one pass: one scope probe, one cow, one tracked-edit
        // completion, one index/charge update.
        let before = self.node(id);
        if before.is_none() || map.is_empty() {
            return;
        }
        let before_shape = self.node_chain_shape(id);
        let keys = self.tracked_node_edit(id, |scope| {
            map.keys().any(|key| {
                self.interner
                    .get_id(key)
                    .is_some_and(|kid| scope.touches_node_key(kid))
            })
        });
        let mut n = self
            .cow_node(id)
            .expect("a live node has a copy-on-write version");
        let mut props_set = 0i64;
        let mut ops = Vec::with_capacity(map.len());
        for (key, value) in map {
            let kid = self.interner.intern(key);
            if value.is_null() {
                n.props.remove(kid);
                ops.push(RedoOp::remove_node_prop(id, key.clone()));
            } else {
                n.props.set(kid, value.clone());
                props_set += 1;
                ops.push(RedoOp::set_node_prop(id, key.clone(), value.clone()));
            }
        }
        // The edited live view is in hand: hand it to the map before it
        // moves into the version chain.
        self.finish_node_edit(keys, id, Some(&n));
        self.put_node(id, n);
        self.bump_stats(|s| s.properties_set += props_set);
        for op in ops {
            self.push_redo(op);
        }
        self.mark_node_dirty(id);
        self.index_add(id);
        self.charge_node_after_edit(id, before.as_ref(), before_shape);
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
            // LABS-370: the pair counts are exact (the counting operator
            // answers from them), so a delete leaves its (start-label, type,
            // end-label) triples — the mirror of the create-path bump.
            self.bump_rel_stats(start, type_id, end, CountDelta::Remove);
            (start, end, type_id)
        };
        let (start, end, type_id) = deleted;
        let adj_before = adj_floor(&self.out_adj, &self.sealed_out, start)
            + adj_floor(&self.in_adj, &self.sealed_in, end);
        let sealed_arrays_before = self.sealed_out.array_bytes() + self.sealed_in.array_bytes();
        // ADR-0551: the hop keeps its slot with a versioned tombstone so an
        // older snapshot still sees it — in the delta (visibility filters it)
        // or in the sealed stream. Recording it unconditionally keeps the
        // write path decode-free; the fold resolves a tombstone whose hop
        // only ever lived in the delta.
        adj_tombstone_deleted(&self.sealed_out, start, type_id, id, ts);
        adj_tombstone_deleted(&self.sealed_in, end, type_id, id, ts);
        let adj_after = adj_floor(&self.out_adj, &self.sealed_out, start)
            + adj_floor(&self.in_adj, &self.sealed_in, end);
        self.charge(
            crate::memory::rel_chain_bytes(before_shape.after_delete()) as i64
                - crate::memory::rel_chain_bytes(before_shape) as i64
                + adj_after as i64
                - adj_before as i64
                + (self.sealed_out.array_bytes() + self.sealed_in.array_bytes()) as i64
                - sealed_arrays_before as i64,
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
        let mut found = false;
        // The tombstone's dying labels, carried out of the `nodes` guard
        // so the reclamation candidates (ADR-0595) are appended after it
        // is released — the lock contract `suspect_postings` states, and
        // the shape the label twins' sites already have.
        let mut dying: Option<Vec<u32>> = None;
        let before_shape = self.node_chain_shape(id);
        if let Some(mut vers) = self.nodes.get_mut(&id) {
            if let Some(live) = vers.tombstone(ts) {
                // LABS-370 (v1 follow-up) / LABS-851: the exact label
                // counts and the total node count follow every delete —
                // the count state does not. The node-liveness seam's
                // one call.
                self.bump_node_population_stats(&live.labels, CountDelta::Remove);
                dying = Some(live.labels.clone());
                found = true;
            }
        }
        if let Some(dying) = dying {
            // ADR-0595: the tombstone's dying buckets — the labels the
            // record carried, or the empty-label bucket when it carried
            // none — become reclamation candidates here, at the removal
            // site, for the apply's tail drain.
            self.suspect_postings(id, &dying);
        }
        if found {
            self.charge(
                crate::memory::node_chain_bytes(before_shape.after_delete()) as i64
                    - crate::memory::node_chain_bytes(before_shape) as i64,
            );
            self.bump_stats(|s| s.nodes_deleted += 1);
            self.push_redo(RedoOp::delete_node(id));
            self.mark_node_dirty(id);
        }
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
