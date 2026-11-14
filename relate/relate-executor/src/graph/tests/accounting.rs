//! Byte-accounting and admission-estimate tests: the incremental charge
//! against the live walk, and the redo-delta estimate against the apply.

use super::*;
use crate::graph::accounting::AdjDelta;

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
    let estimate = g.estimate_redo_delta(&same_batch);
    g.apply_redo(same_batch, g.commit_ts() + 1).unwrap();
    assert_eq!(g.used_bytes(), g.accounted_bytes());
    assert_eq!(g.used_bytes() as i64 - before as i64, estimate);

    let before = g.used_bytes();
    let set_again = vec![RedoOp::SetRelProp {
        id: 1,
        key: "note".into(),
        value: Value::String("x".into()),
    }];
    let estimate = g.estimate_redo_delta(&set_again);
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
    let estimate = g.estimate_redo_delta(&created);
    g.apply_redo(created, g.commit_ts() + 1).unwrap();
    assert_eq!(g.used_bytes(), g.accounted_bytes());
    assert_eq!(g.used_bytes() as i64 - before as i64, estimate);
    assert_eq!(g.rel(1).unwrap().properties.len(), 4);
    assert_eq!(g.rel(2).unwrap().properties.len(), 3);
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

    // The founding region term derived from the layout primitives, not the
    // helper it checks: one chunks-vec entry from empty, the empty encoded
    // stream, one tombstone slot.
    let entry = std::mem::size_of::<(u32, crate::adjacency::SealedChunk)>() as u64;
    let tombstone = crate::adjacency::tombstone_stride();
    let empty_stream = crate::adjacency::encode_chunk(&[]).len();
    let region = (crate::memory::vec_heap(1, entry)
        + crate::memory::vec_heap(empty_stream, 1)
        + crate::memory::vec_heap(1, tombstone)) as i64;
    let AdjDelta {
        refusable: loop_refusable,
        spill: self_loop,
    } = g.estimate_adj_delta(&[RedoOp::DeleteRel { id: loop_id }]);
    let AdjDelta {
        refusable: pair_refusable,
        spill: pair,
    } = g.estimate_adj_delta(&[RedoOp::DeleteRel { id: pair_id }]);
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
    let estimate = g.estimate_redo_delta(&rel);
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
    let estimate = g.estimate_redo_delta(&node);
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
    let estimate = g.estimate_redo_delta(&ops);
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

    let ring = g.out_adj.get(&ring_a).unwrap();
    assert_eq!(ring.chunks.len(), 1);
    assert_eq!(ring.chunks.capacity(), crate::memory::reserved_len(1));
    assert_eq!(ring.chunks[0].1.len(), 1);
    assert_eq!(ring.chunks[0].1.capacity(), crate::memory::reserved_len(1));

    let dense = g.out_adj.get(&hub).unwrap();
    assert_eq!(dense.chunks.len(), 1);
    assert_eq!(dense.chunks.capacity(), crate::memory::reserved_len(1));
    assert_eq!(dense.chunks[0].1.len(), 20);
    assert_eq!(
        dense.chunks[0].1.capacity(),
        crate::memory::reserved_len(20)
    );
}
