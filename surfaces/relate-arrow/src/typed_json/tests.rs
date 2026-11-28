use super::*;
use relate_model::value::{GeometryValue, PointValue};
use relate_model::{VectorCoordKind, VectorCoords};

#[test]
fn graph_properties_and_composites_use_identical_typed_members() {
    let lookalike = CypherValue::Map(IndexMap::from([
        ("__type".into(), CypherValue::Str("point".into())),
        ("type".into(), CypherValue::Str("NODE".into())),
    ]));
    let props = IndexMap::from([("data".into(), lookalike.clone())]);
    let node = CypherValue::Node {
        id: 12,
        labels: vec!["N".into()],
        properties: props.clone(),
    };
    let relationship = CypherValue::Relationship {
        id: 13,
        start_id: 12,
        end_id: 12,
        rel_type: "LOOP".into(),
        properties: props.clone(),
    };
    let path = CypherValue::Path {
        nodes: vec![node.clone(), node.clone()],
        relationships: vec![relationship.clone()],
    };
    let value = CypherValue::List(vec![lookalike, node, relationship, path]);
    let encoded: Value = serde_json::from_str(&encode(&value).unwrap()).unwrap();
    assert_eq!(
        encoded["value"][1]["value"]["properties"],
        serde_json::from_str::<Value>(&encode_props(&props).unwrap()).unwrap()
    );
    assert_eq!(
        encoded["value"][1]["value"]["properties"]["data"]["type"],
        "MAP"
    );
    assert_eq!(decode(&encoded.to_string()).unwrap(), value);
    assert_eq!(decode_props(&encode_props(&props).unwrap()).unwrap(), props);
}

#[test]
fn encoding_checks_each_nested_member_instead_of_serializing_invalid_values() {
    let bad_values = [
        CypherValue::Float(f64::NAN),
        CypherValue::Point(PointValue {
            srid: 7203,
            x: 1.0,
            y: 2.0,
            z: Some(f64::INFINITY),
        }),
        CypherValue::Geometry(GeometryValue::GeometryCollection {
            srid: 7203,
            geometries: vec![GeometryValue::LineString {
                srid: 7203,
                coordinates: vec![[1.0, f64::NAN]],
            }],
        }),
        // The inexact-Float32 vector this list used to carry is
        // unconstructible under ADR-0706's checked constructor; the
        // remaining bad shapes keep the encode error pinned.
        CypherValue::Path {
            nodes: vec![],
            relationships: vec![],
        },
    ];
    for bad in bad_values {
        let props = IndexMap::from([("p".into(), CypherValue::List(vec![bad.clone()]))]);
        assert!(encode_props(&props).is_err(), "{bad:?}");
        assert!(encode(&CypherValue::Map(props)).is_err(), "{bad:?}");
    }
}

#[test]
fn scalar_and_persistence_payload_shape_is_stable() {
    let vector = CypherValue::vector(VectorCoordKind::Float32, VectorCoords::from_f32(vec![-0.0]));
    let payload = encode(&vector).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&payload).unwrap(),
        json!({
            "type": "VECTOR",
            "value": {"__type":"vector","coord":"FLOAT32","dim":1,"values":[-0.0]},
        })
    );
    assert!(payload.contains("-0.0"));
    assert_eq!(decode(&payload).unwrap(), vector);
    assert_eq!(
        encode(&CypherValue::Null).unwrap(),
        r#"{"type":"NULL","value":null}"#
    );
    assert!(decode(r#"{"type":"INTEGER","value":"1"}"#).is_err());
}

#[test]
fn spatial_and_vector_payloads_match_model_in_nested_typed_json() {
    let values = vec![
        CypherValue::Point(PointValue {
            srid: 4326,
            x: -0.0,
            y: 2.5,
            z: Some(3.0),
        }),
        CypherValue::Geometry(GeometryValue::GeometryCollection {
            srid: 4326,
            geometries: vec![GeometryValue::MultiPolygon {
                srid: 4326,
                polygons: vec![vec![vec![[1.0, -0.0], [2.0, 3.0]]]],
            }],
        }),
        CypherValue::vector(
            VectorCoordKind::Float32,
            VectorCoords::from_f32(vec![-0.0, 0.1_f32]),
        ),
    ];
    let props = IndexMap::from([("shapes".into(), CypherValue::List(values.clone()))]);
    let node = CypherValue::Node {
        id: 5,
        labels: vec!["Spatial".into()],
        properties: props.clone(),
    };
    let encoded: Value = serde_json::from_str(&encode(&node).unwrap()).unwrap();
    for (i, value) in values.iter().enumerate() {
        assert_eq!(
            encoded["value"]["properties"]["shapes"]["value"][i]["value"],
            cypher_value_to_json(value).unwrap()
        );
    }
    assert_eq!(
        encoded["value"]["properties"],
        serde_json::from_str::<Value>(&encode_props(&props).unwrap()).unwrap()
    );
    assert_eq!(decode(&encoded.to_string()).unwrap(), node);
}

#[test]
fn model_payload_errors_keep_paths_in_arrow_graph_and_composite_writes() {
    let invalid = [
        (
            CypherValue::Point(PointValue {
                srid: 7203,
                x: f64::INFINITY,
                y: 1.0,
                z: None,
            }),
            "$.x",
        ),
        (
            CypherValue::Geometry(GeometryValue::GeometryCollection {
                srid: 7203,
                geometries: vec![GeometryValue::LineString {
                    srid: 7203,
                    coordinates: vec![[1.0, f64::NAN]],
                }],
            }),
            "$.geometries[0].coordinates[0][1]",
        ),
        // The inexact and non-finite Float32 vectors this table used to
        // carry are unconstructible under ADR-0706's checked constructor;
        // their rejection is the constructor's own now (the model suite's
        // `vector_constructor_rejects_*` pins).
    ];
    for (value, path) in invalid {
        let props = IndexMap::from([("bad".into(), value)]);
        let suffix = path.strip_prefix('$').unwrap();
        let err = encode(&CypherValue::Map(props.clone())).unwrap_err();
        assert!(
            err.to_string().contains(&format!("$.bad{suffix}:")),
            "{err}"
        );
        let err = encode_props(&props).unwrap_err();
        assert!(
            err.to_string()
                .contains(&format!("$.properties.bad{suffix}:")),
            "{err}"
        );
    }
}

#[test]
fn path_payload_requires_exact_nodes_and_relationships_fields() {
    let path = CypherValue::Path {
        nodes: vec![CypherValue::Node {
            id: 1,
            labels: vec![],
            properties: IndexMap::new(),
        }],
        relationships: vec![],
    };
    let encoded: Value = serde_json::from_str(&encode(&path).unwrap()).unwrap();
    assert_eq!(decode(&encoded.to_string()).unwrap(), path);

    let mut extra = encoded.clone();
    extra["value"]["unexpected"] = json!(42);
    for malformed in [
        extra,
        {
            let mut missing = encoded.clone();
            missing["value"].as_object_mut().unwrap().remove("nodes");
            missing
        },
        {
            let mut missing = encoded.clone();
            missing["value"]
                .as_object_mut()
                .unwrap()
                .remove("relationships");
            missing
        },
    ] {
        let err = decode(&malformed.to_string()).unwrap_err();
        assert!(matches!(err, ArrowError::ParseError(_)), "{err}");
        assert!(
            err.to_string()
                .contains("PATH payload requires exactly nodes, relationships"),
            "{err}"
        );
    }
}
