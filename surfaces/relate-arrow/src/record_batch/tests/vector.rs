use super::super::*;
use super::one_col;
use arrow_array::Float32Array;
use relate_model::VectorCoords;
use serde_json::Value;

#[test]
fn vector_maps_to_fixed_size_list() {
    let (cols, rows) = one_col(
        "v",
        vec![CypherValue::vector(
            VectorCoordKind::Float32,
            VectorCoords::from_f32(vec![1.0, 2.0, 3.0]),
        )],
    );
    let batch = result_to_record_batch(&cols, &rows).unwrap();
    match batch.schema().field(0).data_type() {
        DataType::FixedSizeList(item, size) => {
            assert_eq!(*size, 3);
            assert_eq!(item.data_type(), &DataType::Float32);
        }
        other => panic!("expected FixedSizeList, got {other:?}"),
    }
    let arr = batch
        .column(0)
        .as_any()
        .downcast_ref::<FixedSizeListArray>()
        .unwrap();
    assert_eq!(
        batch
            .schema()
            .field(0)
            .metadata()
            .get(RELATE_VALUE_TYPE_KEY)
            .map(String::as_str),
        Some(VALUE_TYPE_VECTOR_V1)
    );
    let child = arr.value(0);
    let floats = child.as_any().downcast_ref::<Float32Array>().unwrap();
    assert_eq!(floats.values(), &[1.0f32, 2.0, 3.0]);
}

/// A well-formed `Vector` value from already-coerced coordinates (the
/// canonical constructor derives `dim`, ADR-0706 — the malformed shapes
/// this helper used to build are unconstructible now, rejected at the
/// constructor instead of the arrow layer).
fn vector(coord: VectorCoordKind, values: Vec<CypherValue>) -> CypherValue {
    CypherValue::vector(
        coord,
        VectorCoords::from_cypher_values(coord, &values)
            .expect("test constructs well-formed vectors"),
    )
}

#[test]
fn malformed_vectors_return_arrow_errors_before_building_or_chunking() {
    // Since ADR-0706 the arrow layer has exactly one malformed value
    // class it still owns — a length over the child budget — plus the
    // physical-shape refusals. The width/precision/non-finite classes
    // are the canonical constructor's own (the model suite's
    // `vector_constructor_rejects_*` pins), and the `Int` family under a
    // float kind is a legal value the float rows widen (F-10-8), so it
    // encodes rather than erroring.
    // The direct builder rejects an unrepresentable Arrow list size on
    // its own (the batch-level budget is
    // `vector_child_budget_is_inclusive_checked_and_full_result`'s).    // A legal Int-family vector under a float kind encodes — through the
    // fixed-size-list child (widened by the float row) in a uniform
    // column, and through the lossless typed-json fallback in a mixed
    // one.
    let int_backed = vector(VectorCoordKind::Float64, vec![CypherValue::Int(5)]);
    let (cols, rows) = one_col("v", vec![CypherValue::Null, int_backed.clone()]);
    assert!(result_to_record_batch(&cols, &rows).is_ok());
    let (cols, rows) = one_col("v", vec![CypherValue::Int(0), int_backed.clone()]);
    assert!(result_to_record_batch(&cols, &rows).is_ok());
    let nested = CypherValue::List(vec![int_backed]);
    let (cols, rows) = one_col("nested", vec![nested]);
    assert!(result_to_record_batch(&cols, &rows).is_ok());
}

#[test]
fn vector_child_budget_is_inclusive_checked_and_full_result() {
    assert!(check_vector_child_budget("v", 4_096, 4_096).is_ok());
    for (dim, rows) in [(4_096, 4_097), (usize::MAX, 2)] {
        let error = check_vector_child_budget("v", dim, rows).unwrap_err();
        for part in [
            "column 'v'",
            &dim.to_string(),
            &rows.to_string(),
            "16777216",
        ] {
            assert!(error.to_string().contains(part), "{error}");
        }
    }

    // A single 100,000-coordinate row fits, but masked null rows need
    // 10,000 * 100,000 child slots. A chunk size of one cannot bypass it.
    let mut rows = vec![IndexMap::new(); 10_000];
    rows[0].insert(
        "v".into(),
        vector(VectorCoordKind::Int8, vec![CypherValue::Int(1); 100_000]),
    );
    let columns = vec!["v".to_string()];
    for error in [
        result_to_record_batch(&columns, &rows).unwrap_err(),
        result_to_record_batches(&columns, &rows, 1).unwrap_err(),
    ] {
        assert!(matches!(error, ArrowError::InvalidArgumentError(_)));
        for part in ["column 'v'", "100000", "10000", "16777216"] {
            assert!(error.to_string().contains(part), "{error}");
        }
    }
}

#[test]
fn float32_exact_values_preserve_bits_including_negative_zero() {
    let xs = [0.1_f32 as f64, f32::from_bits(1) as f64, -0.0_f64];
    let (cols, rows) = one_col(
        "v",
        vec![CypherValue::vector(
            VectorCoordKind::Float32,
            VectorCoords::from_f32(xs.iter().map(|&x| x as f32).collect::<Vec<_>>()),
        )],
    );
    let batch = result_to_record_batch(&cols, &rows).unwrap();
    let arr = batch
        .column(0)
        .as_any()
        .downcast_ref::<FixedSizeListArray>()
        .unwrap();
    let child = arr.value(0);
    let floats = child.as_any().downcast_ref::<Float32Array>().unwrap();
    for (i, value) in xs.into_iter().enumerate() {
        assert_eq!(floats.value(i).to_bits(), (value as f32).to_bits());
        assert_eq!((floats.value(i) as f64).to_bits(), value.to_bits());
    }
    let decoded = crate::from_arrow::record_batch_to_rows(&batch).unwrap().1;
    let CypherValue::Vector { coords, .. } = &decoded[0]["v"] else {
        panic!("expected Vector");
    };
    for (i, expected) in xs.into_iter().enumerate() {
        assert_eq!(coords.coord_f64(i).to_bits(), expected.to_bits());
    }
}

/// Every floating coordinate kind admits only a value its own precision
/// spells exactly (`VectorCoordKind::is_exact`); one that would be silently
/// rounded is rejected — since ADR-0706 at the canonical constructor,
/// where the packed family is built, not at the arrow child converter.
#[test]
fn every_float_coordinate_kind_rejects_a_non_exact_value() {
    let cases = [
        (VectorCoordKind::Float16, 0.1_f64),
        (VectorCoordKind::BFloat16, 0.1_f64),
        (VectorCoordKind::Float32, 0.1_f64),
        // FLOAT64 narrows no finite value, so its non-exact case is the
        // non-finite one.
        (VectorCoordKind::Float64, f64::INFINITY),
    ];
    for (coord, value) in cases {
        assert!(
            VectorCoords::from_cypher_values(coord, &[CypherValue::Float(value)]).is_none(),
            "{coord:?} accepted {value}"
        );
    }
}

#[test]
fn all_eight_vector_kinds_preserve_values_nulls_and_chunked_schemas() {
    let kinds = [
        VectorCoordKind::Int8,
        VectorCoordKind::Int16,
        VectorCoordKind::Int32,
        VectorCoordKind::Int64,
        VectorCoordKind::Float16,
        VectorCoordKind::BFloat16,
        VectorCoordKind::Float32,
        VectorCoordKind::Float64,
    ];
    for kind in kinds {
        let coord = if kind.is_integer() {
            CypherValue::Int(-12)
        } else {
            CypherValue::Float(-12.5)
        };
        let (cols, rows) = one_col(
            "v",
            vec![
                CypherValue::Null,
                vector(kind, vec![coord.clone()]),
                vector(kind, vec![coord]),
            ],
        );
        let single = result_to_record_batch(&cols, &rows).unwrap();
        let chunks = result_to_record_batches(&cols, &rows, 1).unwrap();
        assert_eq!(chunks.len(), 3);
        for batch in &chunks {
            assert_eq!(batch.schema(), single.schema());
            assert_eq!(
                batch
                    .schema()
                    .field(0)
                    .metadata()
                    .get(RELATE_VALUE_TYPE_KEY)
                    .map(String::as_str),
                Some(VALUE_TYPE_VECTOR_V1)
            );
        }
        let arr = single
            .column(0)
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .unwrap();
        assert!(arr.is_null(0));
        assert!(chunks[0].column(0).is_null(0));
        assert_eq!(arr.value(1).data_type(), arr.value(2).data_type());
        match kind {
            VectorCoordKind::Int8 => assert_eq!(
                arr.value(1)
                    .as_any()
                    .downcast_ref::<arrow_array::Int8Array>()
                    .unwrap()
                    .value(0),
                -12
            ),
            VectorCoordKind::Int16 => assert_eq!(
                arr.value(1)
                    .as_any()
                    .downcast_ref::<arrow_array::Int16Array>()
                    .unwrap()
                    .value(0),
                -12
            ),
            VectorCoordKind::Int32 => assert_eq!(
                arr.value(1)
                    .as_any()
                    .downcast_ref::<arrow_array::Int32Array>()
                    .unwrap()
                    .value(0),
                -12
            ),
            VectorCoordKind::Int64 => assert_eq!(
                arr.value(1)
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .unwrap()
                    .value(0),
                -12
            ),
            // The narrow float kinds widen into Arrow's Float32.
            VectorCoordKind::Float16 | VectorCoordKind::BFloat16 | VectorCoordKind::Float32 => {
                assert_eq!(
                    arr.value(1)
                        .as_any()
                        .downcast_ref::<Float32Array>()
                        .unwrap()
                        .value(0),
                    -12.5
                )
            }
            VectorCoordKind::Float64 => assert_eq!(
                arr.value(1)
                    .as_any()
                    .downcast_ref::<Float64Array>()
                    .unwrap()
                    .value(0),
                -12.5
            ),
        }
    }
}

#[test]
fn different_vector_dimensions_use_lossless_typed_columns() {
    let vector = |values: Vec<i64>| {
        CypherValue::vector(VectorCoordKind::Int16, VectorCoords::from_int(values))
    };
    let (cols, rows) = one_col("v", vec![vector(vec![1]), vector(vec![2, 3])]);
    let batches = result_to_record_batches(&cols, &rows, 1).unwrap();
    for (batch, dim) in batches.iter().zip([1, 2]) {
        assert_eq!(
            batch
                .schema()
                .field(0)
                .metadata()
                .get(RELATE_ENCODING_KEY)
                .map(String::as_str),
            Some(ENCODING_TYPED_JSON_V1)
        );
        let arr = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let payload: Value = serde_json::from_str(arr.value(0)).unwrap();
        assert_eq!(payload["type"], "VECTOR");
        assert_eq!(payload["value"]["dim"], dim);
        assert_eq!(
            payload["value"]["values"].as_array().unwrap().len(),
            dim as usize
        );
    }
}
