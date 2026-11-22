//! The sealed tier's lifecycle suites (moved out of the adjacency-walk
//! file): fold placement, tombstoned deletes and reposition, reclaim at the
//! checkpoint, and the write path's accounting against the recount oracle.

use std::collections::BTreeMap;

use relate_core::Value;

use super::super::Graph;

/// The sealed tier is a graph-level arena, not a per-node field: a node
/// whose chunks never folded has no region there, and the checkpoint fold
/// lands one where it seals.
#[test]
fn sealed_side_table_entry_exists_only_where_a_fold_lands() {
    let g = Graph::default();
    let (a, b) = crate::graph::test_support::endpoints(&g);
    g.begin_write().unwrap();
    g.create_rel(a, b, "T".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    assert!(
        g.out_side().sealed().region(a).is_none() && g.in_side().sealed().region(b).is_none(),
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

    let sealed = g
        .out_side()
        .sealed()
        .region(a)
        .expect("the fold lands an entry");
    assert!(sealed.chunk_count() > 0);
    assert!(g.in_side().sealed().region(b).is_some());
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
            .region(from)
            .expect("the fold lands a sealed entry");
        let mut sealed_ids: Vec<_> = sealed.decoded(type_id).iter().map(|hop| hop.id).collect();
        let mut expected = ids.clone();
        sealed_ids.sort_unstable();
        expected.sort_unstable();
        assert_eq!(sealed_ids, expected, "the out fold of {from}");
        assert_eq!(
            crate::graph::test_support::sealed_chunk_len(&g.in_side(), to, type_id),
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
        crate::graph::test_support::sealed_chunk_len(&g.out_side(), a, type_id),
        crate::adjacency::COMPACTION_DELTA_FLOOR
    );

    g.begin_write().unwrap();
    g.delete_rel(ids[5]);
    g.commit_write();

    let region = g.out_side().sealed().region(a).expect("sealed region");
    assert_eq!(
        region.chunk_len(type_id),
        crate::adjacency::COMPACTION_DELTA_FLOOR,
        "the sealed hop stays physical until reclaimed"
    );
    let tombstones = region.tombstones(type_id);
    assert_eq!(tombstones.len(), 1);
    assert_eq!(tombstones[0].id, ids[5]);
    assert!(tombstones[0].xmax > 0);
}

/// A delete records its tombstone without a delta entry: `AdjSide::remove` drops
/// the delta entry one `IdVec` lock before the sealed region's, and a delete
/// landing in that window must still hide the sealed hop (ADR-0551).
#[test]
fn delete_without_a_delta_entry_still_tombstones_the_sealed_hop() {
    let g = Graph::default();
    let (a, _, ids, type_id) = crate::graph::test_support::folded_floor_chunk(&g);

    // The race window's state: delta entry absent, sealed region present.
    assert!(
        g.out_side().delta().remove(&a).is_some(),
        "the fold leaves a delta entry"
    );
    assert!(g.out_side().delta().get(&a).is_none(), "delta entry absent");
    assert!(
        g.out_side().sealed().region(a).is_some(),
        "sealed region present"
    );

    g.begin_write().unwrap();
    g.delete_rel(ids[5]);
    g.commit_write();

    let region = g.out_side().sealed().region(a).expect("sealed region");
    let tombstones = region.tombstones(type_id);
    assert_eq!(tombstones.len(), 1);
    let tombstone = tombstones[0];
    assert_eq!(tombstone.id, ids[5]);
    assert!(tombstone.deleted.is_some());
    let visible = |ts: u64| {
        region
            .visible_hops(type_id, ts)
            .iter()
            .any(|h| h.id == ids[5])
    };
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
        crate::graph::test_support::sealed_chunk_len(&g.out_side(), a, type_id),
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
        let sealed = g.out_side().sealed().region(a).unwrap();
        assert_eq!(sealed.chunk_len(type_id), (floor + delta) as usize);
        assert!(sealed.decoded(type_id).iter().any(|h| h.id == deleted));
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
        let adj = g.out_side().delta().get(&a).unwrap();
        let sealed = g.out_side().sealed().region(a).unwrap();
        assert_eq!(sealed.chunk_len(type_id), (floor + delta - 1) as usize);
        assert!(!sealed.decoded(type_id).iter().any(|h| h.id == deleted));
        let mut physical = Vec::new();
        adj.for_each_hop_of(Some(sealed), type_id, 0, |hop| physical.push(hop.id));
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
        .region(a)
        .is_none_or(|region| !region.has_chunk(type_id)));

    let deleted = ids[5];
    g.begin_write().unwrap();
    g.delete_rel(deleted);
    g.commit_write();
    g.gc_versions();

    let adj = g.out_side().delta().get(&a).unwrap();
    let sealed = g.out_side().sealed().region(a).unwrap();
    assert_eq!(sealed.chunk_len(type_id), floor as usize);
    assert!(
        !adj.flatten(Some(sealed)).contains(&deleted),
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
            g.out_side()
                .sealed()
                .region(a)
                .is_some_and(|region| region.has_chunk(type_id)),
            "round {round}: the write records the zero-hop region"
        );

        g.gc_versions();
        let sealed_tombstones = g
            .sealed_out
            .region(a)
            .map_or(0, |region| region.tombstones(type_id).len());
        let live_delta = g
            .out_adj
            .get(&a)
            .map_or(0, |adj| adj.chunk_hops(type_id).len());
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
        let sealed = g.out_side().sealed().region(a).unwrap();
        assert_eq!(
            sealed.chunk_len(type_id),
            floor as usize,
            "the write touches no sealed bytes"
        );
        sealed.tombstones(type_id)
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
    let tombstones = g.out_side().sealed().region(a).unwrap().tombstones(type_id);
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
        let adj = g.out_side().delta().get(&a).unwrap();
        let sealed = g.out_side().sealed().region(a).unwrap();
        assert_eq!(
            sealed.chunk_len(type_id),
            crate::adjacency::COMPACTION_DELTA_FLOOR - 1,
            "the sealed position is reclaimed at the pinned horizon"
        );
        assert_eq!(
            adj.chunk_hops(type_id).len(),
            1,
            "the relocated hop stays delta while its delete stamp is above the horizon"
        );
        let t = sealed
            .tombstones(type_id)
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
        crate::graph::test_support::sealed_chunk_len(&g.out_side(), a, type_id),
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

/// The sealed arena's floor charges the immutable byte buffer at its exact
/// length ([`vec_heap_exact`]) and the push-grown vectors at their
/// reservation ([`vec_heap`]), so every buffer it charges must hold exactly
/// that capacity. A sealed region after a fold, and the tombstone a delete
/// records.
#[test]
fn sealed_vectors_hold_exactly_the_capacity_their_floor_charges() {
    let g = Graph::default();
    let (a, _, ids, type_id) = crate::graph::test_support::folded_floor_chunk(&g);
    g.begin_write().unwrap();
    g.delete_rel(ids[5]);
    g.commit_write();

    let mut tombstoned = 0;
    for side in [&g.out_side().sealed(), &g.in_side().sealed()] {
        tombstoned += side.assert_fit();
    }
    assert_eq!(
        tombstoned, 2,
        "the delete tombstoned the sealed stream on both endpoints"
    );
    assert!(
        crate::graph::test_support::sealed_chunk_len(&g.out_side(), a, type_id) > 0,
        "the fold sealed the hops"
    );
}

/// F-1-58: the floor's recount charges the sealed arena's span tables beside
/// each arena's own floor — the span-table share of the heap census's sealed
/// `index_bytes`. Dropping the arenas drops exactly those terms and nothing
/// else.
#[test]
fn the_floor_charges_the_sealed_slot_arrays() {
    let g = Graph::default();
    crate::graph::test_support::folded_floor_chunk(&g);
    let arrays = g.out_side().sealed().array_bytes() + g.in_side().sealed().array_bytes();
    assert!(arrays > 0, "the fold created sealed entries");
    let floors = g.out_side().sealed().floor_bytes() + g.in_side().sealed().floor_bytes();
    let with_sealed = g.structure_bytes();
    g.out_side().sealed().clear();
    g.in_side().sealed().clear();
    assert_eq!(
        with_sealed - g.structure_bytes(),
        arrays + floors,
        "the span tables are charged beside the arenas' floors"
    );
}

/// The adaptive seal floor: a mid-degree chunk — steady degree below the
/// knee — survives two checkpoint folds (the first notes its length, the
/// second finds it held) and seals at the third, no write in between. One
/// quiet interval alone never seals — that is the churn the guard refuses.
/// The knee's floor would leave it delta forever; a growing chunk never
/// takes this path (the delta-tier test pins that half).
#[test]
fn the_stability_floor_seals_a_settled_mid_degree_chunk() {
    let g = Graph::default();
    let (a, b) = crate::graph::test_support::endpoints(&g);
    // The settled length is the floor's, not a literal: the band's lower
    // bound is the fact under test.
    let settled = crate::adjacency::STABILITY_SEAL_FLOOR + 12;
    g.begin_write().unwrap();
    for i in 0..settled {
        g.create_rel(
            a,
            b,
            "T".into(),
            BTreeMap::from([("ts".into(), Value::Int(i as i64))]),
        )
        .unwrap();
    }
    g.commit_write();
    let type_id = g.interner.get_id("T").unwrap();
    // First checkpoint: below the knee and unnoted — stays delta, takes
    // its baseline.
    g.gc_versions();
    assert!(
        g.out_side().sealed().region(a).is_none(),
        "one pass only notes the baseline"
    );
    // No writes: the second checkpoint finds the noted length held once.
    g.gc_versions();
    assert!(
        g.out_side().sealed().region(a).is_none(),
        "one held interval is not evidence of a steady state"
    );
    // The hold is confirmed, so the chunk seals whole.
    g.gc_versions();
    assert_eq!(
        crate::graph::test_support::sealed_chunk_len(&g.out_side(), a, type_id),
        settled,
        "its whole stream is sealed"
    );
    assert_eq!(
        crate::graph::test_support::sealed_chunk_len(&g.in_side(), b, type_id),
        settled,
        "the end side settles the same way"
    );
}
