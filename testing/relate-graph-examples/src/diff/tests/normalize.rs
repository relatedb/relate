use super::*;
use relate_model::CypherValue;

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
    assert_eq!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Exact
    );

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
    assert_eq!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Exact
    );
}
