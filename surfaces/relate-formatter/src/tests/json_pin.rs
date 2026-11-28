//! F-1-6 prepass: pin every variant's exact json + jsonl + envelope rendering.
//!
//! These pins record the CURRENT behavior of `cypher_value_to_json` for every
//! `CypherValue` variant, in both JSON and JSONL modes, so the delegation to
//! `wire::value_to_wire_json` is provably behavior-preserving: the pins must
//! pass unedited after the rewrite.

use super::*;
use relate_model::VectorCoords;

/// Render one value under `mode` (JSON, JSONL or envelope) and parse the `v` field
/// back out of the first record.
fn rendered(mode: Mode, value: CypherValue) -> serde_json::Value {
    let cols = vec!["v".to_string()];
    let s = render(mode.clone(), &cols, &[vec![value]]);
    match mode {
        Mode::Json => serde_json::from_str::<serde_json::Value>(&s).unwrap()[0]["v"].clone(),
        Mode::Jsonl => serde_json::from_str::<serde_json::Value>(s.lines().next().unwrap())
            .unwrap()["v"]
            .clone(),
        Mode::Envelope => {
            serde_json::from_str::<serde_json::Value>(&s).unwrap()["rows"][0]["v"].clone()
        }
        _ => unreachable!("rendered() supports only JSON/JSONL/envelope"),
    }
}

/// Pin the exact rendering of one value: the three JSON-family modes (JSON,
/// JSONL, envelope) must agree.
fn pin_json_family(value: CypherValue, expected: serde_json::Value) {
    assert_eq!(
        rendered(Mode::Json, value.clone()),
        expected,
        "json mode drift for {value:?}"
    );
    assert_eq!(
        rendered(Mode::Jsonl, value.clone()),
        expected,
        "jsonl mode drift"
    );
    assert_eq!(
        rendered(Mode::Envelope, value),
        expected,
        "envelope mode drift"
    );
}

fn pin_node_alice() -> CypherValue {
    let mut props = IndexMap::new();
    props.insert("name".to_string(), CypherValue::Str("Alice".into()));
    props.insert("age".to_string(), CypherValue::Int(30));
    CypherValue::Node {
        id: 7,
        labels: vec!["Person".into(), "User".into()],
        properties: props,
    }
}

fn pin_node_bob() -> CypherValue {
    let mut props = IndexMap::new();
    props.insert("name".to_string(), CypherValue::Str("Bob".into()));
    CypherValue::Node {
        id: 8,
        labels: vec!["Person".into()],
        properties: props,
    }
}

fn pin_rel_knows() -> CypherValue {
    let mut props = IndexMap::new();
    props.insert("since".to_string(), CypherValue::Int(2020));
    CypherValue::Relationship {
        id: 11,
        start_id: 7,
        end_id: 8,
        rel_type: "KNOWS".into(),
        properties: props,
    }
}

#[test]
fn test_json_pin_scalar_variants() {
    pin_json_family(CypherValue::Null, serde_json::Value::Null);
    pin_json_family(CypherValue::Bool(true), serde_json::json!(true));
    pin_json_family(CypherValue::Bool(false), serde_json::json!(false));
    pin_json_family(CypherValue::Int(42), serde_json::json!(42));
    pin_json_family(CypherValue::Int(-7), serde_json::json!(-7));
    pin_json_family(CypherValue::Float(3.5), serde_json::json!(3.5));
    pin_json_family(CypherValue::Float(1.0), serde_json::json!(1.0));
    pin_json_family(CypherValue::Float(-0.0), serde_json::json!(-0.0));
    // ADR-0280: non-finite floats render as their Display spellings.
    pin_json_family(CypherValue::Float(f64::NAN), serde_json::json!("NaN"));
    pin_json_family(CypherValue::Float(f64::INFINITY), serde_json::json!("Inf"));
    pin_json_family(
        CypherValue::Float(f64::NEG_INFINITY),
        serde_json::json!("-Inf"),
    );
    pin_json_family(CypherValue::Str("hello".into()), serde_json::json!("hello"));
    pin_json_family(CypherValue::Str(String::new()), serde_json::json!(""));
}

#[test]
fn test_json_pin_temporal_variants() {
    use chrono::{FixedOffset, NaiveDate, NaiveTime, TimeZone};
    use relate_model::value::IsoDuration;

    let offset = FixedOffset::east_opt(2 * 3600).unwrap();
    let ndt = NaiveDate::from_ymd_opt(2024, 3, 14)
        .unwrap()
        .and_hms_opt(12, 30, 45)
        .unwrap();

    pin_json_family(
        CypherValue::Date(NaiveDate::from_ymd_opt(2024, 3, 14).unwrap()),
        serde_json::json!("2024-03-14"),
    );
    // Zoned time.
    pin_json_family(
        CypherValue::Time(NaiveTime::from_hms_opt(12, 30, 45).unwrap(), Some(offset)),
        serde_json::json!("12:30:45+02:00"),
    );
    // Local time (no offset, zero seconds → HH:MM).
    pin_json_family(
        CypherValue::Time(NaiveTime::from_hms_opt(12, 30, 0).unwrap(), None),
        serde_json::json!("12:30"),
    );
    // UTC offset renders as Z.
    pin_json_family(
        CypherValue::Time(
            NaiveTime::from_hms_opt(12, 30, 45).unwrap(),
            Some(FixedOffset::east_opt(0).unwrap()),
        ),
        serde_json::json!("12:30:45Z"),
    );
    pin_json_family(
        CypherValue::DateTime(
            offset.from_local_datetime(&ndt).unwrap(),
            Some("Europe/Stockholm".into()),
        ),
        serde_json::json!("2024-03-14T12:30:45+02:00[Europe/Stockholm]"),
    );
    pin_json_family(
        CypherValue::LocalDateTime(ndt),
        serde_json::json!("2024-03-14T12:30:45"),
    );
    pin_json_family(
        CypherValue::Duration(IsoDuration::new(0, 0, 3661, 0)),
        serde_json::json!("PT1H1M1S"),
    );
    pin_json_family(
        CypherValue::Duration(IsoDuration::new(14, 3, 0, 0)),
        serde_json::json!("P1Y2M3D"),
    );
}

#[test]
fn test_json_pin_point_geometry_vector_wire_forms() {
    use relate_model::value::{GeometryValue, PointValue, VectorCoordKind};

    // Point/Geometry/Vector emit the ADR-0223 wire forms (structured
    // objects), matching the graph-variant pins below (LABS-107, ADR-0367).
    pin_json_family(
        CypherValue::Point(PointValue::cartesian_2d(1.0, 2.0)),
        serde_json::json!({"crs": "cartesian", "srid": 7203, "x": 1.0, "y": 2.0}),
    );
    pin_json_family(
        CypherValue::Point(PointValue::wgs84_2d(-73.98, 40.75)),
        serde_json::json!({"crs": "wgs-84", "srid": 4326, "longitude": -73.98, "latitude": 40.75}),
    );
    pin_json_family(
        CypherValue::Point(PointValue::cartesian_3d(1.0, 2.0, 3.0)),
        serde_json::json!({"crs": "cartesian-3d", "srid": 9157, "x": 1.0, "y": 2.0, "z": 3.0}),
    );
    pin_json_family(
        CypherValue::Geometry(GeometryValue::LineString {
            coordinates: vec![[0.0, 0.0], [1.0, 1.0]],
            srid: 7203,
        }),
        serde_json::json!({"type": "LineString", "coordinates": [[0.0, 0.0], [1.0, 1.0]]}),
    );
    pin_json_family(
        CypherValue::vector(
            VectorCoordKind::Float32,
            VectorCoords::from_f32(vec![1.0, 2.0, 3.0]),
        ),
        serde_json::json!({"type": "FLOAT32", "values": [1.0, 2.0, 3.0], "dim": 3}),
    );
    pin_json_family(
        CypherValue::vector(VectorCoordKind::Int8, VectorCoords::from_int(vec![1, -2])),
        serde_json::json!({"type": "INTEGER8", "values": [1, -2], "dim": 2}),
    );
}

#[test]
fn test_json_pin_graph_variants_wire_forms() {
    // ADR-0223 wire forms, uniformly: kind-discriminated objects for
    // nodes and relationships, the alternating array for paths
    // (LABS-107, the Neo4j JSON precedent — never display strings).
    pin_json_family(
        pin_node_alice(),
        serde_json::json!({
            "kind": "node",
            "id": 7,
            "labels": ["Person", "User"],
            "properties": {"age": 30, "name": "Alice"},
        }),
    );
    pin_json_family(
        pin_rel_knows(),
        serde_json::json!({
            "kind": "relationship",
            "id": 11,
            "type": "KNOWS",
            "start_node": 7,
            "end_node": 8,
            "properties": {"since": 2020},
        }),
    );
    pin_json_family(
        CypherValue::Path {
            nodes: vec![pin_node_alice(), pin_node_bob()],
            relationships: vec![pin_rel_knows()],
        },
        serde_json::json!([
            {"kind": "node", "id": 7, "labels": ["Person", "User"],
             "properties": {"age": 30, "name": "Alice"}},
            {"kind": "relationship", "id": 11, "type": "KNOWS",
             "start_node": 7, "end_node": 8, "properties": {"since": 2020}},
            {"kind": "node", "id": 8, "labels": ["Person"],
             "properties": {"name": "Bob"}},
        ]),
    );
}

#[test]
fn test_json_pin_list_map_variants_with_nested_graph_values() {
    use chrono::NaiveDate;

    // Plain homogeneous containers.
    pin_json_family(
        CypherValue::List(vec![
            CypherValue::Int(1),
            CypherValue::Str("two".into()),
            CypherValue::Bool(true),
            CypherValue::Null,
        ]),
        serde_json::json!([1, "two", true, null]),
    );
    let mut m = IndexMap::new();
    m.insert("a".to_string(), CypherValue::Int(1));
    m.insert("b".to_string(), CypherValue::Str("x".into()));
    pin_json_family(CypherValue::Map(m), serde_json::json!({"a": 1, "b": "x"}));

    // Nested containers recurse through the one wire encoder, so a
    // nested graph value emits its ADR-0223 structured object and a
    // nested temporal keeps its ISO string (ADR-0367).
    let bare_node = CypherValue::Node {
        id: 0,
        labels: vec!["Person".into()],
        properties: IndexMap::new(),
    };
    pin_json_family(
        CypherValue::List(vec![
            CypherValue::Int(1),
            bare_node.clone(),
            CypherValue::Date(NaiveDate::from_ymd_opt(2024, 3, 14).unwrap()),
        ]),
        serde_json::json!([1, {"kind": "node", "id": 0, "labels": ["Person"], "properties": {}}, "2024-03-14"]),
    );
    let mut nested = IndexMap::new();
    nested.insert("a".to_string(), CypherValue::Int(1));
    nested.insert("n".to_string(), bare_node.clone());
    nested.insert("inner".to_string(), CypherValue::List(vec![bare_node]));
    pin_json_family(
        CypherValue::Map(nested),
        serde_json::json!({"a": 1, "n": {"kind": "node", "id": 0, "labels": ["Person"], "properties": {}}, "inner": [{"kind": "node", "id": 0, "labels": ["Person"], "properties": {}}]}),
    );
}
