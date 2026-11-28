use super::*;
use relate_model::VectorCoords;
use std::sync::Arc;

/// The spatial/vector property pair both graph fixtures carry — one
/// spelling, so the two cannot drift (the module's temporals precedent).
fn spatial_and_vector_properties() -> BTreeMap<String, Value> {
    BTreeMap::from([
        (
            "point".into(),
            Value::Point(PointValue {
                srid: 4326,
                x: 1.5,
                y: -2.25,
                z: None,
            }),
        ),
        (
            "vector".into(),
            Value::vector(
                VectorCoordKind::Float32,
                VectorCoords::from_f32(vec![1.5, -0.0]),
            ),
        ),
    ])
}

/// The one three-entity skeleton the graph fixtures share: node 1
/// (labelled `N`), node 2 (labelled `N`) and rel 3 from 1 to 2 (type
/// `R`), each carrying the payload the caller passes — the ids, labels
/// and relationship live in one place.
pub(super) fn graph_with(
    node1: BTreeMap<String, Value>,
    node2: BTreeMap<String, Value>,
    rel: BTreeMap<String, Value>,
) -> Graph {
    let graph = Graph::default();
    graph
        .load_checkpoint_node(Node {
            id: 1,
            labels: Arc::new(vec!["N".into()]),
            properties: relate_core::PropertyMap::share(node1),
        })
        .unwrap();
    graph
        .load_checkpoint_node(Node {
            id: 2,
            labels: Arc::new(vec!["N".into()]),
            properties: relate_core::PropertyMap::share(node2),
        })
        .unwrap();
    graph
        .load_checkpoint_rel(Relationship {
            id: 3,
            start: 1,
            end: 2,
            rel_type: "R".into(),
            properties: relate_core::PropertyMap::share(rel),
        })
        .unwrap();
    graph.set_commit_ts(1);
    graph
}

pub(super) fn graph_with_typed_and_user_maps() -> Graph {
    let point_like_map = Value::Map(
        BTreeMap::from([
            ("__type".into(), Value::String("point".into())),
            ("srid".into(), Value::Int(4326)),
            ("x".into(), Value::Float(1.5)),
        ])
        .into(),
    );
    graph_with(
        spatial_and_vector_properties(),
        BTreeMap::from([(
            "payload".into(),
            Value::List(vec![point_like_map.clone()].into()),
        )]),
        BTreeMap::from([(
            "payload".into(),
            Value::Map(
                BTreeMap::from([
                    ("__type".into(), Value::String("custom".into())),
                    ("nested".into(), Value::List(vec![point_like_map].into())),
                    (
                        "duration".into(),
                        Value::Temporal(Temporal::Duration {
                            months: 2,
                            days: 3,
                            nanos: 4_000_000_005,
                        }),
                    ),
                ])
                .into(),
            ),
        )]),
    )
}

fn temporal(kind: &str, value: &str) -> Value {
    Value::Temporal(parse_iso(kind, value).unwrap())
}

pub(super) fn graph_with_lossless_temporals() -> Graph {
    graph_with(
        BTreeMap::from([
            ("date".into(), temporal("date", "2024-02-29")),
            ("localtime".into(), temporal("localtime", "12:34:56.123")),
            ("time".into(), temporal("time", "12:34:56.123+01:30")),
            (
                "localdatetime".into(),
                temporal("localdatetime", "2024-02-29T12:34:56.123"),
            ),
            (
                "datetime".into(),
                temporal("datetime", "2024-02-29T12:34:56.123-05:30"),
            ),
            (
                "duration".into(),
                Value::Temporal(Temporal::Duration {
                    months: 2,
                    days: -3,
                    nanos: -4_000_000_005,
                }),
            ),
            (
                "duration_high".into(),
                Value::Temporal(Temporal::Duration {
                    months: 0,
                    days: 0,
                    nanos: i64::MAX as i128 * 1_000_000_000 + 42,
                }),
            ),
            (
                "duration_low".into(),
                Value::Temporal(Temporal::Duration {
                    months: 0,
                    days: 0,
                    nanos: i64::MIN as i128 * 1_000_000_000 - 42,
                }),
            ),
        ]),
        spatial_and_vector_properties(),
        BTreeMap::from([(
            "payload".into(),
            Value::Map(
                BTreeMap::from([(
                    "items".into(),
                    Value::List(vec![temporal("time", "01:02:03-01:30")].into()),
                )])
                .into(),
            ),
        )]),
    )
}

pub(super) fn graph_with_invalid_temporal(value: Value, relationship: bool, nested: bool) -> Graph {
    let graph = Graph::default();
    let properties = if nested {
        BTreeMap::from([(
            "payload".into(),
            Value::Map(
                BTreeMap::from([(
                    "items".into(),
                    Value::List(
                        vec![Value::Map(BTreeMap::from([("bad".into(), value)]).into())].into(),
                    ),
                )])
                .into(),
            ),
        )])
    } else {
        BTreeMap::from([("bad".into(), value)])
    };
    graph
        .load_checkpoint_node(Node {
            id: 1,
            labels: Arc::new(vec![]),
            properties: relate_core::PropertyMap::share(if relationship {
                BTreeMap::new()
            } else {
                properties.clone()
            }),
        })
        .unwrap();
    graph
        .load_checkpoint_node(Node {
            id: 2,
            labels: Arc::new(vec![]),
            properties: relate_core::PropertyMap::share(BTreeMap::new()),
        })
        .unwrap();
    if relationship {
        graph
            .load_checkpoint_rel(Relationship {
                id: 3,
                start: 1,
                end: 2,
                rel_type: "R".into(),
                properties: relate_core::PropertyMap::share(properties),
            })
            .unwrap();
    }
    graph.set_commit_ts(1);
    graph
}
