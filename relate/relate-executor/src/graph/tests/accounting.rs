//! Byte-accounting and admission-estimate tests: the incremental charge
//! against the live walk, and the redo-delta estimate against the apply.

use super::*;
use crate::adjacency::{MAX_SPAN_TYPES, RESTORE_SEAL_HOPS};
use crate::graph::accounting::{sealed_arena_breaches, AdjDelta};
use relate_core::{CapacityKind, Error};

#[test]
fn used_bytes_matches_live_walk_after_mutate_and_delete() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let mut p = BTreeMap::new();
    p.insert("name".into(), Value::String("abcdefghij".into()));
    let a = g.create_node(vec!["Person".into()], p).unwrap();
    let b = g
        .create_node(vec!["Person".into()], BTreeMap::new())
        .unwrap();
    g.create_rel(a, b, "KNOWS".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    assert_eq!(g.used_bytes(), g.accounted_bytes());
    assert!(g.used_bytes() > 0);

    g.begin_write().unwrap();
    g.set_property_node(a, "name", Value::String("x".into()));
    g.commit_write();
    assert_eq!(g.used_bytes(), g.accounted_bytes());

    g.begin_write().unwrap();
    g.delete_node(a, true).unwrap();
    g.commit_write();
    assert_eq!(g.used_bytes(), g.accounted_bytes());
}

/// A relationship stored as `Other` stays in property vectors when a later
/// `SET` makes the map look like `{id, amount, ts}`. `RelPayload` never
/// demotes `Other` to `Triple`, so the estimate and the incremental charge
/// have to keep those vectors or `used_bytes` falls behind the live walk.
#[test]
fn other_rel_that_gains_triple_keys_stays_on_the_floor() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    let b = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    g.commit_write();

    let partial = BTreeMap::from([
        ("id".into(), Value::String("a".into())),
        ("amount".into(), Value::Int(1)),
    ]);
    let same_batch = vec![
        RedoOp::CreateRel {
            id: 1,
            start: a,
            end: b,
            rel_type: "PAYS".into(),
            properties: relate_core::PropertyMap::share(partial),
        },
        RedoOp::SetRelProp {
            id: 1,
            key: "ts".into(),
            value: Value::Int(5),
        },
    ];
    let before = g.used_bytes();
    let estimate = g.estimate_redo_delta(&same_batch).total;
    g.apply_redo(same_batch, g.commit_ts() + 1).unwrap();
    assert_eq!(g.used_bytes(), g.accounted_bytes());
    assert_eq!(g.used_bytes() as i64 - before as i64, estimate);

    let before = g.used_bytes();
    let set_again = vec![RedoOp::SetRelProp {
        id: 1,
        key: "note".into(),
        value: Value::String("x".into()),
    }];
    let estimate = g.estimate_redo_delta(&set_again).total;
    g.apply_redo(set_again, g.commit_ts() + 1).unwrap();
    assert_eq!(g.used_bytes(), g.accounted_bytes());
    assert_eq!(g.used_bytes() as i64 - before as i64, estimate);

    let triple = BTreeMap::from([
        ("id".into(), Value::String("b".into())),
        ("amount".into(), Value::Int(2)),
        ("ts".into(), Value::Int(9)),
    ]);
    let created = vec![RedoOp::CreateRel {
        id: 2,
        start: a,
        end: b,
        rel_type: "PAYS".into(),
        properties: relate_core::PropertyMap::share(triple),
    }];
    let before = g.used_bytes();
    let estimate = g.estimate_redo_delta(&created).total;
    g.apply_redo(created, g.commit_ts() + 1).unwrap();
    assert_eq!(g.used_bytes(), g.accounted_bytes());
    assert_eq!(g.used_bytes() as i64 - before as i64, estimate);
    assert_eq!(g.rel(1).unwrap().properties.len(), 4);
    assert_eq!(g.rel(2).unwrap().properties.len(), 3);
}

/// The once-per-walk seeded-mate protocol, stated once for every
/// dense-span family: the crossing estimate advances the seed, the mate
/// estimated from that advance pays no rung the walk reserved, the mate
/// still pays its own record, and no estimate ever moves the seed. A test
/// supplies only the ops — which family's rung they cross is its business —
/// and asserts the megabyte-scale gap on the returned totals. (Every
/// message stays value-free: CodeQL's cleartext-logging heuristic reads the
/// `*_account` accounting helpers as sensitive, and an interpolated byte
/// count is the one sink that trips it.)
fn seeded_mate_gain(g: &Graph, crossing: &[RedoOp], mate: &[RedoOp]) -> (i64, i64) {
    let seed = g.dense_spans();
    // Each estimate derives its own ADR-0439 term — the walk's own
    // protocol: a caller holding a seed supplies the term for the batch it
    // prices, never a foreign batch's.
    let (_, advanced) = g.estimate_redo_delta_with_constraint_growth(
        crossing,
        g.estimate_constraint_key_entry_growth(crossing),
        seed,
    );
    let (seeded, _) = g.estimate_redo_delta_with_constraint_growth(
        mate,
        g.estimate_constraint_key_entry_growth(mate),
        advanced,
    );
    // The single-record entry point, which seeds from the committed spans.
    let fresh = g.estimate_redo_delta(mate);
    assert!(
        seeded.total > 0,
        "the mate still pays its own record: seeding removes the rung re-charge, nothing else"
    );
    // An estimate never moves its seed: a refused slot must not leave a
    // phantom rung behind for the next mate.
    assert_eq!(
        seed,
        g.dense_spans(),
        "the seed is the committed spans, untouched by any estimate"
    );
    (seeded.total, fresh.total)
}

/// A dense growth rung is charged once per admission walk, never once per
/// estimate: the walk's advanced spans seed the next estimate, so a create
/// placing an id inside the rung an earlier create of the same walk already
/// reserved prices no span growth — where a fresh estimate from the
/// committed span, which cannot advance mid-walk, re-charges the whole
/// rung (megabyte-scale at these ids).
#[test]
fn seeded_spans_price_a_mate_inside_the_reserved_rung() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    let b = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    g.commit_write();
    let rel = |id: i64| {
        vec![RedoOp::create_rel(
            id,
            a,
            b,
            "KNOWS".into(),
            BTreeMap::new(),
        )]
    };
    let (seeded, fresh) = seeded_mate_gain(&g, &rel(60_000), &rel(60_001));
    assert!(
        fresh - seeded > 1024 * 1024,
        "the re-charged chain rung is megabyte-scale"
    );
}

/// The adjacency tier's dense rungs follow the same one-charge-per-walk
/// protocol: a rel create placing an endpoint inside the adjacency rung an
/// earlier create of the same walk reserved prices no adjacency growth —
/// where a fresh estimate from the committed span, which cannot advance
/// mid-walk, re-charges the whole rung (the endpoints sit at ids no
/// committed adjacency entry covers, so only the adjacency term differs
/// between the two estimates; the rel ids stay low so the chain spans do
/// not).
#[test]
fn seeded_adjacency_spans_price_a_mate_inside_the_reserved_rung() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let b = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    g.commit_write();
    let rel = |id: i64, start: i64| {
        vec![RedoOp::create_rel(
            id,
            start,
            b,
            "KNOWS".into(),
            BTreeMap::new(),
        )]
    };
    let (seeded, fresh) = seeded_mate_gain(&g, &rel(1, 60_000), &rel(2, 60_001));
    assert!(
        fresh - seeded > 1024 * 1024,
        "the re-charged adjacency rung is megabyte-scale"
    );
}

/// F-1-30: a self-loop's delete charges both on-demand zero-hop regions —
/// the two sides share the node, but not their side table. Both tables are
/// pre-seeded past these ids, so the estimate is the region term alone.
#[test]
fn self_loop_delete_charges_both_sealed_regions() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    let b = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    let c = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    let x1 = g.create_rel(a, c, "X".into(), BTreeMap::new()).unwrap();
    let x2 = g.create_rel(c, b, "X".into(), BTreeMap::new()).unwrap();
    let loop_id = g.create_rel(a, a, "T".into(), BTreeMap::new()).unwrap();
    let pair_id = g.create_rel(a, b, "T".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    g.begin_write().unwrap();
    g.delete_rel(x1);
    g.delete_rel(x2);
    g.commit_write();

    // The joining-chunk term derived from the layout primitives, not the
    // helper it checks: one 12-byte arena entry into the existing column,
    // one tombstone slot (the zero-hop stream appends no arena bytes,
    // ADR-0599).
    let (entries, tombstones) = g.out_side().sealed().entry_counts();
    let entry = std::mem::size_of::<crate::adjacency::Entry>() as u64;
    let column = std::mem::size_of::<Vec<crate::adjacency::Tombstone>>() as u64;
    let tombstone = crate::adjacency::tombstone_stride();
    let region = (crate::memory::vec_growth(entries, 1, entry)
        + crate::memory::vec_growth(tombstones, 1, column)
        + crate::memory::vec_heap(1, tombstone)) as i64;
    let delta = |id: i64| {
        let AdjDelta { refusable, spill } = g
            .estimate_adj_delta(&[RedoOp::DeleteRel { id }], &g.dense_spans())
            .0;
        (refusable, spill)
    };
    let (loop_refusable, self_loop) = delta(loop_id);
    let (pair_refusable, pair) = delta(pair_id);
    assert_eq!(
        (loop_refusable, pair_refusable),
        (0, 0),
        "no slot growth at these ids: the chunk and tombstone terms are the spill \
         (the founding-slot routing is pinned in chain.rs)"
    );
    assert_eq!(self_loop, 2 * region, "both sides of the self-loop charge");
    assert_eq!(pair, 2 * region, "one region per endpoint");
}

/// LABS-370 (security follow-up): the redo-delta estimate includes the
/// count family's entry growth — one rel between two multi-label nodes
/// materializes labels(start) × labels(end) pair entries plus the
/// start-side histogram entries, and a batch creating labeled nodes
/// materializes label entries — all charged BEFORE the commit applies, so
/// an oversized-label write is rejected at admission, not after its
/// entries land past the cap. Exact against the applied delta.
#[test]
fn estimate_redo_delta_includes_stats_entry_growth() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g
        .create_node(vec!["X1".into(), "X2".into()], BTreeMap::new())
        .unwrap();
    let b = g
        .create_node(vec!["Y1".into(), "Y2".into()], BTreeMap::new())
        .unwrap();
    g.commit_write();
    let rel = vec![RedoOp::CreateRel {
        id: 1,
        start: a,
        end: b,
        rel_type: "KNOWS".into(),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    }];
    let before = g.used_bytes();
    let estimate = g.estimate_redo_delta(&rel).total;
    g.apply_redo(rel, g.commit_ts() + 1).unwrap();
    assert_eq!(g.used_bytes(), g.accounted_bytes());
    assert_eq!(g.used_bytes() as i64 - before as i64, estimate);
    // The stats term dominates the rel's own bytes: 2×2 = 4 pair entries
    // plus 2 (start-label, type) histogram entries.
    assert!(
        estimate >= (4 * Graph::PAIR_COUNT_ENTRY_BYTES + 2 * Graph::HISTOGRAM_ENTRY_BYTES) as i64
    );
    // A batch creating labeled nodes grows the label-count map too.
    let node = vec![RedoOp::CreateNode {
        id: 900,
        labels: Arc::new(vec!["Z".into()]),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    }];
    let before = g.used_bytes();
    let estimate = g.estimate_redo_delta(&node).total;
    g.apply_redo(node, g.commit_ts() + 1).unwrap();
    assert_eq!(g.used_bytes(), g.accounted_bytes());
    assert_eq!(g.used_bytes() as i64 - before as i64, estimate);
    assert!(estimate >= Graph::LABEL_COUNT_ENTRY_BYTES as i64);
}

/// A batch that creates an index and, in the same batch, deletes, rewrites,
/// and creates nodes under it must estimate exactly what applying it costs:
/// the deleted node stays charged until GC, the rewritten one keeps its old
/// version and adds a new one at the new charge, the untouched one picks up
/// the new charge, and the created one is charged from the start.
#[test]
fn estimate_redo_delta_is_exact_for_ddl_mixed_with_writes() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let title = |s: &str| BTreeMap::from([("title".into(), Value::String(s.into()))]);
    let doomed = g
        .create_node(vec!["Book".into()], title("Gone With the Wind"))
        .unwrap();
    let rewritten = g.create_node(vec!["Book".into()], title("Emma")).unwrap();
    g.create_node(vec!["Book".into()], title("War and Peace"))
        .unwrap();
    g.create_node(vec!["Person".into()], title("not indexed"))
        .unwrap();
    g.commit_write();
    let before = g.used_bytes();
    assert_eq!(before, g.accounted_bytes());

    let ops = vec![
        RedoOp::CatalogIndexDetail {
            name: "titleIndex".into(),
            detail: crate::redo::IndexDetailSpec {
                kind: crate::redo::IndexKindSpec::Text,
                relationship: false,
                labels: vec!["Book".into()],
                properties: vec!["title".into()],
                with_properties: vec![],
            },
            options: redo::props_json(&BTreeMap::new()),
        },
        RedoOp::DeleteNode { id: doomed },
        RedoOp::SetNodeProp {
            id: rewritten,
            key: "title".into(),
            value: Value::String("Emma, or the Misfortunes of Virtue".into()),
        },
        RedoOp::CreateNode {
            id: 1_000,
            labels: Arc::new(vec!["Book".into()]),
            properties: relate_core::PropertyMap::share(title("Middlemarch")),
        },
    ];
    let estimate = g.estimate_redo_delta(&ops).total;
    g.apply_redo(ops, g.commit_ts() + 1).unwrap();
    assert_eq!(g.used_bytes(), g.accounted_bytes());
    assert_eq!(g.used_bytes() as i64 - before as i64, estimate);
}

#[test]
fn range_index_increases_the_node_floor() {
    let plain = {
        let g = Graph::default();
        g.begin_write().unwrap();
        g.create_node(
            vec!["Flat".into()],
            BTreeMap::from([("id".into(), Value::Int(1))]),
        )
        .unwrap();
        g.commit_write();
        g.used_bytes()
    };
    let g = Graph::default();
    g.apply_redo(
        vec![RedoOp::CatalogIndexDetail {
            name: "Flat_id".into(),
            detail: crate::redo::IndexDetailSpec {
                kind: crate::redo::IndexKindSpec::Range,
                relationship: false,
                labels: vec!["Flat".into()],
                properties: vec!["id".into()],
                with_properties: vec![],
            },
            options: redo::props_json(&BTreeMap::new()),
        }],
        1,
    )
    .unwrap();
    g.begin_write().unwrap();
    g.create_node(
        vec!["Flat".into()],
        BTreeMap::from([("id".into(), Value::Int(1))]),
    )
    .unwrap();
    g.commit_write();
    let indexed = g.used_bytes();
    assert!(
        indexed > plain,
        "plain={plain} indexed={indexed} delta={}",
        indexed as i64 - plain as i64
    );
}

#[test]
fn floor_reserves_node_ring_edge_and_dense_node() {
    let g = Graph::default();
    assert_eq!(g.used_bytes(), g.accounted_bytes());

    g.begin_write().unwrap();
    let hub = g
        .create_node(
            vec!["Dense".into()],
            BTreeMap::from([("id".into(), Value::Int(0))]),
        )
        .unwrap();
    let mut leaves = Vec::new();
    for i in 1..=20 {
        leaves.push(
            g.create_node(
                vec!["Dense".into()],
                BTreeMap::from([("id".into(), Value::Int(i))]),
            )
            .unwrap(),
        );
    }
    let ring_a = g
        .create_node(
            vec!["Ring".into()],
            BTreeMap::from([("id".into(), Value::Int(0))]),
        )
        .unwrap();
    let ring_b = g
        .create_node(
            vec!["Ring".into()],
            BTreeMap::from([("id".into(), Value::Int(1))]),
        )
        .unwrap();
    g.create_rel(ring_a, ring_b, "NEXT".into(), BTreeMap::new())
        .unwrap();
    for leaf in &leaves {
        g.create_rel(hub, *leaf, "LINK".into(), BTreeMap::new())
            .unwrap();
    }
    g.commit_write();
    assert_eq!(g.used_bytes(), g.accounted_bytes());

    let node = g.nodes.get(&hub).unwrap();
    assert!(node.is_inline());
    let live = node.last().live;
    assert_eq!(live.labels.len(), 1);
    assert_eq!(live.labels.capacity(), crate::memory::reserved_len(1));
    assert_eq!(live.props.len(), 1);
    assert_eq!(
        live.props.key_capacity(),
        crate::memory::reserved_len(live.props.len())
    );
    assert_eq!(
        live.props.val_capacity(),
        crate::memory::reserved_len(live.props.len())
    );

    let ring = g.out_side().delta().get(&ring_a).unwrap();
    assert_eq!(ring.chunks.len(), 1);
    assert_eq!(ring.chunks.capacity(), crate::memory::reserved_len(1));
    assert_eq!(ring.chunks[0].hops.len(), 1);
    assert_eq!(
        ring.chunks[0].hops.capacity(),
        crate::memory::reserved_len(1)
    );

    let dense = g.out_side().delta().get(&hub).unwrap();
    assert_eq!(dense.chunks.len(), 1);
    assert_eq!(dense.chunks.capacity(), crate::memory::reserved_len(1));
    assert_eq!(dense.chunks[0].hops.len(), 20);
    assert_eq!(
        dense.chunks[0].hops.capacity(),
        crate::memory::reserved_len(20)
    );
}

/// The span-length wall (ADR-0599): a node at `MAX_SPAN_TYPES` distinct
/// out types refuses the next *new* type at admission — a held type, the
/// other direction, and another node stay free — and the refusal leaves
/// the graph serving (the walk holds read guards only, so nothing is
/// poisoned). The direct mutation path is admission-free, so the store
/// reaches the wall through it; `enforce_adjacency_limits` is the seam
/// that would have refused the write before it logged.
#[test]
fn enforce_adjacency_limits_refuses_the_65_536th_type_on_a_node() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    let b = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    for i in 0..MAX_SPAN_TYPES {
        g.create_rel(a, b, format!("T{i}"), BTreeMap::new())
            .unwrap();
    }
    g.commit_write();

    let op = |start, end, ty: &str| RedoOp::CreateRel {
        id: 1,
        start,
        end,
        rel_type: ty.into(),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    };
    // The 65,536th distinct out type on `a` (and in type on `b`) is the
    // span length's u16 wall, refused before the redo is logged.
    let refusal = g
        .enforce_adjacency_limits(&[op(a, b, "wall")], &[])
        .unwrap_err();
    assert!(matches!(
        &refusal,
        Error::Capacity {
            kind: CapacityKind::AdjacencyTypes,
            ..
        }
    ));
    // A held type prices no fresh slot, a delete founds only held types,
    // and the two reverse directions of the same nodes are free.
    g.enforce_adjacency_limits(&[op(a, b, "T0")], &[]).unwrap();
    g.enforce_adjacency_limits(&[RedoOp::DeleteRel { id: 0 }], &[])
        .unwrap();
    g.enforce_adjacency_limits(&[op(b, a, "wall")], &[])
        .unwrap();
    // The refused walk held read guards only: adjacency still answers.
    assert_eq!(g.out_side().hop_counts(a).len(), MAX_SPAN_TYPES);
}

/// The wall is per group commit: a slot that passes alone is refused when
/// an already-accepted slot of the same group contributed types to the
/// same node and direction — the two half-wall slots must not both land.
#[test]
fn enforce_adjacency_limits_counts_the_whole_group() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    let b = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    for i in 0..MAX_SPAN_TYPES.saturating_sub(1) {
        g.create_rel(a, b, format!("T{i}"), BTreeMap::new())
            .unwrap();
    }
    g.commit_write();
    let op = |ty: &str| RedoOp::CreateRel {
        id: 1,
        start: a,
        end: b,
        rel_type: ty.into(),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    };
    // One short of the wall: the first slot's single new type admits.
    g.enforce_adjacency_limits(&[op("slot-a")], &[]).unwrap();
    // The second slot also passes alone — but on top of the accepted
    // first slot it is the 65,536th type, refused.
    let earlier = [vec![op("slot-a")]];
    let earlier_slices: Vec<&[RedoOp]> = earlier.iter().map(|v| v.as_slice()).collect();
    let refusal = g
        .enforce_adjacency_limits(&[op("slot-b")], &earlier_slices)
        .unwrap_err();
    assert!(matches!(
        &refusal,
        Error::Capacity {
            kind: CapacityKind::AdjacencyTypes,
            ..
        }
    ));
}

/// The entry-offset wall (ADR-0599) is the `u32` the arena's entry offsets
/// address: one direction's arena streams plus the store's live charge
/// refuse past it, and the saturating sum never wraps an over-bound store
/// back under.
#[test]
fn sealed_arena_bound_is_the_u32_offset_wall() {
    assert!(!sealed_arena_breaches(u32::MAX as u64, 0));
    assert!(sealed_arena_breaches(u32::MAX as u64, 1));
    assert!(sealed_arena_breaches(u64::MAX, u64::MAX));
}

/// The wholesale build clamps at the tier's field walls instead of dying
/// in the write guard (ADR-0599): a node whose sealable types exceed the
/// span length's `u16` bound rebuilds with its span exactly at the bound
/// and the surplus types still delta — every row answers, nothing panics,
/// and a delete of a surplus type retires its hop without founding past
/// the wall. The build is the one fold path no admission guards, so it is
/// the one that must degrade rather than trust the wall.
#[test]
fn wholesale_build_clamps_at_the_span_length_wall() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    let b = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    // Every type carries RESTORE_SEAL_HOPS hops, so the build's gate takes
    // every one of them and the span wall is what stops it. The last type
    // is one the wall leaves delta; its first rel is the delete victim.
    let types = MAX_SPAN_TYPES + 8;
    let mut victim = RelId::default();
    for i in 0..types {
        for hop in 0..RESTORE_SEAL_HOPS {
            let id = g
                .create_rel(a, b, format!("T{i}"), BTreeMap::new())
                .unwrap();
            if i == types - 1 && hop == 0 {
                victim = id;
            }
        }
    }
    g.commit_write();

    g.rebuild_adj();

    let span = g
        .out_side()
        .sealed()
        .region(a)
        .map(|r| r.type_ids().len())
        .unwrap_or(0);
    assert_eq!(span, MAX_SPAN_TYPES, "the span holds exactly the bound");
    let hops: usize = g.out_side().hop_counts(a).iter().map(|c| c.hops).sum();
    assert_eq!(
        hops,
        types * RESTORE_SEAL_HOPS,
        "every row still answers, sealed or delta"
    );
    // A delete of a surplus type's hop retires it — the version map holds
    // the truth, the tier founds nothing past the wall, nothing panics.
    // The hop prunes at the GC pass (LABS-1116), so it is counted after it.
    g.begin_write().unwrap();
    g.delete_rel(victim);
    g.commit_write();
    g.gc_versions();
    let hops: usize = g.out_side().hop_counts(a).iter().map(|c| c.hops).sum();
    assert_eq!(
        hops,
        types * RESTORE_SEAL_HOPS - 1,
        "the deleted row is gone"
    );
    assert_eq!(
        g.out_side()
            .sealed()
            .region(a)
            .map(|r| r.type_ids().len())
            .unwrap_or(0),
        MAX_SPAN_TYPES,
        "the delete founded no span entry"
    );
    // Nothing folded past a replacement, so compaction has nothing to
    // reclaim and leaves the build's node-major layout untouched.
    let before = g.out_side().sealed().arena_bytes();
    g.out_side().sealed().compact();
    assert_eq!(g.out_side().sealed().arena_bytes(), before);
}
