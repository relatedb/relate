use super::*;
use relate_names::SmallName;
use relate_planner::DegreeMoments;

mod accounting;
mod adjacency;
mod adjacency_model_proptest;
mod chain;
mod correctness;
mod label_set_counts;
mod pattern_stats;
mod point;
mod poisoned;
mod restore;
mod sealed_arena;
mod services;
mod spatial_profile;

#[test]
fn schema_capture_never_waits_for_theater_while_holding_catalog() {
    use std::sync::{mpsc, Arc, Barrier};
    use std::time::Duration;

    let graph = Arc::new(Graph::default());
    // Repeatedly force capture to wait for theater while another thread
    // acquires catalog. Even on failure, release theater before joining:
    // the regression is reported as a timeout, not a hung test process.
    for _ in 0..32 {
        let theater = graph.theater.lock().unwrap();
        let arrived = Arc::new(Barrier::new(2));
        let capture = {
            let graph = Arc::clone(&graph);
            let arrived = Arc::clone(&arrived);
            std::thread::spawn(move || {
                graph.schema_snapshot_with_hook(|| {
                    arrived.wait();
                });
            })
        };
        arrived.wait();
        let (tx, rx) = mpsc::channel();
        let catalog = {
            let graph = Arc::clone(&graph);
            std::thread::spawn(move || tx.send(graph.named_constraints()).unwrap())
        };
        let progressed = rx.recv_timeout(Duration::from_secs(2)).is_ok();
        drop(theater);
        capture.join().unwrap();
        catalog.join().unwrap();
        assert!(
            progressed,
            "schema capture held catalog while waiting for theater"
        );
    }
}

#[test]
fn write_timestamp_sentinel_rejected_before_changing_graph_state() {
    let graph = Graph::default();
    graph.begin_write().unwrap();
    graph
        .create_node(vec!["Kept".into()], BTreeMap::new())
        .unwrap();
    let pending_ts = graph.pending_commit_ts();
    let stats = graph.stats.lock().unwrap().nodes_created;
    let redo = graph.pending_redo.lock().unwrap().clone();
    graph.set_commit_ts(u64::MAX - 1);

    for err in [
        graph.begin_write().unwrap_err(),
        graph.begin_write_at(u64::MAX).unwrap_err(),
    ] {
        assert!(
            matches!(err, relate_core::Error::Execution(ref message) if message == "write timestamp exhausted")
        );
        assert!(graph.in_write_window());
        assert_eq!(graph.pending_commit_ts(), pending_ts);
        assert_eq!(graph.stats.lock().unwrap().nodes_created, stats);
        assert_eq!(*graph.pending_redo.lock().unwrap(), redo);
        assert!(graph.record_redo.load(Ordering::Acquire));
        assert_eq!(graph.commit_ts(), u64::MAX - 1);
    }

    let fresh = Graph::default();
    fresh.begin_write_at(u64::MAX - 1).unwrap();
    assert_eq!(fresh.pending_commit_ts(), u64::MAX - 1);
    fresh.commit_write();
    assert_eq!(fresh.commit_ts(), u64::MAX - 1);
    assert!(matches!(
        fresh.begin_write(),
        Err(relate_core::Error::Execution(_))
    ));
    assert!(!fresh.in_write_window());
}

#[test]
fn apply_redo_rejects_max_timestamp_before_installing_any_ops() {
    let graph = Graph::default();
    let ops = vec![RedoOp::CreateNode {
        id: 0,
        labels: Arc::new(vec!["Kept".into()]),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    }];
    assert!(matches!(
        graph.apply_redo(ops.clone(), u64::MAX),
        Err(relate_core::Error::Storage(_))
    ));
    assert!(!graph.in_write_window());
    assert_eq!(graph.pending_commit_ts(), 0);
    assert_eq!(graph.commit_ts(), 0);
    assert_eq!(graph.next_node_id(), 0);
    assert!(graph.all_node_ids().is_empty());
    assert!(graph.pending_redo.lock().unwrap().is_empty());

    graph.apply_redo(ops, u64::MAX - 1).unwrap();
    assert_eq!(graph.commit_ts(), u64::MAX - 1);
    assert_eq!(graph.all_node_ids(), vec![0]);
}

#[test]
fn redo_preflight_rejects_zero_and_nonforward_timestamps_without_a_prefix() {
    let node = |id| RedoOp::CreateNode {
        id,
        labels: Arc::new(vec!["Kept".into()]),
        properties: relate_core::PropertyMap::share(BTreeMap::from([(
            "value".into(),
            Value::Int(1),
        )])),
    };
    let rel = |id, end| RedoOp::CreateRel {
        id,
        start: 0,
        end,
        rel_type: "R".into(),
        properties: relate_core::PropertyMap::share(BTreeMap::from([(
            "value".into(),
            Value::Int(2),
        )])),
    };
    let versions = |g: &Graph| {
        let mut nodes = BTreeMap::new();
        g.nodes.for_each(|id, vers| {
            nodes.insert(
                id,
                vers.iter()
                    .map(|v| (v.xmin, v.xmax, v.live.to_node(id, &g.interner)))
                    .collect::<Vec<_>>(),
            );
        });
        let mut rels = BTreeMap::new();
        g.rels.for_each(|id, vers| {
            rels.insert(
                id,
                vers.iter()
                    .map(|v| (v.xmin, v.xmax, v.live.to_rel(id, &g.interner)))
                    .collect::<Vec<_>>(),
            );
        });
        (nodes, rels)
    };
    // Reproductions: (published ts, next node ID, proposed ts).
    for (published, next_id, proposed) in [(0, 0, 0), (2, 1, 1), (2, 1, 2)] {
        let g = Graph::default();
        if published != 0 {
            g.apply_redo(vec![node(0), rel(0, 0)], published).unwrap();
        }
        assert_eq!(g.next_node_id(), next_id);
        let before_nodes = g.all_node_ids();
        let before_rels = g.all_rel_ids();
        let before_versions = versions(&g);
        let ops = vec![
            node(next_id),
            rel(next_id, next_id),
            RedoOp::SetNodeProp {
                id: 0,
                key: "value".into(),
                value: Value::Int(3),
            },
            RedoOp::SetRelProp {
                id: 0,
                key: "value".into(),
                value: Value::Int(4),
            },
        ];
        assert!(matches!(
            g.validate_redo_applicability(&ops, proposed),
            Err(relate_core::Error::Storage(_))
        ));
        assert!(matches!(
            g.apply_redo(ops, proposed),
            Err(relate_core::Error::Storage(_))
        ));
        assert_eq!(g.next_node_id(), next_id);
        assert_eq!(g.next_rel_id(), next_id);
        assert_eq!(g.commit_ts(), published);
        assert_eq!(g.pending_commit_ts(), published);
        assert_eq!(g.all_node_ids(), before_nodes);
        assert_eq!(g.all_rel_ids(), before_rels);
        // Compare every version, including invisible ones and the xmax of
        // existing IDs; visible rows and map sizes alone miss both.
        assert_eq!(versions(&g), before_versions);
        g.nodes
            .for_each(|_, vers| assert!(vers.iter().all(|v| v.xmin != 0)));
        g.rels
            .for_each(|_, vers| assert!(vers.iter().all(|v| v.xmin != 0)));
        assert!(!g.in_write_window());
    }
}

#[test]
fn redo_preflight_accepts_same_record_node_and_relationship_at_advancing_ts() {
    for prior in [0, 2] {
        let g = Graph::default();
        if prior != 0 {
            g.apply_redo(vec![], prior).unwrap();
        }
        let ops = vec![
            RedoOp::CreateNode {
                id: 0,
                labels: Arc::new(vec!["N".into()]),
                properties: relate_core::PropertyMap::share(BTreeMap::new()),
            },
            RedoOp::CreateRel {
                id: 0,
                start: 0,
                end: 0,
                rel_type: "R".into(),
                properties: relate_core::PropertyMap::share(BTreeMap::new()),
            },
        ];
        g.validate_redo_applicability(&ops, prior + 1).unwrap();
        g.apply_redo(ops, prior + 1).unwrap();
        assert_eq!(g.commit_ts(), prior + 1);
        assert_eq!(g.all_node_ids(), vec![0]);
        assert_eq!(g.all_rel_ids(), vec![0]);
    }
}

#[test]
fn apply_redo_rejects_max_ids_before_installing_any_ops() {
    for ops in [
        vec![RedoOp::CreateNode {
            id: i64::MAX,
            labels: Arc::new(vec![]),
            properties: relate_core::PropertyMap::share(BTreeMap::new()),
        }],
        vec![
            RedoOp::CreateNode {
                id: 0,
                labels: Arc::new(vec![]),
                properties: relate_core::PropertyMap::share(BTreeMap::new()),
            },
            RedoOp::CreateRel {
                id: i64::MAX,
                start: 0,
                end: 0,
                rel_type: "R".into(),
                properties: relate_core::PropertyMap::share(BTreeMap::new()),
            },
        ],
        vec![
            RedoOp::CreateNode {
                id: 0,
                labels: Arc::new(vec![]),
                properties: relate_core::PropertyMap::share(BTreeMap::new()),
            },
            RedoOp::CreateRel {
                id: 0,
                start: 0,
                end: i64::MAX,
                rel_type: "R".into(),
                properties: relate_core::PropertyMap::share(BTreeMap::new()),
            },
        ],
    ] {
        let graph = Graph::default();
        assert!(matches!(
            graph.apply_redo(ops, 1),
            Err(relate_core::Error::Storage(_))
        ));
        assert_eq!(graph.next_node_id(), 0);
        assert_eq!(graph.next_rel_id(), 0);
        assert_eq!(graph.commit_ts(), 0);
    }
}

#[test]
fn redo_preflight_rejects_later_endpoint_without_installing_prefix() {
    let node = |id| RedoOp::CreateNode {
        id,
        labels: Arc::new(vec!["Fresh".into()]),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    };
    let rel = |id, start, end| RedoOp::CreateRel {
        id,
        start,
        end,
        rel_type: "R".into(),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    };
    for suffix in [
        vec![rel(2, 1, 99)],
        vec![RedoOp::DeleteNode { id: 1 }, rel(2, 1, 0)],
        vec![rel(1, 1, 0), RedoOp::DeleteNode { id: 1 }, rel(2, 0, 1)],
    ] {
        let g = Graph::default();
        g.apply_redo(vec![node(0)], 1).unwrap();
        let ops = std::iter::once(node(1)).chain(suffix).collect::<Vec<_>>();
        assert!(matches!(
            g.validate_redo_applicability(&ops, 2),
            Err(relate_core::Error::Semantic(_))
        ));
        assert!(matches!(
            g.apply_redo(ops, 2),
            Err(relate_core::Error::Semantic(_))
        ));
        assert_eq!(g.commit_ts(), 1);
        assert_eq!(g.next_node_id(), 1);
        assert_eq!(g.next_rel_id(), 0);
        assert_eq!(g.all_node_ids(), vec![0]);
        assert!(g.all_rel_ids().is_empty());
        assert!(!g.in_write_window());
        assert_eq!(g.pending_commit_ts(), 1);
    }
}

#[test]
fn direct_redo_rejects_incomplete_label_metadata_without_a_prefix() {
    let graph = Graph::default();
    let ops = vec![
        RedoOp::CreateNode {
            id: 0,
            labels: Arc::new(vec!["Person".into()]),
            properties: relate_core::PropertyMap::share(BTreeMap::new()),
        },
        RedoOp::create_constraint(ConstraintDef {
            name: "rule".into(),
            relationship: false,
            label: "Person".into(),
            properties: vec![],
            kind: "nodeLabelExistence".into(),
            property_type: None,
            enforced_label: None,
        }),
    ];
    assert!(matches!(
        graph.apply_redo(ops, 1),
        Err(relate_core::Error::Storage(_))
    ));
    assert_eq!(graph.commit_ts(), 0);
    assert_eq!(graph.next_node_id(), 0);
    assert!(graph.named_constraints().is_empty());
    assert!(!graph.in_write_window());
}

#[test]
fn redo_preflight_catalog_duplicates_and_bad_options_are_atomic() {
    let node = RedoOp::CreateNode {
        id: 1,
        labels: Arc::new(vec!["Fresh".into()]),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    };
    let index = |name: &str, property: &str| RedoOp::CreateIndex {
        name: name.into(),
        label: "L".into(),
        property: property.into(),
    };
    let constraint = |name: &str, kind: &str| {
        RedoOp::create_constraint(ConstraintDef {
            name: name.into(),
            relationship: false,
            label: "L".into(),
            properties: vec!["p".into()],
            kind: kind.into(),
            property_type: None,
            enforced_label: None,
        })
    };
    let invalid = [
        vec![index("first", "p"), index("second", "p")],
        vec![index("first", "p"), index("first", "q")],
        vec![
            constraint("first", "UNIQUE"),
            constraint("second", "UNIQUE"),
        ],
        vec![constraint("first", "UNIQUE"), constraint("first", "EXISTS")],
        vec![RedoOp::CatalogIndexDetail {
            name: "bad".into(),
            detail: vector_index_detail(),
            options: serde_json::json!({"config": {"untagged": true}}),
        }],
    ];
    for suffix in invalid {
        let g = Graph::default();
        g.apply_redo(
            vec![RedoOp::CreateNode {
                id: 0,
                labels: Arc::new(vec!["Kept".into()]),
                properties: relate_core::PropertyMap::share(BTreeMap::from([(
                    "p".into(),
                    Value::Int(7),
                )])),
            }],
            1,
        )
        .unwrap();
        let ops = std::iter::once(node.clone())
            .chain(suffix)
            .collect::<Vec<_>>();
        let err = g.validate_redo_applicability(&ops, 2).unwrap_err();
        assert!(
            matches!(
                err,
                relate_core::Error::Semantic(_) | relate_core::Error::Storage(_)
            ),
            "{err}"
        );
        assert!(g.apply_redo(ops, 2).is_err());
        assert_eq!(g.commit_ts(), 1);
        assert_eq!((g.next_node_id(), g.next_rel_id()), (1, 0));
        assert_eq!(g.all_node_ids(), vec![0]);
        assert_eq!(g.node(0).unwrap().properties["p"], Value::Int(7));
        assert!(g.named_indexes().is_empty());
        assert!(g.named_constraints().is_empty());
        assert!(g.theater.lock().unwrap().index_details.is_empty());
        assert!(!g.in_write_window());
    }
}

#[test]
fn redo_preflight_invalid_pull_local_id_keeps_prior_rows() {
    for bad in [-1, i64::MAX] {
        let g = Graph::default();
        let ops = vec![
            RedoOp::CreateNode {
                id: 0,
                labels: Arc::new(vec![]),
                properties: relate_core::PropertyMap::share(BTreeMap::new()),
            },
            RedoOp::PullIdentityPut {
                source: "src".into(),
                kind: PullKind::Node,
                remote_id: "remote".into(),
                local_id: bad,
            },
        ];
        assert!(matches!(
            g.validate_redo_applicability(&ops, 1),
            Err(relate_core::Error::Storage(_))
        ));
        assert!(matches!(
            g.apply_redo(ops, 1),
            Err(relate_core::Error::Storage(_))
        ));
        assert_eq!(g.commit_ts(), 0);
        assert_eq!(g.next_node_id(), 0);
        assert!(g.all_node_ids().is_empty());
        assert_eq!(g.pull_identity_get("src", PullKind::Node, "remote"), None);
    }
}

#[test]
fn redo_preflight_accepts_ordered_detach_schema_and_pull_updates() {
    let g = Graph::default();
    let node = |id| RedoOp::create_node(id, Vec::new(), BTreeMap::new());
    let rel = |id| RedoOp::create_rel(id, 0, 1, "R".into(), BTreeMap::new());
    let index = |name: &str| RedoOp::CreateIndex {
        name: name.into(),
        label: "L".into(),
        property: "p".into(),
    };
    let constraint = |name: &str| {
        RedoOp::create_constraint(ConstraintDef {
            name: name.into(),
            relationship: false,
            label: "L".into(),
            properties: vec!["p".into()],
            kind: relate_model::ConstraintType::Unique.as_str().into(),
            property_type: None,
            enforced_label: None,
        })
    };
    let ops = vec![
        node(0),
        node(1),
        rel(0),
        RedoOp::DeleteNode { id: 1 },
        node(1),
        rel(1),
        index("i"),
        index("i"),
        RedoOp::DropIndex { name: "i".into() },
        index("j"),
        constraint("c"),
        constraint("c"),
        RedoOp::DropConstraint { name: "c".into() },
        constraint("d"),
        RedoOp::PullIdentityPut {
            source: "src".into(),
            kind: PullKind::Node,
            remote_id: "remote".into(),
            local_id: 1,
        },
    ];
    g.validate_redo_applicability(&ops, 1).unwrap();
    g.apply_redo(ops, 1).unwrap();
    assert_eq!(g.commit_ts(), 1);
    assert_eq!(g.all_node_ids(), vec![0, 1]);
    assert_eq!(g.all_rel_ids(), vec![1]);
    assert_eq!(
        g.pull_identity_get("src", PullKind::Node, "remote"),
        Some(1)
    );
    assert_eq!(g.named_indexes()[0].name, "j");
    assert_eq!(g.named_constraints()[0].name, "d");
}

#[test]
fn redo_group_preflight_tracks_prior_records_without_applying_them() {
    let g = Graph::default();
    let first = [
        RedoOp::CreateNode {
            id: 0,
            labels: Arc::new(vec![]),
            properties: relate_core::PropertyMap::share(BTreeMap::new()),
        },
        RedoOp::CreateIndex {
            name: "i".into(),
            label: "L".into(),
            property: "p".into(),
        },
    ];
    let second = [
        RedoOp::CreateRel {
            id: 0,
            start: 0,
            end: 0,
            rel_type: "R".into(),
            properties: relate_core::PropertyMap::share(BTreeMap::new()),
        },
        RedoOp::PullIdentityPut {
            source: "src".into(),
            kind: PullKind::Rel,
            remote_id: "remote".into(),
            local_id: 0,
        },
    ];
    g.validate_redo_group_applicability([(&first[..], 1), (&second[..], 2)])
        .unwrap();
    for timestamps in [[0, 1], [1, 1], [2, 1], [1, u64::MAX]] {
        assert!(matches!(
            g.validate_redo_group_applicability([
                (&first[..], timestamps[0]),
                (&second[..], timestamps[1])
            ]),
            Err(relate_core::Error::Storage(_))
        ));
    }
    let bad = [RedoOp::CreateIndex {
        name: "other".into(),
        label: "L".into(),
        property: "p".into(),
    }];
    assert!(matches!(
        g.validate_redo_group_applicability([(&first[..], 1), (&bad[..], 2)]),
        Err(relate_core::Error::Semantic(_))
    ));
    let deleted = [RedoOp::DeleteNode { id: 0 }];
    assert!(matches!(
        g.validate_redo_group_applicability([(&first[..], 1), (&deleted[..], 2), (&second[..], 3)]),
        Err(relate_core::Error::Semantic(_))
    ));
    assert_eq!(g.commit_ts(), 0);
    assert!(g.all_node_ids().is_empty());
    assert!(g.named_indexes().is_empty());
}

#[test]
fn direct_graph_creation_exhaustion_is_typed_and_has_no_side_effects() {
    let graph = Graph::default();
    let node = graph.create_node(vec![], BTreeMap::new()).unwrap();
    graph.set_next_node_id(i64::MAX);
    graph.set_next_rel_id(i64::MAX);
    let used = graph.used_bytes();
    graph.begin_write().unwrap();
    assert!(matches!(
        graph.create_node(vec!["Bad".into()], BTreeMap::new()),
        Err(relate_core::Error::Execution(ref message)) if message == "node ID exhausted"
    ));
    assert!(matches!(
        graph.create_rel(node, node, "Bad".into(), BTreeMap::new()),
        Err(relate_core::Error::Execution(ref message)) if message == "relationship ID exhausted"
    ));
    assert!(graph.take_redo().is_empty());
    assert_eq!(graph.next_node_id(), i64::MAX);
    assert_eq!(graph.next_rel_id(), i64::MAX);
    assert_eq!(graph.used_bytes(), used);
    assert_eq!(graph.stats.lock().unwrap().nodes_created, 0);
    assert_eq!(graph.stats.lock().unwrap().relationships_created, 0);
    assert!(graph.dirty_node_ids().iter().all(|id| *id != i64::MAX));
}

#[test]
fn failed_relationship_does_not_consume_last_valid_id() {
    let graph = Graph::default();
    let node = graph.create_node(vec![], BTreeMap::new()).unwrap();
    graph.begin_write().unwrap();
    graph.set_next_rel_id(i64::MAX - 1);
    let used = graph.used_bytes();
    assert!(matches!(
        graph.create_rel(node, 42, "Missing".into(), BTreeMap::new()),
        Err(relate_core::Error::Semantic(_))
    ));
    assert!(matches!(
        graph.create_rel(i64::MAX, node, "Invalid".into(), BTreeMap::new()),
        Err(relate_core::Error::Storage(_))
    ));
    assert_eq!(graph.next_rel_id(), i64::MAX - 1);
    assert!(graph.take_redo().is_empty());
    assert!(graph.dirty_rel_ids().is_empty());
    assert_eq!(graph.used_bytes(), used);
    assert_eq!(graph.stats.lock().unwrap().relationships_created, 0);

    let id = graph
        .create_rel(node, node, "Valid".into(), BTreeMap::new())
        .unwrap();
    assert_eq!(id, i64::MAX - 1);
    assert_eq!(graph.next_rel_id(), i64::MAX);
    assert_eq!(graph.take_redo().len(), 1);
    assert!(matches!(
        graph.create_rel(node, node, "Extra".into(), BTreeMap::new()),
        Err(relate_core::Error::Execution(ref message)) if message == "relationship ID exhausted"
    ));
}

#[test]
fn concurrent_last_id_has_exactly_one_winner_per_entity_kind() {
    use std::sync::{Arc, Barrier};

    let graph = Arc::new(Graph::default());
    let root = graph.create_node(vec![], BTreeMap::new()).unwrap();
    graph.begin_write().unwrap();
    graph.set_next_node_id(i64::MAX - 1);
    graph.set_next_rel_id(i64::MAX - 1);
    let barrier = Arc::new(Barrier::new(16));
    let handles: Vec<_> = (0..16)
        .map(|_| {
            let graph = Arc::clone(&graph);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                (
                    graph.create_node(vec![], BTreeMap::new()),
                    graph.create_rel(root, root, "R".into(), BTreeMap::new()),
                )
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|(node, _)| node.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|(_, rel)| rel.is_ok()).count(), 1);
    assert!(results.iter().all(|(node, rel)| {
        node.as_ref().map_or(true, |id| *id == i64::MAX - 1)
            && rel.as_ref().map_or(true, |id| *id == i64::MAX - 1)
    }));
    assert_eq!(graph.next_node_id(), i64::MAX);
    assert_eq!(graph.next_rel_id(), i64::MAX);
    assert!(graph.node(i64::MAX - 1).is_some());
    assert!(graph.rel(i64::MAX - 1).is_some());
    assert!(graph.node(i64::MAX).is_none());
    assert!(graph.rel(i64::MAX).is_none());
}

#[test]
fn create_and_rel() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    assert_eq!(g.neighbors(a, relate_ir::Direction::Outgoing).len(), 1);
}

/// `node_labels_at` is the label-only read; it must agree with
/// `node_at(..).labels` for multi-label nodes and carry no properties.
#[test]
fn node_labels_at_matches_node_at_labels() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let mut props = BTreeMap::new();
    props.insert("name".to_string(), Value::String("Anna".into()));
    let a = g
        .create_node(vec!["Person".into(), "Admin".into()], props)
        .unwrap();
    let b = g
        .create_node(vec!["Person".into()], BTreeMap::new())
        .unwrap();
    g.commit_write();
    let ts = g.commit_ts();

    let mut a_labels = g.node_labels_at(a, ts).unwrap();
    let mut expected = (*g.node_at(a, ts).unwrap().labels).clone();
    a_labels.sort();
    expected.sort();
    assert_eq!(a_labels, expected);
    assert_eq!(a_labels, vec!["Admin".to_string(), "Person".to_string()]);
    assert_eq!(g.node_labels_at(b, ts).unwrap(), vec!["Person".to_string()]);
    // Invisible / absent ids yield None rather than an empty label set.
    assert!(g.node_labels_at(999, ts).is_none());
}

#[test]
fn string_contains_lookup_is_selective() {
    let g = Graph::default();
    g.begin_write().unwrap();
    g.create_node(
        vec!["C".into()],
        BTreeMap::from([("name".into(), Value::String("needle-here".into()))]),
    )
    .unwrap();
    g.create_node(
        vec!["C".into()],
        BTreeMap::from([("name".into(), Value::String("other".into()))]),
    )
    .unwrap();
    g.commit_write();
    g.materialize_text_index("C", "name");
    let ids = g
        .lookup_string_contains_at(&["C".into()], "name", "needle", g.commit_ts())
        .unwrap();
    assert_eq!(ids.len(), 1);
    let none = g
        .lookup_string_contains_at(&["C".into()], "name", "zzz", g.commit_ts())
        .unwrap();
    assert!(none.is_empty());
    assert!(g
        .lookup_string_contains_at(&["C".into()], "name", "", g.commit_ts())
        .is_none());
}

#[test]
fn short_contains_needle_does_not_seek() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let id = g
        .create_node(
            vec!["C".into()],
            BTreeMap::from([("name".into(), Value::String("xxabxx".into()))]),
        )
        .unwrap();
    g.commit_write();
    g.materialize_text_index("C", "name");
    assert!(g
        .lookup_string_contains_at(&["C".into()], "name", "a", g.commit_ts())
        .is_none());
    assert!(g
        .lookup_string_contains_at(&["C".into()], "name", "ab", g.commit_ts())
        .is_none());
    let ids = g
        .lookup_string_contains_at(&["C".into()], "name", "xab", g.commit_ts())
        .unwrap();
    assert_eq!(ids, vec![id]);
    let none = g
        .lookup_string_contains_at(&["C".into()], "name", "zzz", g.commit_ts())
        .unwrap();
    assert!(none.is_empty());
    let r = crate::exec::test_support::exec_cypher(
        &g,
        "MATCH (n:C) WHERE n.name CONTAINS 'ab' RETURN n.name AS n",
    );
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0][0].as_string().unwrap(), "xxabxx");
}

#[test]
fn string_prefix_lookup() {
    let g = Graph::default();
    g.begin_write().unwrap();
    g.create_node(
        vec!["C".into()],
        BTreeMap::from([("name".into(), Value::String("Ada".into()))]),
    )
    .unwrap();
    g.create_node(
        vec!["C".into()],
        BTreeMap::from([("name".into(), Value::String("Bob".into()))]),
    )
    .unwrap();
    g.commit_write();
    g.materialize_range_index("C", "name");
    let ids = g
        .lookup_string_prefix_at(&["C".into()], "name", "Ad", g.commit_ts())
        .unwrap();
    assert_eq!(ids.len(), 1);
}

#[test]
fn string_index_is_lazy_until_first_seek() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g
        .create_node(
            vec!["C".into()],
            BTreeMap::from([("name".into(), Value::String("needle-here".into()))]),
        )
        .unwrap();
    g.create_node(
        vec!["C".into()],
        BTreeMap::from([("name".into(), Value::String("other".into()))]),
    )
    .unwrap();
    g.commit_write();
    // Writes alone never materialize trigram postings. CONTAINS scans
    // until a TEXT index names the pair; STARTS WITH scans until a RANGE
    // index does.
    assert_eq!(g.gram_pair_state("C", "name"), None);
    assert!(g
        .lookup_string_contains_at(&["C".into()], "name", "needle", g.commit_ts())
        .is_none());
    assert!(g
        .lookup_string_prefix_at(&["C".into()], "name", "need", g.commit_ts())
        .is_none());
    g.materialize_text_index("C", "name");
    let ids = g
        .lookup_string_contains_at(&["C".into()], "name", "needle", g.commit_ts())
        .unwrap();
    assert_eq!(ids, vec![a]);
    let (grams, dirty) = g.gram_pair_state("C", "name").unwrap();
    assert!(grams > 0 && dirty == 0);
    g.materialize_range_index("C", "name");
    let ids = g
        .lookup_string_prefix_at(&["C".into()], "name", "need", g.commit_ts())
        .unwrap();
    assert_eq!(ids, vec![a]);
}

#[test]
fn string_index_dirty_catchup_after_writes() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g
        .create_node(
            vec!["C".into()],
            BTreeMap::from([("name".into(), Value::String("needle-a".into()))]),
        )
        .unwrap();
    let b = g
        .create_node(
            vec!["C".into()],
            BTreeMap::from([("name".into(), Value::String("plain".into()))]),
        )
        .unwrap();
    g.commit_write();
    g.materialize_text_index("C", "name");
    let ids = g
        .lookup_string_contains_at(&["C".into()], "name", "needle", g.commit_ts())
        .unwrap();
    assert_eq!(ids, vec![a]);

    // Writes after the build: one create, one SET into a matching
    // string, one SET away from it, one delete (ADR-0063 dirty drain).
    g.begin_write().unwrap();
    let c = g
        .create_node(
            vec!["C".into()],
            BTreeMap::from([("name".into(), Value::String("needle-c".into()))]),
        )
        .unwrap();
    g.set_property_node(b, "name", Value::String("needle-b".into()));
    g.set_property_node(a, "name", Value::String("gone".into()));
    g.commit_write();
    let (grams, dirty) = g.gram_pair_state("C", "name").unwrap();
    assert!(grams > 0);
    assert_eq!(dirty, 3, "create + two SETs dirty three nodes");
    let ids = g
        .lookup_string_contains_at(&["C".into()], "name", "needle", g.commit_ts())
        .unwrap();
    assert_eq!(
        ids,
        vec![b, c],
        "drain adds new matches, residual drops stale"
    );
    let (_, dirty) = g.gram_pair_state("C", "name").unwrap();
    assert_eq!(dirty, 0);

    g.begin_write().unwrap();
    g.delete_node(c, true).unwrap();
    g.commit_write();
    let ids = g
        .lookup_string_contains_at(&["C".into()], "name", "needle", g.commit_ts())
        .unwrap();
    assert_eq!(ids, vec![b]);
}

#[test]
fn string_index_add_labels_catchup() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g
        .create_node(
            vec!["A".into()],
            BTreeMap::from([("name".into(), Value::String("needle-a".into()))]),
        )
        .unwrap();
    g.commit_write();
    g.materialize_text_index("B", "name");
    // The text index exists on an empty label, then the node moves into :B.
    let ids = g
        .lookup_string_contains_at(&["B".into()], "name", "needle", g.commit_ts())
        .unwrap();
    assert!(ids.is_empty());
    g.begin_write().unwrap();
    g.add_labels(a, &["B".into()]);
    g.commit_write();
    let ids = g
        .lookup_string_contains_at(&["B".into()], "name", "needle", g.commit_ts())
        .unwrap();
    assert_eq!(
        ids,
        vec![a],
        "label add must dirty the new (label, key) pair"
    );
    // REMOVE LABEL leaves a stale posting; the residual check filters it.
    g.begin_write().unwrap();
    g.remove_labels(a, &["B".into()]);
    g.commit_write();
    let ids = g
        .lookup_string_contains_at(&["B".into()], "name", "needle", g.commit_ts())
        .unwrap();
    assert!(ids.is_empty());
}

#[test]
fn string_index_serves_pre_build_snapshot() {
    // ADR-0063: a seek pinned before the pair was built must see the same
    // versions eager maintenance would have posted, so builds index every
    // version in the chain, not just the latest.
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g
        .create_node(
            vec!["C".into()],
            BTreeMap::from([("name".into(), Value::String("needle-a".into()))]),
        )
        .unwrap();
    g.commit_write();
    let pin = g.pin_now().unwrap();
    let old_ts = pin.ts();
    g.begin_write().unwrap();
    g.set_property_node(a, "name", Value::String("renamed".into()));
    g.commit_write();
    // The index is built after the overwrite and still posts every version,
    // so the pinned snapshot matches the old string.
    g.materialize_text_index("C", "name");
    let ids = g
        .lookup_string_contains_at(&["C".into()], "name", "needle", old_ts)
        .unwrap();
    assert_eq!(ids, vec![a]);
    let ids = g
        .lookup_string_contains_at(&["C".into()], "name", "needle", g.commit_ts())
        .unwrap();
    assert!(ids.is_empty());
    drop(pin);
}

#[test]
fn string_index_star_pair_covers_all_labels() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g
        .create_node(
            vec!["A".into()],
            BTreeMap::from([("name".into(), Value::String("needle-a".into()))]),
        )
        .unwrap();
    let b = g
        .create_node(
            vec!["B".into()],
            BTreeMap::from([("name".into(), Value::String("needle-b".into()))]),
        )
        .unwrap();
    g.commit_write();
    // An unlabeled CONTAINS is not covered by any one text index.
    assert!(g
        .lookup_string_contains_at(&[], "name", "needle", g.commit_ts())
        .is_none());
    g.materialize_text_index("A", "name");
    g.materialize_text_index("B", "name");
    let ids = g
        .lookup_string_contains_at(&["A".into()], "name", "needle", g.commit_ts())
        .unwrap();
    assert_eq!(ids, vec![a]);
    let ids = g
        .lookup_string_contains_at(&["B".into()], "name", "needle", g.commit_ts())
        .unwrap();
    assert_eq!(ids, vec![b]);
}

#[test]
fn range_index_orders_numbers_and_prefix() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let low = g
        .create_node(
            vec!["N".into()],
            BTreeMap::from([("x".into(), Value::Int(1))]),
        )
        .unwrap();
    let high = g
        .create_node(
            vec!["N".into()],
            BTreeMap::from([("x".into(), Value::Int(5))]),
        )
        .unwrap();
    g.commit_write();
    assert!(g
        .lookup_range_seek(
            &["N".into()],
            "x",
            &RangeSeek::Cmp {
                op: relate_ir::BinaryOp::Gt,
                bound: Value::Int(1),
            },
            g.commit_ts(),
        )
        .is_none());
    g.materialize_range_index("N", "x");
    let ids = g
        .lookup_range_seek(
            &["N".into()],
            "x",
            &RangeSeek::Cmp {
                op: relate_ir::BinaryOp::Gt,
                bound: Value::Int(1),
            },
            g.commit_ts(),
        )
        .unwrap();
    assert_eq!(ids, vec![high]);
    let ids = g
        .lookup_nodes(
            &["N".into()],
            &BTreeMap::from([("x".into(), Value::Int(1))]),
        )
        .unwrap();
    assert_eq!(ids, vec![low]);
}

#[test]
fn range_seek_scans_when_a_datetime_does_not_fit_the_order_key() {
    use chrono::NaiveDate;
    use relate_core::Temporal;

    let g = Graph::default();
    g.begin_write().unwrap();
    let far = NaiveDate::from_ymd_opt(9999, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    g.create_node(
        vec!["N".into()],
        BTreeMap::from([("t".into(), Value::Temporal(Temporal::LocalDateTime(far)))]),
    )
    .unwrap();
    g.commit_write();
    g.materialize_range_index("N", "t");
    assert!(g
        .lookup_range_seek(
            &["N".into()],
            "t",
            &RangeSeek::Cmp {
                op: relate_ir::BinaryOp::Gt,
                bound: Value::Int(0),
            },
            g.commit_ts(),
        )
        .is_none());
}

#[test]
fn catalog_index_detail_builds_and_drop_discards() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let book = g
        .create_node(
            vec!["Book".into()],
            BTreeMap::from([("title".into(), Value::String("War and Peace".into()))]),
        )
        .unwrap();
    let place = g
        .create_node(
            vec!["Place".into()],
            BTreeMap::from([
                ("name".into(), Value::String("Ada".into())),
                ("n".into(), Value::Int(4)),
                (
                    "loc".into(),
                    Value::Point(relate_core::PointValue {
                        srid: 7203,
                        x: 1.0,
                        y: 1.0,
                        z: None,
                    }),
                ),
            ]),
        )
        .unwrap();
    g.commit_write();
    let before = g.used_bytes();
    assert_eq!(before, g.accounted_bytes());

    let text = crate::redo::IndexDetailSpec {
        kind: crate::redo::IndexKindSpec::Text,
        relationship: false,
        labels: vec!["Book".into()],
        properties: vec!["title".into()],
        with_properties: vec![],
    };
    g.apply_redo(
        vec![RedoOp::CatalogIndexDetail {
            name: "titleIndex".into(),
            detail: text,
            options: redo::props_json(&BTreeMap::new()),
        }],
        g.commit_ts() + 1,
    )
    .unwrap();
    let ids = g
        .lookup_string_contains_at(&["Book".into()], "title", "and", g.commit_ts())
        .unwrap();
    assert_eq!(ids, vec![book]);
    assert!(g.used_bytes() > before);
    assert_eq!(g.used_bytes(), g.accounted_bytes());
    g.rebuild_indexes();
    let ids = g
        .lookup_string_contains_at(&["Book".into()], "title", "and", g.commit_ts())
        .unwrap();
    assert_eq!(ids, vec![book]);

    g.apply_redo(
        vec![RedoOp::DropIndex {
            name: "titleIndex".into(),
        }],
        g.commit_ts() + 1,
    )
    .unwrap();
    assert!(g
        .lookup_string_contains_at(&["Book".into()], "title", "and", g.commit_ts())
        .is_none());
    assert_eq!(g.used_bytes(), before);
    assert_eq!(g.used_bytes(), g.accounted_bytes());

    let range = crate::redo::IndexDetailSpec {
        kind: crate::redo::IndexKindSpec::Range,
        relationship: false,
        labels: vec!["Place".into()],
        properties: vec!["n".into()],
        with_properties: vec![],
    };
    g.apply_redo(
        vec![RedoOp::CatalogIndexDetail {
            name: "nIndex".into(),
            detail: range,
            options: redo::props_json(&BTreeMap::new()),
        }],
        g.commit_ts() + 1,
    )
    .unwrap();
    let ids = g
        .lookup_range_seek(
            &["Place".into()],
            "n",
            &RangeSeek::Cmp {
                op: relate_ir::BinaryOp::Gte,
                bound: Value::Int(4),
            },
            g.commit_ts(),
        )
        .unwrap();
    assert_eq!(ids, vec![place]);
    let ids = g.lookup_string_prefix_at(&["Place".into()], "name", "Ad", g.commit_ts());
    assert!(ids.is_none(), "name has no range index");

    let point = crate::redo::IndexDetailSpec {
        kind: crate::redo::IndexKindSpec::Point,
        relationship: false,
        labels: vec!["Place".into()],
        properties: vec!["loc".into()],
        with_properties: vec![],
    };
    g.apply_redo(
        vec![RedoOp::CatalogIndexDetail {
            name: "locIndex".into(),
            detail: point,
            options: redo::props_json(&BTreeMap::new()),
        }],
        g.commit_ts() + 1,
    )
    .unwrap();
    let ids = g
        .lookup_spatial_seek(
            &["Place".into()],
            "loc",
            &SpatialSeek::BBox {
                lo: Value::Point(relate_core::PointValue {
                    srid: 7203,
                    x: 0.0,
                    y: 0.0,
                    z: None,
                }),
                hi: Value::Point(relate_core::PointValue {
                    srid: 7203,
                    x: 2.0,
                    y: 2.0,
                    z: None,
                }),
            },
            g.commit_ts(),
        )
        .unwrap();
    assert_eq!(ids, vec![place]);
    assert_eq!(g.used_bytes(), g.accounted_bytes());
}

#[test]
fn mvcc_old_snapshot_hides_new_node() {
    let g = Graph::default();
    g.begin_write().unwrap();
    g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    g.commit_write();
    assert_eq!(g.live_node_count(), 1);
    g.begin_write().unwrap();
    g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    assert_eq!(g.all_node_ids_at(g.commit_ts()).len(), 1);
    g.commit_write();
    assert_eq!(g.live_node_count(), 2);
}

#[test]
fn abort_then_apply_redo_is_the_commit() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    let ops = g.take_redo();
    let ts = g.pending_commit_ts();
    g.abort_write();
    assert_eq!(g.live_node_count(), 0);
    g.apply_redo(ops.clone(), ts).unwrap();
    assert_eq!(g.live_node_count(), 2);
    assert_eq!(g.neighbors(a, relate_ir::Direction::Outgoing).len(), 1);
}

/// Commit the open write by aborting it and applying its redo at the
/// pending timestamp, as `abort_then_apply_redo_is_the_commit` pins down.
/// Returns the applied redo.
fn commit_via_replay(g: &Graph) -> Vec<RedoOp> {
    let ops = g.take_redo();
    let ts = g.pending_commit_ts();
    g.abort_write();
    g.apply_redo(ops.clone(), ts).unwrap();
    ops
}

#[test]
fn write_set_conflicts_on_later_set() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let id = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    g.set_property_node(id, "n", Value::Int(0));
    commit_via_replay(&g);

    let set = vec![RedoOp::SetNodeProp {
        id,
        key: "n".into(),
        value: Value::Int(1),
    }];
    assert!(!g.write_set_conflicts(g.commit_ts(), &set));
    assert!(g.write_set_conflicts(0, &set));
}

#[test]
fn write_set_allows_disjoint_create() {
    let g = Graph::default();
    g.begin_write().unwrap();
    g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let ops = commit_via_replay(&g);

    let create = vec![RedoOp::CreateNode {
        id: 99,
        labels: Arc::new(vec!["B".into()]),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    }];
    assert!(!g.write_set_conflicts(0, &create));
    assert!(g.write_set_conflicts(0, &ops));
}

#[test]
fn later_set_conflicts_with_set_not_with_disjoint_create() {
    let set_a = vec![RedoOp::SetNodeProp {
        id: 1,
        key: "n".into(),
        value: Value::Int(1),
    }];
    let set_b = vec![RedoOp::SetNodeProp {
        id: 1,
        key: "n".into(),
        value: Value::Int(2),
    }];
    let create = vec![RedoOp::CreateNode {
        id: 99,
        labels: Arc::new(vec!["B".into()]),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    }];
    let rel = vec![RedoOp::CreateRel {
        id: 7,
        start: 1,
        end: 2,
        rel_type: "T".into(),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    }];
    assert!(Graph::redo_conflicts_with_later(&set_b, &set_a));
    assert!(!Graph::redo_conflicts_with_later(&create, &set_a));
    assert!(!Graph::redo_conflicts_with_later(&rel, &set_a));
    assert!(Graph::redo_conflicts_with_later(
        &rel,
        &[RedoOp::DeleteNode { id: 1 }]
    ));
}

#[test]
fn apply_does_not_change_pinned_snapshot() {
    use std::sync::Arc;
    let g = Graph::default();
    g.begin_write().unwrap();
    for i in 0..200 {
        g.create_node(
            vec!["A".into()],
            BTreeMap::from([("i".into(), Value::Int(i))]),
        )
        .unwrap();
    }
    commit_via_replay(&g);
    let pin = g.commit_ts();
    assert_eq!(g.all_node_ids_at(pin).len(), 200);

    let g = Arc::new(g);
    let reader = {
        let g = Arc::clone(&g);
        std::thread::spawn(move || {
            for _ in 0..50 {
                assert_eq!(g.all_node_ids_at(pin).len(), 200);
            }
        })
    };
    g.begin_write().unwrap();
    let extra = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    commit_via_replay(&g);
    reader.join().unwrap();
    assert_eq!(g.all_node_ids_at(pin).len(), 200);
    assert_eq!(g.live_node_count(), 201);
    assert!(g.node_at(extra, g.commit_ts()).is_some());
    assert!(g.node_at(extra, pin).is_none());
}

#[test]
fn gc_drops_superseded_version_after_pin_releases() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let id = g
        .create_node(
            vec!["A".into()],
            BTreeMap::from([("n".into(), Value::Int(1))]),
        )
        .unwrap();
    commit_via_replay(&g);

    let pin = g.pin_now().unwrap();
    g.begin_write().unwrap();
    g.set_property_node(id, "n", Value::Int(2));
    commit_via_replay(&g);

    g.gc_versions();
    assert_eq!(
        g.node_at(id, pin.ts()).unwrap().properties["n"],
        Value::Int(1)
    );
    assert_eq!(
        g.node_at(id, g.commit_ts()).unwrap().properties["n"],
        Value::Int(2)
    );
    drop(pin);
    g.gc_versions();
    assert!(g.node_at(id, 1).is_none());
    let found = g
        .lookup_nodes_at(
            &["A".into()],
            &BTreeMap::from([("n".into(), Value::Int(1))]),
            g.commit_ts(),
        )
        .unwrap();
    assert!(found.is_empty());
    let found = g
        .lookup_nodes_at(
            &["A".into()],
            &BTreeMap::from([("n".into(), Value::Int(2))]),
            g.commit_ts(),
        )
        .unwrap();
    assert_eq!(found, vec![id]);
}

#[test]
fn claiming_pin_blocks_gc_horizon() {
    let g = Graph::default();
    g.begin_write().unwrap();
    g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    commit_via_replay(&g);
    assert_eq!(g.gc_horizon(), g.commit_ts());
    g.pins[0].store(PIN_CLAIMING, Ordering::Release);
    assert_eq!(g.gc_horizon(), 0);
    g.pins[0].store(PIN_FREE, Ordering::Release);
    assert_eq!(g.gc_horizon(), g.commit_ts());
}

#[test]
fn full_pin_table_fails_fast_with_snapshot_pin_and_recovers() {
    let g = Graph::default();
    let mut held = Vec::new();
    for _ in 0..PIN_SLOTS {
        held.push(g.pin_now().unwrap());
    }
    // Count the real acquisition loop's retries, not scheduler-dependent elapsed time.
    let mut retries = 0;
    let error = g
        .pin_now_with_retry(|| {
            retries += 1;
            assert!(
                retries <= PIN_ACQUIRE_RETRIES,
                "pin acquisition exceeded its retry budget"
            );
        })
        .err()
        .expect("a full pin table must fail");
    assert_eq!(retries, PIN_ACQUIRE_RETRIES);
    assert_eq!(
        error,
        relate_core::Error::Execution(format!("snapshot pin: all {PIN_SLOTS} slots are held"))
    );
    // Releasing one pin lets the next acquisition succeed again (ADR-0111).
    held.pop();
    assert!(g.pin_now().is_ok());
}

#[test]
fn concurrent_pins_survive_gc() {
    use std::sync::{Arc, Barrier};
    let g = Graph::default();
    g.begin_write().unwrap();
    let id = g
        .create_node(
            vec!["A".into()],
            BTreeMap::from([("n".into(), Value::Int(1))]),
        )
        .unwrap();
    commit_via_replay(&g);

    let g = Arc::new(g);
    let held = g.pin_now().unwrap();
    let n = 4;
    let barrier = Arc::new(Barrier::new(n + 1));
    let mut readers = Vec::new();
    for _ in 0..n {
        let g = Arc::clone(&g);
        let barrier = Arc::clone(&barrier);
        readers.push(std::thread::spawn(move || {
            let pin = g.pin_now().unwrap();
            barrier.wait();
            assert_eq!(
                g.node_at(id, pin.ts()).unwrap().properties["n"],
                Value::Int(1)
            );
            barrier.wait();
            drop(pin);
        }));
    }
    barrier.wait();
    g.begin_write().unwrap();
    g.set_property_node(id, "n", Value::Int(2));
    commit_via_replay(&g);
    g.gc_versions();
    assert_eq!(
        g.node_at(id, held.ts()).unwrap().properties["n"],
        Value::Int(1)
    );
    barrier.wait();
    drop(held);
    for t in readers {
        t.join().unwrap();
    }
    g.gc_versions();
    assert_eq!(
        g.node_at(id, g.commit_ts()).unwrap().properties["n"],
        Value::Int(2)
    );
    assert!(g.node_at(id, 1).is_none());
}

#[test]
fn gc_removes_deleted_node_from_label_index() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let keep = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let gone = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    commit_via_replay(&g);

    g.begin_write().unwrap();
    g.delete_node(gone, true).unwrap();
    commit_via_replay(&g);

    g.gc_versions();
    let found = g
        .lookup_nodes_at(&["A".into()], &BTreeMap::new(), g.commit_ts())
        .unwrap();
    assert_eq!(found, vec![keep]);
    assert!(g.node_at(gone, g.commit_ts()).is_none());
}

#[test]
fn compact_transfer_payload_roundtrips() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let mut p = BTreeMap::new();
    p.insert("id".into(), Value::String("tx-1".into()));
    p.insert("amount".into(), Value::Float(12.5));
    p.insert("ts".into(), Value::Int(7));
    let rid = g.create_rel(a, b, "TRANSFER".into(), p).unwrap();
    g.commit_write();
    let r = g.rel(rid).unwrap();
    assert_eq!(r.rel_type, "TRANSFER");
    assert_eq!(r.properties.get("id"), Some(&Value::String("tx-1".into())));
    assert_eq!(r.properties.get("amount"), Some(&Value::Float(12.5)));
    assert_eq!(r.properties.get("ts"), Some(&Value::Int(7)));

    g.begin_write().unwrap();
    g.set_property_rel(rid, "note", Value::String("held".into()));
    g.commit_write();
    let r = g.rel(rid).unwrap();
    assert_eq!(
        r.properties.get("note"),
        Some(&Value::String("held".into()))
    );
    assert_eq!(r.properties.get("amount"), Some(&Value::Float(12.5)));
}

#[test]
fn stub_has_no_props_prop_lookup_does() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let mut p = BTreeMap::new();
    p.insert("name".into(), Value::String("Ada".into()));
    let id = g.create_node(vec!["P".into()], p).unwrap();
    g.commit_write();
    let stub = g.node_stub_at(id, g.commit_ts()).unwrap();
    assert!(stub.properties.is_empty());
    assert_eq!(
        g.node_prop_at(id, g.commit_ts(), "name"),
        Some(Value::String("Ada".into()))
    );
}

#[test]
fn label_stub_cache_is_charged_once() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let id = g
        .create_node(vec!["Person".into()], BTreeMap::new())
        .unwrap();
    g.commit_write();
    let before = g.used_bytes();
    let first = g.node_stub_at(id, g.commit_ts()).unwrap();
    let charged = g.used_bytes();
    assert!(charged > before, "before={before} charged={charged}");
    let second = g.node_stub_at(id, g.commit_ts()).unwrap();
    assert!(std::sync::Arc::ptr_eq(&first.labels, &second.labels));
    assert_eq!(g.used_bytes(), charged);
    assert_eq!(g.used_bytes(), g.accounted_bytes());
}

fn vector_index_detail() -> crate::redo::IndexDetailSpec {
    crate::redo::IndexDetailSpec {
        kind: crate::redo::IndexKindSpec::Vector,
        relationship: false,
        labels: vec!["Doc".into()],
        properties: vec!["embedding".into()],
        with_properties: vec!["ttl".into()],
    }
}

#[test]
fn apply_redo_catalog_index_detail_stores_the_typed_model() {
    let g = Graph::default();
    let detail = vector_index_detail();
    let op = RedoOp::CatalogIndexDetail {
        name: "vec_idx".into(),
        detail: detail.clone(),
        options: redo::props_json(&BTreeMap::from([(
            "indexConfig".to_string(),
            Value::Map(
                BTreeMap::from([("vector.dimensions".to_string(), Value::Int(1536))]).into(),
            ),
        )])),
    };
    g.apply_redo(vec![op], 1).unwrap();
    let theater = g.theater.lock().unwrap();
    assert_eq!(theater.index_details.get("vec_idx"), Some(&detail));
    assert_eq!(
        theater.index_options["vec_idx"]["indexConfig"],
        Value::Map(BTreeMap::from([("vector.dimensions".to_string(), Value::Int(1536))]).into())
    );
}

#[test]
fn apply_redo_malformed_index_detail_options_returns_err() {
    let g = Graph::default();
    let op = RedoOp::CatalogIndexDetail {
        name: "idx".into(),
        detail: crate::redo::IndexDetailSpec {
            kind: crate::redo::IndexKindSpec::Range,
            relationship: false,
            labels: vec!["L".into()],
            properties: vec!["p".into()],
            with_properties: vec![],
        },
        options: serde_json::json!("corrupted"),
    };
    assert!(g.apply_redo(vec![op], 1).is_err());
    // Nothing was silently stored: the decode failure aborts the op.
    let theater = g.theater.lock().unwrap();
    assert!(theater.index_details.is_empty());
    assert!(theater.index_options.is_empty());
}

#[test]
fn apply_redo_malformed_index_detail_option_value_returns_err() {
    let g = Graph::default();
    let op = RedoOp::CatalogIndexDetail {
        name: "idx".into(),
        detail: vector_index_detail(),
        options: serde_json::json!({"indexConfig": {"not": "tagged"}}),
    };
    assert!(g.apply_redo(vec![op], 1).is_err());
    assert!(g.theater.lock().unwrap().index_details.is_empty());
}

#[test]
fn apply_redo_graph_type_ops_store_the_typed_model() {
    let g = Graph::default();
    let stmt = relate_parser::parse(
        "CREATE GRAPH TYPE gt {
            (s:Site => {name :: STRING IS KEY}),
            (s)-[:LINK => {since :: DATE}]->(s)
        }",
    )
    .expect("parses");
    let Some(relate_parser::Clause::CreateGraphType(cgt)) = stmt.clauses.first() else {
        panic!("expected CreateGraphType");
    };
    let spec =
        crate::graph_type::GraphTypeSpec::from_body(&cgt.body).expect("shape-validated body");

    g.apply_redo(
        vec![RedoOp::PutGraphType {
            name: "gt".into(),
            body: spec.clone(),
        }],
        1,
    )
    .unwrap();
    assert_eq!(g.theater.lock().unwrap().graph_types.get("gt"), Some(&spec));

    g.apply_redo(vec![RedoOp::SetCurrentGraphType { body: spec }], 2)
        .unwrap();
    let theater = g.theater.lock().unwrap();
    assert_eq!(theater.current_graph_type.to_body(), cgt.body);
    // The derived constraint set is recomputed from the carried model.
    // (`relate_model::Constraint` has no PartialEq; compare the
    // stable projection.)
    let project = |m: &BTreeMap<String, relate_model::Constraint>| {
        m.values()
            .map(|c| {
                (
                    c.name.clone(),
                    c.label.clone(),
                    c.properties.clone(),
                    c.constraint_type.as_str().to_string(),
                    c.property_type.clone(),
                    c.entity_type.clone(),
                    c.enforced_label.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    let expected: BTreeMap<_, _> = crate::graph_type::derive_constraints(&cgt.body)
        .expect("a validated stored body derives")
        .into_iter()
        .map(|c| (c.name.clone(), c))
        .collect();
    assert_eq!(project(&theater.derived_constraints), project(&expected));
    drop(theater);
    assert_eq!(
        g.graph_type_spec()
            .expect("a stored validated spec canonicalizes")
            .as_deref(),
        Some(
            crate::graph_type::canonicalize_graph_type_body(&cgt.body)
                .expect("a validated body canonicalizes")
                .as_str()
        )
    );
}

#[test]
fn apply_redo_graph_type_model_needs_no_parse() {
    // A body whose canonical string form would not re-parse to the
    // same state (non-literal OPTIONS rendered as `?`) still replays:
    // the typed model is stored verbatim.
    let g = Graph::default();
    let stmt = relate_parser::parse(
        "CREATE GRAPH TYPE gt AS {
            CONSTRAINT FOR (p:Foo) REQUIRE p.a IS KEY OPTIONS {x: 1 + 1}
        }",
    )
    .expect("parses");
    let Some(relate_parser::Clause::CreateGraphType(cgt)) = stmt.clauses.first() else {
        panic!("expected CreateGraphType");
    };
    let spec =
        crate::graph_type::GraphTypeSpec::from_body(&cgt.body).expect("shape-validated body");
    g.apply_redo(
        vec![RedoOp::PutGraphType {
            name: "gt".into(),
            body: spec.clone(),
        }],
        1,
    )
    .unwrap();
    let stored = g.theater.lock().unwrap().graph_types.get("gt").cloned();
    assert_eq!(stored, Some(spec));
}

#[test]
fn pull_identity_put_survives_apply_and_drops_on_delete() {
    let g = Graph::default();
    g.apply_redo(
        vec![
            RedoOp::CreateNode {
                id: 1,
                labels: Arc::new(vec!["Person".into()]),
                properties: relate_core::PropertyMap::share(BTreeMap::new()),
            },
            RedoOp::PullIdentityPut {
                source: "http://src/db/neo4j/query/v2".into(),
                kind: crate::redo::PullKind::Node,
                remote_id: "4:aaa:0".into(),
                local_id: 1,
            },
        ],
        1,
    )
    .unwrap();
    assert_eq!(
        g.pull_identity_get(
            "http://src/db/neo4j/query/v2",
            crate::redo::PullKind::Node,
            "4:aaa:0"
        ),
        Some(1)
    );
    g.apply_redo(vec![RedoOp::DeleteNode { id: 1 }], 2).unwrap();
    assert_eq!(
        g.pull_identity_get(
            "http://src/db/neo4j/query/v2",
            crate::redo::PullKind::Node,
            "4:aaa:0"
        ),
        None
    );
}
