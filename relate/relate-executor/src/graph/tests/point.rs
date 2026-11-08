//! Graph-level POINT index tests: the seek, the dirty-drain repost, and
//! the backend's superset contract exercised through `Graph`.

use std::collections::BTreeMap;

use relate_test_support::values::point;

use super::super::{Graph, SpatialSeek};

#[test]
fn point_index_answers_bbox() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let inside = g
        .create_node(
            vec!["P".into()],
            BTreeMap::from([("loc".into(), point(7203, 1.0, 1.0))]),
        )
        .unwrap();
    g.create_node(
        vec!["P".into()],
        BTreeMap::from([("loc".into(), point(7203, 10.0, 10.0))]),
    )
    .unwrap();
    g.commit_write();
    let seek = SpatialSeek::BBox {
        lo: point(7203, 0.0, 0.0),
        hi: point(7203, 2.0, 2.0),
    };
    assert!(g
        .lookup_spatial_seek(&["P".into()], "loc", &seek, g.commit_ts())
        .is_none());
    g.materialize_point_index("P", "loc");
    let ids = g
        .lookup_spatial_seek(&["P".into()], "loc", &seek, g.commit_ts())
        .unwrap();
    assert_eq!(ids, vec![inside]);
    let ids = g
        .lookup_spatial_seek(
            &["P".into()],
            "loc",
            &SpatialSeek::Distance {
                origin: point(7203, 0.0, 0.0),
                max: 2.0,
                inclusive: true,
            },
            g.commit_ts(),
        )
        .unwrap();
    assert_eq!(ids, vec![inside]);
    let ids = g
        .lookup_spatial_seek(
            &["P".into()],
            "loc",
            &SpatialSeek::BBox {
                lo: point(7203, 0.0, 0.0),
                hi: point(7203, 1.0e12, 1.0e12),
            },
            g.commit_ts(),
        )
        .unwrap();
    assert_eq!(ids.len(), 2);
}

#[test]
fn point_index_dirty_drain_reposts_the_moved_point() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let node = g
        .create_node(
            vec!["P".into()],
            BTreeMap::from([("loc".into(), point(7203, 0.0, 0.0))]),
        )
        .unwrap();
    g.commit_write();
    g.materialize_point_index("P", "loc");
    let near_origin = SpatialSeek::BBox {
        lo: point(7203, -0.1, -0.1),
        hi: point(7203, 0.1, 0.1),
    };
    assert_eq!(
        g.lookup_spatial_seek(&["P".into()], "loc", &near_origin, g.commit_ts())
            .unwrap(),
        vec![node]
    );
    // Move the point; the next seek drains the dirty mark and reposts.
    g.begin_write().unwrap();
    g.set_property_node(node, "loc", point(7203, 100.0, 100.0));
    g.commit_write();
    assert!(g
        .lookup_spatial_seek(&["P".into()], "loc", &near_origin, g.commit_ts())
        .unwrap()
        .is_empty());
    let near_new = SpatialSeek::BBox {
        lo: point(7203, 99.9, 99.9),
        hi: point(7203, 100.1, 100.1),
    };
    assert_eq!(
        g.lookup_spatial_seek(&["P".into()], "loc", &near_new, g.commit_ts())
            .unwrap(),
        vec![node]
    );
}
