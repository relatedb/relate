use super::*;
use relate_model::CypherValue;

fn row_json(v: Json) -> Vec<Json> {
    vec![serde_json::json!({ "c": v })]
}
fn row_cypher(v: CypherValue) -> Vec<CypherValue> {
    vec![CypherValue::Map(
        [("c".to_string(), v)].into_iter().collect(),
    )]
}
/// Several reference rows, one per value — the multi-row form of
/// [`row_json`].
fn rows_json(values: &[Json]) -> Vec<Json> {
    values.iter().cloned().flat_map(row_json).collect()
}
/// Several relate rows, one per name — the multi-row form of
/// [`row_cypher`].
fn rows_cypher(names: &[&str]) -> Vec<CypherValue> {
    names
        .iter()
        .flat_map(|s| row_cypher(CypherValue::Str((*s).into())))
        .collect()
}

#[test]
fn unordered_rows_compare_as_a_bag() {
    let left = vec![serde_json::json!({"n": 1}), serde_json::json!({"n": 2})];
    let right = vec![
        CypherValue::Map(
            [("n".to_string(), CypherValue::Int(2))]
                .into_iter()
                .collect(),
        ),
        CypherValue::Map(
            [("n".to_string(), CypherValue::Int(1))]
                .into_iter()
                .collect(),
        ),
    ];
    assert_eq!(compare_rows(&left, &right, false), Relation::Exact);
}

#[test]
fn ordered_rows_compare_positionally() {
    let left = row_json(Json::Number(serde_json::Number::from(1)));
    let right = row_cypher(CypherValue::Int(2));
    assert!(matches!(
        compare_rows(&left, &right, true),
        Relation::Different(_)
    ));
}

#[test]
fn floats_compare_with_epsilon() {
    let left = row_json(serde_json::json!(0.30000000000000004));
    let right = row_cypher(CypherValue::Float(0.3));
    assert_eq!(compare_rows(&left, &right, false), Relation::Exact);
}

#[test]
fn neo4j_typed_dates_fold_to_strings() {
    // Both tag spellings fold; the real v2 wire shape is
    // `{"$type", "_value"}` (cf. relate-load's extract.rs).
    for typed in [
        serde_json::json!({"$type": "Date", "_value": "2024-03-14"}),
        serde_json::json!({"__type": "date", "_value": "2024-03-14"}),
    ] {
        let left = row_json(typed);
        let right = row_cypher(CypherValue::Str("2024-03-14".into()));
        assert_eq!(compare_rows(&left, &right, false), Relation::Exact);
    }
}

#[test]
fn unknown_type_tags_stay_objects_and_diff_visibly() {
    // An unknown `$type` must not collapse to a string: it stays an
    // object and diffs against a plain string on the engine side.
    let v = serde_json::json!({"$type": "Bogus", "_value": "2024-03-14"});
    assert!(normalize_neo4j(&v).is_object());
    let left = row_json(v);
    let right = row_cypher(CypherValue::Str("2024-03-14".into()));
    assert!(matches!(
        compare_rows(&left, &right, false),
        Relation::Different(_)
    ));
}

#[test]
fn non_finite_floats_fold_to_strings_not_null() {
    assert_eq!(
        normalize_cypher(&CypherValue::Float(f64::NAN)),
        Json::String("NaN".into())
    );
    assert_eq!(
        normalize_cypher(&CypherValue::Float(f64::INFINITY)),
        Json::String("Inf".into())
    );
    assert_eq!(
        normalize_cypher(&CypherValue::Float(f64::NEG_INFINITY)),
        Json::String("-Inf".into())
    );
    // NaN no longer conflates with a genuine null.
    let left = row_json(Json::Null);
    let right = row_cypher(CypherValue::Float(f64::NAN));
    assert!(matches!(
        compare_rows(&left, &right, false),
        Relation::Different(_)
    ));
}

#[test]
fn integers_compare_exactly_above_the_f64_mantissa() {
    // 2^53 and 2^53+1 are distinct integers but the same f64.
    let left = row_json(serde_json::json!(9007199254740992i64));
    let right = row_cypher(CypherValue::Int(9007199254740993));
    assert!(matches!(
        compare_rows(&left, &right, false),
        Relation::Different(_)
    ));
    // …and equal large integers still match.
    let right = row_cypher(CypherValue::Int(9007199254740992));
    assert_eq!(compare_rows(&left, &right, false), Relation::Exact);
}

#[test]
fn float_epsilon_applies_only_with_a_genuine_float() {
    // 0.1 + 0.2 vs 0.3: classic f64 drift, inside the epsilon.
    let left = row_json(serde_json::json!(0.1f64 + 0.2f64));
    let right = row_cypher(CypherValue::Float(0.3));
    assert_eq!(compare_rows(&left, &right, false), Relation::Exact);
}

#[test]
fn point_normalizes_to_the_engine_display_spelling() {
    let p = relate_model::value::PointValue::wgs84_2d(-73.9857, 40.7484);
    assert_eq!(
        normalize_cypher(&CypherValue::Point(p)),
        Json::String("point({srid: 4326, x: -73.9857, y: 40.7484})".into())
    );
}

/// F-1-1: the typed-JSON wire point
/// (`{"$type": "Point", "_value": "SRID=…;POINT …"}`) and the engine
/// `Point` canonicalize to the same `Display` string — 2D and 3D.
#[test]
fn typed_wire_points_compare_equal_to_engine_points() {
    let left = row_json(serde_json::json!({
        "$type": "Point",
        "_value": "SRID=7203;POINT (1.0 2.0)"
    }));
    let right = row_cypher(CypherValue::Point(PointValue::cartesian_2d(1.0, 2.0)));
    assert_eq!(compare_rows(&left, &right, false), Relation::Exact);

    let left = row_json(serde_json::json!({
        "$type": "Point",
        "_value": "SRID=9157;POINT Z (1.0 2.0 3.0)"
    }));
    let right = row_cypher(CypherValue::Point(PointValue::cartesian_3d(1.0, 2.0, 3.0)));
    assert_eq!(compare_rows(&left, &right, false), Relation::Exact);
}

/// F-1-1: the suite's reference client requests plain JSON
/// (`Accept: application/json`), where a POINT arrives as a bare EWKT
/// string — that spelling canonicalizes identically, 2D and 3D.
#[test]
fn plain_json_wire_points_compare_equal_to_engine_points() {
    let left = row_json(serde_json::json!("SRID=4326;POINT (-73.9857 40.7484)"));
    let right = row_cypher(CypherValue::Point(PointValue::wgs84_2d(-73.9857, 40.7484)));
    assert_eq!(compare_rows(&left, &right, false), Relation::Exact);

    let left = row_json(serde_json::json!(
        "SRID=4979;POINT Z (-73.9857 40.7484 100.5)"
    ));
    let right = row_cypher(CypherValue::Point(PointValue::wgs84_3d(
        -73.9857, 40.7484, 100.5,
    )));
    assert_eq!(compare_rows(&left, &right, false), Relation::Exact);
}

#[test]
fn differing_or_unparseable_points_still_diff() {
    // Same CRS, different coordinate.
    let left = row_json(serde_json::json!("SRID=7203;POINT (1.0 2.0)"));
    let right = row_cypher(CypherValue::Point(PointValue::cartesian_2d(1.0, 3.0)));
    assert!(matches!(
        compare_rows(&left, &right, false),
        Relation::Different(_)
    ));
    // Different SRID, same coordinates.
    let left = row_json(serde_json::json!("SRID=4326;POINT (1.0 2.0)"));
    assert!(matches!(
        compare_rows(&left, &right, false),
        Relation::Different(_)
    ));
    // SRID-less WKT cannot recover the CRS: it stays as-is and diffs
    // visibly rather than guessing a coordinate space.
    let left = row_json(serde_json::json!("POINT (1.0 2.0)"));
    assert!(matches!(
        compare_rows(&left, &right, false),
        Relation::Different(_)
    ));
    // A plain string that merely mentions POINT is not EWKT.
    let left = row_json(serde_json::json!("a POINT (1.0 2.0) of interest"));
    assert!(matches!(
        compare_rows(&left, &right, false),
        Relation::Different(_)
    ));
}

/// F-1-11: an order-only list difference is `UpToListOrder` carrying
/// the strict failure text, not a diff.
#[test]
fn list_order_only_difference_is_up_to_list_order() {
    let left = row_json(serde_json::json!([1, 2]));
    let right = row_cypher(CypherValue::List(vec![
        CypherValue::Int(2),
        CypherValue::Int(1),
    ]));
    match compare_rows(&left, &right, false) {
        Relation::UpToListOrder(strict) => {
            assert!(strict.contains("with no relate match"), "{strict}");
        }
        other => panic!("expected UpToListOrder, got {other:?}"),
    }
}

/// F-1-11: with `ordered`, an `ORDER BY` tie (rows equal but swapped)
/// is `UpToListOrder` too; the strict text is the positional one.
#[test]
fn ordered_row_ties_are_up_to_list_order() {
    let left = vec![serde_json::json!({"n": 1}), serde_json::json!({"n": 2})];
    let right = vec![
        CypherValue::Map(
            [("n".to_string(), CypherValue::Int(2))]
                .into_iter()
                .collect(),
        ),
        CypherValue::Map(
            [("n".to_string(), CypherValue::Int(1))]
                .into_iter()
                .collect(),
        ),
    ];
    match compare_rows(&left, &right, true) {
        Relation::UpToListOrder(strict) => {
            assert!(strict.contains("row 0 differs"), "{strict}");
        }
        other => panic!("expected UpToListOrder, got {other:?}"),
    }
}

/// F-1-11: the strict row-count failure text survives in `Different`.
#[test]
fn row_count_mismatch_keeps_the_strict_text() {
    let left = vec![serde_json::json!({"n": 1}), serde_json::json!({"n": 2})];
    let right = row_cypher(CypherValue::Int(1));
    match compare_rows(&left, &right, false) {
        Relation::Different(strict) => {
            assert_eq!(strict, "row count differs: neo4j 2 vs relate 1");
        }
        other => panic!("expected Different, got {other:?}"),
    }
}

/// A row mismatch renders both sides as compact serde_json — never
/// Rust Debug (`Array [String("…")]`), which the relate side used
/// to produce via `{:?}`.
#[test]
fn mismatch_messages_render_compact_json_not_rust_debug() {
    let left = row_json(serde_json::json!(["a", "b"]));
    let right = row_cypher(CypherValue::List(vec![
        CypherValue::Str("x".into()),
        CypherValue::Str("y".into()),
    ]));
    match compare_rows(&left, &right, false) {
        Relation::Different(msg) => {
            assert!(!msg.contains("Array ["), "{msg}");
            assert!(!msg.contains("String("), "{msg}");
            assert!(msg.contains(r#"["a","b"]"#), "{msg}");
            assert!(msg.contains(r#"["x","y"]"#), "{msg}");
        }
        other => panic!("expected Different, got {other:?}"),
    }
}

/// A wide row cannot flood the report: each side of a mismatch is
/// elided at the report's cap with the `…` marker.
#[test]
fn mismatch_messages_elide_long_rows() {
    let long = "x".repeat(ELIDE_AT * 2);
    let left = row_json(serde_json::json!([long]));
    let right = row_cypher(CypherValue::List(vec![CypherValue::Str("y".into())]));
    match compare_rows(&left, &right, false) {
        Relation::Different(msg) => {
            assert!(msg.contains('…'), "{msg}");
            assert!(msg.len() < ELIDE_AT * 3, "{}", msg.len());
        }
        other => panic!("expected Different, got {other:?}"),
    }
}

/// Both sides of a no-match message render row by row, each in a `row N:`
/// frame naming the row's result-set index — never one nested list whose
/// outer brackets read as an extra value-nesting level (the LABS-1299
/// misdiagnosis) — and never more rows per side than [`ROWS_SHOWN`].
#[test]
fn no_match_message_frames_rows_one_by_one() {
    let left = row_json(serde_json::json!(["a", "b"]));
    let right = row_cypher(CypherValue::List(vec![CypherValue::Str("x".into())]));
    match compare_rows(&left, &right, false) {
        Relation::Different(msg) => {
            assert!(msg.contains("with no relate match"), "{msg}");
            assert!(
                msg.contains("neo4j rows with no relate match (1): row 0:"),
                "{msg}"
            );
            assert!(msg.contains("relate rows left over (1): row 0:"), "{msg}");
            assert!(!msg.contains("relate rows: ["), "{msg}");
        }
        other => panic!("expected Different, got {other:?}"),
    }
}

/// The difference the message reports is the completed bag match's, not a
/// mid-loop snapshot: left rows that match consume their right rows even
/// after an earlier left row failed, so the frames name exactly the rows
/// left over at the end, by their true result-set indices.
#[test]
fn no_match_message_reports_the_completed_difference() {
    // Left row 0 matches nothing; rows 1-4 consume relate rows 0-3, so
    // the leftover is relate row 4 — not the five a snapshot would show.
    let left = rows_json(&[
        serde_json::json!(["a", "b"]),
        serde_json::json!("x"),
        serde_json::json!("y"),
        serde_json::json!("z"),
        serde_json::json!("w"),
    ]);
    let right = rows_cypher(&["x", "y", "z", "w", "v"]);
    match compare_rows(&left, &right, false) {
        Relation::Different(msg) => {
            assert!(
                msg.contains("neo4j rows with no relate match (1): row 0:"),
                "{msg}"
            );
            assert!(msg.contains("relate rows left over (1): row 4:"), "{msg}");
        }
        other => panic!("expected Different, got {other:?}"),
    }
}

/// With more rows on a side than [`ROWS_SHOWN`], the message names the
/// rest by count instead of rendering them.
#[test]
fn no_match_message_names_the_unrendered_rows_by_count() {
    // No left row matches any right row, so both sides of the difference
    // hold all five rows.
    let left = rows_json(&[
        serde_json::json!(["a", "b"]),
        serde_json::json!(["c", "d"]),
        serde_json::json!(["e", "f"]),
        serde_json::json!(["g", "h"]),
        serde_json::json!(["i", "j"]),
    ]);
    let right = rows_cypher(&["x", "y", "z", "w", "v"]);
    match compare_rows(&left, &right, false) {
        Relation::Different(msg) => {
            assert!(msg.contains("neo4j rows with no relate match (5)"), "{msg}");
            assert!(msg.contains("relate rows left over (5)"), "{msg}");
            assert!(msg.contains("row 2:"), "{msg}");
            assert!(!msg.contains("row 3:"), "{msg}");
            assert!(msg.contains("(+2 more)"), "{msg}");
        }
        other => panic!("expected Different, got {other:?}"),
    }
}

/// A frame's `row N` is the row's index in its result set, not its
/// position among the still-unmatched rows: after an earlier successful
/// match removes an entry from the pool, the frame keeps naming the
/// original index (the drift the round-1 review caught).
#[test]
fn no_match_message_names_true_result_indices() {
    let left = rows_json(&[serde_json::json!("a"), serde_json::json!("c")]);
    let right = rows_cypher(&["a", "b"]);
    match compare_rows(&left, &right, false) {
        Relation::Different(msg) => {
            // Relate row 0 matched the first reference row; the row left
            // over is result-set row 1, whatever the pool order is.
            assert!(msg.contains("relate rows left over (1): row 1:"), "{msg}");
        }
        other => panic!("expected Different, got {other:?}"),
    }
}

#[test]
fn vector_normalizes_to_a_structured_encoding_never_a_string() {
    let v = CypherValue::Vector {
        coord: relate_model::VectorCoordKind::Float32,
        dim: 2,
        values: vec![CypherValue::Float(1.5), CypherValue::Float(2.5)],
    };
    assert_eq!(
        normalize_cypher(&v),
        serde_json::json!({"type": "FLOAT32", "dim": 2, "values": [1.5, 2.5]})
    );
}

#[test]
fn geometry_normalizes_to_a_structured_encoding() {
    let g = relate_model::GeometryValue::LineString {
        coordinates: vec![[0.0, 0.0], [1.0, 1.0]],
        srid: 7203,
    };
    assert_eq!(
        normalize_cypher(&CypherValue::Geometry(g)),
        serde_json::json!({"type": "LineString", "coordinates": [[0.0, 0.0], [1.0, 1.0]]})
    );
}

#[test]
fn temporal_values_normalize_via_their_iso_display() {
    let d = CypherValue::Duration(relate_model::value::IsoDuration::new(0, 5, 3661, 0));
    assert_eq!(normalize_cypher(&d), Json::String("P5DT1H1M1S".into()));
}

#[test]
fn entities_compare_by_content_not_id() {
    // Query API v2 node: {elementId, labels, properties}
    let left = row_json(serde_json::json!({
        "elementId": "4:aaa:0",
        "labels": ["Person"],
        "properties": {"name": "Tom", "born": 1962}
    }));
    let right = row_cypher(CypherValue::Node {
        id: 99,
        labels: vec!["Person".into()],
        properties: [
            ("name".to_string(), CypherValue::Str("Tom".into())),
            ("born".to_string(), CypherValue::Int(1962)),
        ]
        .into_iter()
        .collect(),
    });
    assert_eq!(compare_rows(&left, &right, false), Relation::Exact);

    // relationship: {elementId, startNodeElementId, endNodeElementId, type, properties}
    let left = row_json(serde_json::json!({
        "elementId": "5:aaa:0",
        "startNodeElementId": "4:aaa:0",
        "endNodeElementId": "4:aaa:1",
        "type": "ACTED_IN",
        "properties": {"roles": ["Neo"]}
    }));
    let right = row_cypher(CypherValue::Relationship {
        id: 7,
        start_id: 98,
        end_id: 99,
        rel_type: "ACTED_IN".into(),
        properties: [(
            "roles".to_string(),
            CypherValue::List(vec![CypherValue::Str("Neo".into())]),
        )]
        .into_iter()
        .collect(),
    });
    assert_eq!(compare_rows(&left, &right, false), Relation::Exact);
}

fn neo4j_node(element_id: &str, labels: &[&str], props: Json) -> Json {
    serde_json::json!({
        "elementId": element_id,
        "labels": labels,
        "properties": props,
    })
}

fn neo4j_rel(element_id: &str, rel_type: &str, props: Json) -> Json {
    serde_json::json!({
        "elementId": element_id,
        "startNodeElementId": "4:aaa:0",
        "endNodeElementId": "4:aaa:1",
        "type": rel_type,
        "properties": props,
    })
}

fn cypher_node(id: i64, labels: &[&str], props: Vec<(&str, CypherValue)>) -> CypherValue {
    CypherValue::Node {
        id,
        labels: labels.iter().map(|s| s.to_string()).collect(),
        properties: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    }
}

fn cypher_rel(id: i64, rel_type: &str, props: Vec<(&str, CypherValue)>) -> CypherValue {
    CypherValue::Relationship {
        id,
        start_id: 0,
        end_id: 1,
        rel_type: rel_type.into(),
        properties: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    }
}

/// The LABS-171 case: a 1-hop path is `[node, rel, node]` on the
/// Query API v2 wire but `Path { nodes, relationships }` on the
/// relate side; both must canonicalize to the same form.
#[test]
fn one_hop_paths_compare_equal_across_wire_shapes() {
    let left = row_json(serde_json::json!([
        neo4j_node("4:aaa:0", &["Person"], serde_json::json!({"name": "Tom"})),
        neo4j_rel("5:aaa:0", "KNOWS", serde_json::json!({"since": 2020})),
        neo4j_node("4:aaa:1", &["Person"], serde_json::json!({"name": "Ann"})),
    ]));
    let right = row_cypher(CypherValue::Path {
        nodes: vec![
            cypher_node(
                0,
                &["Person"],
                vec![("name", CypherValue::Str("Tom".into()))],
            ),
            cypher_node(
                1,
                &["Person"],
                vec![("name", CypherValue::Str("Ann".into()))],
            ),
        ],
        relationships: vec![cypher_rel(
            0,
            "KNOWS",
            vec![("since", CypherValue::Int(2020))],
        )],
    });
    assert_eq!(compare_rows(&left, &right, false), Relation::Exact);
    // rows_subset (the LIMIT/SKIP check) shares the normalization.
    assert!(rows_subset(&left, &right).is_ok());
}

#[test]
fn multi_hop_paths_compare_equal_across_wire_shapes() {
    let left = row_json(serde_json::json!([
        neo4j_node("4:aaa:0", &["A"], serde_json::json!({"n": 1})),
        neo4j_rel("5:aaa:0", "R1", serde_json::json!({})),
        neo4j_node("4:aaa:1", &["B"], serde_json::json!({"n": 2})),
        neo4j_rel("5:aaa:1", "R2", serde_json::json!({"w": 0.5})),
        neo4j_node("4:aaa:2", &["C"], serde_json::json!({"n": 3})),
    ]));
    let right = row_cypher(CypherValue::Path {
        nodes: vec![
            cypher_node(0, &["A"], vec![("n", CypherValue::Int(1))]),
            cypher_node(1, &["B"], vec![("n", CypherValue::Int(2))]),
            cypher_node(2, &["C"], vec![("n", CypherValue::Int(3))]),
        ],
        relationships: vec![
            cypher_rel(0, "R1", vec![]),
            cypher_rel(1, "R2", vec![("w", CypherValue::Float(0.5))]),
        ],
    });
    assert_eq!(compare_rows(&left, &right, false), Relation::Exact);
}

#[test]
fn differing_paths_still_diff() {
    let wire = serde_json::json!([
        neo4j_node("4:aaa:0", &["Person"], serde_json::json!({"name": "Tom"})),
        neo4j_rel("5:aaa:0", "KNOWS", serde_json::json!({})),
        neo4j_node("4:aaa:1", &["Person"], serde_json::json!({"name": "Ann"})),
    ]);
    let tom = cypher_node(
        0,
        &["Person"],
        vec![("name", CypherValue::Str("Tom".into()))],
    );
    let ann = cypher_node(
        1,
        &["Person"],
        vec![("name", CypherValue::Str("Ann".into()))],
    );

    // Node property differs.
    let right = row_cypher(CypherValue::Path {
        nodes: vec![
            tom.clone(),
            cypher_node(
                1,
                &["Person"],
                vec![("name", CypherValue::Str("Bob".into()))],
            ),
        ],
        relationships: vec![cypher_rel(0, "KNOWS", vec![])],
    });
    assert!(matches!(
        compare_rows(&row_json(wire.clone()), &right, false),
        Relation::Different(_)
    ));

    // Relationship type differs.
    let right = row_cypher(CypherValue::Path {
        nodes: vec![tom.clone(), ann.clone()],
        relationships: vec![cypher_rel(0, "LIKES", vec![])],
    });
    assert!(matches!(
        compare_rows(&row_json(wire.clone()), &right, false),
        Relation::Different(_)
    ));

    // Hop count differs.
    let right = row_cypher(CypherValue::Path {
        nodes: vec![tom, ann.clone(), ann],
        relationships: vec![
            cypher_rel(0, "KNOWS", vec![]),
            cypher_rel(1, "KNOWS", vec![]),
        ],
    });
    assert!(matches!(
        compare_rows(&row_json(wire), &right, false),
        Relation::Different(_)
    ));
}

#[test]
fn non_path_rows_compare_as_before() {
    // Lists stay positional (no reordering) in the strict comparison —
    // a reordered list is an order-only difference, not a diff.
    let left = row_json(serde_json::json!([1, 2]));
    let right = row_cypher(CypherValue::List(vec![
        CypherValue::Int(2),
        CypherValue::Int(1),
    ]));
    assert!(matches!(
        compare_rows(&left, &right, false),
        Relation::UpToListOrder(_)
    ));
    // …and equal in order.
    let right = row_cypher(CypherValue::List(vec![
        CypherValue::Int(1),
        CypherValue::Int(2),
    ]));
    assert_eq!(compare_rows(&left, &right, false), Relation::Exact);
    // Scalars and maps are untouched by the path canonicalization.
    let left = row_json(serde_json::json!({"a": 1, "b": "x"}));
    let right = row_cypher(CypherValue::Map(
        [
            ("a".to_string(), CypherValue::Int(1)),
            ("b".to_string(), CypherValue::Str("x".into())),
        ]
        .into_iter()
        .collect(),
    ));
    assert_eq!(compare_rows(&left, &right, false), Relation::Exact);
}

/// A rendered JSON payload at the cap is untouched; one past the cap is
/// cut in the middle — head and tail kept, the cut size named — so a
/// deep difference at either end stays visible (LABS-1299).
#[test]
fn elide_cuts_the_middle_and_names_the_cut() {
    let exact = "x".repeat(ELIDE_AT);
    assert_eq!(elide(&exact), exact);
    let out = elide(&"x".repeat(ELIDE_AT + 10));
    assert!(out.contains("…(+10 chars)…"), "{out}");
    // Only the head and tail shares survive the cut.
    assert_eq!(out.chars().filter(|c| *c == 'x').count(), ELIDE_AT, "{out}");
    let tagged = format!(
        "{}{}{}",
        "h".repeat(ELIDE_HEAD),
        "…".repeat(50),
        "t".repeat(ELIDE_TAIL)
    );
    let out = elide(&tagged);
    assert!(out.starts_with(&"h".repeat(ELIDE_HEAD)), "{out}");
    assert!(out.ends_with(&"t".repeat(ELIDE_TAIL)), "{out}");
}

// ---- db.schema.visualization normalization (LABS-257 follow-up) ----

/// A relate string list for the fixtures below.
fn str_list(ss: &[&str]) -> CypherValue {
    CypherValue::List(
        ss.iter()
            .map(|s| CypherValue::Str((*s).into()))
            .collect::<Vec<_>>(),
    )
}

/// A relate schema-entry map for the fixtures below.
fn entry(label: &str, constraints: &[&str], indexes: &[&str]) -> CypherValue {
    CypherValue::Map(
        [
            (":LABELS".into(), str_list(&[label])),
            ("constraints".into(), str_list(constraints)),
            ("indexes".into(), str_list(indexes)),
            ("name".into(), CypherValue::Str(label.into())),
        ]
        .into_iter()
        .collect(),
    )
}

const NEO_CONSTRAINT: &str = "Constraint( id=30, name='constraint_2876b1c0', type='NODE_PROPERTY_UNIQUENESS', schema=(:_Bloom_Perspective_ {id}), ownedIndex=31, graphTypeDependence='UNDESIGNATED' )";
const CYL_CONSTRAINT: &str = "Constraint( name='constraint_2876b1c0', type='NODE_PROPERTY_UNIQUENESS', schema=(:_Bloom_Perspective_ {id}) )";

/// Neo4j's Constraint repr folds to relate's canonical form (server
/// internals id/ownedIndex/graphTypeDependence dropped), and a label-entry
/// list compares by content regardless of catalog walk order.
#[test]
fn schema_visualization_rows_normalize_to_content() {
    let neo4j = serde_json::json!([[
        {":LABELS": ["Place"], "constraints": [], "indexes": ["location", "id", "name"], "name": "Place"},
        {":LABELS": ["_Bloom_Perspective_"], "constraints": [NEO_CONSTRAINT], "indexes": [], "name": "_Bloom_Perspective_"}
    ]]);
    let cypher = vec![CypherValue::List(vec![CypherValue::Str(
        CYL_CONSTRAINT.into(),
    )])];
    assert_eq!(
        compare_rows_schema_viz(
            serde_json::json!([[NEO_CONSTRAINT]]).as_array().unwrap(),
            &cypher,
        ),
        super::Relation::Exact,
        "constraint reprs fold equal"
    );
    // label-entry list order is normalized away (Place vs Bloom order)
    let cypher_labels = vec![CypherValue::List(
        vec![
            entry(
                "_Bloom_Perspective_",
                &["Constraint( name='constraint_2876b1c0', type='NODE_PROPERTY_UNIQUENESS', schema=(:_Bloom_Perspective_ {id}) )"],
                &[],
            ),
            entry("Place", &[], &["location", "id", "name"]),
        ],
    )];
    assert_eq!(
        compare_rows_schema_viz(neo4j.as_array().unwrap(), &cypher_labels),
        super::Relation::Exact
    );
}

/// A real result list whose elements carry a "name" key but are NOT
/// schema-visualization entries must not be reordered.
#[test]
fn plain_rows_with_name_keys_keep_their_order() {
    let neo4j = serde_json::json!([{"name": "b", "n": 2}, {"name": "a", "n": 1}]);
    let cypher = vec![
        CypherValue::Map(
            [
                ("name".into(), CypherValue::Str("b".into())),
                ("n".into(), CypherValue::Int(2)),
            ]
            .into_iter()
            .collect(),
        ),
        CypherValue::Map(
            [
                ("name".into(), CypherValue::Str("a".into())),
                ("n".into(), CypherValue::Int(1)),
            ]
            .into_iter()
            .collect(),
        ),
    ];
    assert_eq!(
        compare_rows(neo4j.as_array().unwrap(), &cypher, true),
        super::Relation::Exact,
        "ordered comparison keeps the real row order"
    );
}

/// The `indexes`/`constraints` cell lists inside an entry map are catalog
/// order artifacts; they sort as content (while a real string list in a
/// non-entry map is untouched).
#[test]
fn schema_entry_cells_sort_by_content() {
    let neo4j = serde_json::json!([[{
        ":LABELS": ["Person"], "constraints": [],
        "indexes": ["name", "confirmedtime", "healthstatus", "id"], "name": "Person"
    }]]);
    let cypher = vec![CypherValue::List(vec![entry(
        "Person",
        &[],
        &["confirmedtime", "healthstatus", "id", "name"],
    )])];
    assert_eq!(
        compare_rows_schema_viz(neo4j.as_array().unwrap(), &cypher),
        super::Relation::Exact
    );
}
