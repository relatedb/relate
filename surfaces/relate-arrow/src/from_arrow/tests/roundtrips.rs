use super::super::*;
use super::fixtures::*;
use crate::record_batch::{result_to_record_batch, result_to_string_batch};
use chrono::TimeZone;
use relate_model::value::{GeometryValue, IsoDuration, PointValue};

#[test]
fn test_record_batch_to_rows_basic() {
    let cols = vec!["name".to_string(), "age".to_string()];
    let mut r1 = IndexMap::new();
    r1.insert("name".to_string(), CypherValue::Str("Alice".into()));
    r1.insert("age".to_string(), CypherValue::Int(30));
    let batch = result_to_record_batch(&cols, &[r1]).unwrap();
    let (c, rows) = record_batch_to_rows(&batch).unwrap();
    assert_eq!(c, vec!["name", "age"]);
    assert_eq!(rows[0].get("name"), Some(&CypherValue::Str("Alice".into())));
    assert_eq!(rows[0].get("age"), Some(&CypherValue::Int(30)));
}

#[test]
fn scalar_roundtrips() {
    let out = roundtrip(vec![
        CypherValue::Int(42),
        CypherValue::Null,
        CypherValue::Int(7),
    ]);
    assert_eq!(out[0], CypherValue::Int(42));
    assert_eq!(out[1], CypherValue::Null);
    assert_eq!(out[2], CypherValue::Int(7));
}

#[test]
fn string_123_stays_a_string_in_mixed_column() {
    // B7: the whole point — a string "123" alongside an int must NOT come
    // back as an integer.
    let (cols, rows) = one_col(
        "x",
        vec![CypherValue::Str("123".into()), CypherValue::Int(123)],
    );
    let batch = result_to_record_batch(&cols, &rows).unwrap();
    let (_c, out) = record_batch_to_rows(&batch).unwrap();
    assert_eq!(out[0].get("x"), Some(&CypherValue::Str("123".into())));
    assert_eq!(out[1].get("x"), Some(&CypherValue::Int(123)));
}

#[test]
fn plain_numeric_string_column_stays_string() {
    // A homogeneous Str column of "123" must decode to Str, not Int.
    let out = roundtrip(vec![CypherValue::Str("123".into())]);
    assert_eq!(out[0], CypherValue::Str("123".into()));
}

#[test]
fn date_roundtrips() {
    let d = NaiveDate::from_ymd_opt(2024, 3, 14).unwrap();
    let out = roundtrip(vec![CypherValue::Date(d)]);
    assert_eq!(out[0], CypherValue::Date(d));
}

#[test]
fn localdatetime_roundtrips() {
    let ndt = NaiveDate::from_ymd_opt(2024, 3, 14)
        .unwrap()
        .and_hms_opt(12, 30, 45)
        .unwrap();
    let out = roundtrip(vec![CypherValue::LocalDateTime(ndt)]);
    assert_eq!(out[0], CypherValue::LocalDateTime(ndt));
}

#[test]
fn datetime_with_offset_roundtrips_instant() {
    let off = FixedOffset::east_opt(2 * 3600).unwrap();
    let ndt = NaiveDate::from_ymd_opt(2024, 3, 14)
        .unwrap()
        .and_hms_opt(12, 30, 45)
        .unwrap();
    let zoned = off.from_local_datetime(&ndt).unwrap();
    let out = roundtrip(vec![CypherValue::DateTime(zoned, None)]);
    match &out[0] {
        CypherValue::DateTime(dt, _) => {
            assert_eq!(dt.timestamp_nanos_opt(), zoned.timestamp_nanos_opt());
            assert_eq!(dt.offset().local_minus_utc(), 2 * 3600);
        }
        other => panic!("expected DateTime, got {other:?}"),
    }
}

#[test]
fn duration_roundtrips() {
    let d = IsoDuration::new(14, 3, 3661, 500);
    let out = roundtrip(vec![CypherValue::Duration(d.clone())]);
    assert_eq!(out[0], CypherValue::Duration(d));
}

#[test]
fn vector_roundtrips() {
    let v = CypherValue::vector(
        VectorCoordKind::Int8,
        VectorCoords::from_int(vec![1, -2, 3]),
    );
    let out = roundtrip(vec![v.clone()]);
    assert_eq!(out[0], v);
}

#[test]
fn point_roundtrips_through_wkb() {
    let p = CypherValue::Point(PointValue::cartesian_2d(1.5, 2.5));
    let out = roundtrip(vec![p]);
    match &out[0] {
        CypherValue::Point(p) => {
            assert!((p.x - 1.5).abs() < 1e-9);
            assert!((p.y - 2.5).abs() < 1e-9);
            assert_eq!(p.srid, 7203);
        }
        other => panic!("expected Point, got {other:?}"),
    }
}

#[test]
fn geometry_roundtrips_through_wkb() {
    let g = CypherValue::Geometry(GeometryValue::LineString {
        coordinates: vec![[0.0, 0.0], [1.0, 1.0]],
        srid: 4326,
    });
    let out = roundtrip(vec![g]);
    match &out[0] {
        CypherValue::Geometry(GeometryValue::LineString { coordinates, srid }) => {
            assert_eq!(coordinates.len(), 2);
            assert_eq!(*srid, 4326);
        }
        other => panic!("expected LineString, got {other:?}"),
    }
}

#[test]
fn node_roundtrips() {
    let mut props = IndexMap::new();
    props.insert("name".to_string(), CypherValue::Str("Alice".into()));
    props.insert("age".to_string(), CypherValue::Int(30));
    let n = CypherValue::Node {
        id: 42,
        labels: vec!["Person".into(), "User".into()],
        properties: props,
    };
    let out = roundtrip(vec![n.clone()]);
    assert_eq!(out[0], n);
}

#[test]
fn relationship_roundtrips() {
    let mut props = IndexMap::new();
    props.insert("since".to_string(), CypherValue::Int(2020));
    let r = CypherValue::Relationship {
        id: 11,
        start_id: 7,
        end_id: 8,
        rel_type: "KNOWS".into(),
        properties: props,
    };
    let out = roundtrip(vec![r.clone()]);
    assert_eq!(out[0], r);
}

#[test]
fn list_roundtrips() {
    let l = CypherValue::List(vec![CypherValue::Int(1), CypherValue::Int(2)]);
    let out = roundtrip(vec![l.clone()]);
    assert_eq!(out[0], l);
}

#[test]
fn list_of_nodes_preserves_node_variant() {
    // Regression: a wire-encoded List<Node> in a TypedJson column must NOT come
    // back as a List<Map> (the persistence decoder would demote it).
    let list = CypherValue::List(vec![node(1, "Person", "Alice"), node(2, "Person", "Bob")]);
    let out = roundtrip(vec![list.clone()]);
    assert_eq!(out[0], list, "List<Node> lost fidelity through Arrow");
    match &out[0] {
        CypherValue::List(items) => {
            assert!(
                matches!(items[0], CypherValue::Node { .. }),
                "element demoted from Node to {:?}",
                items[0]
            );
        }
        other => panic!("expected List, got {other:?}"),
    }
}

#[test]
fn list_of_relationships_preserves_relationship_variant() {
    let list = CypherValue::List(vec![rel(11, 1, 2), rel(12, 2, 3)]);
    let out = roundtrip(vec![list.clone()]);
    assert_eq!(
        out[0], list,
        "List<Relationship> lost fidelity through Arrow"
    );
}

#[test]
fn map_with_nested_node_preserves_node_variant() {
    let mut m = IndexMap::new();
    m.insert("who".to_string(), node(1, "Person", "Alice"));
    m.insert("count".to_string(), CypherValue::Int(5));
    let map = CypherValue::Map(m);
    let out = roundtrip(vec![map.clone()]);
    assert_eq!(out[0], map, "Map with a nested Node lost fidelity");
}

#[test]
fn path_preserves_path_variant() {
    // A wire-encoded Path (flat alternating array) must reconstruct as a
    // Path, not a flat List.
    let path = CypherValue::Path {
        nodes: vec![node(1, "Person", "Alice"), node(2, "Person", "Bob")],
        relationships: vec![rel(11, 1, 2)],
    };
    let out = roundtrip(vec![path.clone()]);
    assert_eq!(out[0], path, "Path lost fidelity through Arrow");
    assert!(
        matches!(out[0], CypherValue::Path { .. }),
        "path demoted to {:?}",
        out[0]
    );
}

#[test]
fn mixed_column_node_preserves_node_variant() {
    // A Node alongside an Int forces a TypedJson column; the Node must
    // still decode as a Node, not a Map.
    let (cols, rows) = one_col("x", vec![node(1, "Person", "Alice"), CypherValue::Int(7)]);
    let batch = result_to_record_batch(&cols, &rows).unwrap();
    let (_c, out) = record_batch_to_rows(&batch).unwrap();
    assert_eq!(out[0].get("x"), Some(&node(1, "Person", "Alice")));
    assert_eq!(out[1].get("x"), Some(&CypherValue::Int(7)));
}

#[test]
fn ordinary_list_of_ints_stays_a_list() {
    // The wire-aware decoder must not over-eagerly turn a plain list into a
    // path or its scalars into structured values.
    let l = CypherValue::List(vec![
        CypherValue::Int(1),
        CypherValue::Int(2),
        CypherValue::Int(3),
    ]);
    let out = roundtrip(vec![l.clone()]);
    assert_eq!(out[0], l);
}

#[test]
fn test_null_handling() {
    let out = roundtrip(vec![
        CypherValue::Int(1),
        CypherValue::Null,
        CypherValue::Int(3),
    ]);
    assert_eq!(out[0], CypherValue::Int(1));
    assert_eq!(out[1], CypherValue::Null);
    assert_eq!(out[2], CypherValue::Int(3));
}

#[test]
fn test_empty_batch() {
    let cols = vec!["a".to_string()];
    let rows: Vec<IndexMap<String, CypherValue>> = vec![];
    let batch = result_to_record_batch(&cols, &rows).unwrap();
    let (c, rows) = record_batch_to_rows(&batch).unwrap();
    assert_eq!(c, vec!["a"]);
    assert!(rows.is_empty());
}

#[test]
fn string_batch_still_decodes() {
    // result_to_string_batch has no encoding metadata, so its Utf8 columns
    // decode verbatim (age "30" stays the string "30").
    let cols = vec!["age".to_string()];
    let mut r = IndexMap::new();
    r.insert("age".to_string(), CypherValue::Int(30));
    let batch = result_to_string_batch(&cols, &[r]).unwrap();
    let (_c, rows) = record_batch_to_rows(&batch).unwrap();
    assert_eq!(rows[0].get("age"), Some(&CypherValue::Str("30".into())));
}
