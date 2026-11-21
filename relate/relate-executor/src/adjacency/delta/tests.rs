use super::*;
use crate::adjacency::SealedArena;

/// The due predicate for one type id, as the tests read it: the selection
/// the production pass makes ([`TypedAdj::due_chunks`]), narrowed to a type.
fn due(a: &TypedAdj, sealed: Option<SealedRegion<'_>>, type_id: u32, horizon: u64) -> bool {
    a.due_chunks(sealed, horizon)
        .iter()
        .any(|candidate| candidate.type_id == type_id)
}

/// The production fold's two steps: due-ness against the node's sealed
/// region, then the fold into it. Returns whether anything was folded.
fn fold(a: &mut TypedAdj, sealed: SealedRegion<'_>, horizon: u64) -> bool {
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
        a.chunk_hops(1).iter().map(|h| h.id).collect::<Vec<_>>(),
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
    let hops: [(Hop, u32); 5] = [
        (Hop { ts: Some(3), id: 7 }, 1),
        (Hop { ts: None, id: 2 }, 2),
        (Hop { ts: None, id: 9 }, 1),
        (Hop { ts: Some(3), id: 4 }, 2),
        (Hop { ts: Some(1), id: 5 }, 2),
    ];
    let mut ordered = hops;
    ordered.sort_unstable_by_key(|(hop, _)| hop.id);
    let mut inserted = TypedAdj::default();
    for (hop, type_id) in &ordered {
        inserted.insert(*type_id, hop.id, hop.ts);
    }
    // One run per type: its type id and its hops, unsorted and keyless —
    // `from_runs` derives the chunk-order key from the hops itself.
    let runs: Vec<(u32, Vec<Hop>)> = [1u32, 2]
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
        built.chunks.iter().map(|c| c.type_id).collect::<Vec<_>>(),
        vec![2, 1]
    );
    assert_eq!(built.flatten(None), inserted.flatten(None));
    for type_id in [1u32, 2] {
        assert_eq!(built.chunk_hops(type_id), inserted.chunk_hops(type_id));
    }
}

/// `TypedAdj` is the delta alone: the sealed tier lives in the Graph's
/// graph-level arena, so the slot is back to the pre-feature 24-byte vector
/// and a node without a fold carries no sealed pointer.
#[test]
fn typed_adj_slot_is_the_24_byte_delta() {
    use std::mem::size_of;
    assert_eq!(size_of::<TypedAdj>(), 24);
    assert_eq!(size_of::<Option<TypedAdj>>(), 24);
}

#[test]
fn typed_adj_orders_ts_desc_nulls_first() {
    let mut a = TypedAdj::default();
    let arena = SealedArena::default();
    let sealed = arena.region_or_default(0);
    a.insert(1, 1, Some(10));
    a.insert(1, 2, Some(30));
    a.insert(1, 3, None);
    a.insert(1, 4, Some(20));
    assert_eq!(
        a.chunk_hops(1),
        vec![
            Hop { ts: None, id: 3 },
            Hop {
                ts: Some(30),
                id: 2
            },
            Hop {
                ts: Some(20),
                id: 4
            },
            Hop {
                ts: Some(10),
                id: 1
            }
        ]
    );
    a.reposition(sealed, 1, 1, Some(40), 0);
    assert_eq!(
        a.chunk_hops(1),
        vec![
            Hop { ts: None, id: 3 },
            Hop {
                ts: Some(40),
                id: 1
            },
            Hop {
                ts: Some(30),
                id: 2
            },
            Hop {
                ts: Some(20),
                id: 4
            }
        ]
    );
}

/// The one per-chunk length pass reports each tier separately: a folded
/// chunk shows an empty delta and its sealed length, and a hop inserted
/// after the seal shows up in the delta.
#[test]
fn chunk_lens_yields_delta_and_sealed_per_chunk() {
    let mut a = TypedAdj::default();
    let arena = SealedArena::default();
    let sealed = arena.region_or_default(0);
    a.insert(1, 10, None);
    a.insert(2, 20, None);
    for i in 0..COMPACTION_DELTA_FLOOR as i64 {
        a.insert(3, 100 + i, Some(i));
    }
    assert!(fold(&mut a, sealed, 0));

    let lens: Vec<_> = a.chunk_lens(Some(sealed)).collect();
    assert_eq!(
        lens,
        vec![(1, 1, 0), (2, 1, 0), (3, 0, COMPACTION_DELTA_FLOOR)]
    );

    a.insert(3, 500, Some(500));
    assert_eq!(
        a.chunk_lens(Some(sealed)).nth(2),
        Some((3, 1, COMPACTION_DELTA_FLOOR))
    );
}

/// The floor seals a never-sealed chunk, and a read merges the delta
/// (an older-ts hop inserted after the seal) into its sorted slot ahead
/// of the sealed stream (ADR-0551).
#[test]
fn sealed_and_delta_hops_merge_ts_desc_delta_first() {
    let mut a = TypedAdj::default();
    let arena = SealedArena::default();
    let sealed = arena.region_or_default(0);
    let type_id = 7;
    for i in 0..COMPACTION_DELTA_FLOOR as i64 {
        a.insert(type_id, i, Some(i));
    }
    assert!(due(&a, Some(sealed), type_id, u64::MAX));
    assert!(fold(&mut a, sealed, u64::MAX));
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR);
    assert!(a.chunk_hops(type_id).is_empty(), "the delta folded");

    // A hop older than every sealed hop must merge into its sorted slot,
    // not trail the sealed stream.
    a.insert(type_id, 100, Some(5));
    let mut seen = Vec::new();
    a.for_each_hop_of(Some(sealed), type_id, u64::MAX, |hop| {
        seen.push((hop.ts, hop.id))
    });
    let mut expected: Vec<(Option<i64>, RelId)> = (0..COMPACTION_DELTA_FLOOR as i64)
        .map(|i| (Some(i), i))
        .collect();
    expected.push((Some(5), 100));
    expected
        .sort_unstable_by(|x, y| cmp_ts_desc(&Hop { ts: x.0, id: x.1 }, &Hop { ts: y.0, id: y.1 }));
    assert_eq!(seen, expected);
}

/// A delete of a sealed hop records the versioned tombstone: the sealed
/// stream keeps the physical hop, and a read before `xmax` still sees it
/// (ADR-0551).
#[test]
fn delete_of_a_sealed_hop_records_a_versioned_tombstone() {
    let mut a = TypedAdj::default();
    let arena = SealedArena::default();
    let sealed = arena.region_or_default(0);
    let type_id = 3;
    for i in 0..COMPACTION_DELTA_FLOOR as i64 {
        a.insert(type_id, i, Some(i));
    }
    fold(&mut a, sealed, 0);
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR);

    a.remove(Some(sealed), type_id, 5);
    sealed.tombstone_deleted(type_id, 5, 40, Supersede::Deleted);
    assert_eq!(
        sealed.tombstones(type_id),
        [Tombstone {
            id: 5,
            xmax: 40,
            deleted: Some(40),
        }]
    );

    let mut before = Vec::new();
    a.for_each_hop_of(Some(sealed), type_id, 39, |hop| before.push(hop.id));
    assert!(before.contains(&5), "a snapshot before xmax sees the hop");
    let mut after = Vec::new();
    a.for_each_hop_of(Some(sealed), type_id, 40, |hop| after.push(hop.id));
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
    let arena = SealedArena::default();
    let sealed = arena.region_or_default(0);
    for i in 0..(COMPACTION_DELTA_FLOOR - 1) as i64 {
        a.insert(type_id, i, Some(i));
    }
    assert!(
        !due(&a, Some(sealed), type_id, 0),
        "one hop below the floor"
    );
    a.insert(type_id, 1000, Some(1000));
    assert!(due(&a, Some(sealed), type_id, 0), "the floor");
    assert!(fold(&mut a, sealed, 0));
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR);

    // 2. Ratio: a delta past 25% of the sealed hops folds.
    for i in 0..(COMPACTION_DELTA_FLOOR / 4) as i64 {
        a.insert(type_id, 2000 + i, Some(2000 + i));
    }
    assert!(
        !due(&a, Some(sealed), type_id, 0),
        "exactly 25% of the sealed hops is not past it"
    );
    a.insert(type_id, 3000, Some(3000));
    assert!(
        due(&a, Some(sealed), type_id, 0),
        "past 25% of the sealed hops"
    );
    assert!(fold(&mut a, sealed, 0));
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 9);

    // 3. Tombstone: a sealed hop reclaimable at the horizon folds away
    // even with an empty delta.
    sealed.tombstone_deleted(type_id, 5, 40, Supersede::Deleted);
    assert!(
        !due(&a, Some(sealed), type_id, 39),
        "the tombstone is not yet at the horizon"
    );
    assert!(
        due(&a, Some(sealed), type_id, 40),
        "the tombstone is reclaimable"
    );
    assert!(fold(&mut a, sealed, 40));
    assert!(sealed.tombstones(type_id).is_empty());
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 8);
    assert!(
        !due(&a, Some(sealed), type_id, 41),
        "a clean sealed chunk is not due"
    );
}

/// The fold is idempotent and reclaims a tombstoned hop at the horizon.
#[test]
fn compaction_reclaims_at_the_horizon_and_is_idempotent() {
    let mut a = TypedAdj::default();
    let arena = SealedArena::default();
    let sealed = arena.region_or_default(0);
    let type_id = 2;
    for i in 0..COMPACTION_DELTA_FLOOR as i64 {
        a.insert(type_id, i, Some(i));
    }
    assert!(fold(&mut a, sealed, 0));
    sealed.tombstone_deleted(type_id, 5, 40, Supersede::Deleted);
    // Below the horizon nothing is due again.
    assert!(!due(&a, Some(sealed), type_id, 39));
    assert!(!fold(&mut a, sealed, 39));
    // At the horizon the tombstone is reclaimable and the fold drops it.
    assert!(fold(&mut a, sealed, 40));
    assert!(sealed.tombstones(type_id).is_empty());
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR - 1);
    assert!(
        !due(&a, Some(sealed), type_id, 41),
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
    let arena = SealedArena::default();
    let sealed = arena.region_or_default(0);
    let type_id = 4;
    for i in 0..COMPACTION_DELTA_FLOOR as i64 {
        a.insert(type_id, i, Some(i));
    }
    assert!(fold(&mut a, sealed, 0));
    a.insert(type_id, 100, Some(100));
    sealed.tombstone_deleted(type_id, 100, 40, Supersede::Deleted);
    for i in 0..9 {
        a.insert(type_id, 200 + i, Some(200 + i));
    }
    assert!(
        fold(&mut a, sealed, 0),
        "the delta is past 25% of the sealed hops"
    );
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 10);
    assert!(sealed.decoded(type_id).iter().any(|h| h.id == 100));

    assert!(fold(&mut a, sealed, 40), "the tombstone is at the horizon");
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 9);
    assert!(!sealed.decoded(type_id).iter().any(|h| h.id == 100));
    assert!(sealed.tombstones(type_id).is_empty());
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
    let small_arena = SealedArena::default();
    let small_sealed = small_arena.region_or_default(0);
    for i in 0..3 {
        small.insert(11, i, Some(i));
    }
    small_sealed.tombstone_deleted(11, 1, 40, Supersede::Deleted);
    assert!(
        !due(&small, Some(small_sealed), 11, 39),
        "the floor gates the first seal while the tombstone is live"
    );
    // At the horizon the fold is the reclaim alone: the tombstone still
    // names the delta hop, so nothing is reclaimed and nothing seals.
    assert!(!fold(&mut small, small_sealed, u64::MAX));
    assert_eq!(small_sealed.chunk_len(11), 0, "no early seal");

    let mut a = TypedAdj::default();
    let arena = SealedArena::default();
    let sealed = arena.region_or_default(0);
    let type_id = 11;
    for i in 0..(COMPACTION_DELTA_FLOOR + 1) as i64 {
        a.insert(type_id, i, Some(i));
    }
    assert!(!sealed.has_chunk(type_id), "the chunk is still delta");

    sealed.tombstone_deleted(type_id, 5, 40, Supersede::Deleted);
    assert!(
        sealed.has_chunk(type_id),
        "the tombstone creates the zero-hop sealed chunk"
    );
    assert_eq!(sealed.chunk_len(type_id), 0, "the zero-hop entry");
    assert_eq!(
        sealed.tombstones(type_id),
        [Tombstone {
            id: 5,
            xmax: 40,
            deleted: Some(40),
        }]
    );
    assert!(
        due(&a, Some(sealed), type_id, 39),
        "the delta reaches the floor"
    );

    // Below the horizon the fold seals the dead hop under its tombstone,
    // so an older snapshot still sees it.
    assert!(fold(&mut a, sealed, 39));
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 1);
    let mut before = Vec::new();
    a.for_each_hop_of(Some(sealed), type_id, 39, |hop| before.push(hop.id));
    assert!(before.contains(&5));

    assert!(
        due(&a, Some(sealed), type_id, 40),
        "the tombstone is reclaimable"
    );
    assert!(fold(&mut a, sealed, 40));
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR);
    assert!(sealed.tombstones(type_id).is_empty());
    assert!(!a.flatten(Some(sealed)).contains(&5), "no ghost hop");
}

/// A never-sealed chunk's reposition, then delete, keeps the relocated
/// delta hop at a horizon between the two stamps: the sealed position
/// (none here) reclaims while the live hop waits for its delete stamp.
#[test]
fn a_never_sealed_reposition_then_delete_keeps_the_relocated_hop() {
    let mut a = TypedAdj::default();
    let arena = SealedArena::default();
    let sealed = arena.region_or_default(0);
    let type_id = 13;
    for i in 0..(COMPACTION_DELTA_FLOOR + 1) as i64 {
        a.insert(type_id, i, Some(i));
    }
    a.reposition(sealed, type_id, 5, Some(40), 40);
    sealed.tombstone_deleted(type_id, 5, 60, Supersede::Deleted);

    // A horizon between the two stamps changes nothing: the relocated hop
    // is the id's live position, so the fold guard keeps it in the delta.
    fold(&mut a, sealed, 40);
    assert_eq!(a.chunk_hops(type_id).len(), COMPACTION_DELTA_FLOOR + 1);
    assert_eq!(sealed.chunk_len(type_id), 0);
    assert_eq!(
        sealed.tombstones(type_id),
        [Tombstone {
            id: 5,
            xmax: 40,
            deleted: Some(60),
        }]
    );

    assert!(
        fold(&mut a, sealed, 60),
        "the delete stamp reaches the horizon"
    );
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR);
    assert!(!a.flatten(Some(sealed)).contains(&5));
}

/// The fold guard blocks only a reposition whose id still holds a sealed
/// position. A reposition (or delete) of a delta-only hop records a
/// tombstone the fold resolves, so it never wedges the chunk; a sealed
/// hop's tombstone would hide the id's live delta hop and holds the fold
/// until the horizon passes.
#[test]
fn the_fold_guard_only_blocks_a_reposition_of_a_sealed_hop() {
    let mut a = TypedAdj::default();
    let arena = SealedArena::default();
    let sealed = arena.region_or_default(0);
    let type_id = 8;
    for i in 0..COMPACTION_DELTA_FLOOR as i64 {
        a.insert(type_id, i, Some(i));
    }
    assert!(fold(&mut a, sealed, 0));

    // A delta-only hop repositioned inside the sealed chunk: the
    // tombstone names no sealed position and is dropped, and the hop
    // folds in as the id's live position.
    a.insert(type_id, 100, Some(1));
    a.reposition(sealed, type_id, 100, Some(2), 40);
    for i in 0..9 {
        a.insert(type_id, 200 + i, Some(200 + i));
    }
    assert!(fold(&mut a, sealed, 0));
    assert!(sealed.tombstones(type_id).is_empty());
    let mut seen = Vec::new();
    a.for_each_hop_of(Some(sealed), type_id, u64::MAX, |hop| {
        seen.push((hop.ts, hop.id))
    });
    assert!(seen.contains(&(Some(2), 100)));

    // A sealed hop's reposition holds the fold while its tombstone is
    // below the horizon: a bare-id tombstone cannot name one position.
    a.reposition(sealed, type_id, 3, Some(33), 40);
    for i in 0..10 {
        a.insert(type_id, 300 + i, Some(300 + i));
    }
    assert!(!fold(&mut a, sealed, 0), "the reposition holds the fold");
    assert!(fold(&mut a, sealed, 40), "past the horizon the fold runs");
    let mut seen = Vec::new();
    a.for_each_hop_of(Some(sealed), type_id, u64::MAX, |hop| {
        seen.push((hop.ts, hop.id))
    });
    assert!(seen.contains(&(Some(33), 3)), "the live position folds in");
    assert!(!seen.contains(&(Some(3), 3)), "the sealed position is gone");
    assert!(sealed.tombstones(type_id).is_empty());
}

/// The fold guard blocks a delete that followed a reposition while its
/// delete stamp is above the horizon: folding the relocated hop under the
/// tombstone would hide it from a reader between the two stamps. The
/// sealed position still reclaims at the reposition's stamp.
#[test]
fn the_fold_guard_blocks_a_delete_above_the_horizon_after_a_reposition() {
    let mut a = TypedAdj::default();
    let arena = SealedArena::default();
    let sealed = arena.region_or_default(0);
    let type_id = 12;
    for i in 0..COMPACTION_DELTA_FLOOR as i64 {
        a.insert(type_id, i, Some(i));
    }
    assert!(fold(&mut a, sealed, 0));
    // The sealed position is superseded at 40; the relocated hop is
    // deleted at 60.
    a.reposition(sealed, type_id, 3, Some(40), 40);
    sealed.tombstone_deleted(type_id, 3, 60, Supersede::Deleted);
    for i in 0..10 {
        a.insert(type_id, 300 + i, Some(300 + i));
    }
    assert!(
        !fold(&mut a, sealed, 0),
        "nothing reclaims below both stamps"
    );
    assert!(
        fold(&mut a, sealed, 40),
        "the sealed position reclaims, the relocated hop stays"
    );
    assert!(
        !fold(&mut a, sealed, 40),
        "the second pass has nothing left to do"
    );
    {
        assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR - 1);
        assert_eq!(
            a.chunk_hops(type_id).len(),
            11,
            "the relocated hop stays delta"
        );
        assert_eq!(
            sealed.tombstones(type_id),
            [Tombstone {
                id: 3,
                xmax: 40,
                deleted: Some(60),
            }]
        );
    }
    assert!(
        fold(&mut a, sealed, 60),
        "the delete stamp reaches the horizon"
    );
    assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 9);
    assert!(sealed.tombstones(type_id).is_empty());
    let mut seen = Vec::new();
    a.for_each_hop_of(Some(sealed), type_id, u64::MAX, |hop| seen.push(hop.id));
    assert!(!seen.contains(&3));
}

/// ADR-0599 accounting, pinned on a hand-built arena: the floor charges the
/// arena's byte buffer at its exact length plus its 12-byte entry and its
/// tombstone slot, and the delta at live `AdjHop` bytes — never the sealed
/// hops at decoded hop size.
#[test]
fn floor_prices_sealed_at_stream_length_and_delta_at_live_hop_bytes() {
    use std::mem::size_of;

    let sealed_hops: Vec<Hop> = (0..COMPACTION_DELTA_FLOOR as i64)
        .map(|i| Hop {
            ts: Some(10_000 - i),
            id: i,
        })
        .collect();
    let bytes = crate::adjacency::codec::encode_chunk(&sealed_hops);
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

    let arena = SealedArena::default();
    let sealed = arena.region_or_default(9);
    assert!(sealed.put_chunk(9, &sealed_hops, tombstones.clone()));

    // The arena floor: the byte buffer exact, one 12-byte entry, the
    // tombstone column header, and the tombstone slots.
    let expected = crate::memory::vec_heap_exact(bytes.len(), 1)
        + crate::memory::vec_heap(1, size_of::<crate::adjacency::Entry>() as u64)
        + crate::memory::vec_heap(1, size_of::<Vec<Tombstone>>() as u64)
        + crate::memory::vec_heap(tombstones.len(), size_of::<Tombstone>() as u64);
    assert_eq!(arena.floor_bytes(), expected);
    assert_eq!(sealed.chunk_len(9), COMPACTION_DELTA_FLOOR);

    let delta = [Hop {
        ts: Some(20_000),
        id: 999,
    }];
    let adj = TypedAdj {
        chunks: vec![DeltaChunk {
            type_id: 9,
            absent: 0,
            stable_len: 0,
            hops: vec![AdjHop {
                ts: 20_000,
                id: 999,
            }],
        }],
    };
    assert_eq!(
        adj.floor_bytes(),
        adjacency_bytes(std::iter::once(delta.len()))
    );
}

/// The 16-byte hop and the 40-byte chunk entry — presence in the old
/// `(u32, Vec)` tuple's padding, the stability baseline in the word the
/// adaptive seal floor adds — the two strides the adjacency floor
/// arithmetic charts, so neither can drift silently.
#[test]
fn adj_hop_is_sixteen_bytes() {
    use std::mem::size_of;
    assert_eq!(size_of::<AdjHop>(), 16);
    assert_eq!(
        size_of::<DeltaChunk>(),
        40,
        "presence and the stability baseline"
    );
    assert_eq!(size_of::<Option<i64>>(), 16, "the Option this layout drops");
}

/// The adjacency floor's two strides: the 16-byte hop (presence lives in
/// the chunk header's padding, not the hop) and the 40-byte chunk entry —
/// the padding keeps presence free of charge, the stability baseline pays
/// for its own word.
#[test]
fn adjacency_bytes_prices_the_hop_at_sixteen_bytes() {
    // One 3-hop chunk: the entry plus three hops, both at exact fit.
    assert_eq!(
        adjacency_bytes(std::iter::once(3)),
        crate::memory::vec_heap(1, 40) + crate::memory::vec_heap(3, 16),
        "the chunk entry at 40 B and the hop at 16 B"
    );
}

/// The chunk header's presence flags — the `absent` boundary in the
/// padding — roundtrip the per-hop `Option<i64>` the pre-16-byte `AdjHop`
/// carried: all three shapes (all-present, all-absent, mixed) read back
/// hop-for-hop through every mutation arm (insert, remove, reposition)
/// and across the fold into the sealed tier and back.
#[test]
fn header_presence_flags_roundtrip_the_option_semantics() {
    // All-present and all-absent, as the raw flags themselves.
    let mut present = TypedAdj::default();
    for i in 0..3 {
        present.insert(1, i, Some(30 - i));
    }
    assert_eq!(present.chunks[0].absent, 0, "all-present: boundary 0");
    let mut absent = TypedAdj::default();
    for i in 0..3 {
        absent.insert(1, i, None);
    }
    assert_eq!(
        absent.chunks[0].absent as usize,
        absent.chunks[0].hops.len(),
        "all-absent: boundary at the end"
    );

    // Mixed: absent hops lead, present ones follow in ts DESC order, and
    // both arms of a removal move the boundary the way the `Option` did.
    let mut a = TypedAdj::default();
    for (id, ts) in [
        (1, Some(10)),
        (2, None),
        (3, Some(30)),
        (4, None),
        (5, Some(10)),
    ] {
        a.insert(1, id, ts);
    }
    assert_eq!(a.chunks[0].absent, 2, "the mixed boundary");
    assert_eq!(
        a.chunk_hops(1),
        vec![
            Hop { ts: None, id: 4 },
            Hop { ts: None, id: 2 },
            Hop {
                ts: Some(30),
                id: 3
            },
            Hop {
                ts: Some(10),
                id: 5
            },
            Hop {
                ts: Some(10),
                id: 1
            },
        ]
    );
    a.remove(None, 1, 2); // an absent hop leaves
    assert_eq!(a.chunks[0].absent, 1, "the boundary shrinks with it");
    a.remove(None, 1, 3); // a present hop leaves
    assert_eq!(a.chunks[0].absent, 1, "and is untouched by one");
    // A reposition onto `None` moves the hop to the absent head.
    let arena = SealedArena::default();
    a.reposition(arena.region_or_default(0), 1, 1, None, 0);
    assert_eq!(a.chunks[0].absent, 2);
    assert_eq!(
        a.chunk_hops(1),
        vec![
            Hop { ts: None, id: 4 },
            Hop { ts: None, id: 1 },
            Hop {
                ts: Some(10),
                id: 5
            },
        ]
    );

    // Across the tier boundary: a mixed chunk folds into the sealed
    // stream and reads back with the same per-hop `Option`s, and a hop
    // inserted into the folded chunk merges in.
    let sealed = arena.region_or_default(0);
    for i in 0..COMPACTION_DELTA_FLOOR as i64 {
        a.insert(1, 100 + i, if i % 3 == 0 { None } else { Some(50 - i) });
    }
    let before: Vec<Hop> = a.chunk_hops(1);
    assert!(due(&a, Some(sealed), 1, u64::MAX));
    assert!(fold(&mut a, sealed, u64::MAX));
    let mut after = Vec::new();
    a.for_each_hop_of(Some(sealed), 1, u64::MAX, |hop| after.push(hop));
    assert_eq!(after, before, "the fold preserves the per-hop Options");
    assert!(a.chunk_hops(1).is_empty(), "the delta folded");

    a.insert(1, 7, None);
    let mut merged = Vec::new();
    a.for_each_hop_of(Some(sealed), 1, u64::MAX, |hop| merged.push(hop));
    assert_eq!(
        merged.len(),
        before.len() + 1,
        "the absent delta hop merges into the sealed stream"
    );
    assert!(merged.contains(&Hop { ts: None, id: 7 }));
    // The merged stream keeps the shape the Options had: absent hops
    // lead (id DESC among themselves), present ones follow.
    let first_present = merged
        .iter()
        .position(|h| h.ts.is_some())
        .expect("present hops");
    assert!(merged[..first_present].iter().all(|h| h.ts.is_none()));
    assert!(merged[first_present..].iter().all(|h| h.ts.is_some()));
}

/// The adaptive floor's three facts (REL8-280): a never-sealed chunk
/// below the knee is due once its length holds across a pass; growth
/// resets the baseline (a still-growing chunk keeps the knee's floor);
/// the sub-[`STABILITY_SEAL_FLOOR`] band stays delta — the header
/// outweighs the stream it would buy.
#[test]
fn the_stability_floor_reads_the_noted_baseline() {
    let mut a = TypedAdj::default();
    // The eligible length is the floor's, not a literal: the band's lower
    // bound is the fact under test.
    for i in 0..(STABILITY_SEAL_FLOOR + 4) as i64 {
        a.insert(1, i, Some(100 - i));
    }
    assert!(
        !due(&a, None, 1, 0),
        "an unnoted chunk is never stability-sealed"
    );
    assert!(
        a.has_fold_work(None, 0),
        "a sub-knee chunk carries a baseline"
    );
    a.note_stable(None);
    assert!(due(&a, None, 1, 0), "the held length seals below the knee");

    // Growth invalidates the baseline; the new length needs its own
    // stable pass before it can seal.
    a.insert(1, 99, Some(1));
    assert!(!due(&a, None, 1, 0), "growth resets the stability baseline");
    a.note_stable(None);
    assert!(due(&a, None, 1, 0), "the new length holds");

    // The header-overhead band: noted and stable, still delta.
    let mut small = TypedAdj::default();
    for i in 0..(STABILITY_SEAL_FLOOR - 1) as i64 {
        small.insert(1, i, Some(50 - i));
    }
    small.note_stable(None);
    assert!(
        !due(&small, None, 1, 0),
        "the sub-stability band stays delta"
    );
}
