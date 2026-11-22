use super::*;
use crate::adjacency::SealedArena;

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

/// The adaptive floor's three facts (REL8-280): a never-sealed chunk
/// below the knee is due once its length holds across a pass; growth
/// The adaptive floor's facts: a never-sealed chunk below the knee is due
/// only once its length held across two fold passes — one quiet interval
/// is not evidence the chunk stopped growing; growth resets the baseline
/// (a still-growing chunk keeps the knee's floor); the
/// sub-[`STABILITY_SEAL_FLOOR`] band stays delta — the header outweighs
/// the stream it would buy.
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
    assert!(
        !due(&a, None, 1, 0),
        "one held interval is not evidence of a steady state"
    );
    a.note_stable(None);
    assert!(
        due(&a, None, 1, 0),
        "the twice-held length seals below the knee"
    );

    // Growth invalidates the baseline; the new length needs its own two
    // stable passes before it can seal.
    a.insert(1, 99, Some(1));
    assert!(!due(&a, None, 1, 0), "growth resets the stability baseline");
    a.note_stable(None);
    assert!(!due(&a, None, 1, 0), "the new length must hold twice too");
    a.note_stable(None);
    assert!(due(&a, None, 1, 0), "the new length held");

    // The header-overhead band: noted and held, still delta.
    let mut small = TypedAdj::default();
    for i in 0..(STABILITY_SEAL_FLOOR - 1) as i64 {
        small.insert(1, i, Some(50 - i));
    }
    small.note_stable(None);
    small.note_stable(None);
    assert!(
        !due(&small, None, 1, 0),
        "the sub-stability band stays delta"
    );
}

/// The derived floor's residency bound: once the settle passes run — note,
/// held confirmation, seal — no chunk at or above the floor is still
/// unsealed, and the sub-floor band is still delta.
#[test]
fn the_settled_band_drains_into_the_sealed_stream() {
    let arena = SealedArena::default();
    let sealed = arena.region_or_default(7);
    let mut a = TypedAdj::default();
    let settled = STABILITY_SEAL_FLOOR + 3;
    for i in 0..settled as i64 {
        a.insert(1, i, Some(100 - i));
    }
    for i in 0..(STABILITY_SEAL_FLOOR - 1) as i64 {
        a.insert(2, i, Some(100 - i));
    }
    for _ in 0..3 {
        a.fold_at(sealed, 0);
    }
    assert_eq!(
        sealed.chunk_len(1),
        settled,
        "the settled chunk's whole stream is sealed"
    );
    assert_eq!(
        a.chunk_hops(2).len(),
        STABILITY_SEAL_FLOOR - 1,
        "the sub-floor band stays delta"
    );
    assert_eq!(sealed.chunk_len(2), 0, "the sub-floor band is never sealed");
}
