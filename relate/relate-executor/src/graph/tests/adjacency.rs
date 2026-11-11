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
/// in-adjacency a stale one.
fn reused_rel_id_graph() -> Graph {
    use crate::graph::RedoOp;

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
    g
}

/// The reused-rel-id fixture at the hop spelling: the `Both` walk must key
/// dedup on the rel id — an endpoint check would double-emit — and a stale
/// hop whose resolved endpoints no longer touch the walked node drops via
/// `hop_other`.
#[test]
fn hop_walk_dedups_a_reused_rel_ids_stale_hops() {
    use relate_ir::Direction;

    use crate::store::RelTypes;

    let g = reused_rel_id_graph();

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

/// The same reused-rel-id fixture at the kernel's other call sites: the
/// rid-only spelling (`try_for_each_neighbor_resolved_at`) and the
/// name-keyed boundary walk (`for_each_neighbor_at`) ride the one hop walk,
/// so they drop the stale hop and dedup the reused id exactly as it does.
#[test]
fn neighbor_walk_drops_a_reused_rel_ids_stale_hops() {
    use relate_ir::Direction;

    use crate::store::RelTypes;

    let g = reused_rel_id_graph();

    // The rid-only kernel spelling: the hop walk projected to rids.
    let rids = |at, dir| {
        let mut out = Vec::new();
        g.try_for_each_neighbor_resolved_at(at, dir, g.commit_ts(), &RelTypes::Any, |rid| {
            out.push(rid);
            Ok::<(), ()>(())
        })
        .unwrap();
        out
    };
    assert_eq!(
        rids(1, Direction::Both),
        vec![6],
        "Both dedups the reused id"
    );
    assert_eq!(
        rids(1, Direction::Outgoing),
        vec![6, 6],
        "the directed arm keeps both repeat entries"
    );
    assert_eq!(
        rids(2, Direction::Both),
        Vec::<relate_core::RelId>::new(),
        "the stale hop drops: node 2 is not an endpoint"
    );
    assert_eq!(rids(3, Direction::Both), vec![6]);

    // The name-keyed boundary walk agrees with the rid-only spelling.
    let named = |at, dir| {
        let mut out = Vec::new();
        g.for_each_neighbor_at(at, dir, g.commit_ts(), &["R".to_string()], |rid| {
            out.push(rid)
        });
        out
    };
    assert_eq!(named(1, Direction::Both), vec![6]);
    assert_eq!(named(1, Direction::Outgoing), vec![6, 6]);
    assert_eq!(named(2, Direction::Both), Vec::<relate_core::RelId>::new());
    assert_eq!(named(3, Direction::Both), vec![6]);
}

/// The sealed tier is a side table, not a per-node field: a node whose
/// chunks never folded has no entry there, and the checkpoint fold lands one
/// where it seals.
#[test]
fn sealed_side_table_entry_exists_only_where_a_fold_lands() {
    let g = Graph::default();
    let (a, b) = crate::graph::test_support::endpoints(&g);
    g.begin_write().unwrap();
    g.create_rel(a, b, "T".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    assert!(
        g.sealed_out.get(&a).is_none() && g.sealed_in.get(&b).is_none(),
        "a chunk below the floor has no sealed entry"
    );

    let floor = crate::adjacency::COMPACTION_DELTA_FLOOR as i64;
    g.begin_write().unwrap();
    for i in 0..floor - 1 {
        g.create_rel(
            a,
            b,
            "T".into(),
            BTreeMap::from([("ts".into(), Value::Int(i))]),
        )
        .unwrap();
    }
    g.commit_write();
    g.gc_versions();

    let sealed = g.sealed_out.get(&a).expect("the fold lands an entry");
    assert!(sealed.chunks.iter().all(|(_, chunk)| chunk.len() > 0));
    assert!(g.sealed_in.get(&b).is_some());
}

/// One checkpoint pass folds several nodes' due chunks, each under its own
/// per-node write guard: every pair's out and in side seals its hops.
#[test]
fn a_multi_node_fold_seals_every_due_chunk() {
    let g = Graph::default();
    let (a, b) = crate::graph::test_support::endpoints(&g);
    let (c, d) = crate::graph::test_support::endpoints(&g);
    let floor = crate::adjacency::COMPACTION_DELTA_FLOOR as i64;
    let mut created: [Vec<_>; 2] = [Vec::new(), Vec::new()];
    g.begin_write().unwrap();
    for i in 0..floor {
        for (pair, (from, to)) in [(a, b), (c, d)].iter().enumerate() {
            created[pair].push(
                g.create_rel(
                    *from,
                    *to,
                    "T".into(),
                    BTreeMap::from([("ts".into(), Value::Int(i))]),
                )
                .unwrap(),
            );
        }
    }
    g.commit_write();
    let type_id = g.interner.get_id("T").unwrap();
    g.gc_versions();

    for ((from, to), ids) in [((a, b), &created[0]), ((c, d), &created[1])] {
        let sealed = g
            .sealed_out
            .get(&from)
            .expect("the fold lands a sealed entry");
        let mut sealed_ids: Vec<_> = sealed
            .chunk(type_id)
            .expect("the sealed chunk")
            .decoded()
            .iter()
            .map(|hop| hop.id)
            .collect();
        let mut expected = ids.clone();
        sealed_ids.sort_unstable();
        expected.sort_unstable();
        assert_eq!(sealed_ids, expected, "the out fold of {from}");
        assert_eq!(
            g.sealed_in.get(&to).unwrap().chunk_len(type_id),
            crate::adjacency::COMPACTION_DELTA_FLOOR,
            "the in side of {to} seals too"
        );
    }
}

/// A delete of a sealed hop records the versioned tombstone on the real
/// delete path (ADR-0551): reaching the seal floor folds the delta, then
/// `delete_rel` must leave the physical sealed hop and a tombstone behind.
#[test]
fn delete_of_a_sealed_hop_records_the_versioned_tombstone() {
    let g = Graph::default();
    let (a, _, ids, type_id) = crate::graph::test_support::folded_floor_chunk(&g);
    assert_eq!(
        g.sealed_out.get(&a).unwrap().chunk_len(type_id),
        crate::adjacency::COMPACTION_DELTA_FLOOR
    );

    g.begin_write().unwrap();
    g.delete_rel(ids[5]);
    g.commit_write();

    let region = g.sealed_out.get(&a).expect("sealed region");
    let sealed = region.chunk(type_id).expect("sealed chunk");
    assert_eq!(
        sealed.len(),
        crate::adjacency::COMPACTION_DELTA_FLOOR,
        "the sealed hop stays physical until reclaimed"
    );
    assert_eq!(sealed.tombstones.len(), 1);
    assert_eq!(sealed.tombstones[0].id, ids[5]);
    assert!(sealed.tombstones[0].xmax > 0);
}

/// A delete records its tombstone without a delta entry: `adj_remove` drops
/// the delta entry one `IdVec` lock before the sealed region's, and a delete
/// landing in that window must still hide the sealed hop (ADR-0551).
#[test]
fn delete_without_a_delta_entry_still_tombstones_the_sealed_hop() {
    let g = Graph::default();
    let (a, _, ids, type_id) = crate::graph::test_support::folded_floor_chunk(&g);

    // The race window's state: delta entry absent, sealed region present.
    assert!(
        g.out_adj.remove(&a).is_some(),
        "the fold leaves a delta entry"
    );
    assert!(g.out_adj.get(&a).is_none(), "delta entry absent");
    assert!(g.sealed_out.get(&a).is_some(), "sealed region present");

    g.begin_write().unwrap();
    g.delete_rel(ids[5]);
    g.commit_write();

    let region = g.sealed_out.get(&a).expect("sealed region");
    let sealed = region.chunk(type_id).expect("sealed chunk");
    assert_eq!(sealed.tombstones.len(), 1);
    let tombstone = sealed.tombstones[0];
    assert_eq!(tombstone.id, ids[5]);
    assert!(tombstone.deleted.is_some());
    let visible = |ts: u64| sealed.visible_hops(ts).iter().any(|h| h.id == ids[5]);
    assert!(
        visible(tombstone.xmax - 1),
        "an older snapshot sees the hop"
    );
    assert!(!visible(tombstone.xmax), "the tombstone hides it at xmax");
}

/// A deleted rel leaves no ghost hop in the sealed stream. The fold runs
/// before the chain GC, so a deleted hop still in the delta gets sealed —
/// under its tombstone — instead of escaping the GC's prune; once the
/// horizon passes, the reclaim drops the hop and the sealed counts with it.
#[test]
fn deleted_delta_hop_reclaims_out_of_the_sealed_stream_without_a_ghost() {
    use relate_ir::Direction;

    let g = Graph::default();
    let (a, b) = crate::graph::test_support::endpoints(&g);
    let floor = crate::adjacency::COMPACTION_DELTA_FLOOR as i64;
    g.begin_write().unwrap();
    for i in 0..floor {
        g.create_rel(
            a,
            b,
            "T".into(),
            BTreeMap::from([("ts".into(), Value::Int(i))]),
        )
        .unwrap();
    }
    g.commit_write();
    let type_id = g.interner.get_id("T").unwrap();
    g.compact_adjacency_at_watermark(0);
    assert_eq!(
        g.sealed_out.get(&a).unwrap().chunk_len(type_id),
        floor as usize
    );

    // A delta past 25% of the sealed hops makes the chunk due, and the
    // deleted hop is still in that delta when the fold runs.
    let delta = floor / 4 + 1;
    let mut extra = Vec::new();
    g.begin_write().unwrap();
    for i in 0..delta {
        extra.push(
            g.create_rel(
                a,
                b,
                "T".into(),
                BTreeMap::from([("ts".into(), Value::Int(floor + i))]),
            )
            .unwrap(),
        );
    }
    g.commit_write();
    let deleted = extra[0];
    g.begin_write().unwrap();
    g.delete_rel(deleted);
    g.commit_write();

    // The fold below the horizon seals the hop under its tombstone, so an
    // older snapshot still sees it (ADR-0551).
    g.compact_adjacency_at_watermark(0);
    {
        let sealed = g.sealed_out.get(&a).unwrap();
        assert_eq!(sealed.chunk_len(type_id), (floor + delta) as usize);
        assert!(sealed
            .chunk(type_id)
            .unwrap()
            .decoded()
            .iter()
            .any(|h| h.id == deleted));
    }
    let mut walked = Vec::new();
    g.for_each_neighbor_at(
        a,
        Direction::Outgoing,
        g.commit_ts(),
        &["T".to_string()],
        |rid| walked.push(rid),
    );
    assert!(
        !walked.contains(&deleted),
        "visibility hides the deleted hop"
    );

    // Past the horizon the reclaim drops it: no trace in the sealed stream,
    // the physical hop stream, or the hop counts.
    g.gc_versions();
    {
        let adj = g.out_adj.get(&a).unwrap();
        let sealed = g.sealed_out.get(&a).unwrap();
        assert_eq!(sealed.chunk_len(type_id), (floor + delta - 1) as usize);
        assert!(!sealed
            .chunk(type_id)
            .unwrap()
            .decoded()
            .iter()
            .any(|h| h.id == deleted));
        let mut physical = Vec::new();
        adj.for_each_hop_of(Some(&sealed), type_id, 0, |hop| physical.push(hop.id));
        assert!(!physical.contains(&deleted), "no ghost hop");
    }
    assert_eq!(
        g.adj_len(a, Direction::Outgoing),
        (floor + delta - 1) as usize
    );
}

/// A node whose chunk never sealed (33 hops in one write) still records a
/// delete's tombstone: the chunk gets a zero-hop sealed stream on demand, so
/// the checkpoint GC's fold drops the dead delta hop instead of sealing it
/// as a ghost the chain GC can no longer prune (ADR-0551).
#[test]
fn a_never_sealed_chunk_never_seals_a_deleted_hop() {
    use relate_ir::Direction;

    let g = Graph::default();
    let (a, b) = crate::graph::test_support::endpoints(&g);
    let floor = crate::adjacency::COMPACTION_DELTA_FLOOR as i64;
    let mut ids = Vec::new();
    g.begin_write().unwrap();
    for i in 0..=floor {
        ids.push(
            g.create_rel(
                a,
                b,
                "T".into(),
                BTreeMap::from([("ts".into(), Value::Int(i))]),
            )
            .unwrap(),
        );
    }
    g.commit_write();
    let type_id = g.interner.get_id("T").unwrap();
    assert!(g
        .sealed_out
        .get(&a)
        .is_none_or(|region| region.chunk(type_id).is_none()));

    let deleted = ids[5];
    g.begin_write().unwrap();
    g.delete_rel(deleted);
    g.commit_write();
    g.gc_versions();

    let adj = g.out_adj.get(&a).unwrap();
    let sealed = g.sealed_out.get(&a).unwrap();
    assert_eq!(sealed.chunk_len(type_id), floor as usize);
    assert!(
        !adj.flatten(Some(&sealed)).contains(&deleted),
        "no ghost hop"
    );
    assert_eq!(g.adj_len(a, Direction::Outgoing), floor as usize);
}

/// A zero-hop sealed region reclaims its tombstones at every checkpoint: a
/// sub-floor chunk churned by delete/reposition folds the reclaim alone, so
/// the region and its tombstone set stay bounded by the live delta instead
/// of growing with every write (ADR-0551).
#[test]
fn a_zero_hop_sealed_region_reclaims_its_tombstones_at_the_checkpoint() {
    let g = Graph::default();
    let (a, b) = crate::graph::test_support::endpoints(&g);
    let type_id = g.interner.intern("T");
    // Two live hops below the floor keep the chunk non-empty through the
    // remove a reposition runs, so each round's tombstone stays on the
    // zero-hop region until the checkpoint fold.
    g.begin_write().unwrap();
    for ts in 0..2 {
        g.create_rel(
            a,
            b,
            "T".into(),
            BTreeMap::from([("ts".into(), Value::Int(ts))]),
        )
        .unwrap();
    }
    g.commit_write();

    for round in 2..10i64 {
        g.begin_write().unwrap();
        let id = g
            .create_rel(
                a,
                b,
                "T".into(),
                BTreeMap::from([("ts".into(), Value::Int(round))]),
            )
            .unwrap();
        g.commit_write();
        g.begin_write().unwrap();
        g.set_property_rel(id, "ts", Value::Int(1000 + round));
        g.delete_rel(id);
        g.commit_write();
        assert!(
            g.sealed_out
                .get(&a)
                .is_some_and(|region| region.chunk(type_id).is_some()),
            "round {round}: the write records the zero-hop region"
        );

        g.gc_versions();
        let sealed_tombstones = match g.sealed_out.get(&a) {
            Some(region) => region
                .chunk(type_id)
                .map_or(0, |chunk| chunk.tombstones.len()),
            None => 0,
        };
        let live_delta = g.out_adj.get(&a).map_or(0, |adj| adj.chunk(type_id).len());
        assert_eq!(
            live_delta, 2,
            "round {round}: the churned hop reclaimed, the live delta stays"
        );
        // The set never grows: the stale tombstone of the previous round is
        // reclaimed at this checkpoint, leaving only the round's own.
        assert_eq!(
            sealed_tombstones, 1,
            "round {round}: the tombstone set stays bounded by the live delta"
        );
    }
}

/// The write path records the superseded position without decoding the
/// sealed stream. A delete or ts-reposition of a hop that never sealed still
/// records its tombstone — the membership decode the write path used to run
/// would answer "absent" for it and record nothing, leaving the hop to seal
/// as a ghost.
#[test]
fn delta_hop_delete_and_reposition_record_the_tombstone_without_a_decode() {
    let g = Graph::default();
    let (a, b) = crate::graph::test_support::endpoints(&g);
    let floor = crate::adjacency::COMPACTION_DELTA_FLOOR as i64;
    g.begin_write().unwrap();
    for i in 0..floor {
        g.create_rel(
            a,
            b,
            "T".into(),
            BTreeMap::from([("ts".into(), Value::Int(i))]),
        )
        .unwrap();
    }
    g.commit_write();
    let type_id = g.interner.get_id("T").unwrap();
    g.compact_adjacency_at_watermark(0);

    // Two hops in one sealed chunk's delta, neither of them sealed.
    g.begin_write().unwrap();
    let deleted = g
        .create_rel(
            a,
            b,
            "T".into(),
            BTreeMap::from([("ts".into(), Value::Int(floor))]),
        )
        .unwrap();
    let moved = g
        .create_rel(
            a,
            b,
            "T".into(),
            BTreeMap::from([("ts".into(), Value::Int(floor + 1))]),
        )
        .unwrap();
    g.commit_write();
    g.begin_write().unwrap();
    g.delete_rel(deleted);
    g.commit_write();

    let tombstones = {
        let sealed = g.sealed_out.get(&a).unwrap();
        assert_eq!(
            sealed.chunk_len(type_id),
            floor as usize,
            "the write touches no sealed bytes"
        );
        sealed.chunk(type_id).unwrap().tombstones.clone()
    };
    let t = tombstones
        .iter()
        .find(|t| t.id == deleted)
        .expect("the delete records the tombstone");
    assert_eq!(t.deleted, Some(t.xmax), "a delete stamps both positions");
    assert!(t.xmax > 0);

    g.begin_write().unwrap();
    g.set_property_rel(moved, "ts", Value::Int(floor + 5));
    g.commit_write();
    let tombstones = {
        let sealed = g.sealed_out.get(&a).unwrap();
        sealed.chunk(type_id).unwrap().tombstones.clone()
    };
    let t = tombstones
        .iter()
        .find(|t| t.id == moved)
        .expect("the reposition records the tombstone");
    assert!(
        t.deleted.is_none(),
        "a reposition only supersedes the sealed position"
    );
}

/// Audit 2 trace 1-5: a sealed rel repositioned at `T`, a snapshot pinned at
/// `T`, then a delete at a later write, and GC at the pinned horizon. The
/// reclaim drops the sealed position on its supersede stamp and keeps the
/// relocated delta hop on its delete stamp, which the fold guard holds in
/// the delta — so the pinned reader still sees the rel as a neighbor.
#[test]
fn gc_at_the_pinned_horizon_keeps_a_relocated_delta_hop() {
    use relate_ir::Direction;

    let g = Graph::default();
    let (a, _, ids, type_id) = crate::graph::test_support::folded_floor_chunk(&g);
    let id = ids[5];

    g.begin_write().unwrap();
    g.set_property_rel(id, "ts", Value::Int(100));
    g.commit_write();
    let pin = g.pin_now().unwrap();
    let pinned = pin.ts();
    assert_eq!(pinned, g.commit_ts());

    g.begin_write().unwrap();
    g.delete_rel(id);
    g.commit_write();
    let deleted_at = g.commit_ts();
    assert!(deleted_at > pinned);

    g.gc_versions();

    {
        let adj = g.out_adj.get(&a).unwrap();
        let sealed = g.sealed_out.get(&a).unwrap();
        assert_eq!(
            sealed.chunk_len(type_id),
            crate::adjacency::COMPACTION_DELTA_FLOOR - 1,
            "the sealed position is reclaimed at the pinned horizon"
        );
        assert_eq!(
            adj.chunk(type_id).len(),
            1,
            "the relocated hop stays delta while its delete stamp is above the horizon"
        );
        let t = sealed
            .chunk(type_id)
            .unwrap()
            .tombstones
            .first()
            .copied()
            .expect("the tombstone still names the deleted delta hop");
        assert_eq!((t.xmax, t.deleted), (pinned, Some(deleted_at)));
    }

    let mut seen = Vec::new();
    g.for_each_neighbor_at(a, Direction::Outgoing, pinned, &["T".to_string()], |rid| {
        seen.push(rid)
    });
    assert!(
        seen.contains(&id),
        "the pinned reader at {pinned} still sees the rel: {seen:?}"
    );
    assert!(
        !g.neighbors(a, Direction::Outgoing).contains(&id),
        "the live reader sees the rel deleted"
    );
}

/// ADR-0551 accounting on the write path: once a chunk is sealed, a create,
/// a delete of a sealed hop, and a ts-reposition each charge only the layout
/// change they cause, and `used_bytes` still equals the recount oracle's
/// full walk after every write (and after the GC's reclaim-and-reseal).
#[test]
fn sealed_chunk_write_path_charges_match_the_recount_oracle() {
    let g = Graph::default();
    let (a, b, ids, type_id) = crate::graph::test_support::folded_floor_chunk(&g);
    let floor = crate::adjacency::COMPACTION_DELTA_FLOOR as i64;
    assert_eq!(
        g.sealed_out.get(&a).unwrap().chunk_len(type_id),
        floor as usize
    );
    assert_eq!(g.used_bytes(), g.accounted_bytes());

    // A create lands in the sealed chunk's delta: priced at live hop size.
    g.begin_write().unwrap();
    g.create_rel(
        a,
        b,
        "T".into(),
        BTreeMap::from([("ts".into(), Value::Int(floor))]),
    )
    .unwrap();
    g.commit_write();
    assert_eq!(g.used_bytes(), g.accounted_bytes());

    // A delete of a sealed hop charges its tombstone, not the sealed stream.
    g.begin_write().unwrap();
    g.delete_rel(ids[5]);
    g.commit_write();
    assert_eq!(g.used_bytes(), g.accounted_bytes());

    // A ts-reposition tombstones the sealed hop and adds a delta hop.
    g.begin_write().unwrap();
    g.set_property_rel(ids[6], "ts", Value::Int(floor + 1));
    g.commit_write();
    assert_eq!(g.used_bytes(), g.accounted_bytes());

    // The GC reclaims at the horizon, re-seals, and re-tallies the floor.
    g.gc_versions();
    assert_eq!(g.used_bytes(), g.accounted_bytes());
}

/// The sealed tier's floor is `vec_heap(len, stride)`, i.e.
/// `reserved_len(len) * stride`, so every vector it charges must hold
/// exactly that capacity — the reservation discipline the rest of the store
/// keeps. A sealed region after a fold, and the tombstone a delete records.
#[test]
fn sealed_vectors_hold_exactly_the_reserved_capacity() {
    use crate::memory::reserved_len;

    let g = Graph::default();
    let (a, _, ids, type_id) = crate::graph::test_support::folded_floor_chunk(&g);
    g.begin_write().unwrap();
    g.delete_rel(ids[5]);
    g.commit_write();

    let mut tombstoned = 0;
    for side in [&g.sealed_out, &g.sealed_in] {
        side.for_each(|_id, region| {
            assert_eq!(
                region.chunks.capacity(),
                reserved_len(region.chunks.len()),
                "the chunk vector holds its reservation"
            );
            for (_, chunk) in &region.chunks {
                assert_eq!(
                    chunk.bytes.capacity(),
                    reserved_len(chunk.bytes.len()),
                    "the sealed stream holds its reservation"
                );
                assert_eq!(
                    chunk.tombstones.capacity(),
                    reserved_len(chunk.tombstones.len()),
                    "the tombstones hold their reservation"
                );
                if !chunk.tombstones.is_empty() {
                    tombstoned += 1;
                }
            }
        });
    }
    assert_eq!(
        tombstoned, 2,
        "the delete tombstoned the sealed stream on both endpoints"
    );
    assert!(
        g.sealed_out.get(&a).unwrap().chunk_len(type_id) > 0,
        "the fold sealed the hops"
    );
}

/// F-1-58: the floor's recount charges the sealed tier's slot arrays beside
/// each region's own floor — the term the heap census reads as
/// `slot_bytes`. Dropping the side-table entries drops exactly those two
/// terms and nothing else.
#[test]
fn the_floor_charges_the_sealed_slot_arrays() {
    let g = Graph::default();
    crate::graph::test_support::folded_floor_chunk(&g);
    let arrays = g.sealed_out.array_bytes() + g.sealed_in.array_bytes();
    assert!(arrays > 0, "the fold created sealed entries");
    let mut floors = 0u64;
    for sealed in [&g.sealed_out, &g.sealed_in] {
        sealed.for_each(|_id, region| floors += region.floor_bytes());
    }
    let with_sealed = g.structure_bytes();
    g.sealed_out.clear();
    g.sealed_in.clear();
    assert_eq!(
        with_sealed - g.structure_bytes(),
        arrays + floors,
        "the slot arrays are charged beside the regions' floors"
    );
}
