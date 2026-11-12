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

/// The estimate-then-apply oracle both charge-exactness tests run: the
/// estimate equals the charge the apply lands, the recount agrees with the
/// used bytes, and the delete spill is reported on its own.
fn step_matches_charge(g: &Graph, ops: Vec<RedoOp>, spill: bool) {
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
    assert_eq!(estimate.spill > 0, spill, "{ops_dbg}");
}

/// The admission estimate equals the charge the apply lands, for every
/// transition of both shapes, and reports a delete's spill on its own.
#[test]
fn estimate_matches_the_charge_across_shapes() {
    let g = Graph::default();
    let step = |ops: Vec<RedoOp>, spill: bool| step_matches_charge(&g, ops, spill);
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
    let wide = crate::graph::test_support::first_side_map_id(slot);
    step(vec![create(wide)], false);
    // A create's adjacency slot growth — dense span or the side-map entry a
    // wide id takes instead — is committed structure, refusable, like the
    // node and rel stores' own estimator charges it.
    step(
        vec![RedoOp::create_rel(wide, 3, wide, "R".into(), props(1))],
        false,
    );
    // A delete that drains the last (A, S, A) pair entry refunds its
    // creation charge — the stats entry term priced (the pair count is
    // the counting operator's answer set, so the drain removes the
    // entry and returns its bytes).
    step(vec![create(7), create(8)], false);
    step(
        vec![RedoOp::create_rel(9, 7, 8, "S".into(), props(1))],
        false,
    );
    step(vec![RedoOp::delete_rel(9)], true);

    // The same drain for a rel created and deleted in one batch nets the
    // create's charge against the refund — pinned after the AddLabels
    // section below, on endpoints its own batch creates.

    // A repeat delete of a dead committed rel tombstones nothing —
    // priced once, however many deletes follow.
    // Both rels ride the oracle's own step — every batch in this test
    // carries the estimate==charge assertion. The survivor keeps the
    // (A, S3, A) entry alive; the drain itself is pinned above.
    step(
        vec![
            RedoOp::create_rel(11, 7, 8, "S3".into(), props(1)),
            RedoOp::create_rel(12, 7, 8, "S3".into(), props(1)),
        ],
        false,
    );
    let doomed = 11;
    step(
        vec![
            RedoOp::delete_rel(doomed),
            RedoOp::delete_rel(doomed),
            RedoOp::delete_rel(doomed),
        ],
        true,
    );

    // An AddLabels on a node whose self-loop rides both adjacency sides:
    // the (joined, type, joined) diagonal is charged once — the apply's
    // own self-loop policy (`Graph::self_loop_charge_sets`), consulted
    // through the record. Pricing the plain orientation instead would
    // charge that diagonal on both sides.
    step(vec![create(13), create(14)], false);
    step(
        vec![RedoOp::create_rel(15, 13, 13, "T".into(), props(1))],
        false,
    );
    // The add and the drain share one batch — the overlay is seeded once
    // per batch, so only here does the add's count for the diagonal show
    // in the delete's refund. The delete drains every (label, T, label)
    // key the add charged: pricing the plain orientation charges the
    // diagonal on both sides, and the drain then leaves one entry
    // un-refunded.
    step(
        vec![
            RedoOp::add_labels(13, vec!["B".into()]),
            RedoOp::delete_rel(15),
        ],
        true,
    );

    // The drain of a rel created and deleted in one batch, beside an
    // AddLabels on its endpoint: the count entries the create charged —
    // labels(start) × labels(end) pairs for its type — are refunded by
    // the delete, and the delete's tombstone hides the batch-create's
    // hop, the hop and never the chunk, so the batch nets to the label
    // add alone.
    step(
        vec![
            RedoOp::create_rel(16, 13, 14, "U".into(), props(1)),
            RedoOp::delete_rel(16),
            RedoOp::add_labels(13, vec!["C".into()]),
        ],
        true,
    );

    // Collection returns the spill.
    let before = g.used_bytes();
    g.gc_versions();
    assert!(g.used_bytes() < before);
    assert!(node_inline(&g, 3));
}

/// A delete's sealed-tier growth — the on-demand region, its slot array, and
/// the tombstone on an already-sealed chunk — is estimated exactly. The
/// fold-reclaimed terms land in the spill (ADR-0529: a delete at the cap
/// commits); the founding slot growth the dense span keeps is refusable,
/// pinned non-vacuously below. A create batch spills nothing.
#[test]
fn estimate_matches_the_charge_on_sealed_deletes() {
    let g = Graph::default();
    let step = |ops: Vec<RedoOp>, spill: bool| step_matches_charge(&g, ops, spill);
    // Label-less endpoints: a delete must not also drop a count entry, which
    // the estimate reports as growth only.
    let create = |id: NodeId| RedoOp::create_node(id, Arc::new(Vec::new()), props(1));
    step(vec![create(0), create(1)], false);
    step(
        vec![RedoOp::create_rel(2, 0, 1, "U".into(), props(1))],
        false,
    );

    // A type that never sealed: the delete creates the on-demand region.
    step(vec![RedoOp::delete_rel(2)], true);

    // A rel created and deleted in one batch never reaches the committed
    // store, so the delete's endpoints come from the batch's own create —
    // its tombstones land on the region the delete above founded.
    step(
        vec![
            RedoOp::create_rel(3, 0, 1, "U".into(), props(1)),
            RedoOp::delete_rel(3),
        ],
        true,
    );

    // A hop in a folded chunk: the delete grows that chunk's tombstones —
    // one per endpoint — and that growth is all spill.
    let (a, b, ids, _) = crate::graph::test_support::folded_floor_chunk(&g);
    let sealed = vec![RedoOp::delete_rel(ids[0])];
    let (refusable, spill) = g.estimate_adj_delta(&sealed);
    let stride = crate::adjacency::tombstone_stride() as i64;
    assert_eq!(refusable, 0);
    assert_eq!(spill, 2 * stride, "one tombstone slot per endpoint");
    step(sealed, true);

    // A never-sealed type joining a region the endpoint already holds: the
    // on-demand chunk is priced as entry growth on that region, not the
    // region-founding term.
    let joiner = crate::graph::test_support::doomed_rel(&g, a, b, "V");
    step(vec![RedoOp::delete_rel(joiner)], true);

    // Two never-sealed types joining one region in a single batch: the
    // chunks-vec entry growth is one before-and-after term per region, not
    // one per type — the pow2 reservation makes the difference bite (a
    // two-chunk region gaining two entries charges vec_heap(4) - vec_heap(2),
    // not twice vec_heap(3) - vec_heap(2)).
    let pair = [
        crate::graph::test_support::doomed_rel(&g, a, b, "W"),
        crate::graph::test_support::doomed_rel(&g, a, b, "X"),
    ];
    step(
        vec![RedoOp::delete_rel(pair[0]), RedoOp::delete_rel(pair[1])],
        true,
    );

    // The routing, pinned non-vacuously: a founding delete at an endpoint
    // beyond the sealed side table's span charges its slot growth as
    // refusable (committed structure, as the store's own estimator charges
    // it) and its chunk and tombstone terms as spill.
    let (edge, other) = crate::graph::test_support::endpoints(&g);
    let founder = crate::graph::test_support::doomed_rel(&g, edge, other, "Y");
    let (found_refusable, found_spill) = g.estimate_adj_delta(&[RedoOp::delete_rel(founder)]);
    assert!(
        found_refusable > 0,
        "a founding delete's slot growth is refusable"
    );
    assert!(found_spill > 0, "its chunk and tombstone terms spill");
    step(vec![RedoOp::delete_rel(founder)], true);

    // The same founding delete at a side-map id — past the dense cap, so the
    // store holds the entry in its side map: the entry is reclaimed with the
    // region, so it is spill and the delete refuses nothing.
    let sparse = crate::graph::test_support::first_side_map_id(crate::graph::sealed_slot());
    g.apply_redo(vec![create(sparse)], g.commit_ts() + 1)
        .unwrap();
    let doomed = crate::graph::test_support::doomed_rel(&g, sparse, 0, "Z");
    let (sparse_refusable, sparse_spill) = g.estimate_adj_delta(&[RedoOp::delete_rel(doomed)]);
    assert_eq!(
        sparse_refusable, 0,
        "a founding delete at a side-map id refuses nothing"
    );
    assert!(
        sparse_spill > 0,
        "its side-map entry is reclaimed, so spill"
    );
    step(vec![RedoOp::delete_rel(doomed)], true);
}
