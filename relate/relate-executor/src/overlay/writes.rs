//! The write verbs: every overlay mutation (`create_node`, `create_rel`,
//! `add_labels`/`remove_labels`, the `set_property_*` /
//! `merge_properties_*` / `replace_properties_*` family, `delete_*`)
//! funnels through [`WriteScratch::push_redo`], so the redo log and the
//! lazy equality indexes stay current for every write. Stats accounting
//! (creates, deletes, properties set/removed, net label deltas) happens
//! here, next to the mutation it counts: the openCypher graph-diff
//! counters (net new label names, property set/removed pairs) and Neo4j's
//! update counters beside them (labels per node, one property write per
//! write, ADR-0309).

use std::collections::{BTreeMap, HashSet};

use relate_core::{Node, NodeId, RelId, Relationship, Result, Value};
use relate_ir::Direction;

use crate::graph::Graph;
use crate::redo::RedoOp;

use super::label_probe::LabelChange;
use super::{distinct_labels, ExecGraph, WriteScratch};

impl ExecGraph<'_> {
    pub fn create_node(
        &self,
        labels: Vec<String>,
        properties: BTreeMap<String, Value>,
    ) -> Result<NodeId> {
        let mut s = self.scratch_mut();
        let labels = std::sync::Arc::new(labels);
        let properties = std::sync::Arc::new(relate_core::PropertyMap::from(properties));
        let (labels_added, labels_removed) = self.label_deltas(&s, &labels, LabelChange::Gain);
        s.reserve_new_node(&Node {
            id: 0,
            labels: std::sync::Arc::clone(&labels),
            properties: std::sync::Arc::clone(&properties),
        })?;
        let id = s.alloc_node()?;
        s.statement.stats.nodes_created += 1;
        s.net_label_stats(labels_added, labels_removed);
        s.statement.stats.properties_set += properties.len() as i64;
        s.batch.nodes.insert(
            id,
            Node {
                id,
                labels: std::sync::Arc::clone(&labels),
                properties: std::sync::Arc::clone(&properties),
            },
        );
        s.bump_live_labels(&labels, 1);
        s.batch.created_nodes.insert(id);
        s.push_redo(RedoOp::CreateNode {
            id,
            labels,
            properties,
        });
        drop(s);
        relate_core::tokens::poll()?;
        Ok(id)
    }

    pub fn create_rel(
        &self,
        start: NodeId,
        end: NodeId,
        rel_type: String,
        properties: BTreeMap<String, Value>,
    ) -> Result<RelId> {
        if !self.node_exists(start) || !self.node_exists(end) {
            return Err(crate::graph::missing_rel_endpoint_error());
        }
        let properties = std::sync::Arc::new(relate_core::PropertyMap::from(properties));
        let mut s = self.scratch_mut();
        s.reserve_new_rel(&Relationship {
            id: 0,
            start,
            end,
            rel_type: rel_type.clone(),
            properties: std::sync::Arc::clone(&properties),
        })?;
        let id = s.alloc_rel()?;
        s.statement.stats.relationships_created += 1;
        s.statement.stats.properties_set += properties.len() as i64;
        s.batch.rels.insert(
            id,
            Relationship {
                id,
                start,
                end,
                rel_type: rel_type.clone(),
                properties: std::sync::Arc::clone(&properties),
            },
        );
        s.batch.out_adj.entry(start).or_default().push(id);
        s.batch.in_adj.entry(end).or_default().push(id);
        s.batch.created_rels.insert(id);
        s.push_redo(RedoOp::CreateRel {
            id,
            start,
            end,
            rel_type,
            properties,
        });
        drop(s);
        relate_core::tokens::poll()?;
        Ok(id)
    }

    fn cow_node<'b>(s: &'b mut WriteScratch, base: &Graph, id: NodeId) -> Option<&'b mut Node> {
        if s.batch.deleted_nodes.contains(&id) {
            return None;
        }
        if !s.batch.nodes.contains_key(&id) {
            let n = base.node_at(id, s.snapshot_ts())?;
            // LABS-1085 review: a copy-on-write introduces the base node
            // into the scratch, so its whole label/property vocabulary
            // posts into the equality index — exactly what a create
            // posts. Without this, a pair built before the cow misses
            // the node's pre-existing properties, and a keyed lookup
            // (MERGE's match arm) silently misses the node and creates
            // a duplicate. Set/label ops on the copy post on top;
            // postings are sets, so a double post is harmless.
            for (key, value) in &*n.properties {
                s.batch.prop_ix.post_value(id, &n.labels, key, value);
            }
            s.batch.nodes.insert(id, n);
        }
        s.batch.nodes.get_mut(&id)
    }

    fn cow_rel<'b>(
        s: &'b mut WriteScratch,
        base: &Graph,
        id: RelId,
    ) -> Option<&'b mut Relationship> {
        if s.batch.deleted_rels.contains(&id) {
            return None;
        }
        if !s.batch.rels.contains_key(&id) {
            let r = base.rel_at(id, s.snapshot_ts())?;
            // The node cow's posting contract (above), on the rel index.
            for (key, value) in &*r.properties {
                s.batch.rel_prop_ix.post_value(id, &r.rel_type, key, value);
            }
            s.batch.rels.insert(id, r);
        }
        s.batch.rels.get_mut(&id)
    }

    pub fn add_labels(&self, id: NodeId, labels: &[String]) {
        if labels.is_empty() {
            return;
        }
        let existing = self.node_labels(id).unwrap_or_default();
        // The labels this write actually adds: the distinct requested labels
        // not already on the node, in first-occurrence order. Computed once
        // and reused for the accounting and the cow block; `n.labels ==
        // existing` at cow time, so the cow block pushes `fresh` as-is
        // instead of re-answering the membership question per label.
        let fresh: Vec<String> = distinct_labels(labels)
            .into_iter()
            .filter(|l| !existing.iter().any(|x| x == *l))
            .map(str::to_string)
            .collect();
        let mut s = self.scratch_mut();
        let (labels_added, labels_removed) = self.label_deltas(&s, &fresh, LabelChange::Gain);
        let created = s.batch.created_nodes.contains(&id);
        let label_bytes = node_label_bytes(id);
        let before_labels = created_snapshot(&s, created, &label_bytes);
        let mut added = Vec::new();
        if let Some(n) = Self::cow_node(&mut s, self.base, id) {
            std::sync::Arc::make_mut(&mut n.labels).extend(fresh.iter().cloned());
            added = fresh;
        }
        shift_created(&mut s, before_labels, &label_bytes);
        s.net_label_stats(labels_added, labels_removed);
        if !added.is_empty() {
            s.bump_live_labels(&added, 1);
            s.push_redo(RedoOp::add_labels(id, added));
        }
    }

    pub fn remove_labels(&self, id: NodeId, labels: &[String]) {
        // Cow the node first and derive everything from `n.labels` in one
        // pass: `extract_if` walks the node's labels once, dropping the
        // ones the request names and collecting them into `actually` in
        // node order. `requested` answers membership in O(1) per node
        // label; `actually` feeds the accounting and the live-label bump
        // (both consumers dedupe, so node order is as good as request
        // order). A missing or scratch-deleted node leaves `actually`
        // empty, exactly what the pre-cow read answered for it.
        let requested: HashSet<&str> = labels.iter().map(String::as_str).collect();
        let mut s = self.scratch_mut();
        let created = s.batch.created_nodes.contains(&id);
        let label_bytes = node_label_bytes(id);
        let before_labels = created_snapshot(&s, created, &label_bytes);
        let mut actually: Vec<String> = Vec::new();
        if let Some(n) = Self::cow_node(&mut s, self.base, id) {
            actually = std::sync::Arc::make_mut(&mut n.labels)
                .extract_if(.., |l| requested.contains(l.as_str()))
                .collect();
        }
        shift_created(&mut s, before_labels, &label_bytes);
        let (labels_added, labels_removed) = self.label_deltas(&s, &actually, LabelChange::Loss);
        s.net_label_stats(labels_added, labels_removed);
        if !actually.is_empty() {
            s.bump_live_labels(&actually, -1);
            // The op carries the labels the node actually lost, so the
            // ADR-0309 per-node count is derived from it (a requested label
            // the node did not carry is a no-op).
            s.push_redo(RedoOp::remove_labels(id, actually));
        }
    }

    /// CIP-162 overwrite semantics, spelled once beside the primitives it
    /// composes: the node's labels become exactly `labels` — the ones it
    /// carries but the list omits leave ([`remove_labels`]), then the
    /// list's labels arrive ([`add_labels`]). Each half keeps its own
    /// accounting and redo, so the overwrite is their compose, not a third
    /// label-write path.
    pub fn set_labels(&self, id: NodeId, labels: &[String]) {
        let current = self.node_labels(id).unwrap_or_default();
        let remove: Vec<String> = current
            .into_iter()
            .filter(|label| !labels.contains(label))
            .collect();
        // An empty diff skips the remove half (mirroring add_labels' own
        // empty-list guard) instead of cowing the node for nothing.
        if !remove.is_empty() {
            self.remove_labels(id, &remove);
        }
        self.add_labels(id, labels);
    }

    pub fn set_property_node(&self, id: NodeId, key: &str, value: Value) {
        let mut s = self.scratch_mut();
        let created = s.batch.created_nodes.contains(&id);
        let prop_bytes = node_property_bytes(id);
        let before_props = created_snapshot(&s, created, &prop_bytes);
        let outcome = Self::cow_node(&mut s, self.base, id).map(|n| {
            set_prop_core(
                std::sync::Arc::make_mut(&mut n.properties),
                created,
                key,
                &value,
            )
        });
        shift_created(&mut s, before_props, &prop_bytes);
        let PropWrite { set, removed, op } = outcome.unwrap_or_default();
        s.statement.stats.properties_set += set;
        s.statement.stats.properties_removed += removed;
        if let Some(op) = op {
            s.push_redo(match op {
                PropOpKind::Set(k, v) => RedoOp::set_node_prop(id, k, v),
                PropOpKind::Remove(k) => RedoOp::remove_node_prop(id, k),
            });
        }
    }

    pub fn set_property_rel(&self, id: RelId, key: &str, value: Value) {
        let mut s = self.scratch_mut();
        let created = s.batch.created_rels.contains(&id);
        let prop_bytes = rel_property_bytes(id);
        let before_props = created_snapshot(&s, created, &prop_bytes);
        let outcome = Self::cow_rel(&mut s, self.base, id).map(|r| {
            set_prop_core(
                std::sync::Arc::make_mut(&mut r.properties),
                created,
                key,
                &value,
            )
        });
        shift_created(&mut s, before_props, &prop_bytes);
        let PropWrite { set, removed, op } = outcome.unwrap_or_default();
        s.statement.stats.properties_set += set;
        s.statement.stats.properties_removed += removed;
        if let Some(op) = op {
            s.push_redo(match op {
                PropOpKind::Set(k, v) => RedoOp::set_rel_prop(id, k, v),
                PropOpKind::Remove(k) => RedoOp::remove_rel_prop(id, k),
            });
        }
    }

    /// `SET n += map`: per-key merge through the set verb (an existing
    /// key keeps its position, a new one is added) — the node and
    /// relationship forms are the same loop over their set verbs.
    pub fn merge_properties_node(&self, id: NodeId, map: &BTreeMap<String, Value>) {
        for (k, v) in map {
            self.set_property_node(id, k, v.clone());
        }
    }

    /// The relationship form of [`Self::merge_properties_node`].
    pub fn merge_properties_rel(&self, id: RelId, map: &BTreeMap<String, Value>) {
        for (k, v) in map {
            self.set_property_rel(id, k, v.clone());
        }
    }

    pub fn replace_properties_node(&self, id: NodeId, map: BTreeMap<String, Value>) {
        let keys: Vec<String> = self
            .node(id)
            .map(|n| n.properties.keys().cloned().collect())
            .unwrap_or_default();
        replace_props(keys, map, |k, v| self.set_property_node(id, k, v));
    }

    pub fn replace_properties_rel(&self, id: RelId, map: BTreeMap<String, Value>) {
        let keys: Vec<String> = self
            .rel(id)
            .map(|r| r.properties.keys().cloned().collect())
            .unwrap_or_default();
        replace_props(keys, map, |k, v| self.set_property_rel(id, k, v));
    }

    pub fn delete_rel(&self, id: RelId) {
        let nprops = match self.rel(id) {
            Some(r) => r.properties.len() as i64,
            None => return,
        };
        let mut s = self.scratch_mut();
        let removed = created_rel_removal(&s, id);
        s.batch.deleted_rels.insert(id);
        s.batch.rels.remove(&id);
        if s.batch.created_rels.remove(&id) {
            if let Some(delta) = removed {
                s.shift_workspace(delta, 0);
            }
            s.statement.stats.relationships_created =
                s.statement.stats.relationships_created.saturating_sub(1);
            s.statement.stats.properties_set =
                s.statement.stats.properties_set.saturating_sub(nprops);
        } else {
            s.statement.stats.relationships_deleted += 1;
            s.statement.stats.properties_removed += nprops;
        }
        s.push_redo(RedoOp::delete_rel(id));
    }

    pub fn delete_node(&self, id: NodeId, detach: bool) -> Result<()> {
        let incident: Vec<RelId> = self.neighbors(id, Direction::Both);
        if !detach && !incident.is_empty() {
            return Err(crate::graph::delete_connected_node_error());
        }
        for rid in &incident {
            self.delete_rel(*rid);
        }
        // One fetch answers both "is the node live" and "what does it
        // carry": a missing node has nothing to delete.
        let Some(node) = self.node(id) else {
            return Ok(());
        };
        let nprops = node.properties.len() as i64;
        let labels = node.labels;
        let mut s = self.scratch_mut();
        let (labels_added, labels_removed) = self.label_deltas(&s, &labels, LabelChange::Loss);
        let removed = created_node_removal(&s, id);
        s.batch.deleted_nodes.insert(id);
        s.batch.nodes.remove(&id);
        s.bump_live_labels(&labels, -1);
        s.net_label_stats(labels_added, labels_removed);
        if s.batch.created_nodes.remove(&id) {
            if let Some(delta) = removed {
                s.shift_workspace(delta, 0);
            }
            s.statement.stats.nodes_created = s.statement.stats.nodes_created.saturating_sub(1);
            s.statement.stats.properties_set =
                s.statement.stats.properties_set.saturating_sub(nprops);
        } else {
            s.statement.stats.nodes_deleted += 1;
            s.statement.stats.properties_removed += nprops;
        }
        s.push_redo(RedoOp::delete_node(id));
        Ok(())
    }
}

/// The workspace bytes the created node `id`'s labels hold now, `None`
/// when the scratch has no copy of it.
fn node_label_bytes(id: NodeId) -> impl Fn(&WriteScratch) -> Option<u64> {
    move |s| {
        s.batch
            .nodes
            .get(&id)
            .map(|node| crate::workspace::created_label_bytes(&node.labels))
    }
}

/// The workspace bytes the created node `id`'s properties hold now.
fn node_property_bytes(id: NodeId) -> impl Fn(&WriteScratch) -> Option<u64> {
    move |s| {
        s.batch
            .nodes
            .get(&id)
            .map(|node| crate::workspace::created_property_delta(&node.properties))
    }
}

/// The workspace bytes the created relationship `id`'s properties hold now.
fn rel_property_bytes(id: RelId) -> impl Fn(&WriteScratch) -> Option<u64> {
    move |s| {
        s.batch
            .rels
            .get(&id)
            .map(|rel| crate::workspace::created_property_delta(&rel.properties))
    }
}

/// The "before" half of the created-entity workspace shift: the bytes
/// `bytes` reads off the scratch copy, only when workspace accounting is on
/// and the entity was created in this batch (`created`).
fn created_snapshot(
    s: &WriteScratch,
    created: bool,
    bytes: &impl Fn(&WriteScratch) -> Option<u64>,
) -> Option<u64> {
    if !created || !relate_core::tokens::workspace_accounting() {
        return None;
    }
    bytes(s)
}

/// The "after" half: re-read the bytes after the write and shift the
/// workspace by the difference. A `before` snapshot exists only for an
/// entity the scratch already held, and the cow never removes it.
fn shift_created(
    s: &mut WriteScratch,
    before: Option<u64>,
    bytes: &impl Fn(&WriteScratch) -> Option<u64>,
) {
    let Some(before) = before else {
        return;
    };
    let after = bytes(s).expect("the cow inserted the scratch copy");
    s.shift_workspace(before, after);
}

fn created_rel_removal(s: &WriteScratch, id: RelId) -> Option<u64> {
    if !relate_core::tokens::workspace_accounting() || !s.batch.created_rels.contains(&id) {
        return None;
    }
    s.batch.rels.get(&id).map(|rel| {
        crate::workspace::created_rel_remove_delta(
            s.batch.created_rels.len().saturating_sub(1),
            rel,
        )
    })
}

fn created_node_removal(s: &WriteScratch, id: NodeId) -> Option<u64> {
    if !relate_core::tokens::workspace_accounting() || !s.batch.created_nodes.contains(&id) {
        return None;
    }
    s.batch.nodes.get(&id).map(|node| {
        crate::workspace::created_node_remove_delta(
            s.batch.created_nodes.len().saturating_sub(1),
            node,
        )
    })
}

/// The redo-op payload of one set_property, before the node/rel verb wraps
/// it into its [`RedoOp`] variant.
enum PropOpKind {
    Set(String, Value),
    Remove(String),
}

/// What one set_property did: the stats deltas plus the redo-op payload.
#[derive(Default)]
struct PropWrite {
    set: i64,
    removed: i64,
    op: Option<PropOpKind>,
}

/// The shared core of `set_property_node`/`set_property_rel`: apply the
/// null-means-remove / otherwise-upsert contract to one properties map and
/// report the stats deltas plus the redo-op payload. A null value removing
/// an absent key is a no-op (no deltas, no op).
fn set_prop_core(
    props: &mut relate_core::PropertyMap,
    created: bool,
    key: &str,
    value: &Value,
) -> PropWrite {
    if value.is_null() {
        if props.remove(key).is_some() {
            return PropWrite {
                set: 0,
                removed: 1,
                op: Some(PropOpKind::Remove(key.to_string())),
            };
        }
        return PropWrite::default();
    }
    let existed = props.contains_key(key);
    props.insert(key.to_string(), value.clone());
    let (set, removed) = prop_stat_deltas(existed, created);
    PropWrite {
        set,
        removed,
        op: Some(PropOpKind::Set(key.to_string(), value.clone())),
    }
}

/// The shared core of `replace_properties_*`: null out every current key the
/// map does not name, then set every entry, all through the one setter.
fn replace_props(
    keys: Vec<String>,
    map: BTreeMap<String, Value>,
    mut set: impl FnMut(&str, Value),
) {
    for k in &keys {
        if !map.contains_key(k) {
            set(k, Value::Null);
        }
    }
    for (k, v) in map {
        set(&k, v);
    }
}

/// Stats deltas for a set_property on a pre-existing (or CREATE-added) map:
/// overwriting an existing key counts set+removed; a fresh key counts set;
/// CREATE already counting the key (`created` on an `existed` map cell) is
/// the no-op.
fn prop_stat_deltas(existed: bool, created: bool) -> (i64, i64) {
    if existed && !created {
        (1, 1)
    } else if !existed {
        (1, 0)
    } else {
        (0, 0)
    }
}
