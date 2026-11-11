use super::*;

/// The production fold's two steps: due-ness against the node's sealed
/// entry, then the fold into it. Returns whether anything was folded.
fn fold(a: &mut TypedAdj, sealed: &mut SealedAdj, horizon: u64) -> bool {
    let due = a.due_chunks(Some(sealed), horizon);
    a.fold_due(sealed, &due, horizon)
}

#[test]
fn typed_adj_chunks() {
    let mut a = TypedAdj::default();
    a.insert(1, 10, None);
    a.insert(2, 20, None);
    a.insert(1, 11, None);
    // Newest-first within a chunk (ADR-0203): the Neo4j reference
    // expands a node's relationship chain head-first, so the
    // highest-id relationship of a type comes first.
    assert_eq!(
        a.chunk(1).iter().map(|h| h.id).collect::<Vec<_>>(),
        vec![11, 10]
    );
    assert_eq!(a.flatten(None), vec![11, 10, 20]);
}

/// A wholesale `from_runs` build is the same adjacency as an id-ordered
/// insert load — the invariant the two construction paths must share.
/// The hops are chosen so first-rel-id order and type-id order differ
/// (type 2 holds the smaller first id), which is what makes this test
/// distinguish the two orderings.
#[test]
fn from_runs_matches_an_id_ordered_insert_load() {
    // (hop, type) pairs, in a deliberately hostile order.
    let hops: [(AdjHop, u32); 5] = [
        (AdjHop { ts: Some(3), id: 7 }, 1),
        (AdjHop { ts: None, id: 2 }, 2),
        (AdjHop { ts: None, id: 9 }, 1),
        (AdjHop { ts: Some(3), id: 4 }, 2),
        (AdjHop { ts: Some(1), id: 5 }, 2),
    ];
    let mut ordered = hops;
    ordered.sort_unstable_by_key(|(hop, _)| hop.id);
    let mut inserted = TypedAdj::default();
    for (hop, type_id) in &ordered {
        inserted.insert(*type_id, hop.id, hop.ts);
    }
    // One run per type: its type id and its hops, unsorted and keyless —
    // `from_runs` derives the chunk-order key from the hops itself.
    let runs: Vec<(u32, Vec<AdjHop>)> = [1u32, 2]
        .into_iter()
        .map(|type_id| {
            (
                type_id,
                hops.iter()
                    .filter(|(_, t)| *t == type_id)
                    .map(|(hop, _)| *hop)
                    .collect(),
            )
        })
        .collect();
    let built = TypedAdj::from_runs(runs);
    // Chunks in first-rel-id order (type 2's id 2 before type 1's id 7),
    // not type-id order.
    assert_eq!(
        built.chunks.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
        vec![2, 1]
    );
    assert_eq!(built.flatten(None), inserted.flatten(None));
    for type_id in [1u32, 2] {
        assert_eq!(
            built
                .chunk(type_id)
                .iter()
                .map(|h| (h.id, h.ts))
                .collect::<Vec<_>>(),
            inserted
                .chunk(type_id)
                .iter()
                .map(|h| (h.id, h.ts))
                .collect::<Vec<_>>()
        );
    }
}

/// `TypedAdj` is the delta alone: the sealed tier lives in the Graph's
/// side table, so the slot is back to the pre-feature 24-byte vector and
/// a node without a fold carries no sealed pointer.
#[test]
fn typed_adj_slot_is_the_24_byte_delta() {
    use std::mem::size_of;
    assert_eq!(size_of::<TypedAdj>(), 24);
    assert_eq!(size_of::<Option<TypedAdj>>(), 24);
}

#[test]
fn typed_adj_orders_ts_desc_nulls_first() {
    let mut a = TypedAdj::default();
    let mut sealed = SealedAdj::default();
    a.insert(1, 1, Some(10));
    a.insert(1, 2, Some(30));
    a.insert(1, 3, None);
    a.insert(1, 4, Some(20));
    let ids: Vec<_> = a.chunk(1).iter().map(|h| (h.ts, h.id)).collect();
    assert_eq!(
        ids,
        vec![(None, 3), (Some(30), 2), (Some(20), 4), (Some(10), 1)]
    );
    a.reposition(&mut sealed, 1, 1, Some(40), 0);
    let ids: Vec<_> = a.chunk(1).iter().map(|h| (h.ts, h.id)).collect();
    assert_eq!(
        ids,
        vec![(None, 3), (Some(40), 1), (Some(30), 2), (Some(20), 4)]
    );
}

/// The one per-chunk length pass reports each tier separately: a folded
/// chunk shows an empty delta and its sealed length, and a hop inserted
/// after the seal shows up in the delta.
#[test]
fn chunk_lens_yields_delta_and_sealed_per_chunk() {
    let mut a = TypedAdj::default();
    let mut sealed = SealedAdj::default();
    a.insert(1, 10, None);
    a.insert(2, 20, None);
    for i in 0..COMPACTION_DELTA_FLOOR as i64 {
        a.insert(3, 100 + i, Some(i));
    }
    assert!(fold(&mut a, &mut sealed, 0));

    let lens: Vec<_> = a.chunk_lens(Some(&sealed)).collect();
    assert_eq!(
        lens,
        vec![(1, 1, 0), (2, 1, 0), (3, 0, COMPACTION_DELTA_FLOOR)]
    );

    a.insert(3, 500, Some(500));
    assert_eq!(
        a.chunk_lens(Some(&sealed)).nth(2),
        Some((3, 1, COMPACTION_DELTA_FLOOR))
    );
}

/// The floor seals a never-sealed chunk, and a read merges the delta
/// (an older-ts hop inserted after the seal) into its sorted slot ahead
/// of the sealed stream (ADR-0551).
#[test]
fn sealed_and_delta_hops_merge_ts_desc_delta_first() {
    let mut a = TypedAdj::default();
    let mut sealed = SealedAdj::default();
    let type_id = 7;
    for i in 0..COMPACTION_DELTA_FLOOR as i64 {
        a.insert(type_id, i, Some(i));
    }
    assert!(a.chunk_due(Some(&sealed), type_id, u64::MAX));
    assert!(fold(&mut a, &mut sealed, u64::MAX));
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR);
    assert!(a.chunk(type_id).is_empty(), "the delta folded");

    // A hop older than every sealed hop must merge into its sorted slot,
    // not trail the sealed stream.
    a.insert(type_id, 100, Some(5));
    let mut seen = Vec::new();
    a.for_each_hop_of(Some(&sealed), type_id, u64::MAX, |hop| {
        seen.push((hop.ts, hop.id))
    });
    let mut expected: Vec<(Option<i64>, RelId)> = (0..COMPACTION_DELTA_FLOOR as i64)
        .map(|i| (Some(i), i))
        .collect();
    expected.push((Some(5), 100));
    expected.sort_unstable_by(|x, y| {
        cmp_ts_desc(&AdjHop { ts: x.0, id: x.1 }, &AdjHop { ts: y.0, id: y.1 })
    });
    assert_eq!(seen, expected);
}

/// A delete of a sealed hop records the versioned tombstone: the sealed
/// stream keeps the physical hop, and a read before `xmax` still sees it
/// (ADR-0551).
#[test]
fn delete_of_a_sealed_hop_records_a_versioned_tombstone() {
    let mut a = TypedAdj::default();
    let mut sealed = SealedAdj::default();
    let type_id = 3;
    for i in 0..COMPACTION_DELTA_FLOOR as i64 {
        a.insert(type_id, i, Some(i));
    }
    fold(&mut a, &mut sealed, 0);
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR);

    a.remove(Some(&mut sealed), type_id, 5);
    sealed.tombstone_deleted(type_id, 5, 40, Supersede::Deleted);
    let chunk = sealed.chunk(type_id).expect("sealed");
    assert_eq!(
        chunk.tombstones,
        [Tombstone {
            id: 5,
            xmax: 40,
            deleted: Some(40),
        }]
    );

    let mut before = Vec::new();
    a.for_each_hop_of(Some(&sealed), type_id, 39, |hop| before.push(hop.id));
    assert!(before.contains(&5), "a snapshot before xmax sees the hop");
    let mut after = Vec::new();
    a.for_each_hop_of(Some(&sealed), type_id, 40, |hop| after.push(hop.id));
    assert!(!after.contains(&5), "the tombstone hides it at xmax");
    assert_eq!(
        sealed.chunk_len(type_id),
        COMPACTION_DELTA_FLOOR,
        "still physical"
    );
}

/// The three fold conditions (ADR-0551), each on its own chunk: a
/// never-sealed chunk at the floor, a delta past 25% of the sealed hops,
/// and a tombstone reclaimable at the GC horizon. A chunk below its gate
/// stays delta.
#[test]
fn compaction_folds_on_floor_ratio_and_reclaimable_tombstone() {
    let type_id = 9;
    // 1. Floor: a never-sealed chunk seals at `COMPACTION_DELTA_FLOOR`.
    let mut a = TypedAdj::default();
    let mut sealed = SealedAdj::default();
    for i in 0..(COMPACTION_DELTA_FLOOR - 1) as i64 {
        a.insert(type_id, i, Some(i));
    }
    assert!(
        !a.chunk_due(Some(&sealed), type_id, 0),
        "one hop below the floor"
    );
    a.insert(type_id, 1000, Some(1000));
    assert!(a.chunk_due(Some(&sealed), type_id, 0), "the floor");
    assert!(fold(&mut a, &mut sealed, 0));
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR);

    // 2. Ratio: a delta past 25% of the sealed hops folds.
    for i in 0..(COMPACTION_DELTA_FLOOR / 4) as i64 {
        a.insert(type_id, 2000 + i, Some(2000 + i));
    }
    assert!(
        !a.chunk_due(Some(&sealed), type_id, 0),
        "exactly 25% of the sealed hops is not past it"
    );
    a.insert(type_id, 3000, Some(3000));
    assert!(
        a.chunk_due(Some(&sealed), type_id, 0),
        "past 25% of the sealed hops"
    );
    assert!(fold(&mut a, &mut sealed, 0));
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 9);

    // 3. Tombstone: a sealed hop reclaimable at the horizon folds away
    // even with an empty delta.
    sealed.tombstone_deleted(type_id, 5, 40, Supersede::Deleted);
    assert!(
        !a.chunk_due(Some(&sealed), type_id, 39),
        "the tombstone is not yet at the horizon"
    );
    assert!(
        a.chunk_due(Some(&sealed), type_id, 40),
        "the tombstone is reclaimable"
    );
    assert!(fold(&mut a, &mut sealed, 40));
    assert!(sealed.chunk(type_id).unwrap().tombstones.is_empty());
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 8);
    assert!(
        !a.chunk_due(Some(&sealed), type_id, 41),
        "a clean sealed chunk is not due"
    );
}

/// The fold is idempotent and reclaims a tombstoned hop at the horizon.
#[test]
fn compaction_reclaims_at_the_horizon_and_is_idempotent() {
    let mut a = TypedAdj::default();
    let mut sealed = SealedAdj::default();
    let type_id = 2;
    for i in 0..COMPACTION_DELTA_FLOOR as i64 {
        a.insert(type_id, i, Some(i));
    }
    assert!(fold(&mut a, &mut sealed, 0));
    sealed.tombstone_deleted(type_id, 5, 40, Supersede::Deleted);
    // Below the horizon nothing is due again.
    assert!(!a.chunk_due(Some(&sealed), type_id, 39));
    assert!(!fold(&mut a, &mut sealed, 39));
    // At the horizon the tombstone is reclaimable and the fold drops it.
    assert!(fold(&mut a, &mut sealed, 40));
    let chunk = sealed.chunk(type_id).expect("still sealed");
    assert!(chunk.tombstones.is_empty());
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR - 1);
    assert!(
        !a.chunk_due(Some(&sealed), type_id, 41),
        "a clean sealed chunk is not due"
    );
}

/// The fold drops a deleted row's delta hop once its tombstone reaches
/// the horizon, instead of sealing a hop the chain GC can no longer
/// prune. Below the horizon it seals under its tombstone, so an older
/// snapshot still sees it.
#[test]
fn the_fold_drops_a_deleted_rows_delta_hop_at_the_horizon() {
    let mut a = TypedAdj::default();
    let mut sealed = SealedAdj::default();
    let type_id = 4;
    for i in 0..COMPACTION_DELTA_FLOOR as i64 {
        a.insert(type_id, i, Some(i));
    }
    assert!(fold(&mut a, &mut sealed, 0));
    a.insert(type_id, 100, Some(100));
    sealed.tombstone_deleted(type_id, 100, 40, Supersede::Deleted);
    for i in 0..9 {
        a.insert(type_id, 200 + i, Some(200 + i));
    }
    assert!(
        fold(&mut a, &mut sealed, 0),
        "the delta is past 25% of the sealed hops"
    );
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 10);
    assert!(sealed
        .chunk(type_id)
        .unwrap()
        .decoded()
        .iter()
        .any(|h| h.id == 100));

    assert!(
        fold(&mut a, &mut sealed, 40),
        "the tombstone is at the horizon"
    );
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 9);
    assert!(!sealed
        .chunk(type_id)
        .unwrap()
        .decoded()
        .iter()
        .any(|h| h.id == 100));
    assert!(sealed.chunk(type_id).unwrap().tombstones.is_empty());
}

/// A delete on a never-sealed chunk records its tombstone against a
/// zero-hop sealed stream created on demand, so the fold drops the dead
/// delta hop instead of sealing it as a ghost (ADR-0551). The floor still
/// gates that chunk's first seal, and the fold drops or keeps the delta
/// hop per the tombstone's two version stamps.
#[test]
fn a_never_sealed_chunk_records_its_tombstone_and_folds_the_dead_hop_away() {
    // The tombstone does not seal a small chunk early: the floor gates a
    // never-sealed chunk's first seal.
    let mut small = TypedAdj::default();
    let mut small_sealed = SealedAdj::default();
    for i in 0..3 {
        small.insert(11, i, Some(i));
    }
    small_sealed.tombstone_deleted(11, 1, 40, Supersede::Deleted);
    assert!(
        !small.chunk_due(Some(&small_sealed), 11, 39),
        "the floor gates the first seal while the tombstone is live"
    );
    // At the horizon the fold is the reclaim alone: the tombstone still
    // names the delta hop, so nothing is reclaimed and nothing seals.
    assert!(!fold(&mut small, &mut small_sealed, u64::MAX));
    assert_eq!(small_sealed.chunk_len(11), 0, "no early seal");

    let mut a = TypedAdj::default();
    let mut sealed = SealedAdj::default();
    let type_id = 11;
    for i in 0..(COMPACTION_DELTA_FLOOR + 1) as i64 {
        a.insert(type_id, i, Some(i));
    }
    assert!(sealed.chunk(type_id).is_none(), "the chunk is still delta");

    sealed.tombstone_deleted(type_id, 5, 40, Supersede::Deleted);
    let chunk = sealed
        .chunk(type_id)
        .expect("the tombstone creates the sealed chunk");
    assert_eq!(
        chunk.bytes,
        crate::adjacency::encode_chunk(&[]),
        "the 4-byte empty stream"
    );
    assert_eq!(
        chunk.tombstones,
        [Tombstone {
            id: 5,
            xmax: 40,
            deleted: Some(40),
        }]
    );
    assert!(
        a.chunk_due(Some(&sealed), type_id, 39),
        "the delta reaches the floor"
    );

    // Below the horizon the fold seals the dead hop under its tombstone,
    // so an older snapshot still sees it.
    assert!(fold(&mut a, &mut sealed, 39));
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 1);
    let mut before = Vec::new();
    a.for_each_hop_of(Some(&sealed), type_id, 39, |hop| before.push(hop.id));
    assert!(before.contains(&5));

    assert!(
        a.chunk_due(Some(&sealed), type_id, 40),
        "the tombstone is reclaimable"
    );
    assert!(fold(&mut a, &mut sealed, 40));
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR);
    assert!(sealed.chunk(type_id).unwrap().tombstones.is_empty());
    assert!(!a.flatten(Some(&sealed)).contains(&5), "no ghost hop");
}

/// A never-sealed chunk's reposition, then delete, keeps the relocated
/// delta hop at a horizon between the two stamps: the sealed position
/// (none here) reclaims while the live hop waits for its delete stamp.
#[test]
fn a_never_sealed_reposition_then_delete_keeps_the_relocated_hop() {
    let mut a = TypedAdj::default();
    let mut sealed = SealedAdj::default();
    let type_id = 13;
    for i in 0..(COMPACTION_DELTA_FLOOR + 1) as i64 {
        a.insert(type_id, i, Some(i));
    }
    a.reposition(&mut sealed, type_id, 5, Some(40), 40);
    sealed.tombstone_deleted(type_id, 5, 60, Supersede::Deleted);

    // A horizon between the two stamps changes nothing: the relocated hop
    // is the id's live position, so the fold guard keeps it in the delta.
    fold(&mut a, &mut sealed, 40);
    assert_eq!(a.chunk(type_id).len(), COMPACTION_DELTA_FLOOR + 1);
    assert_eq!(sealed.chunk_len(type_id), 0);
    assert_eq!(
        sealed.chunk(type_id).unwrap().tombstones,
        [Tombstone {
            id: 5,
            xmax: 40,
            deleted: Some(60),
        }]
    );

    assert!(
        fold(&mut a, &mut sealed, 60),
        "the delete stamp reaches the horizon"
    );
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR);
    assert!(!a.flatten(Some(&sealed)).contains(&5));
}

/// The fold guard blocks only a reposition whose id still holds a sealed
/// position. A reposition (or delete) of a delta-only hop records a
/// tombstone the fold resolves, so it never wedges the chunk; a sealed
/// hop's tombstone would hide the id's live delta hop and holds the fold
/// until the horizon passes.
#[test]
fn the_fold_guard_only_blocks_a_reposition_of_a_sealed_hop() {
    let mut a = TypedAdj::default();
    let mut sealed = SealedAdj::default();
    let type_id = 8;
    for i in 0..COMPACTION_DELTA_FLOOR as i64 {
        a.insert(type_id, i, Some(i));
    }
    assert!(fold(&mut a, &mut sealed, 0));

    // A delta-only hop repositioned inside the sealed chunk: the
    // tombstone names no sealed position and is dropped, and the hop
    // folds in as the id's live position.
    a.insert(type_id, 100, Some(1));
    a.reposition(&mut sealed, type_id, 100, Some(2), 40);
    for i in 0..9 {
        a.insert(type_id, 200 + i, Some(200 + i));
    }
    assert!(fold(&mut a, &mut sealed, 0));
    assert!(sealed.chunk(type_id).unwrap().tombstones.is_empty());
    let mut seen = Vec::new();
    a.for_each_hop_of(Some(&sealed), type_id, u64::MAX, |hop| {
        seen.push((hop.ts, hop.id))
    });
    assert!(seen.contains(&(Some(2), 100)));

    // A sealed hop's reposition holds the fold while its tombstone is
    // below the horizon: a bare-id tombstone cannot name one position.
    a.reposition(&mut sealed, type_id, 3, Some(33), 40);
    for i in 0..10 {
        a.insert(type_id, 300 + i, Some(300 + i));
    }
    assert!(
        !fold(&mut a, &mut sealed, 0),
        "the reposition holds the fold"
    );
    assert!(
        fold(&mut a, &mut sealed, 40),
        "past the horizon the fold runs"
    );
    let mut seen = Vec::new();
    a.for_each_hop_of(Some(&sealed), type_id, u64::MAX, |hop| {
        seen.push((hop.ts, hop.id))
    });
    assert!(seen.contains(&(Some(33), 3)), "the live position folds in");
    assert!(!seen.contains(&(Some(3), 3)), "the sealed position is gone");
    assert!(sealed.chunk(type_id).unwrap().tombstones.is_empty());
}

/// The fold guard blocks a delete that followed a reposition while its
/// delete stamp is above the horizon: folding the relocated hop under the
/// tombstone would hide it from a reader between the two stamps. The
/// sealed position still reclaims at the reposition's stamp.
#[test]
fn the_fold_guard_blocks_a_delete_above_the_horizon_after_a_reposition() {
    let mut a = TypedAdj::default();
    let mut sealed = SealedAdj::default();
    let type_id = 12;
    for i in 0..COMPACTION_DELTA_FLOOR as i64 {
        a.insert(type_id, i, Some(i));
    }
    assert!(fold(&mut a, &mut sealed, 0));
    // The sealed position is superseded at 40; the relocated hop is
    // deleted at 60.
    a.reposition(&mut sealed, type_id, 3, Some(40), 40);
    sealed.tombstone_deleted(type_id, 3, 60, Supersede::Deleted);
    for i in 0..10 {
        a.insert(type_id, 300 + i, Some(300 + i));
    }
    assert!(
        !fold(&mut a, &mut sealed, 0),
        "nothing reclaims below both stamps"
    );
    assert!(
        fold(&mut a, &mut sealed, 40),
        "the sealed position reclaims, the relocated hop stays"
    );
    assert!(
        !fold(&mut a, &mut sealed, 40),
        "the second pass has nothing left to do"
    );
    {
        let chunk = sealed.chunk(type_id).unwrap();
        assert_eq!(chunk.len(), COMPACTION_DELTA_FLOOR - 1);
        assert_eq!(a.chunk(type_id).len(), 11, "the relocated hop stays delta");
        assert_eq!(
            chunk.tombstones,
            [Tombstone {
                id: 3,
                xmax: 40,
                deleted: Some(60),
            }]
        );
    }
    assert!(
        fold(&mut a, &mut sealed, 60),
        "the delete stamp reaches the horizon"
    );
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 9);
    assert!(sealed.chunk(type_id).unwrap().tombstones.is_empty());
    let mut seen = Vec::new();
    a.for_each_hop_of(Some(&sealed), type_id, u64::MAX, |hop| seen.push(hop.id));
    assert!(!seen.contains(&3));
}

/// ADR-0551 accounting, pinned on a hand-built sealed chunk (no
/// compaction): the floor charges the sealed stream at its compressed
/// byte length plus its tombstone heap, and the delta at live `AdjHop`
/// bytes — never the sealed hops at decoded hop size.
#[test]
fn floor_prices_sealed_at_stream_length_and_delta_at_live_hop_bytes() {
    use std::mem::size_of;

    let sealed_hops: Vec<AdjHop> = (0..COMPACTION_DELTA_FLOOR as i64)
        .map(|i| AdjHop {
            ts: Some(10_000 - i),
            id: i,
        })
        .collect();
    let bytes = crate::adjacency::encode_chunk(&sealed_hops);
    assert!(
        (bytes.len() as u64) < sealed_hops.len() as u64 * size_of::<AdjHop>() as u64,
        "the codec compresses: {} bytes for {} hops",
        bytes.len(),
        sealed_hops.len()
    );
    let tombstones = vec![Tombstone {
        id: 0,
        xmax: 7,
        deleted: Some(7),
    }];
    let sealed_floor = crate::memory::vec_heap(1, size_of::<(u32, SealedChunk)>() as u64)
        + crate::memory::vec_heap(bytes.len(), 1)
        + crate::memory::vec_heap(tombstones.len(), size_of::<Tombstone>() as u64);

    let delta = vec![AdjHop {
        ts: Some(20_000),
        id: 999,
    }];
    let adj = TypedAdj {
        chunks: vec![(9, delta.clone())],
    };
    let sealed = SealedAdj {
        chunks: vec![(
            9,
            SealedChunk {
                bytes: bytes.clone(),
                tombstones,
            },
        )],
    };

    assert_eq!(
        sealed.chunk(9).unwrap().floor_bytes(),
        crate::memory::vec_heap(bytes.len(), 1)
            + crate::memory::vec_heap(1, size_of::<Tombstone>() as u64)
    );
    assert_eq!(
        adj.floor_bytes() + sealed.floor_bytes(),
        crate::memory::adjacency_bytes(&[delta.len()]) + sealed_floor
    );
}
