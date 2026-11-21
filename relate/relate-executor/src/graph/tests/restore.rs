use super::*;
use crate::graph::rebuild::take_adj_builds;
use crate::graph::test_support::{live_rows, node_adjacencies, node_expansions};
use std::sync::Arc;

#[test]
fn direct_restore_rejects_sentinel_before_mutating() {
    let graph = Graph::default();
    assert!(matches!(
        graph.load_checkpoint_node(Node {
            id: i64::MAX,
            labels: Arc::new(vec![]),
            properties: relate_core::PropertyMap::share(BTreeMap::new())
        }),
        Err(relate_core::Error::Storage(_))
    ));
    assert!(matches!(
        graph.load_checkpoint_rel(Relationship {
            id: i64::MAX,
            start: 0,
            end: 0,
            rel_type: "R".into(),
            properties: relate_core::PropertyMap::share(BTreeMap::new())
        }),
        Err(relate_core::Error::Storage(_))
    ));
    assert!(matches!(
        graph.load_checkpoint_rel(Relationship {
            id: 0,
            start: i64::MAX,
            end: 0,
            rel_type: "R".into(),
            properties: relate_core::PropertyMap::share(BTreeMap::new())
        }),
        Err(relate_core::Error::Storage(_))
    ));
    assert_eq!(graph.next_node_id(), 0);
    assert_eq!(graph.next_rel_id(), 0);
    assert_eq!(graph.live_node_count(), 0);
}

/// The restore fixture: two nodes and one `rels`-hop `:T` chunk between
/// them, commit ts set — the rows a checkpoint of the smallest graph with
/// a `rels`-sized chunk holds.
fn restored_graph(rels: RelId) -> Graph {
    let g = Graph::default();
    for id in 0..2 {
        g.load_checkpoint_node(restored_node(id, "N", "n")).unwrap();
    }
    for id in 0..rels {
        let rel = Relationship {
            id,
            ..restored_rel(0, 1, "T", id)
        };
        g.load_checkpoint_rel(rel).unwrap();
    }
    g.set_commit_ts(1);
    g
}

/// LABS-1658: a store reopened from its checkpoint carries the folded
/// chunks its GC pass had folded — the wholesale build emits the durable
/// shape directly (ADR-0608), not a fresh all-delta rebuild. The build's
/// gate is this tier's own 4-hop break-even, below the write path's floor.
#[test]
fn a_restore_builds_the_durable_folded_shape() {
    let floor = crate::adjacency::RESTORE_SEAL_HOPS;
    let restored = restored_graph(floor as RelId);
    restored.finish_restore(relate_engine_api::AdjacencySource::RelRows);
    let type_id = restored.interner.get_id("T").unwrap();

    // Both endpoints carry a floor-sized `:T` chunk here (node 0's out
    // side, node 1's in side), so the build folds both.
    assert_eq!(
        crate::graph::test_support::sealed_chunk_len(&restored.out_side(), 0, type_id),
        floor,
        "the restore emitted the floor chunk folded (LABS-1658)"
    );
    assert_eq!(
        crate::graph::test_support::sealed_chunk_len(&restored.in_side(), 1, type_id),
        floor
    );
    // The fold is a layout, not a fact: the adjacency answers the same.
    assert_eq!(restored.degree_out(0).len(), floor);
    assert_eq!(restored.degree_in(1).len(), floor);

    // The floor gates the emission: the same rows with one hop fewer
    // build nothing folded.
    let below = restored_graph(floor as RelId - 1);
    below.finish_restore(relate_engine_api::AdjacencySource::RelRows);
    assert!(
        below.out_side().sealed().region(0).is_none(),
        "below the floor"
    );
    assert!(
        below.in_side().sealed().region(1).is_none(),
        "below the floor"
    );
}

fn restored_node(id: NodeId, label: &str, name: &str) -> Node {
    Node {
        id,
        labels: Arc::new(vec![label.into()]),
        properties: relate_core::PropertyMap::share(BTreeMap::from([(
            "name".to_string(),
            Value::String(name.into()),
        )])),
    }
}

fn restored_rel(start: NodeId, end: NodeId, rel_type: &str, ts: i64) -> Relationship {
    Relationship {
        id: 0,
        start,
        end,
        rel_type: rel_type.into(),
        properties: relate_core::PropertyMap::share(BTreeMap::from([(
            "ts".to_string(),
            Value::Int(ts),
        )])),
    }
}

/// A multi-element slice install equals the one-element slice install of
/// the same rows: two node part windows and three rel part windows, the
/// shape the Parquet codec hands one part over in batch-sized slices. The
/// batch methods are required, so this also pins the per-row default
/// adapters over a one-element slice.
#[test]
fn multi_element_slice_install_equals_one_element_slice_install() {
    let node_parts: Vec<Vec<Node>> = vec![
        (0..3).map(|id| restored_node(id, "N", "n")).collect(),
        (3..6).map(|id| restored_node(id, "M", "m")).collect(),
    ];
    let rel_parts: Vec<Vec<Relationship>> = vec![
        (0..3)
            .map(|id| Relationship {
                id,
                ..restored_rel(id, (id + 1) % 3, "T", id)
            })
            .collect(),
        vec![
            Relationship {
                id: 3,
                ..restored_rel(0, 3, "U", 3)
            },
            Relationship {
                id: 4,
                ..restored_rel(3, 4, "T", 4)
            },
        ],
        vec![Relationship {
            id: 5,
            ..restored_rel(4, 5, "U", 5)
        }],
    ];

    let bulk = Graph::default();
    for part in &node_parts {
        bulk.load_checkpoint_nodes(part).unwrap();
    }
    for part in &rel_parts {
        bulk.load_checkpoint_rels(part).unwrap();
    }
    let serial = Graph::default();
    for node in node_parts.iter().flatten() {
        serial.load_checkpoint_node(node.clone()).unwrap();
    }
    for rel in rel_parts.iter().flatten() {
        serial.load_checkpoint_rel(rel.clone()).unwrap();
    }
    for g in [&bulk, &serial] {
        g.set_commit_ts(1);
        g.finish_restore(relate_engine_api::AdjacencySource::RelRows);
        g.rebuild_indexes();
    }

    let (bulk_nodes, bulk_rels) = live_rows(&bulk, 1);
    let (serial_nodes, serial_rels) = live_rows(&serial, 1);
    let catalog = |g: &Graph| -> (Vec<String>, Vec<String>) {
        let catalog = g.catalog.lock().unwrap();
        (
            catalog.labels.iter().cloned().collect(),
            catalog.rel_types.iter().cloned().collect(),
        )
    };

    assert_eq!(bulk_nodes, serial_nodes);
    assert_eq!(bulk_rels, serial_rels);
    assert_eq!(node_adjacencies(&bulk), node_adjacencies(&serial));
    assert_eq!(catalog(&bulk), catalog(&serial));
    assert_eq!(bulk.next_node_id(), serial.next_node_id());
    assert_eq!(bulk.next_rel_id(), serial.next_rel_id());
}

/// Overlapping legacy parts (ADR-0183) can restore an id twice; the later
/// row replaces the earlier one, so a delete leaves no copy visible.
#[test]
fn restoring_a_node_twice_keeps_one_version_of_the_later_row() {
    let g = Graph::default();
    g.load_checkpoint_node(restored_node(3, "Old", "first"))
        .unwrap();
    g.load_checkpoint_node(restored_node(3, "New", "second"))
        .unwrap();
    g.set_commit_ts(1);
    g.finish_restore(relate_engine_api::AdjacencySource::RelRows);
    // The open flow's single post-replay site owns the index rebuild — a
    // bare load (this test's shape) supplies it the same way.
    g.rebuild_indexes();
    assert_eq!(g.nodes.get(&3).unwrap().len(), 1);
    let node = g.node(3).unwrap();
    assert_eq!(node.labels.as_slice(), ["New".to_string()].as_slice());
    assert_eq!(
        node.properties,
        restored_node(3, "New", "second").properties
    );
    let no_props = BTreeMap::new();
    assert_eq!(g.lookup_nodes(&["Old".into()], &no_props), Some(vec![]));
    assert_eq!(g.lookup_nodes(&["New".into()], &no_props), Some(vec![3]));

    g.begin_write().unwrap();
    g.delete_node(3, false).unwrap();
    g.commit_write();
    assert!(g.node(3).is_none());
    assert!(g.all_node_ids().is_empty());
}

#[test]
fn restoring_a_rel_twice_keeps_one_version_and_one_hop_per_endpoint() {
    use relate_ir::Direction;

    let g = Graph::default();
    for id in 0..4 {
        g.load_checkpoint_node(restored_node(id, "N", "n")).unwrap();
    }
    // Same endpoints, but a different `ts` sorts the hop elsewhere.
    g.load_checkpoint_rel(restored_rel(0, 1, "R", 1)).unwrap();
    g.load_checkpoint_rel(restored_rel(0, 1, "R", 2)).unwrap();
    assert_eq!(g.rels.get(&0).unwrap().len(), 1);
    // Restore leaves adjacency to `finish_restore`.
    assert!(g.degree_out(0).is_empty());
    // Other endpoints and type: the later row wins entirely.
    g.load_checkpoint_rel(restored_rel(2, 3, "S", 3)).unwrap();
    g.set_commit_ts(1);
    g.finish_restore(relate_engine_api::AdjacencySource::RelRows);
    assert_eq!(g.rels.get(&0).unwrap().len(), 1);
    let rel = g.rel(0).unwrap();
    assert_eq!((rel.start, rel.end, rel.rel_type.as_str()), (2, 3, "S"));
    assert_eq!(rel.properties, restored_rel(2, 3, "S", 3).properties);
    assert!(g.degree_out(0).is_empty());
    assert!(g.degree_in(1).is_empty());
    assert_eq!(g.degree_out(2), [0]);
    assert_eq!(g.degree_in(3), [0]);
    assert!(g.neighbors(0, Direction::Both).is_empty());
    assert_eq!(g.neighbors(2, Direction::Both), [0]);

    g.begin_write().unwrap();
    g.delete_rel(0);
    g.commit_write();
    assert!(g.rel(0).is_none());
    assert!(g.all_rel_ids().is_empty());
    assert!(g.neighbors(2, Direction::Both).is_empty());
    assert!(g.neighbors(3, Direction::Both).is_empty());
}

/// Rows restored out of id order still give each node the adjacency of an
/// id-ordered load: type chunks in first-id order, hops `ts` DESC.
#[test]
fn restored_adjacency_follows_id_order_not_load_order() {
    let g = Graph::default();
    for id in 0..2 {
        g.load_checkpoint_node(restored_node(id, "N", "n")).unwrap();
    }
    for (id, rel_type, ts) in [(3, "B", 9), (1, "A", 5), (2, "B", 1), (0, "A", 7)] {
        g.load_checkpoint_rel(Relationship {
            id,
            ..restored_rel(0, 1, rel_type, ts)
        })
        .unwrap();
    }
    g.set_commit_ts(1);
    g.finish_restore(relate_engine_api::AdjacencySource::RelRows);
    assert_eq!(g.degree_out(0), [0, 1, 3, 2]);
    assert_eq!(g.degree_in(1), [0, 1, 3, 2]);
}

#[test]
fn restore_during_an_open_write_is_a_storage_error() {
    let g = Graph::default();
    g.load_checkpoint_node(restored_node(0, "N", "n")).unwrap();
    g.set_commit_ts(1);
    g.finish_restore(relate_engine_api::AdjacencySource::RelRows);
    g.begin_write().unwrap();
    let written = g.create_node(vec!["W".into()], BTreeMap::new()).unwrap();

    for result in [
        g.load_checkpoint_node(restored_node(written, "Other", "x")),
        g.load_checkpoint_node(restored_node(7, "Other", "x")),
        g.load_checkpoint_rel(restored_rel(0, written, "R", 1)),
    ] {
        assert!(
            matches!(result, Err(relate_core::Error::Storage(ref message)) if message.contains("write is open")),
            "{result:?}"
        );
    }
    assert_eq!(g.next_node_id(), written + 1);
    assert_eq!(g.next_rel_id(), 0);
    assert!(g.rels.is_empty() && !g.nodes.contains_key(&7));
    assert_eq!(g.nodes.get(&written).unwrap().len(), 1);
    assert_eq!(
        g.node(written).unwrap().labels.as_slice(),
        ["W".to_string()].as_slice()
    );
    g.commit_write();
    assert_eq!(g.all_node_ids(), [0, written]);
}

/// The refusal keys on the restore phase, not on a slot's chain shape: a
/// committed single-version write leaves the `Inline` chain a restored row
/// holds, and the load is still refused, the written row untouched.
#[test]
fn restore_over_a_committed_single_version_write_is_refused() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let written = g.create_node(vec!["W".into()], BTreeMap::new()).unwrap();
    g.commit_write();
    assert!(g.nodes.get(&written).unwrap().is_inline());

    assert!(matches!(
        g.load_checkpoint_node(restored_node(written, "N", "restored")),
        Err(relate_core::Error::Storage(ref message))
            if message == "checkpoint restore into a graph with committed writes"
    ));
    assert_eq!(g.nodes.get(&written).unwrap().len(), 1);
    assert_eq!(
        g.node(written).unwrap().labels.as_slice(),
        ["W".to_string()].as_slice()
    );
}

#[test]
fn restore_over_a_committed_write_is_a_storage_error() {
    let g = Graph::default();
    g.load_checkpoint_node(restored_node(0, "N", "first"))
        .unwrap();
    g.set_commit_ts(1);
    g.finish_restore(relate_engine_api::AdjacencySource::RelRows);
    g.begin_write().unwrap();
    g.set_property_node(0, "name", Value::String("second".into()));
    g.commit_write();
    assert_eq!(g.nodes.get(&0).unwrap().len(), 2);

    assert!(matches!(
        g.load_checkpoint_node(restored_node(0, "N", "third")),
        Err(relate_core::Error::Storage(_))
    ));
    assert_eq!(g.nodes.get(&0).unwrap().len(), 2);
    assert_eq!(
        g.node(0).unwrap().properties,
        restored_node(0, "N", "second").properties
    );
}

fn create_rel(id: RelId, start: NodeId, end: NodeId, rel_type: &str) -> RedoOp {
    RedoOp::CreateRel {
        id,
        start,
        end,
        rel_type: rel_type.into(),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    }
}

/// The calls a store open makes — restore the checkpoint rows,
/// `finish_restore`, replay the WAL tail, `rebuild_indexes` — build
/// adjacency once, and the opened graph has the writer's adjacency.
#[test]
fn checkpoint_load_and_wal_replay_build_adjacency_once() {
    let checkpointed = vec![
        RedoOp::CreateNode {
            id: 0,
            labels: Arc::new(vec!["N".into()]),
            properties: relate_core::PropertyMap::share(BTreeMap::new()),
        },
        RedoOp::CreateNode {
            id: 1,
            labels: Arc::new(vec!["N".into()]),
            properties: relate_core::PropertyMap::share(BTreeMap::new()),
        },
        RedoOp::CreateNode {
            id: 2,
            labels: Arc::new(vec![]),
            properties: relate_core::PropertyMap::share(BTreeMap::new()),
        },
        create_rel(0, 0, 1, "A"),
        create_rel(1, 0, 2, "B"),
        create_rel(2, 1, 2, "A"),
    ];
    let tail = vec![
        vec![create_rel(3, 0, 2, "A"), RedoOp::DeleteRel { id: 0 }],
        vec![
            create_rel(4, 2, 0, "B"),
            RedoOp::SetRelProp {
                id: 2,
                key: "ts".into(),
                value: Value::Int(5),
            },
        ],
        vec![RedoOp::DeleteRel { id: 4 }],
    ];

    // The writer commits the checkpointed rows, then the tail.
    let writer = Graph::default();
    writer.apply_redo(checkpointed, 1).unwrap();
    let (nodes, rels) = live_rows(&writer, 1);
    for (ops, ts) in tail.iter().cloned().zip(2..) {
        writer.apply_redo(ops, ts).unwrap();
    }

    take_adj_builds();
    let opened = Graph::default();
    opened.set_commit_ts(1);
    for node in nodes {
        opened.load_checkpoint_node(node).unwrap();
    }
    for rel in rels {
        opened.load_checkpoint_rel(rel).unwrap();
    }
    opened.finish_restore(relate_engine_api::AdjacencySource::RelRows);
    for (ops, ts) in tail.into_iter().zip(2..) {
        opened.apply_replayed_redo(ops, ts).unwrap();
    }
    opened.rebuild_indexes();
    assert_eq!(take_adj_builds(), 1);

    assert_eq!(opened.commit_ts(), writer.commit_ts());
    assert_eq!(node_adjacencies(&opened), node_adjacencies(&writer));
    // Relationships the tail deleted expand nowhere.
    for adj in node_adjacencies(&opened) {
        for rels in adj.expands {
            assert!(
                !rels.contains(&0) && !rels.contains(&4),
                "{}: {rels:?}",
                adj.id
            );
        }
    }
}

/// The checkpoint dumps node and rel rows and folds its adjacency afterwards
/// (ADR-0551): restoring those rows rebuilds adjacency, and the build emits
/// the durable shape — the fold-sized chunks folded (ADR-0608), no sealed
/// bytes or tombstones serialized — so a WAL tail replayed onto it leaves
/// the reader's adjacency equal to the writer's, replayed deletes recording
/// their tombstones against the folded chunks exactly as the writer's did.
#[test]
fn compaction_restore_roundtrip_rebuilds_the_durable_shape() {
    let floor = crate::adjacency::COMPACTION_DELTA_FLOOR;
    let mut checkpointed: Vec<RedoOp> = (0..2)
        .map(|id| RedoOp::CreateNode {
            id,
            labels: Arc::new(vec!["N".into()]),
            properties: relate_core::PropertyMap::share(BTreeMap::new()),
        })
        .collect();
    checkpointed.extend((0..floor as RelId).map(|id| create_rel(id, 0, 1, "T")));

    let writer = Graph::default();
    writer.apply_redo(checkpointed, 1).unwrap();
    let (nodes, rels) = live_rows(&writer, 1);
    let serialized = |g: &Graph| -> u64 {
        let nodes: u64 = g
            .all_node_ids_at(1)
            .into_iter()
            .map(|id| {
                g.node_checkpoint_bytes(id, 1)
                    .unwrap()
                    .expect("a live node at ts has checkpoint bytes")
            })
            .sum();
        let rels: u64 = g
            .all_rel_ids_at(1)
            .into_iter()
            .map(|id| {
                g.rel_checkpoint_bytes(id, 1)
                    .unwrap()
                    .expect("a live rel at ts has checkpoint bytes")
            })
            .sum();
        nodes + rels
    };
    let dumped = serialized(&writer);

    // The checkpoint's GC pass folds the checkpointed delta; the rows it
    // dumped carry none of it.
    writer.gc_versions();
    let type_id = writer.interner.get_id("T").unwrap();
    assert_eq!(
        crate::graph::test_support::sealed_chunk_len(&writer.out_side(), 0, type_id),
        floor
    );
    assert_eq!(
        serialized(&writer),
        dumped,
        "the folded layout adds no checkpoint bytes"
    );

    // The WAL tail: a new type, a delete of a sealed hop, and a ts update of
    // a sealed hop.
    let tail = [
        (
            2,
            vec![
                create_rel(floor as RelId, 0, 1, "U"),
                RedoOp::DeleteRel { id: 5 },
            ],
        ),
        (
            3,
            vec![RedoOp::SetRelProp {
                id: 7,
                key: "ts".into(),
                value: Value::Int(100),
            }],
        ),
    ];
    for (ts, ops) in tail.iter().cloned() {
        writer.apply_redo(ops, ts).unwrap();
    }

    let opened = Graph::default();
    opened.set_commit_ts(1);
    for node in nodes {
        opened.load_checkpoint_node(node).unwrap();
    }
    for rel in rels {
        opened.load_checkpoint_rel(rel).unwrap();
    }
    opened.finish_restore(relate_engine_api::AdjacencySource::RelRows);
    for (ts, ops) in tail {
        opened.apply_replayed_redo(ops, ts).unwrap();
    }
    opened.rebuild_indexes();

    assert_eq!(opened.commit_ts(), writer.commit_ts());
    // The restore rebuilt the durable shape (ADR-0608): the fold-sized
    // chunks folded, as the writer's checkpoint GC left them — the
    // replayed delete then records its tombstone against the folded chunk
    // the way it did on the writer.
    let type_id = opened.interner.get_id("T").unwrap();
    assert_eq!(
        crate::graph::test_support::sealed_chunk_len(&opened.out_side(), 0, type_id),
        floor,
        "the restore folded the checkpointed chunk"
    );
    assert_eq!(node_adjacencies(&opened), node_adjacencies(&writer));

    // The dumped rows are the whole durable state: the next checkpoint's
    // fold reclaims the replayed delete's tombstone, so the sealed stream
    // loses its hop — the writer still holds it only because its GC ran
    // before the tail.
    opened.gc_versions();
    assert_eq!(
        crate::graph::test_support::sealed_chunk_len(&opened.out_side(), 0, type_id),
        floor - 1,
        "the restored fold reclaims, without the deleted hop"
    );
    assert_eq!(node_expansions(&opened), node_expansions(&writer));
}

/// The wholesale build's emission (ADR-0599 / ADR-0608): a restored store's
/// chunks at or above [`crate::adjacency::RESTORE_SEAL_HOPS`] seal in the
/// build pass itself, those below stay delta — the measured break-even, not
/// the write path's 32-hop floor — and both directions fold.
#[test]
fn restore_seals_multi_hop_chunks_and_leaves_short_ones_delta() {
    use crate::adjacency::RESTORE_SEAL_HOPS;

    // Node 0 carries two out types: one at and above the threshold, one
    // below. Their far endpoints (1 and 2) get the matching incoming chunks.
    let long = RESTORE_SEAL_HOPS as RelId + 1;
    let short = RESTORE_SEAL_HOPS as RelId - 1;
    let mut checkpointed: Vec<RedoOp> = (0..3)
        .map(|id| RedoOp::CreateNode {
            id,
            labels: Arc::new(vec!["N".into()]),
            properties: relate_core::PropertyMap::share(BTreeMap::new()),
        })
        .collect();
    checkpointed.extend((0..long).map(|id| create_rel(id, 0, 1, "LONG")));
    checkpointed.extend((long..long + short).map(|id| create_rel(id, 0, 2, "SHORT")));

    let writer = Graph::default();
    writer.apply_redo(checkpointed, 1).unwrap();
    let (nodes, rels) = live_rows(&writer, 1);

    // The open sequence: restore the rows, then the completion steps. The
    // build inside `finish_restore` is the emission; no pass follows it.
    let opened = Graph::default();
    opened.set_commit_ts(1);
    for node in nodes {
        opened.load_checkpoint_node(node).unwrap();
    }
    for rel in rels {
        opened.load_checkpoint_rel(rel).unwrap();
    }
    opened.finish_restore(relate_engine_api::AdjacencySource::RelRows);
    opened.rebuild_indexes();

    // The build ends with both arenas at exact size (ADR-0599): every
    // buffer at the capacity its floor charges, the byte buffer at its
    // encoded length. No entry carries a tombstone at a fresh restore.
    assert_eq!(opened.out_side().sealed().assert_fit(), 0);
    assert_eq!(opened.in_side().sealed().assert_fit(), 0);

    // (delta hops, sealed hops) of `node`'s `rel_type` chunk in one direction.
    let lens = |node: NodeId, rel_type: &str, out: bool| -> (usize, usize) {
        let type_id = opened.interner.get_id(rel_type).unwrap();
        let side = if out {
            opened.out_side()
        } else {
            opened.in_side()
        };
        side.delta()
            .get(&node)
            .unwrap()
            .chunk_lens(side.sealed().region(node))
            .find(|(tid, _, _)| *tid == type_id)
            .map(|(_, delta, sealed)| (delta, sealed))
            .unwrap()
    };

    // The long type seals on both sides: no delta left, its hops in the stream.
    assert_eq!(lens(0, "LONG", true), (0, long as usize));
    assert_eq!(lens(1, "LONG", false), (0, long as usize));
    // The short type stays delta: nothing sealed, its hops untouched.
    assert_eq!(lens(0, "SHORT", true), (short as usize, 0));
    assert_eq!(lens(2, "SHORT", false), (short as usize, 0));
}
