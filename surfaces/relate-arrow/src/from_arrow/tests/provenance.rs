use super::super::*;
use super::fixtures::*;
use crate::record_batch::result_to_record_batch;
use arrow_array::{
    builder::{BinaryBuilder, ListBuilder, StringBuilder},
    ArrayRef, Int32Array,
};
use relate_model::value::PointValue;
use std::{collections::HashMap, sync::Arc};

#[test]
fn marked_graph_struct_layouts_reject_duplicate_and_extra_fields() {
    for value in [node(42, "Person", "Alice"), rel(7, 1, 2)] {
        let (cols, rows) = one_col("x", vec![value]);
        let source = result_to_record_batch(&cols, &rows).unwrap();
        let duplicate = appended_child(&source, "id", Arc::new(Int64Array::from(vec![99])));
        let error = record_batch_to_rows(&duplicate).unwrap_err();
        assert!(error.to_string().contains("duplicate"), "{error}");
        assert!(error.to_string().contains("id"), "{error}");

        let extra = appended_child(
            &source,
            "unexpected",
            Arc::new(StringArray::from(vec!["discarded"])),
        );
        let error = record_batch_to_rows(&extra).unwrap_err();
        assert!(error.to_string().contains("unexpected"), "{error}");

        // Layout is checked even when every parent row is null.
        // Keep the marked schema from a real value but mask its parent row.
        let original = source
            .column(0)
            .as_any()
            .downcast_ref::<StructArray>()
            .unwrap();
        let masked = Arc::new(
            StructArray::try_new(
                original.fields().clone(),
                original.columns().to_vec(),
                Some(arrow::buffer::NullBuffer::from(vec![false])),
            )
            .unwrap(),
        );
        let masked = batch(source.schema().field(0).clone(), masked);
        assert_eq!(
            record_batch_to_rows(&masked).unwrap().1[0]["x"],
            CypherValue::Null
        );
        let malformed_null = appended_child(&masked, "id", Arc::new(Int64Array::from(vec![99])));
        assert!(record_batch_to_rows(&malformed_null).is_err());
    }
}

#[test]
fn typed_graph_payloads_reject_unrecognized_fields() {
    let field = with_metadata(
        &Field::new("x", DataType::Utf8, true),
        RELATE_ENCODING_KEY,
        ENCODING_TYPED_JSON_V1,
    );
    for payload in [
        r#"{"type":"NODE","value":{"id":1,"labels":[],"properties":{},"extra":7}}"#,
        r#"{"type":"RELATIONSHIP","value":{"id":1,"type":"KNOWS","start_node":2,"end_node":3,"properties":{},"extra":7}}"#,
        r#"{"type":"NODE","value":{"id":1,"labels":[],"extra":7}}"#,
        r#"{"type":"RELATIONSHIP","value":{"id":1,"type":"KNOWS","start_node":2,"end_node":3,"extra":7}}"#,
    ] {
        let error = record_batch_to_rows(&batch(
            field.clone(),
            Arc::new(StringArray::from(vec![payload])),
        ))
        .unwrap_err();
        assert!(
            error.to_string().contains("payload requires exactly"),
            "{error}"
        );
    }
}

#[test]
fn marked_structs_reject_invalid_children_without_fabricated_ids() {
    let (cols, rows) = one_col("x", vec![node(42, "Person", "Alice")]);
    let source = result_to_record_batch(&cols, &rows).unwrap();
    for (name, child) in [
        ("id", Arc::new(Int64Array::from(vec![None])) as ArrayRef),
        ("id", Arc::new(StringArray::from(vec!["42"])) as ArrayRef),
        (
            "labels",
            Arc::new(StringArray::from(vec!["Person"])) as ArrayRef,
        ),
        (
            "properties",
            Arc::new(StringArray::from(vec![None::<&str>])) as ArrayRef,
        ),
        (
            "properties",
            Arc::new(StringArray::from(vec!["not JSON"])) as ArrayRef,
        ),
        (
            "properties",
            Arc::new(StringArray::from(vec![
                r#"{"p":{"type":"INTEGER","value":"wrong"}}"#,
            ])) as ArrayRef,
        ),
    ] {
        let err = record_batch_to_rows(&bad_child(&source, name, child)).unwrap_err();
        assert!(err.to_string().contains(name), "{name}: {err}");
    }
    let mut labels = ListBuilder::new(StringBuilder::new());
    labels.values().append_null();
    labels.append(true);
    assert!(
        record_batch_to_rows(&bad_child(&source, "labels", Arc::new(labels.finish()))).is_err()
    );

    let (cols, rows) = one_col("x", vec![rel(11, 1, 2)]);
    let source = result_to_record_batch(&cols, &rows).unwrap();
    for name in ["id", "start_node", "end_node"] {
        for child in [
            Arc::new(Int64Array::from(vec![None])) as ArrayRef,
            Arc::new(StringArray::from(vec!["1"])) as ArrayRef,
        ] {
            assert!(
                record_batch_to_rows(&bad_child(&source, name, child)).is_err(),
                "{name}"
            );
        }
    }
    for child in [
        Arc::new(StringArray::from(vec![None::<&str>])) as ArrayRef,
        Arc::new(Int64Array::from(vec![1])) as ArrayRef,
    ] {
        assert!(record_batch_to_rows(&bad_child(&source, "type", child)).is_err());
    }
    assert!(record_batch_to_rows(&bad_child(
        &source,
        "properties",
        Arc::new(Int32Array::from(vec![1]))
    ))
    .is_err());
}

#[test]
fn empty_and_all_null_graph_structs_preflight_child_layouts() {
    for (graph, child_name) in [
        (node(42, "Person", "Alice"), "id"),
        (rel(7, 1, 2), "start_node"),
    ] {
        let (cols, rows) = one_col("x", vec![graph]);
        let source = result_to_record_batch(&cols, &rows).unwrap();
        let empty = batch(
            source.schema().field(0).clone(),
            source.column(0).slice(0, 0),
        );
        let malformed = bad_child(
            &empty,
            child_name,
            Arc::new(StringArray::from(Vec::<Option<&str>>::new())),
        );
        let err = record_batch_to_rows(&malformed).unwrap_err();
        assert!(err.to_string().contains(child_name), "{err}");

        let original = source
            .column(0)
            .as_any()
            .downcast_ref::<StructArray>()
            .unwrap();
        let masked = Arc::new(
            StructArray::try_new(
                original.fields().clone(),
                original.columns().to_vec(),
                Some(arrow::buffer::NullBuffer::from(vec![false])),
            )
            .unwrap(),
        );
        let malformed = bad_child(
            &batch(source.schema().field(0).clone(), masked),
            child_name,
            Arc::new(StringArray::from(vec!["not an ID"])),
        );
        let err = record_batch_to_rows(&malformed).unwrap_err();
        assert!(err.to_string().contains(child_name), "{err}");
    }
}

#[test]
fn missing_marked_graph_ids_error_before_decoding_any_rows() {
    let cases: [(CypherValue, &[&str]); 2] = [
        (node(42, "Person", "Alice"), &["id"]),
        (rel(7, 1, 2), &["id", "start_node", "end_node"]),
    ];
    for (graph, required) in cases {
        let (cols, rows) = one_col("x", vec![graph]);
        let source = result_to_record_batch(&cols, &rows).unwrap();
        let empty = batch(
            source.schema().field(0).clone(),
            source.column(0).slice(0, 0),
        );
        let original = source
            .column(0)
            .as_any()
            .downcast_ref::<StructArray>()
            .unwrap();
        let masked = Arc::new(
            StructArray::try_new(
                original.fields().clone(),
                original.columns().to_vec(),
                Some(arrow::buffer::NullBuffer::from(vec![false])),
            )
            .unwrap(),
        );
        let all_null = batch(source.schema().field(0).clone(), masked);
        for &name in required {
            for input in [&source, &empty, &all_null] {
                let err = record_batch_to_rows(&missing_child(input, name)).unwrap_err();
                assert!(err.to_string().contains("missing"), "{err}");
                assert!(err.to_string().contains(name), "{err}");
            }
        }
    }
}

#[test]
fn nested_child_metadata_and_physical_types_are_preflighted() {
    // No cell is decoded in either batch: validation must visit the list
    // child itself rather than relying on a non-null parent row.
    for empty in [true, false] {
        let mut labels = ListBuilder::new(StringBuilder::new());
        if !empty {
            labels.append(false);
        }
        let labels = labels.finish();
        let item = with_metadata(
            &Field::new("item", DataType::Utf8, true),
            RELATE_ENCODING_KEY,
            "typed-json-v2",
        );
        let bad_metadata = ListArray::try_new(
            Arc::new(item),
            labels.offsets().clone(),
            labels.values().clone(),
            labels.nulls().cloned(),
        )
        .unwrap();
        let field = Field::new("list", bad_metadata.data_type().clone(), true);
        let err = record_batch_to_rows(&batch(field, Arc::new(bad_metadata))).unwrap_err();
        assert!(err.to_string().contains("typed-json-v2"), "{err}");

        let mut binary = ListBuilder::new(BinaryBuilder::new());
        if !empty {
            binary.append(false);
        }
        let binary = binary.finish();
        let field = Field::new("list", binary.data_type().clone(), true);
        let err = record_batch_to_rows(&batch(field, Arc::new(binary))).unwrap_err();
        assert!(err.to_string().contains("geoarrow.wkb"), "{err}");
    }

    // A marked node's labels must physically be List<Utf8>, even if the
    // entire struct is empty and the list has no values to downcast.
    let (cols, rows) = one_col("x", vec![node(1, "P", "A")]);
    let source = result_to_record_batch(&cols, &rows).unwrap();
    let empty = batch(
        source.schema().field(0).clone(),
        source.column(0).slice(0, 0),
    );
    let mut labels = ListBuilder::new(arrow_array::builder::Int32Builder::new());
    let bad = labels.finish();
    let err = record_batch_to_rows(&bad_child(&empty, "labels", Arc::new(bad))).unwrap_err();
    assert!(err.to_string().contains("labels"), "{err}");

    // Even a recognized marker is incompatible with graph-label strings:
    // the graph decoder reads them verbatim, so it must not ignore it.
    let original = empty
        .column(0)
        .as_any()
        .downcast_ref::<StructArray>()
        .unwrap();
    let labels = original
        .column_by_name("labels")
        .unwrap()
        .as_any()
        .downcast_ref::<ListArray>()
        .unwrap();
    let item = with_metadata(
        &Field::new("item", DataType::Utf8, true),
        RELATE_ENCODING_KEY,
        ENCODING_TYPED_JSON_V1,
    );
    let marked_labels = ListArray::try_new(
        Arc::new(item),
        labels.offsets().clone(),
        labels.values().clone(),
        labels.nulls().cloned(),
    )
    .unwrap();
    let err =
        record_batch_to_rows(&bad_child(&empty, "labels", Arc::new(marked_labels))).unwrap_err();
    assert!(err.to_string().contains("labels"), "{err}");
}

#[test]
fn nested_struct_spatial_children_reuse_preflighted_srid() {
    let point = CypherValue::Point(PointValue {
        srid: 4326,
        x: 1.5,
        y: -2.25,
        z: None,
    });
    let (cols, rows) = one_col("p", vec![point.clone(), CypherValue::Null]);
    let source = result_to_record_batch(&cols, &rows).unwrap();
    let child = source.schema().field(0).clone();
    let nested = Arc::new(
        StructArray::try_new(
            vec![Arc::new(child.clone())].into(),
            vec![source.column(0).clone()],
            None,
        )
        .unwrap(),
    );
    let field = Field::new("container", nested.data_type().clone(), true);
    let decoded = record_batch_to_rows(&batch(field, nested)).unwrap().1;
    assert_eq!(
        decoded[0]["container"],
        CypherValue::Map(IndexMap::from([("p".into(), point)]))
    );
    assert_eq!(
        decoded[1]["container"],
        CypherValue::Map(IndexMap::from([("p".into(), CypherValue::Null)]))
    );

    let mut metadata = child.metadata().clone();
    metadata.insert(RELATE_SRID_KEY.into(), "bad".into());
    let invalid = Arc::new(
        StructArray::try_new(
            vec![Arc::new(child.with_metadata(metadata))].into(),
            vec![source.column(0).slice(0, 0)],
            None,
        )
        .unwrap(),
    );
    let field = Field::new("container", invalid.data_type().clone(), true);
    let err = record_batch_to_rows(&batch(field, invalid)).unwrap_err();
    assert!(err.to_string().contains("relate.srid"), "{err}");
}

#[test]
fn unmarked_structs_are_maps_and_fixed_size_lists_are_lists() {
    let (cols, rows) = one_col("x", vec![node(9, "Person", "Alice")]);
    let source = result_to_record_batch(&cols, &rows).unwrap();
    let unmarked = batch(
        Field::new("x", source.column(0).data_type().clone(), true),
        source.column(0).clone(),
    );
    let (_, rows) = record_batch_to_rows(&unmarked).unwrap();
    let CypherValue::Map(map) = &rows[0]["x"] else {
        panic!("unmarked node promoted")
    };
    assert_eq!(map["id"], CypherValue::Int(9));
    assert!(matches!(map["labels"], CypherValue::List(_)));
    assert!(matches!(map["properties"], CypherValue::Str(_)));

    let (cols, rows) = one_col(
        "x",
        vec![CypherValue::Vector {
            coord: VectorCoordKind::Int8,
            dim: 2,
            values: vec![CypherValue::Int(1), CypherValue::Int(2)],
        }],
    );
    let source = result_to_record_batch(&cols, &rows).unwrap();
    let unmarked = batch(
        Field::new("x", source.column(0).data_type().clone(), true),
        source.column(0).clone(),
    );
    assert_eq!(
        record_batch_to_rows(&unmarked).unwrap().1[0]["x"],
        CypherValue::List(vec![CypherValue::Int(1), CypherValue::Int(2)])
    );
}

#[test]
fn metadata_is_exact_and_rejected_even_for_null_cells() {
    let source = Arc::new(StringArray::from(vec![None, Some("123")])) as ArrayRef;
    let field = Field::new("x", DataType::Utf8, true);
    assert_eq!(
        record_batch_to_rows(&batch(field.clone(), source.clone()))
            .unwrap()
            .1[1]["x"],
        CypherValue::Str("123".into())
    );
    for encoding in [
        ENCODING_JSON,
        ENCODING_TAGGED,
        "typed-json-v2",
        "typed-json-v1 ",
    ] {
        assert!(
            record_batch_to_rows(&batch(
                with_metadata(&field, RELATE_ENCODING_KEY, encoding),
                source.clone()
            ))
            .is_err(),
            "{encoding}"
        );
    }
    let future = batch(
        with_metadata(&field, RELATE_VALUE_TYPE_KEY, "node-v2"),
        source.clone(),
    );
    assert!(record_batch_to_rows(&future).is_err());
    let conflict = field.clone().with_metadata(HashMap::from([
        (RELATE_ENCODING_KEY.into(), ENCODING_TYPED_JSON_V1.into()),
        (RELATE_VALUE_TYPE_KEY.into(), VALUE_TYPE_NODE_V1.into()),
    ]));
    assert!(record_batch_to_rows(&batch(conflict, source)).is_err());
    let (cols, rows) = one_col("x", vec![node(1, "P", "A")]);
    let marked = result_to_record_batch(&cols, &rows).unwrap();
    let wrong = with_metadata(
        marked.schema().field(0),
        RELATE_VALUE_TYPE_KEY,
        VALUE_TYPE_VECTOR_V1,
    );
    assert!(record_batch_to_rows(&batch(wrong, marked.column(0).clone())).is_err());
    let empty = batch(
        with_metadata(&field, RELATE_ENCODING_KEY, ENCODING_JSON),
        Arc::new(StringArray::from(Vec::<Option<&str>>::new())),
    );
    assert!(record_batch_to_rows(&empty).is_err());
}

#[test]
fn typed_json_rejects_malformed_tags_and_preserves_lookalike_maps_and_lists() {
    let mut lookalike = IndexMap::new();
    lookalike.insert("id".into(), CypherValue::Int(3));
    lookalike.insert(
        "labels".into(),
        CypherValue::List(vec![CypherValue::Str("P".into())]),
    );
    lookalike.insert("properties".into(), CypherValue::Map(IndexMap::new()));
    let map = CypherValue::Map(lookalike.clone());
    let list = CypherValue::List(vec![map.clone(), map.clone(), map.clone()]);
    assert_eq!(roundtrip(vec![map.clone(), list.clone()]), vec![map, list]);
    let field = with_metadata(
        &Field::new("x", DataType::Utf8, true),
        RELATE_ENCODING_KEY,
        ENCODING_TYPED_JSON_V1,
    );
    for s in [
        "no JSON",
        r#"{"type":"INTEGER","value":"1"}"#,
        r#"{"type":"NODE","value":{"id":null,"labels":[],"properties":{}}}"#,
        r#"{"type":"PATH","value":{"nodes":[],"relationships":[]}}"#,
        r#"{"type":"LIST","value":[{"type":"BOOLEAN","value":7}]}"#,
        r#"{"type":"NULL","value":1}"#,
        r#"{"type":"STRING","value":"a","extra":true}"#,
    ] {
        let error =
            record_batch_to_rows(&batch(field.clone(), Arc::new(StringArray::from(vec![s]))))
                .unwrap_err();
        assert!(error.to_string().contains("row 0"), "{s}: {error}");
    }
}

#[test]
fn spatial_metadata_and_wkb_are_fallible_but_nulls_survive() {
    let mut be = vec![0];
    be.extend_from_slice(&1u32.to_be_bytes());
    be.extend_from_slice(&1.5f64.to_be_bytes());
    be.extend_from_slice(&(-2.25f64).to_be_bytes());
    let field = Field::new("x", DataType::Binary, true).with_metadata(HashMap::from([
        (
            "ARROW:extension:name".into(),
            GEOARROW_EXTENSION_NAME.into(),
        ),
        (RELATE_SRID_KEY.into(), "4326".into()),
    ]));
    let array = Arc::new(BinaryArray::from(vec![None, Some(be.as_slice())])) as ArrayRef;
    let rows = record_batch_to_rows(&batch(field.clone(), array))
        .unwrap()
        .1;
    assert_eq!(rows[0]["x"], CypherValue::Null);
    assert_eq!(
        rows[1]["x"],
        CypherValue::Point(PointValue {
            srid: 4326,
            x: 1.5,
            y: -2.25,
            z: None
        })
    );
    for bytes in [&b"invalid"[..], &be[..20]] {
        assert!(record_batch_to_rows(&batch(
            field.clone(),
            Arc::new(BinaryArray::from(vec![Some(bytes)]))
        ))
        .is_err());
    }
    let mut bad_nested = vec![1];
    bad_nested.extend_from_slice(&4u32.to_le_bytes()); // MultiPoint
    bad_nested.extend_from_slice(&1u32.to_le_bytes());
    bad_nested.extend_from_slice(&be);
    bad_nested[10..14].copy_from_slice(&2u32.to_be_bytes()); // nested LineString
    assert!(record_batch_to_rows(&batch(
        field.clone(),
        Arc::new(BinaryArray::from(vec![Some(bad_nested.as_slice())]))
    ))
    .is_err());
    let bad_srid = field.clone().with_metadata(HashMap::from([
        (
            "ARROW:extension:name".into(),
            GEOARROW_EXTENSION_NAME.into(),
        ),
        (RELATE_SRID_KEY.into(), "bad".into()),
    ]));
    let missing_srid = field.clone().with_metadata(HashMap::from([(
        "ARROW:extension:name".into(),
        GEOARROW_EXTENSION_NAME.into(),
    )]));
    for wrong in [
        Field::new("x", DataType::Binary, true),
        bad_srid,
        missing_srid,
        with_metadata(&field, "ARROW:extension:name", "geoarrow.wkb-v2"),
    ] {
        assert!(record_batch_to_rows(&batch(
            wrong,
            Arc::new(BinaryArray::from(vec![None::<&[u8]>]))
        ))
        .is_err());
    }
    let wrong_array = Int32Array::from(vec![1]);
    assert!(validate_field(&field, &wrong_array).is_err());
}

#[test]
fn invalid_vector_children_error_and_real_nulls_survive() {
    let (cols, rows) = one_col(
        "x",
        vec![
            CypherValue::Null,
            CypherValue::Vector {
                coord: VectorCoordKind::Int8,
                dim: 2,
                values: vec![CypherValue::Int(1), CypherValue::Int(2)],
            },
        ],
    );
    let source = result_to_record_batch(&cols, &rows).unwrap();
    assert_eq!(
        record_batch_to_rows(&source).unwrap().1[0]["x"],
        CypherValue::Null
    );
    let bad = FixedSizeListArray::try_new(
        Arc::new(Field::new("item", DataType::Int8, true)),
        2,
        Arc::new(Int8Array::from(vec![Some(1), None])),
        None,
    )
    .unwrap();
    let marked = with_metadata(
        &Field::new("x", bad.data_type().clone(), true),
        RELATE_VALUE_TYPE_KEY,
        VALUE_TYPE_VECTOR_V1,
    );
    assert!(record_batch_to_rows(&batch(marked, Arc::new(bad))).is_err());
    let wrong = FixedSizeListArray::try_new(
        Arc::new(Field::new("item", DataType::Utf8, true)),
        1,
        Arc::new(StringArray::from(vec!["a"])),
        None,
    )
    .unwrap();
    let marked = with_metadata(
        &Field::new("x", wrong.data_type().clone(), true),
        RELATE_VALUE_TYPE_KEY,
        VALUE_TYPE_VECTOR_V1,
    );
    assert!(record_batch_to_rows(&batch(marked, Arc::new(wrong))).is_err());
}

#[test]
fn invalid_non_null_temporal_cells_do_not_decode_as_null() {
    let date = batch(
        Field::new("d", DataType::Date32, true),
        Arc::new(Date32Array::from(vec![i32::MAX])),
    );
    assert!(record_batch_to_rows(&date).is_err());
    let time = batch(
        Field::new("t", DataType::Time64(TimeUnit::Nanosecond), true),
        Arc::new(Time64NanosecondArray::from(vec![i64::MAX])),
    );
    assert!(record_batch_to_rows(&time).is_err());
    assert!(parse_fixed_offset("+é:00").is_err());
}

#[test]
fn timestamp_timezone_metadata_rejects_bad_signed_offsets_even_without_values() {
    for offset in [
        "+00:60", "-12:99", "+é:00", "+24:00", "-1:00", "+01:2", "+999:00",
    ] {
        for values in [vec![], vec![None], vec![Some(0)]] {
            let arr = TimestampNanosecondArray::from(values).with_timezone(offset);
            let err = record_batch_to_rows(&batch(
                Field::new("ts", arr.data_type().clone(), true),
                Arc::new(arr),
            ))
            .unwrap_err();
            assert!(err.to_string().contains("field 'ts'"), "{err}");
            assert!(err.to_string().contains(offset), "{err}");
        }
    }
}

#[test]
fn timestamp_timezone_metadata_rejects_punctuation_on_empty_null_and_populated_batches() {
    for offset in ["+0/:30", "+/2:30", "+02:/0", "+02:3/"] {
        for values in [vec![], vec![None], vec![Some(0)]] {
            let arr = TimestampNanosecondArray::from(values).with_timezone(offset);
            let err = record_batch_to_rows(&batch(
                Field::new("ts", arr.data_type().clone(), true),
                Arc::new(arr),
            ))
            .unwrap_err();
            assert!(err.to_string().contains("field 'ts'"), "{err}");
            assert!(err.to_string().contains(offset), "{err}");
        }
    }
}

#[test]
fn timestamp_timezone_metadata_keeps_fixed_utc_and_named_zones() {
    for (zone, expected_offset, expected_name) in [
        ("+02:30", 9_000, None),
        ("-05:45", -20_700, None),
        ("Z", 0, None),
        ("UTC", 0, None),
        ("utc", 0, None),
        ("Europe/Stockholm", 0, Some("Europe/Stockholm")),
    ] {
        let arr = TimestampNanosecondArray::from(vec![Some(0), None]).with_timezone(zone);
        let rows = record_batch_to_rows(&batch(
            Field::new("ts", arr.data_type().clone(), true),
            Arc::new(arr),
        ))
        .unwrap()
        .1;
        match &rows[0]["ts"] {
            CypherValue::DateTime(dt, name) => {
                assert_eq!(dt.timestamp_nanos_opt(), Some(0));
                assert_eq!(dt.offset().local_minus_utc(), expected_offset);
                assert_eq!(name.as_deref(), expected_name);
            }
            other => panic!("expected DateTime, got {other:?}"),
        }
        assert_eq!(rows[1]["ts"], CypherValue::Null);
    }
}

#[test]
fn malformed_typed_properties_fail_instead_of_defaulting() {
    assert!(
        typed_json::decode_props(r#"{"p":{"type":"POINT","value":{"__type":"point","x":1}}}"#,)
            .is_err()
    );
    assert!(typed_json::decode_props("not JSON").is_err());
    assert!(typed_json::decode_props(r#"{"p":{"__type":"duration","months":1}}"#).is_err());
}
