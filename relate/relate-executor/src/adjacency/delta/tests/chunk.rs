use super::*;
use crate::adjacency::SealedArena;

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
    assert_eq!(present.chunks[0].absent_len(), 0, "all-present: boundary 0");
    let mut absent = TypedAdj::default();
    for i in 0..3 {
        absent.insert(1, i, None);
    }
    assert_eq!(
        absent.chunks[0].absent_len() as usize,
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
    assert_eq!(a.chunks[0].absent_len(), 2, "the mixed boundary");
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
    assert_eq!(a.chunks[0].absent_len(), 1, "the boundary shrinks with it");
    a.remove(None, 1, 3); // a present hop leaves
    assert_eq!(a.chunks[0].absent_len(), 1, "and is untouched by one");
    // A reposition onto `None` moves the hop to the absent head.
    let arena = SealedArena::default();
    a.reposition(arena.region_or_default(0), 1, 1, None, 0);
    assert_eq!(a.chunks[0].absent_len(), 2);
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
