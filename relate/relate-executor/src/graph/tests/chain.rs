//! The inline slot and its spill (ADR-0529): a never-updated row carries no
//! version list; the first update spills it, abort and GC fold it back.

use std::sync::Arc;

use super::*;

fn props(n: i64) -> BTreeMap<String, Value> {
    BTreeMap::from([("n".into(), Value::Int(n))])
}

fn node_n(g: &Graph, id: NodeId, ts: u64) -> Option<Value> {
    g.node_at(id, ts).map(|n| n.properties["n"].clone())
}

fn node_versions(g: &Graph, id: NodeId) -> Vec<(u64, u64)> {
    g.nodes
        .get(&id)
        .map(|c| c.iter().map(|v| (v.xmin, v.xmax)).collect())
        .unwrap_or_default()
}

fn node_inline(g: &Graph, id: NodeId) -> bool {
    g.nodes.get(&id).is_some_and(|c| c.is_inline())
}

fn rel_inline(g: &Graph, id: RelId) -> bool {
    g.rels.get(&id).is_some_and(|c| c.is_inline())
}

/// A committed node carrying `n = 1`, created at ts 1.
fn committed_node() -> (Graph, NodeId) {
    let g = Graph::default();
    g.begin_write().unwrap();
    let id = g.create_node(vec!["A".into()], props(1)).unwrap();
    g.commit_write();
    (g, id)
}

fn set_n(g: &Graph, id: NodeId, n: i64) {
    g.begin_write().unwrap();
    g.set_property_node(id, "n", Value::Int(n));
    g.commit_write();
}

#[test]
fn create_only_rows_are_inline_and_miss_earlier_pins() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], props(1)).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let r = g.create_rel(a, b, "R".into(), props(1)).unwrap();
    g.commit_write();
    assert!(node_inline(&g, a) && node_inline(&g, b) && rel_inline(&g, r));
    assert_eq!(node_versions(&g, a), [(1, 0)]);
    assert_eq!(g.used_bytes(), g.accounted_bytes());

    g.begin_write().unwrap();
    let c = g.create_node(vec!["C".into()], BTreeMap::new()).unwrap();
    g.commit_write();
    assert!(node_inline(&g, c));
    assert!(
        g.node_at(c, 1).is_none(),
        "a pin before the create misses it"
    );
    assert!(g.node_at(c, 2).is_some());
    assert!(g.node_at(a, 0).is_none());
    assert_eq!(g.used_bytes(), g.accounted_bytes());
}

#[test]
fn first_update_spills_and_each_pin_reads_its_version() {
    let (g, id) = committed_node();
    set_n(&g, id, 2);
    assert_eq!(node_versions(&g, id), [(1, 2), (2, 0)]);
    assert_eq!(g.used_bytes(), g.accounted_bytes());

    g.begin_write().unwrap();
    g.set_property_node(id, "n", Value::Int(3));
    assert_eq!(node_versions(&g, id), [(1, 2), (2, 3), (3, 0)]);
    // The successor is not published: the committed read still sees N(2).
    assert_eq!(node_n(&g, id, g.commit_ts()), Some(Value::Int(2)));
    g.commit_write();

    assert_eq!(node_n(&g, id, 0), None);
    assert_eq!(node_n(&g, id, 1), Some(Value::Int(1)));
    assert_eq!(node_n(&g, id, 2), Some(Value::Int(2)));
    assert_eq!(node_n(&g, id, 3), Some(Value::Int(3)));
    assert_eq!(g.used_bytes(), g.accounted_bytes());
}

#[test]
fn sets_in_one_write_reuse_one_version() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let id = g.create_node(vec!["A".into()], props(1)).unwrap();
    g.set_property_node(id, "n", Value::Int(2));
    g.set_property_node(id, "m", Value::Int(3));
    g.commit_write();
    assert!(
        node_inline(&g, id),
        "edits in the creating write stay inline"
    );

    g.begin_write().unwrap();
    g.set_property_node(id, "n", Value::Int(4));
    g.set_property_node(id, "m", Value::Int(5));
    g.add_labels(id, &["B".into()]);
    g.commit_write();
    assert_eq!(node_versions(&g, id), [(1, 2), (2, 0)]);
    assert_eq!(g.used_bytes(), g.accounted_bytes());
}

#[test]
fn abort_of_the_creating_write_removes_the_slot() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    let b = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    let r = g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    g.abort_write();
    assert!(!g.nodes.contains_key(&a) && !g.nodes.contains_key(&b));
    assert!(!g.rels.contains_key(&r));
    assert_eq!(g.used_bytes(), g.accounted_bytes());
}

#[test]
fn abort_of_the_spilling_write_restores_the_inline_row() {
    let (g, id) = committed_node();
    g.begin_write().unwrap();
    g.set_property_node(id, "n", Value::Int(2));
    assert!(!node_inline(&g, id));
    g.abort_write();
    assert!(node_inline(&g, id));
    assert_eq!(node_versions(&g, id), [(1, 0)]);
    assert_eq!(node_n(&g, id, g.commit_ts()), Some(Value::Int(1)));
    assert_eq!(g.used_bytes(), g.accounted_bytes());
}

#[test]
fn abort_of_a_later_version_pops_it_and_folds_a_lone_survivor() {
    let (g, id) = committed_node();
    set_n(&g, id, 2);
    g.begin_write().unwrap();
    g.set_property_node(id, "n", Value::Int(3));
    g.abort_write();
    assert_eq!(node_versions(&g, id), [(1, 2), (2, 0)]);

    // GC leaves one version; a later aborted write folds back to it.
    g.gc_versions();
    assert!(node_inline(&g, id));
    g.begin_write().unwrap();
    g.set_property_node(id, "n", Value::Int(4));
    g.abort_write();
    assert!(node_inline(&g, id));
    assert_eq!(node_versions(&g, id), [(2, 0)]);
    assert_eq!(node_n(&g, id, g.commit_ts()), Some(Value::Int(2)));
}

#[test]
fn abort_of_a_delete_restores_the_inline_row() {
    let (g, id) = committed_node();
    g.begin_write().unwrap();
    g.delete_node(id, false).unwrap();
    assert_eq!(node_versions(&g, id), [(1, 2)]);
    g.abort_write();
    assert!(node_inline(&g, id));
    assert_eq!(node_n(&g, id, g.commit_ts()), Some(Value::Int(1)));
    assert_eq!(g.used_bytes(), g.accounted_bytes());
}

#[test]
fn gc_folds_a_lone_live_version_and_drops_a_collected_tombstone() {
    let (g, id) = committed_node();
    g.begin_write().unwrap();
    let other = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    let r = g.create_rel(id, other, "R".into(), props(1)).unwrap();
    g.commit_write();
    let pin = g.pin_now().unwrap();
    set_n(&g, id, 2);
    g.begin_write().unwrap();
    g.set_property_rel(r, "n", Value::Int(2));
    g.commit_write();

    g.gc_versions();
    assert!(!node_inline(&g, id), "a pin still reads the old versions");
    drop(pin);
    g.gc_versions();
    assert!(node_inline(&g, id) && rel_inline(&g, r));
    assert_eq!(node_versions(&g, id), [(3, 0)]);
    assert_eq!(node_n(&g, id, g.commit_ts()), Some(Value::Int(2)));
    assert_eq!(g.used_bytes(), g.accounted_bytes());

    g.begin_write().unwrap();
    g.delete_node(other, true).unwrap();
    g.commit_write();
    assert!(!rel_inline(&g, r) && !node_inline(&g, other));
    assert_eq!(g.used_bytes(), g.accounted_bytes());
    g.gc_versions();
    assert!(!g.nodes.contains_key(&other) && !g.rels.contains_key(&r));
    assert_eq!(g.used_bytes(), g.accounted_bytes());
}

#[test]
fn inline_slot_charges_no_version_list_or_xmax() {
    use crate::memory::{node_chain_bytes, ChainShape};
    // The row lives in the id slot. A spilled chain adds its version
    // buffer, and nothing else, on the heap.
    assert_eq!(node_chain_bytes(ChainShape::Inline), 0);
    assert_eq!(
        node_chain_bytes(ChainShape::Spilled(1)),
        crate::graph::node_version_stride()
    );
}

/// The admission estimate equals the charge the apply lands, for every
/// transition of both shapes, and reports a delete's spill on its own.
#[test]
fn estimate_matches_the_charge_across_shapes() {
    let g = Graph::default();
    let step = |ops: Vec<RedoOp>, spill: bool| {
        let ops_dbg = format!("{ops:?}");
        let before = g.used_bytes();
        let estimate = g.estimate_redo_delta_with_constraint_growth(&ops, None);
        g.apply_redo(ops, g.commit_ts() + 1).unwrap();
        assert_eq!(g.used_bytes(), g.accounted_bytes());
        assert_eq!(
            g.used_bytes() as i64 - before as i64,
            estimate.total,
            "{ops_dbg}"
        );
        assert_eq!(estimate.tombstone_spill > 0, spill, "{ops_dbg}");
    };
    let create = |id: NodeId| RedoOp::create_node(id, Arc::new(vec!["A".into()]), props(1));
    let set = |id: NodeId, n: i64| RedoOp::set_node_prop(id, "n".into(), Value::Int(n));
    step(vec![create(0), create(1), create(2), create(3)], false);
    // The self-loop keeps the (A, R, A) count entry alive, so deleting rel
    // 4 moves only its slot.
    step(
        vec![
            RedoOp::create_rel(4, 0, 1, "R".into(), props(1)),
            RedoOp::create_rel(5, 3, 3, "R".into(), props(1)),
        ],
        false,
    );
    // Inline to spilled, then spilled to longer.
    step(vec![set(0, 2)], false);
    step(vec![set(0, 3)], false);
    // A set and a delete in one batch spill once.
    step(
        vec![set(1, 2), RedoOp::delete_rel(4), RedoOp::delete_node(1)],
        true,
    );
    // A delete of an inline row spills it.
    step(vec![RedoOp::delete_node(2)], true);
    // A delete of a spilled row adds nothing.
    step(vec![RedoOp::delete_node(0)], false);
    // Create and delete in one batch.
    step(vec![create(6), RedoOp::delete_node(6)], true);
    // An id past the dense cap is a side-map entry on the node store, the
    // relationship store, and both adjacency lists. The estimate charges
    // that entry, not the slot array the store refuses to allocate.
    let slot = crate::graph::node_slot()
        .min(crate::graph::rel_slot())
        .min(crate::graph::adj_slot());
    let mut wide = 1i64;
    loop {
        let need = usize::try_from(wide).unwrap().saturating_add(1);
        match crate::memory::dense_span_bytes(need, slot) {
            Some(bytes) if bytes <= crate::memory::DENSE_SPAN_CAP_BYTES => {
                wide = wide.saturating_mul(2);
            }
            _ => break,
        }
    }
    step(vec![create(wide)], false);
    step(
        vec![RedoOp::create_rel(wide, 3, wide, "R".into(), props(1))],
        false,
    );
    // Collection returns the spill.
    let before = g.used_bytes();
    g.gc_versions();
    assert!(g.used_bytes() < before);
    assert!(node_inline(&g, 3));
}
