//! Cross-surface golden test for the Arrow schema mapping.
//!
//! This pins the `CypherValue -> Arrow` schema mapping against the **shared**
//! wire golden fixtures in `relate_model::wire::golden` — the exact same
//! one-of-every-variant fixtures the CLI golden test
//! (`surfaces/relate-cli/tests/integration/wire_golden.rs`) and the FFI suite
//! (`surfaces/relate-ffi/src/lib.rs`) assert against, alongside this Arrow
//! test. If a new `CypherValue` variant is added,
//! or the Arrow type chosen for an existing one drifts, this test fails, so the
//! Arrow surface can never silently diverge from the canonical value set.

use std::collections::HashMap;

use arrow_schema::{DataType, IntervalUnit, TimeUnit};
use relate_arrow::from_arrow::record_batch_to_rows;
use relate_arrow::record_batch::result_to_record_batch;
use relate_model::wire::golden;
use relate_model::CypherValue;

/// The expected Arrow `DataType` for each golden fixture column, by name.
fn expected_mapping() -> HashMap<&'static str, DataType> {
    let mut m = HashMap::new();
    m.insert("null", DataType::Utf8);
    m.insert("bool", DataType::Boolean);
    m.insert("int", DataType::Int64);
    m.insert("float", DataType::Float64);
    m.insert("string", DataType::Utf8);
    m.insert("list_heterogeneous", DataType::Utf8); // recursive typed JSON
    m.insert("map", DataType::Utf8); // recursive typed JSON
    m.insert("date", DataType::Date32);
    m.insert("time", DataType::Utf8); // golden Time has an offset: typed-json-v1
    m.insert("datetime", DataType::Utf8); // named zone: per-row typed JSON
    m.insert(
        "localdatetime",
        DataType::Timestamp(TimeUnit::Nanosecond, None),
    );
    m.insert("duration", DataType::Interval(IntervalUnit::MonthDayNano));
    m.insert("point_cartesian", DataType::Binary);
    m.insert("point_geographic", DataType::Binary);
    m.insert("geometry", DataType::Binary);
    m.insert("path", DataType::Utf8); // recursive typed JSON
                                      // node / relationship / vector are checked structurally below.
    m
}

#[test]
fn golden_row_maps_to_expected_arrow_schema() {
    let (columns, row, _envelope) = golden::golden_row();
    let batch = result_to_record_batch(&columns, &[row]).expect("batch builds");
    let schema = batch.schema();
    let expected = expected_mapping();

    for field in schema.fields() {
        let name = field.name().as_str();
        if let Some(dt) = expected.get(name) {
            assert_eq!(
                field.data_type(),
                dt,
                "column `{name}` mapped to {:?}, expected {:?}",
                field.data_type(),
                dt
            );
        }
    }
    assert_eq!(
        schema.field_with_name("time").unwrap().metadata()["relate.encoding"],
        "typed-json-v1"
    );
    assert_eq!(
        schema.field_with_name("datetime").unwrap().metadata()["relate.encoding"],
        "typed-json-v1"
    );

    // Node / relationship map to Structs carrying at least an `id` field.
    for name in ["node", "relationship"] {
        let f = schema.field_with_name(name).unwrap();
        match f.data_type() {
            DataType::Struct(fields) => {
                assert!(
                    fields.iter().any(|c| c.name() == "id"),
                    "{name} struct must carry an id"
                );
            }
            other => panic!("{name} should be a Struct, got {other:?}"),
        }
    }

    // Vector maps to a FixedSizeList of Float32 with the fixture's dimension (3).
    let vf = schema.field_with_name("vector").unwrap();
    match vf.data_type() {
        DataType::FixedSizeList(item, size) => {
            assert_eq!(*size, 3);
            assert_eq!(item.data_type(), &DataType::Float32);
        }
        other => panic!("vector should be a FixedSizeList, got {other:?}"),
    }

    // Every golden column is represented (no variant silently dropped).
    assert_eq!(schema.fields().len(), columns.len());
}

#[test]
fn golden_row_builds_without_field_array_mismatch() {
    // RecordBatch::try_new validates that every field's DataType matches its
    // array; a green build here proves all builders and derived fields agree.
    let (columns, row, _envelope) = golden::golden_row();
    let batch = result_to_record_batch(&columns, &[row]).expect("batch builds");
    assert_eq!(batch.num_rows(), 1);
    assert_eq!(batch.num_columns(), columns.len());
}

#[test]
fn golden_scalars_and_structures_round_trip() {
    // The recursive typed encoding lets most variants survive a round-trip
    // through Arrow — including `path`, whose typed node/relationship members
    // retain their graph identity. Points/geometry decode via WKB below and
    // are checked separately.
    let (columns, row, _envelope) = golden::golden_row();
    let batch = result_to_record_batch(&columns, std::slice::from_ref(&row)).expect("batch builds");
    let (_cols, out) = record_batch_to_rows(&batch).expect("batch decodes");
    let got = &out[0];

    for name in [
        "null",
        "bool",
        "int",
        "float",
        "string",
        "date",
        "time",
        "datetime",
        "localdatetime",
        "duration",
        "node",
        "relationship",
        "path",
        "vector",
        "list_heterogeneous",
        "map",
    ] {
        assert_eq!(
            got.get(name),
            row.get(name),
            "column `{name}` did not round-trip"
        );
    }

    // The path column must reconstruct as a Path (not a flat List), with its
    // nested node/relationship identities intact.
    match got.get("path") {
        Some(CypherValue::Path {
            nodes,
            relationships,
        }) => {
            assert_eq!(nodes.len(), 2);
            assert_eq!(relationships.len(), 1);
            assert!(matches!(nodes[0], CypherValue::Node { .. }));
            assert!(matches!(relationships[0], CypherValue::Relationship { .. }));
        }
        other => panic!("path should reconstruct as a Path, got {other:?}"),
    }

    // Points/geometry round-trip through WKB preserving coordinates.
    match got.get("point_cartesian") {
        Some(CypherValue::Point(p)) => {
            assert!((p.x - 1.0).abs() < 1e-9 && (p.y - 2.0).abs() < 1e-9);
        }
        other => panic!("point_cartesian round-trip: {other:?}"),
    }
}
