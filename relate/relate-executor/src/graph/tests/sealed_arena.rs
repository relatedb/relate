//! The sealed arena's write-path invariants: the per-event charge's
//! price_side arms (the one formula the admission estimate prices delete
//! batches with), the delete estimate's exactness per scenario, and the fully-reclaimed chunk's dead stream.

use relate_core::{NodeId, RelId, Value};

use super::super::accounting::adjacency::AdjDelta;
use super::super::Graph;
use super::chain::{props, step_matches_charge};
use crate::adjacency::{with_arena_wall, Hop, SealedArena, SealedStream, RESTORE_SEAL_HOPS};
use crate::redo::RedoOp;

/// A wholesale install reserves every buffer at the size the closing fit
/// lands it at: the byte buffer's capacity is its length, and `fit` moves
/// none of the four buffers — no reallocation copy, so the build's peak
/// never holds a buffer twice. The streams come from two shards, nodes
/// interleaved, types pushed out of order: the install rebases the second
/// shard's offsets and the entries read back type-sorted.
#[test]
fn a_wholesale_install_leaves_fit_nothing_to_copy() {
    let hops: Vec<Hop> = (0..9).rev().map(|id| Hop { ts: Some(id), id }).collect();
    let arena = SealedArena::default();
    let mut streams = vec![SealedStream::default(), SealedStream::default()];
    for node in 0..40 {
        let stream = &mut streams[node as usize % 2];
        for type_id in [3, 1, 2] {
            assert!(stream.seal_chunk(node, type_id, &hops));
        }
        stream.end_node();
    }

    arena.install(streams);
    let (buffers, capacity, len) = arena.buffer_probe();
    assert_eq!(
        capacity, len,
        "the byte buffer is exactly its encoded length"
    );
    arena.fit();
    assert_eq!(
        arena.buffer_probe(),
        (buffers, capacity, len),
        "fit copied nothing"
    );
    assert_eq!(arena.assert_fit(), 0);

    for node in [0, 7, 39] {
        let region = arena.region(node).expect("every node sealed");
        assert_eq!(region.type_ids(), vec![1, 2, 3]);
        for type_id in 1..=3 {
            assert_eq!(region.decoded(type_id), hops, "node {node} type {type_id}");
        }
    }
}

/// The reposition's sealed charge follows price_side's arms — the one
/// formula the admission estimate prices delete batches with: a second
/// reposition of an already-tombstoned position prices the in-place arm's
/// zero, and a ts update on a type with no sealed entry prices nothing
/// (its delta-only arm records no tombstone). The recount agrees after
/// each write.
#[test]
fn reposition_sealed_charge_prices_the_price_side_arms() {
    let g = Graph::default();
    let (a, b, ids, _) = crate::graph::test_support::folded_floor_chunk(&g);

    // A second reposition of the same sealed hop: the tombstone exists,
    // the in-place arm prices zero.
    g.begin_write().unwrap();
    g.set_property_rel(ids[0], "ts", Value::Int(1000));
    g.commit_write();
    g.begin_write().unwrap();
    g.set_property_rel(ids[0], "ts", Value::Int(1001));
    g.commit_write();
    assert_eq!(g.used_bytes(), g.accounted_bytes());

    // A ts update on a never-sealed type: no sealed entry, no sealed
    // charge.
    let lone = crate::graph::test_support::doomed_rel(&g, a, b, "V");
    g.begin_write().unwrap();
    g.set_property_rel(lone, "ts", Value::Int(7));
    g.commit_write();
    assert_eq!(g.used_bytes(), g.accounted_bytes());
}

/// A chunk the checkpoint fold fully reclaims drops its entry, and its
/// arena stream dies with it: the dead range is marked before the entry
/// goes, so compaction reclaims exactly the stream's bytes — the
/// exact-reclaim invariant ADR-0599's debug assert states, which a missing
/// mark trips (the bytes would leave the arena without ever being dead).
#[test]
fn a_fully_reclaimed_chunk_marks_its_stream_dead_before_the_entry_goes() {
    let g = Graph::default();
    let (_, arena) = crate::graph::test_support::reclaimed_folded_chunk(&g);
    let (_, dead) = g.out_side().sealed().reclaim_pair();
    assert_eq!(
        dead, arena,
        "the dead range is exactly the reclaimed stream's length"
    );

    // Compaction reclaims exactly the dead ranges — the debug assert inside
    // states the equality this pins from outside.
    g.out_side().sealed().compact();
    assert_eq!(g.out_side().sealed().reclaim_pair(), (0, 0));
}

/// A wiped arena drops its dead ranges with everything else — a clear
/// that kept them would leave the wipe charging (and holding) ranges no
/// live entry points at.
#[test]
fn a_wipe_drops_the_dead_ranges_too() {
    let g = Graph::default();
    let (_, arena) = crate::graph::test_support::reclaimed_folded_chunk(&g);
    let (_, dead) = g.out_side().sealed().reclaim_pair();
    assert_eq!(dead, arena, "the reclaimed stream is dead, not compacted");

    g.out_side().sealed().clear();
    assert_eq!(
        g.out_side().sealed().reclaim_pair(),
        (0, 0),
        "the wipe drops the arena, its entries and the dead ranges"
    );
    assert_eq!(g.out_side().sealed().floor_bytes(), 0);
    assert_eq!(g.out_side().sealed().array_bytes(), 0);
}

/// Label-less endpoints (a delete must not also drop a count entry, which
/// the estimate reports as growth only) `0` and `1`, joined by rel `2` of
/// type `U`.
fn created_pair(g: &Graph) {
    let create = |id: NodeId| RedoOp::create_node(id, Vec::new(), props(1));
    step_matches_charge(g, vec![create(0), create(1)], false);
    step_matches_charge(
        g,
        vec![RedoOp::create_rel(2, 0, 1, "U".into(), props(1))],
        false,
    );
}

/// [`created_pair`] plus the delete of rel `2`: the on-demand region the
/// later scenarios join is founded.
fn founded_region() -> Graph {
    let g = Graph::default();
    created_pair(&g);
    step_matches_charge(&g, vec![RedoOp::delete_rel(2)], true);
    g
}

/// The one spelling of this module's single-record seam: a one-delete
/// estimate seeded from the committed spans, priced off the very ops the
/// site then charges — one construction of the delete, never a second
/// spelling beside the estimate. It returns the estimate's own named
/// value beside those ops.
fn delete_delta(g: &Graph, rel: RelId) -> (AdjDelta, Vec<RedoOp>) {
    let ops = vec![RedoOp::delete_rel(rel)];
    (g.estimate_adj_delta(&ops, &g.dense_spans()).0, ops)
}

// A delete's sealed-tier growth — the on-demand region, its slot array, and
// the tombstone on an already-sealed chunk — is estimated exactly. The
// fold-reclaimed terms land in the spill (ADR-0529: a delete at the cap
// commits); the founding slot growth the dense span keeps is refusable,
// pinned non-vacuously by the founding-slot scenarios. A create batch
// spills nothing. One test per scenario, each building its own state.

/// A type that never sealed: the delete creates the on-demand region.
#[test]
fn sealed_delete_founds_the_on_demand_region() {
    let g = Graph::default();
    created_pair(&g);
    step_matches_charge(&g, vec![RedoOp::delete_rel(2)], true);
}

/// A rel created and deleted in one batch never reaches the committed
/// store, so the delete's endpoints come from the batch's own create — its
/// tombstones land on the region an earlier delete founded.
#[test]
fn sealed_delete_of_a_rel_created_in_the_same_batch() {
    let g = founded_region();
    step_matches_charge(
        &g,
        vec![
            RedoOp::create_rel(3, 0, 1, "U".into(), props(1)),
            RedoOp::delete_rel(3),
        ],
        true,
    );
}

/// A hop in a folded chunk: the delete grows that chunk's tombstones — one
/// per endpoint — and that growth is all spill.
#[test]
fn sealed_delete_in_a_folded_chunk_is_all_spill() {
    let g = Graph::default();
    let (_, _, ids, _) = crate::graph::test_support::folded_floor_chunk(&g);
    let (AdjDelta { refusable, spill }, sealed) = delete_delta(&g, ids[0]);
    let stride = crate::adjacency::tombstone_stride() as i64;
    assert_eq!(refusable, 0);
    assert_eq!(spill, 2 * stride, "one tombstone slot per endpoint");
    step_matches_charge(&g, sealed, true);
}

/// A never-sealed type joining a region the endpoint already holds: the
/// on-demand chunk is priced as entry growth on that region, not the
/// region-founding term.
#[test]
fn sealed_delete_of_a_type_joining_a_held_region() {
    let g = Graph::default();
    let (a, b, _, _) = crate::graph::test_support::folded_floor_chunk(&g);
    let joiner = crate::graph::test_support::doomed_rel(&g, a, b, "V");
    step_matches_charge(&g, vec![RedoOp::delete_rel(joiner)], true);
}

/// Two never-sealed types joining one region in a single batch: the
/// chunks-vec entry growth is one before-and-after term per region, not one
/// per type — the pow2 reservation makes the difference bite (a two-chunk
/// region gaining two entries charges vec_heap(4) - vec_heap(2), not twice
/// vec_heap(3) - vec_heap(2)).
#[test]
fn sealed_delete_of_two_types_joining_one_region() {
    let g = Graph::default();
    let (a, b, _, _) = crate::graph::test_support::folded_floor_chunk(&g);
    let pair = [
        crate::graph::test_support::doomed_rel(&g, a, b, "W"),
        crate::graph::test_support::doomed_rel(&g, a, b, "X"),
    ];
    step_matches_charge(
        &g,
        vec![RedoOp::delete_rel(pair[0]), RedoOp::delete_rel(pair[1])],
        true,
    );
}

/// The routing, pinned non-vacuously: a founding delete at an endpoint
/// beyond the sealed side table's span charges its slot growth as
/// refusable (committed structure, as the store's own estimator charges it)
/// and its chunk and tombstone terms as spill.
#[test]
fn sealed_delete_founding_slot_growth_is_refusable() {
    let g = Graph::default();
    crate::graph::test_support::folded_floor_chunk(&g);
    let (edge, other) = crate::graph::test_support::endpoints(&g);
    let founder = crate::graph::test_support::doomed_rel(&g, edge, other, "Y");
    let (
        AdjDelta {
            refusable: found_refusable,
            spill: found_spill,
        },
        founder_ops,
    ) = delete_delta(&g, founder);
    assert!(
        found_refusable > 0,
        "a founding delete's slot growth is refusable"
    );
    assert!(found_spill > 0, "its chunk and tombstone terms spill");
    step_matches_charge(&g, founder_ops, true);
}

/// The same founding delete at a side-map id — past the dense cap, so the
/// store holds the entry in its side map: the entry is reclaimed with the
/// region, so it is spill and the delete refuses nothing.
#[test]
fn sealed_delete_founding_at_a_side_map_id_refuses_nothing() {
    // The founded region already spans endpoint 0, so the only growth left
    // at the doomed rel's far end is the side-map entry.
    let g = founded_region();
    let sparse = crate::graph::test_support::first_side_map_id(crate::graph::sealed_slot());
    g.apply_redo(
        vec![RedoOp::create_node(sparse, Vec::new(), props(1))],
        g.commit_ts() + 1,
    )
    .unwrap();
    let doomed = crate::graph::test_support::doomed_rel(&g, sparse, 0, "Z");
    let (
        AdjDelta {
            refusable: sparse_refusable,
            spill: sparse_spill,
        },
        doomed_ops,
    ) = delete_delta(&g, doomed);
    assert_eq!(
        sparse_refusable, 0,
        "a founding delete at a side-map id refuses nothing"
    );
    assert!(
        sparse_spill > 0,
        "its side-map entry is reclaimed, so spill"
    );
    step_matches_charge(&g, doomed_ops, true);
}

/// Three types of sealable runs from `from` to `to`, committed in one write:
/// each run is `RESTORE_SEAL_HOPS` hops or more, so the build's seal gate
/// takes it on its own.
fn three_sealable_runs(g: &Graph, from: NodeId, to: NodeId) {
    g.begin_write().unwrap();
    for type_name in ["T0", "T1", "T2"] {
        for _ in 0..RESTORE_SEAL_HOPS + 2 {
            g.create_rel(
                from,
                to,
                type_name.into(),
                std::collections::BTreeMap::new(),
            )
            .unwrap();
        }
    }
    g.commit_write();
}

/// How many sealed chunks `node` holds on the out side.
fn sealed_chunks(g: &Graph, node: NodeId) -> usize {
    g.out_side()
        .sealed()
        .region(node)
        .map_or(0, |region| region.chunk_count())
}

/// A wholesale build whose streams would cross the arena's 4 GiB wall keeps
/// the run that does not fit in the delta tier and returns: the open never
/// panics on the wall (ADR-0599). The wall is pinned lower through
/// [`with_arena_wall`] — a real 4 GiB build is not a unit test — and one
/// shard runs inline, so the single stream is the whole build.
#[test]
fn a_wholesale_build_past_the_wall_keeps_the_overflow_in_delta() {
    let g = Graph::default();
    let (a, b) = crate::graph::test_support::endpoints(&g);
    three_sealable_runs(&g, a, b);

    g.rebuild_adj();
    assert_eq!(sealed_chunks(&g, a), 3);
    let whole = g.out_side().sealed().arena_bytes();
    let degree = g.degree_out(a);

    // One byte short of the whole build: the last run's stream crosses it.
    let wall = whole - 1;
    with_arena_wall(wall, || g.rebuild_adj());
    assert!(g.out_side().sealed().arena_bytes() <= wall);
    assert_eq!(
        sealed_chunks(&g, a),
        2,
        "the overflowing run stays in the delta tier"
    );
    assert_eq!(
        g.degree_out(a),
        degree,
        "the build reads back the same rels"
    );
}

/// Two shards that each fit the wall alone but not together: no shard sees
/// the sum, so the side is built again with every run in the delta tier
/// (ADR-0599) and the build still returns.
#[test]
fn shards_that_overflow_the_wall_together_build_all_delta() {
    let g = Graph::default();
    // Sequential ids, so the two starts take different shards (`id % 2`).
    let (a, b) = crate::graph::test_support::endpoints(&g);
    let (c, d) = crate::graph::test_support::endpoints(&g);
    assert_ne!(a % 2, d % 2, "the two starts land in different shards");
    three_sealable_runs(&g, a, b);
    three_sealable_runs(&g, d, c);

    let whole = crate::graph::shard::with_workers(2, || {
        g.rebuild_adj();
        g.out_side().sealed().arena_bytes()
    });
    let degree = (g.degree_out(a), g.degree_out(d));

    crate::graph::shard::with_workers(2, || {
        with_arena_wall(whole - 1, || g.rebuild_adj());
    });
    assert_eq!(g.out_side().sealed().arena_bytes(), 0, "all delta");
    assert_eq!(sealed_chunks(&g, a), 0);
    assert_eq!((g.degree_out(a), g.degree_out(d)), degree);
}
