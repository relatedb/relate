use super::super::*;
use super::one_col;
use relate_model::value::{IsoDuration, PointValue};
use relate_model::VectorCoords;
use serde_json::Value;

#[test]
fn node_maps_to_struct_with_id() {
    let mut props = IndexMap::new();
    props.insert("name".to_string(), CypherValue::Str("Alice".into()));
    let (cols, rows) = one_col(
        "n",
        vec![CypherValue::Node {
            id: 42,
            labels: vec!["Person".into(), "User".into()],
            properties: props,
        }],
    );
    let batch = result_to_record_batch(&cols, &rows).unwrap();
    match batch.schema().field(0).data_type() {
        DataType::Struct(fields) => {
            assert!(fields.iter().any(|f| f.name() == "id"));
            assert!(fields.iter().any(|f| f.name() == "labels"));
            assert!(fields.iter().any(|f| f.name() == "properties"));
        }
        other => panic!("expected Struct, got {other:?}"),
    }
    assert_eq!(
        batch
            .schema()
            .field(0)
            .metadata()
            .get(RELATE_VALUE_TYPE_KEY)
            .map(String::as_str),
        Some(VALUE_TYPE_NODE_V1)
    );
    let arr = batch
        .column(0)
        .as_any()
        .downcast_ref::<StructArray>()
        .unwrap();
    let ids = arr
        .column_by_name("id")
        .unwrap()
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap();
    assert_eq!(ids.value(0), 42);
    let props = arr
        .column_by_name("properties")
        .unwrap()
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(props.value(0)).unwrap(),
        serde_json::json!({"name":{"type":"STRING","value":"Alice"}})
    );
}

#[test]
fn relationship_marker_and_typed_properties() {
    let mut properties = IndexMap::new();
    properties.insert("count".into(), CypherValue::Int(3));
    let (cols, rows) = one_col(
        "r",
        vec![CypherValue::Relationship {
            id: 9,
            start_id: 4,
            end_id: 5,
            rel_type: "KNOWS".into(),
            properties,
        }],
    );
    let batch = result_to_record_batch(&cols, &rows).unwrap();
    assert_eq!(
        batch
            .schema()
            .field(0)
            .metadata()
            .get(RELATE_VALUE_TYPE_KEY)
            .map(String::as_str),
        Some(VALUE_TYPE_RELATIONSHIP_V1)
    );
    let arr = batch
        .column(0)
        .as_any()
        .downcast_ref::<StructArray>()
        .unwrap();
    let props = arr
        .column_by_name("properties")
        .unwrap()
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(props.value(0)).unwrap(),
        serde_json::json!({"count":{"type":"INTEGER","value":3}})
    );
}

#[test]
fn marked_graph_properties_reject_invalid_nested_values_in_single_and_chunked_batches() {
    let bad_props = IndexMap::from([(
        "nested".into(),
        CypherValue::List(vec![CypherValue::Float(f64::INFINITY)]),
    )]);
    for bad in [
        CypherValue::Node {
            id: 1,
            labels: vec![],
            properties: bad_props.clone(),
        },
        CypherValue::Relationship {
            id: 2,
            start_id: 1,
            end_id: 1,
            rel_type: "R".into(),
            properties: bad_props.clone(),
        },
    ] {
        let (cols, rows) = one_col("graph", vec![CypherValue::Null, bad]);
        for err in [
            result_to_record_batch(&cols, &rows).unwrap_err(),
            result_to_record_batches(&cols, &rows, 1).unwrap_err(),
        ] {
            assert!(matches!(err, ArrowError::InvalidArgumentError(_)));
            assert!(err.to_string().contains("column 'graph'"), "{err}");
            assert!(err.to_string().contains("properties"), "{err}");
            assert!(err.to_string().contains("non-finite"), "{err}");
        }
    }
}

#[test]
fn model_payload_errors_identify_column_absolute_row_and_coordinate_path() {
    let point = CypherValue::Point(PointValue {
        srid: 7203,
        x: 1.0,
        y: f64::INFINITY,
        z: None,
    });
    let props = IndexMap::from([(
        "location".into(),
        CypherValue::List(vec![CypherValue::Int(1), point]),
    )]);
    let bad_graph = CypherValue::Node {
        id: 1,
        labels: vec![],
        properties: props,
    };
    // The bad composite this test also carried — an inexact-Float32
    // vector nested in a map — is unconstructible under ADR-0706's
    // checked constructor, so its encode error is the constructor's own
    // now; the nested geometry path below keeps the error shape pinned.
    for (bad, path) in [(bad_graph, "$.properties.location[1].y:")] {
        let (cols, rows) = one_col("payload", vec![CypherValue::Null, bad]);
        for err in [
            result_to_record_batch(&cols, &rows).unwrap_err(),
            result_to_record_batches(&cols, &rows, 1).unwrap_err(),
        ] {
            let message = err.to_string();
            assert!(message.contains("column 'payload'"), "{err}");
            assert!(message.contains("row 1"), "{err}");
            assert!(message.contains(path), "{err}");
        }
    }
}

#[test]
fn nested_graph_values_and_lookalike_maps_have_distinct_envelopes() {
    let node = CypherValue::Node {
        id: 7,
        labels: vec!["Person".into()],
        properties: IndexMap::new(),
    };
    let rel = CypherValue::Relationship {
        id: 8,
        start_id: 7,
        end_id: 7,
        rel_type: "KNOWS".into(),
        properties: IndexMap::new(),
    };
    let mut lookalike = IndexMap::new();
    lookalike.insert("id".into(), CypherValue::Int(7));
    lookalike.insert(
        "labels".into(),
        CypherValue::List(vec![CypherValue::Str("Person".into())]),
    );
    lookalike.insert("__type".into(), CypherValue::Str("point".into()));
    lookalike.insert("type".into(), CypherValue::Str("NODE".into()));
    lookalike.insert("value".into(), CypherValue::Int(42));
    let (cols, rows) = one_col(
        "data",
        vec![CypherValue::List(vec![
            CypherValue::Map(lookalike),
            CypherValue::Path {
                nodes: vec![node.clone(), node],
                relationships: vec![rel],
            },
        ])],
    );
    let batch = result_to_record_batch(&cols, &rows).unwrap();
    let arr = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let value: Value = serde_json::from_str(arr.value(0)).unwrap();
    assert_eq!(value["value"][0]["type"], "MAP");
    assert_eq!(
        value["value"][0]["value"]["__type"],
        serde_json::json!({"type":"STRING","value":"point"})
    );
    assert_eq!(
        value["value"][0]["value"]["type"],
        serde_json::json!({"type":"STRING","value":"NODE"})
    );
    assert_eq!(value["value"][1]["type"], "PATH");
    assert_eq!(value["value"][1]["value"]["nodes"][0]["type"], "NODE");
    assert_eq!(value["value"][1]["value"]["nodes"][0]["value"]["id"], 7);
    assert_eq!(
        value["value"][1]["value"]["relationships"][0]["type"],
        "RELATIONSHIP"
    );
}

#[test]
fn composite_typed_payloads_retain_temporal_spatial_and_vector_tags() {
    let mut map = IndexMap::new();
    map.insert(
        "date".into(),
        CypherValue::Date(NaiveDate::from_ymd_opt(2024, 1, 2).unwrap()),
    );
    map.insert(
        "point".into(),
        CypherValue::Point(PointValue::cartesian_2d(2.0, 3.0)),
    );
    map.insert(
        "vector".into(),
        CypherValue::vector(VectorCoordKind::Int8, VectorCoords::from_int(vec![-2])),
    );
    let (cols, rows) = one_col("data", vec![CypherValue::Map(map)]);
    let batch = result_to_record_batch(&cols, &rows).unwrap();
    let arr = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let value: Value = serde_json::from_str(arr.value(0)).unwrap();
    assert_eq!(
        value["value"]["date"],
        serde_json::json!({"type":"DATE","value":{"__type":"date","value":"2024-01-02"}})
    );
    assert_eq!(value["value"]["point"]["type"], "POINT");
    assert_eq!(value["value"]["point"]["value"]["__type"], "point");
    assert_eq!(value["value"]["vector"]["type"], "VECTOR");
    assert_eq!(value["value"]["vector"]["value"]["__type"], "vector");
    assert_eq!(
        value["value"]["vector"]["value"]["values"],
        serde_json::json!([-2])
    );
}

#[test]
fn mixed_column_is_typed_and_self_describing() {
    // B7: "123" (a string) must not be confusable with the integer 123.
    let (cols, rows) = one_col(
        "x",
        vec![
            CypherValue::Str("123".into()),
            CypherValue::Int(123),
            CypherValue::Null,
        ],
    );
    let batch = result_to_record_batch(&cols, &rows).unwrap();
    assert_eq!(batch.schema().field(0).data_type(), &DataType::Utf8);
    assert_eq!(
        batch
            .schema()
            .field(0)
            .metadata()
            .get(RELATE_ENCODING_KEY)
            .map(|s| s.as_str()),
        Some(ENCODING_TYPED_JSON_V1)
    );
    let arr = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let s0: serde_json::Value = serde_json::from_str(arr.value(0)).unwrap();
    let s1: serde_json::Value = serde_json::from_str(arr.value(1)).unwrap();
    assert!(arr.is_null(2));
    assert_eq!(s0["type"], "STRING");
    assert_eq!(s0["value"], "123");
    assert_eq!(s1["type"], "INTEGER");
    assert_eq!(s1["value"], 123);
}

#[test]
fn list_column_is_recursively_typed_json() {
    let (cols, rows) = one_col(
        "data",
        vec![CypherValue::List(vec![
            CypherValue::Int(1),
            CypherValue::Int(2),
        ])],
    );
    let batch = result_to_record_batch(&cols, &rows).unwrap();
    assert_eq!(
        batch
            .schema()
            .field(0)
            .metadata()
            .get(RELATE_ENCODING_KEY)
            .map(|s| s.as_str()),
        Some(ENCODING_TYPED_JSON_V1)
    );
    let arr = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(arr.value(0)).unwrap();
    assert_eq!(
        v,
        serde_json::json!({"type":"LIST","value":[
            {"type":"INTEGER","value":1}, {"type":"INTEGER","value":2}
        ]})
    );
}

#[test]
fn typed_json_fallbacks_share_exact_field_wire_and_roundtrip() {
    let ancient = chrono::DateTime::parse_from_rfc3339("1500-01-01T12:34:56+02:00").unwrap();
    let node = CypherValue::Node {
        id: 7,
        labels: vec![],
        properties: IndexMap::new(),
    };
    let vector =
        |xs: Vec<i64>| CypherValue::vector(VectorCoordKind::Int16, VectorCoords::from_int(xs));
    let cases = [
        (
            "homogeneous list",
            vec![
                CypherValue::List(vec![CypherValue::Int(1)]),
                CypherValue::List(vec![CypherValue::Int(2)]),
            ],
            serde_json::json!({"type":"LIST","value":[{"type":"INTEGER","value":1}]}),
        ),
        (
            "homogeneous map",
            vec![
                CypherValue::Map(IndexMap::from([("x".into(), CypherValue::Int(1))])),
                CypherValue::Map(IndexMap::new()),
            ],
            serde_json::json!({"type":"MAP","value":{"x":{"type":"INTEGER","value":1}}}),
        ),
        (
            "homogeneous path",
            vec![
                CypherValue::Path {
                    nodes: vec![node.clone()],
                    relationships: vec![],
                },
                CypherValue::Path {
                    nodes: vec![node],
                    relationships: vec![],
                },
            ],
            serde_json::json!({"type":"PATH","value":{"nodes":[{"type":"NODE","value":{"id":7,"labels":[],"properties":{}}}],"relationships":[]}}),
        ),
        (
            "incompatible scalars",
            vec![CypherValue::Int(1), CypherValue::Str("two".into())],
            serde_json::json!({"type":"INTEGER","value":1}),
        ),
        (
            "different vector dimensions",
            vec![vector(vec![1]), vector(vec![2, 3])],
            serde_json::json!({"type":"VECTOR","value":{"__type":"vector","coord":"INTEGER16","dim":1,"values":[1]}}),
        ),
        (
            "out-of-range datetime",
            vec![
                CypherValue::DateTime(ancient, None),
                CypherValue::DateTime(
                    chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00+02:00").unwrap(),
                    None,
                ),
            ],
            serde_json::json!({"type":"DATE_TIME","value":{"__type":"datetime","value":ancient.to_rfc3339()}}),
        ),
        (
            "out-of-range localdatetime",
            vec![
                CypherValue::LocalDateTime(ancient.naive_local()),
                CypherValue::LocalDateTime(ancient.naive_local()),
            ],
            serde_json::json!({"type":"LOCAL_DATE_TIME","value":{"__type":"localdatetime","value":"1500-01-01T12:34:56"}}),
        ),
        (
            "out-of-range duration",
            vec![
                CypherValue::Duration(IsoDuration::new(i32::MAX as i64 + 1, 0, 0, 0)),
                CypherValue::Duration(IsoDuration::new(0, 0, i64::MAX, 0)),
            ],
            serde_json::json!({"type":"DURATION","value":{"__type":"duration","months":2147483648_i64,"days":0,"seconds":0,"nanoseconds":0}}),
        ),
    ];
    let expected_field = Field::new("data", DataType::Utf8, true).with_metadata(HashMap::from([(
        "relate.encoding".to_string(),
        "typed-json-v1".to_string(),
    )]));
    for (case, values, expected_wire) in cases {
        let mut with_null = vec![CypherValue::Null];
        with_null.extend(values);
        let (cols, rows) = one_col("data", with_null.clone());
        assert_eq!(
            schema::infer_column_type("data", &rows),
            ColType::TypedJson,
            "{case}"
        );
        let batch = result_to_record_batch(&cols, &rows).unwrap();
        assert_eq!(batch.schema().field(0), &expected_field, "{case}");
        let array = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert!(array.is_null(0), "{case}");
        assert_eq!(
            serde_json::from_str::<Value>(array.value(1)).unwrap(),
            expected_wire,
            "{case}"
        );
        let decoded = crate::from_arrow::record_batch_to_rows(&batch).unwrap().1;
        for (row, value) in decoded.iter().zip(&with_null) {
            assert_eq!(&row["data"], value, "{case}");
        }
        let chunks = result_to_record_batches(&cols, &rows, 1).unwrap();
        for (chunk, value) in chunks.iter().zip(&with_null) {
            assert_eq!(chunk.schema().field(0), &expected_field, "{case}");
            let out = crate::from_arrow::record_batch_to_rows(chunk).unwrap().1;
            assert_eq!(&out[0]["data"], value, "{case}");
        }
    }
}

#[test]
fn test_complex_value_serialized_as_json() {
    let (cols, rows) = one_col(
        "data",
        vec![CypherValue::List(vec![
            CypherValue::Int(1),
            CypherValue::Int(2),
        ])],
    );
    let batch = result_to_record_batch(&cols, &rows).unwrap();
    let data = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let val = data.value(0);
    assert!(val.contains('1'));
    assert!(val.contains('2'));
}
