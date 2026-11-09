//! The adjacency-walk suites (LABS-801 split): snapshot neighbor
//! iteration, typed-chunk order and filtering, and the resolved
//! interned-id walk.

use std::collections::BTreeMap;

use relate_core::Value;

use super::super::Graph;

#[test]
fn for_each_neighbor_matches_neighbors_at() {
    use relate_ir::Direction;
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let c = g.create_node(vec!["C".into()], BTreeMap::new()).unwrap();
    g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    g.create_rel(c, a, "R".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    for dir in [Direction::Outgoing, Direction::Incoming, Direction::Both] {
        let listed = g.neighbors(a, dir);
        let mut walked = Vec::new();
        g.for_each_neighbor_at(a, dir, g.commit_ts(), &[] as &[String], |rid| {
            walked.push(rid)
        });
        assert_eq!(walked, listed, "dir={dir:?}");
    }
}

#[test]
fn typed_adj_walks_newest_ts_first() {
    use relate_ir::Direction;
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let c = g.create_node(vec!["C".into()], BTreeMap::new()).unwrap();
    let d = g.create_node(vec!["D".into()], BTreeMap::new()).unwrap();
    let old = g
        .create_rel(
            a,
            b,
            "T".into(),
            BTreeMap::from([("ts".into(), Value::Int(1))]),
        )
        .unwrap();
    let newest = g
        .create_rel(
            a,
            c,
            "T".into(),
            BTreeMap::from([("ts".into(), Value::Int(30))]),
        )
        .unwrap();
    let mid = g
        .create_rel(
            a,
            d,
            "T".into(),
            BTreeMap::from([("ts".into(), Value::Int(10))]),
        )
        .unwrap();
    g.commit_write();
    let mut walked = Vec::new();
    g.for_each_neighbor_at(
        a,
        Direction::Outgoing,
        g.commit_ts(),
        &["T".to_string()],
        |rid| walked.push(rid),
    );
    assert_eq!(walked, vec![newest, mid, old]);
    g.begin_write().unwrap();
    g.set_property_rel(old, "ts", Value::Int(40));
    g.commit_write();
    walked.clear();
    g.for_each_neighbor_at(
        a,
        Direction::Outgoing,
        g.commit_ts(),
        &["T".to_string()],
        |rid| walked.push(rid),
    );
    assert_eq!(walked, vec![old, newest, mid]);
}

#[test]
fn typed_adj_skips_other_types() {
    use relate_ir::Direction;
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let c = g.create_node(vec!["C".into()], BTreeMap::new()).unwrap();
    let t = g.create_rel(a, b, "T".into(), BTreeMap::new()).unwrap();
    g.create_rel(a, c, "OTHER".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    let mut only_t = Vec::new();
    g.for_each_neighbor_at(
        a,
        Direction::Outgoing,
        g.commit_ts(),
        &["T".to_string()],
        |rid| only_t.push(rid),
    );
    assert_eq!(only_t, vec![t]);
    let mut dup = Vec::new();
    g.for_each_neighbor_at(
        a,
        Direction::Outgoing,
        g.commit_ts(),
        &["T".to_string(), "T".to_string()],
        |rid| dup.push(rid),
    );
    assert_eq!(dup, vec![t]);
}

/// LABS-801: the resolved walk (`try_for_each_neighbor_resolved_at`) is the
/// id-keyed core every engine walk rides; these pin its filter semantics —
/// one type, several types, a name the store never interned, a mixed
/// known/unknown list, and the untyped `Any`.
#[test]
fn resolved_typed_walk_filters_by_interned_id() {
    use relate_ir::Direction;

    use crate::store::RelTypes;

    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let c = g.create_node(vec!["C".into()], BTreeMap::new()).unwrap();
    let d = g.create_node(vec!["D".into()], BTreeMap::new()).unwrap();
    let r = g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    let s = g.create_rel(a, c, "S".into(), BTreeMap::new()).unwrap();
    g.create_rel(a, d, "T".into(), BTreeMap::new()).unwrap();
    g.commit_write();

    let walk = |types: &RelTypes| {
        let mut out = Vec::new();
        g.try_for_each_neighbor_resolved_at(a, Direction::Outgoing, g.commit_ts(), types, |rid| {
            out.push(rid);
            Ok::<(), ()>(())
        })
        .unwrap();
        out
    };

    // One type.
    assert_eq!(walk(&g.rel_types(&["R"])), vec![r]);
    // Several types: one chunk per type, in the filter's order.
    assert_eq!(walk(&g.rel_types(&["S", "R"])), vec![s, r]);
    // A name the store never interned has no id and matches nothing.
    assert!(walk(&g.rel_types(&["GHOST"])).is_empty());
    // A mixed known/unknown list walks the known type's chunk only.
    assert_eq!(walk(&g.rel_types(&["GHOST", "R"])), vec![r]);
    // The untyped filter walks every chunk.
    assert_eq!(walk(&RelTypes::Any).len(), 3);
}

/// A callback that walks the next node re-enters the same adjacency
/// store. `IdVec` is not re-entrant, so this deadlocks if the hop ids
/// are still borrowed from the store lock. Two hops is the identity
/// loadtest's `MEMBER_OF*1..8` shape.
#[test]
fn neighbor_walk_reenters_the_same_store() {
    use relate_ir::Direction;
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let c = g.create_node(vec!["C".into()], BTreeMap::new()).unwrap();
    g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    let bc = g.create_rel(b, c, "R".into(), BTreeMap::new()).unwrap();
    g.commit_write();

    let mut second_hop = Vec::new();
    g.for_each_neighbor_at(
        a,
        Direction::Outgoing,
        g.commit_ts(),
        &["R".to_string()],
        |rid| {
            let (start, end) = g.rel_ends_at(rid, g.commit_ts()).unwrap();
            let next = if start == a { end } else { start };
            g.for_each_neighbor_at(
                next,
                Direction::Outgoing,
                g.commit_ts(),
                &["R".to_string()],
                |next_rid| second_hop.push(next_rid),
            );
        },
    );
    assert_eq!(second_hop, vec![bc]);
}

/// The hop walk's (rel, peer) pairs for one node — the one collector the
/// two hop-walk tests share.
fn hop_pairs(
    g: &Graph,
    at: relate_core::NodeId,
    dir: relate_ir::Direction,
    ts: u64,
    types: &crate::store::RelTypes,
) -> Vec<(relate_core::RelId, relate_core::NodeId)> {
    let mut out = Vec::new();
    g.try_for_each_hop_resolved_at(at, dir, ts, types, |rid, peer| {
        out.push((rid, peer));
        Ok::<(), ()>(())
    })
    .unwrap();
    out
}

/// The hop spelling's walk pinned against literal expectations: chunk
/// order (ts DESC, newest rel first — ADR-0203), direction filtering,
/// peer resolution, the self-loop's single hop under `Both`, and a rel
/// invisible at the probed ts. Literals, not a differential against the
/// rid-only spelling — that spelling delegates to this walk, so a
/// differential would assert the walk against itself.
#[test]
fn hop_walk_matches_literal_expectations() {
    use crate::store::RelTypes;
    use relate_ir::Direction;

    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let r_ab = g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    let r_ba = g.create_rel(b, a, "R".into(), BTreeMap::new()).unwrap();
    let r_loop = g.create_rel(a, a, "LOOP".into(), BTreeMap::new()).unwrap();
    let r_ghost = g.create_rel(a, b, "GHOST".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    let ts = g.commit_ts();

    let walk = |dir: Direction, types: &RelTypes| hop_pairs(&g, a, dir, ts, types);

    // Outgoing: a's out-chunks in insertion order (R, LOOP, GHOST).
    assert_eq!(
        walk(Direction::Outgoing, &g.rel_types(&["R"])),
        vec![(r_ab, b)]
    );
    assert_eq!(
        walk(Direction::Outgoing, &g.rel_types(&["LOOP"])),
        vec![(r_loop, a)]
    );
    assert_eq!(
        walk(Direction::Outgoing, &RelTypes::Any),
        vec![(r_ab, b), (r_loop, a), (r_ghost, b)]
    );
    // Incoming: a's in-chunks hold only b→a (R) and the loop.
    assert_eq!(
        walk(Direction::Incoming, &g.rel_types(&["R"])),
        vec![(r_ba, b)]
    );
    assert_eq!(
        walk(Direction::Incoming, &RelTypes::Any),
        vec![(r_ba, b), (r_loop, a)]
    );
    // Both: out pass then in pass; the loop dedups to one hop.
    assert_eq!(
        walk(Direction::Both, &g.rel_types(&["R"])),
        vec![(r_ab, b), (r_ba, b)]
    );
    assert_eq!(
        walk(Direction::Both, &g.rel_types(&["LOOP"])),
        vec![(r_loop, a)],
        "the self-loop is one hop under Both, not two"
    );
    assert_eq!(
        walk(Direction::Both, &RelTypes::Any),
        vec![(r_ab, b), (r_loop, a), (r_ghost, b), (r_ba, b)]
    );

    // A rel deleted after the commit is invisible at the later ts — the
    // same matrix mode as the other cells: the same walk closure at the
    // new commit ts, the literal surviving list.
    g.begin_write().unwrap();
    g.delete_rel(r_loop);
    g.commit_write();
    let ts = g.commit_ts();
    let walk = |dir: Direction, types: &RelTypes| hop_pairs(&g, a, dir, ts, types);
    assert_eq!(
        walk(Direction::Both, &RelTypes::Any),
        vec![(r_ab, b), (r_ghost, b), (r_ba, b)],
        "the deleted rel is not a hop"
    );
}

/// A rel id reused with different endpoints (WAL redo replay keeps the
/// first hops as history — the `pair_counts_skip_a_reused_rel_ids_stale_hop`
/// precedent): rel 6 lives (1→2), dies, and its id is re-created (1→3).
/// Node 1's out-adjacency then carries TWO hops named 6 and node 2's
/// in-adjacency a stale one. The `Both` walk must key dedup on the rel
/// id — an endpoint check would double-emit — and a stale hop whose
/// resolved endpoints no longer touch the walked node drops via
/// `hop_other`.
#[test]
fn hop_walk_dedups_a_reused_rel_ids_stale_hops() {
    use relate_ir::Direction;

    use crate::graph::RedoOp;
    use crate::store::RelTypes;

    let g = Graph::default();
    g.apply_redo(
        vec![
            RedoOp::create_node(1, vec!["A".to_string()], BTreeMap::new()),
            RedoOp::create_node(2, vec!["B".to_string()], BTreeMap::new()),
            RedoOp::create_node(3, vec!["C".to_string()], BTreeMap::new()),
        ],
        1,
    )
    .unwrap();
    g.apply_redo(
        vec![RedoOp::create_rel(
            6,
            1,
            2,
            "R".to_string(),
            // Distinct ts payloads per life: equal (ts, id) hops compare
            // equal in TypedAdj::insert_hop and the second create's hop
            // would be dropped from the chunk — the double-hop fixture
            // needs both to survive.
            BTreeMap::from([("ts".to_string(), Value::Int(1))]),
        )],
        2,
    )
    .unwrap();
    g.apply_redo(vec![RedoOp::delete_rel(6)], 3).unwrap();
    g.apply_redo(
        vec![RedoOp::create_rel(
            6,
            1,
            3,
            "R".to_string(),
            BTreeMap::from([("ts".to_string(), Value::Int(2))]),
        )],
        4,
    )
    .unwrap();

    let walk = |at, dir| hop_pairs(&g, at, dir, g.commit_ts(), &RelTypes::Any);

    // Per-arm contract under the reuse: Both dedups by rel id, so the two
    // surviving out-hops named 6 emit once, with the resolved peer. The
    // directed arms do not collapse a reused id's repeat entries — the
    // walk's policy since before this change (storage-level pruning of
    // stale hops is the deferred invariant, LABS-928).
    assert_eq!(
        walk(1, Direction::Both),
        vec![(6, 3)],
        "Both collapses the two surviving out-hops named 6"
    );
    assert_eq!(
        walk(1, Direction::Outgoing),
        vec![(6, 3), (6, 3)],
        "the directed arms keep both repeat entries (pre-existing policy, LABS-928)"
    );
    assert_eq!(walk(1, Direction::Incoming), vec![]);
    // The stale hop in node 2's in-adjacency resolves to (1→3): node 2 is
    // not an endpoint, so the hop drops.
    assert_eq!(walk(2, Direction::Both), vec![]);
    assert_eq!(walk(3, Direction::Both), vec![(6, 1)]);
}
