use super::super::*;
use super::one_col;
use arrow_array::Float32Array;
use serde_json::Value;

#[test]
fn vector_maps_to_fixed_size_list() {
    let (cols, rows) = one_col(
        "v",
        vec![CypherValue::Vector {
            coord: VectorCoordKind::Float32,
            dim: 3,
            values: vec![
                CypherValue::Float(1.0),
                CypherValue::Float(2.0),
                CypherValue::Float(3.0),
            ],
        }],
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

/// A `Vector` value; `dim` is passed apart from `values` so malformed
/// declarations stay expressible.
fn vector(coord: VectorCoordKind, dim: usize, values: Vec<CypherValue>) -> CypherValue {
    CypherValue::Vector { coord, dim, values }
}

#[test]
fn malformed_vectors_return_arrow_errors_before_building_or_chunking() {
    let invalid = vec![
        vector(VectorCoordKind::Int8, 2, vec![CypherValue::Int(1)]),
        vector(VectorCoordKind::Int8, i32::MAX as usize + 1, vec![]),
        vector(
            VectorCoordKind::Int8,
            1,
            vec![CypherValue::Str("bad".into())],
        ),
        vector(VectorCoordKind::Int8, 1, vec![CypherValue::Int(128)]),
        vector(VectorCoordKind::Int16, 1, vec![CypherValue::Int(32768)]),
        vector(VectorCoordKind::Int32, 1, vec![CypherValue::Int(i64::MAX)]),
        vector(
            VectorCoordKind::Float32,
            1,
            vec![CypherValue::Float(f64::MAX)],
        ),
        vector(
            VectorCoordKind::Float32,
            1,
            vec![CypherValue::Float(1e-300)],
        ),
        vector(VectorCoordKind::Float32, 1, vec![CypherValue::Float(0.1)]),
        vector(
            VectorCoordKind::Float32,
            1,
            vec![CypherValue::Float(f64::NAN)],
        ),
        vector(
            VectorCoordKind::Float32,
            1,
            vec![CypherValue::Float(f64::INFINITY)],
        ),
        vector(VectorCoordKind::Float64, 1, vec![CypherValue::Int(5)]),
    ];
    for bad in invalid {
        let (cols, rows) = one_col("v", vec![CypherValue::Null, bad.clone()]);
        let dimension_exceeds_budget =
            matches!(&bad, CypherValue::Vector { dim, .. } if *dim > MAX_VECTOR_CHILD_ELEMENTS);
        for error in [
            result_to_record_batch(&cols, &rows).unwrap_err(),
            result_to_record_batches(&cols, &rows, 1).unwrap_err(),
        ] {
            assert!(
                matches!(error, ArrowError::InvalidArgumentError(_)),
                "{bad:?}: {error}"
            );
            if dimension_exceeds_budget {
                assert!(error.to_string().contains("child element bound"), "{error}");
            } else {
                assert!(error.to_string().contains("column 'v' row 1"), "{error}");
            }
        }
        if dimension_exceeds_budget {
            // The full-result budget runs first; the direct builder still
            // rejects an unrepresentable Arrow list size on its own.
            let err = build_vector_array(
                "v",
                VectorCoordKind::Int8,
                i32::MAX as usize + 1,
                std::slice::from_ref(&bad),
                0,
            )
            .unwrap_err();
            assert!(err.to_string().contains("exceeds i32"), "{err}");
        }
        // Malformed values cannot bypass validation via the mixed fallback.
        let (cols, rows) = one_col("v", vec![CypherValue::Int(0), bad]);
        assert!(result_to_record_batch(&cols, &rows).is_err());
    }
    let nested = CypherValue::List(vec![CypherValue::Vector {
        coord: VectorCoordKind::Int8,
        dim: 1,
        values: vec![CypherValue::Int(128)],
    }]);
    let (cols, rows) = one_col("nested", vec![nested]);
    assert!(result_to_record_batch(&cols, &rows).is_err());
}

#[test]
fn malformed_direct_vector_in_later_chunk_returns_no_partial_batches() {
    let good = CypherValue::Vector {
        coord: VectorCoordKind::Int8,
        dim: 1,
        values: vec![CypherValue::Int(7)],
    };
    let bad = CypherValue::Vector {
        coord: VectorCoordKind::Int8,
        dim: 1,
        values: vec![CypherValue::Int(128)],
    };
    let (cols, rows) = one_col("v", vec![good.clone(), good, bad]);
    assert_eq!(
        result_to_record_batches(&cols, &rows[..2], 1)
            .unwrap()
            .len(),
        2
    );
    let err = result_to_record_batches(&cols, &rows, 1).unwrap_err();
    assert!(matches!(err, ArrowError::InvalidArgumentError(_)));
    assert!(err.to_string().contains("column 'v' row 2"), "{err}");
    assert!(err.to_string().contains("out of range"), "{err}");
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
        CypherValue::Vector {
            coord: VectorCoordKind::Int8,
            dim: 100_000,
            values: vec![CypherValue::Int(1); 100_000],
        },
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
        vec![CypherValue::Vector {
            coord: VectorCoordKind::Float32,
            dim: xs.len(),
            values: xs.iter().copied().map(CypherValue::Float).collect(),
        }],
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
    let CypherValue::Vector { values, .. } = &decoded[0]["v"] else {
        panic!("expected Vector");
    };
    for (value, expected) in values.iter().zip(xs) {
        let CypherValue::Float(value) = value else {
            panic!("expected Float")
        };
        assert_eq!(value.to_bits(), expected.to_bits());
    }
}

/// Every floating coordinate kind admits only a value its own precision
/// spells exactly (`VectorCoordKind::is_exact`); one that would be silently
/// rounded is rejected instead of widened into the array.
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
        let (cols, rows) = one_col(
            "v",
            vec![CypherValue::Vector {
                coord,
                dim: 1,
                values: vec![CypherValue::Float(value)],
            }],
        );
        let error = result_to_record_batch(&cols, &rows).unwrap_err();
        assert!(
            error.to_string().contains("loses precision"),
            "{coord:?}: {error}"
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
                CypherValue::Vector {
                    coord: kind,
                    dim: 1,
                    values: vec![coord.clone()],
                },
                CypherValue::Vector {
                    coord: kind,
                    dim: 1,
                    values: vec![coord],
                },
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
    let vector = |values: Vec<i64>| CypherValue::Vector {
        coord: VectorCoordKind::Int16,
        dim: values.len(),
        values: values.into_iter().map(CypherValue::Int).collect(),
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
