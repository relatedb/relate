use super::*;

fn props(pairs: &[(&str, Value)]) -> Arc<PropertyMap> {
    let mut map = PropertyMap::new();
    for (key, value) in pairs {
        map.insert(key.to_string(), value.clone());
    }
    Arc::new(map)
}

fn ids(n: i64) -> Vec<i64> {
    (0..n).collect()
}

#[test]
fn promotes_a_key_present_on_half_the_rows() {
    let columns = derive_columns(&ids(4), |id| {
        Some(props(if id < 2 { &[("k", Value::Int(1))] } else { &[] }))
    });
    assert_eq!(
        columns,
        vec![("k".to_string(), ColumnType::Int)],
        "2 of 4 rows is exactly the half threshold"
    );
}

#[test]
fn below_half_stays_residual() {
    let columns = derive_columns(&ids(5), |id| {
        Some(props(if id < 2 {
            &[("rare", Value::Int(1))]
        } else {
            &[("common", Value::Int(2))]
        }))
    });
    assert_eq!(
        columns,
        vec![("common".to_string(), ColumnType::Int)],
        "2 of 5 rows misses the threshold; 3 of 5 meets it"
    );
}

#[test]
fn a_key_over_the_promotion_bound_stays_residual() {
    // On every row and flat — promoted by every rule but the byte bound:
    // its name would repeat through the file's metadata
    // (MAX_PROMOTED_KEY_BYTES).
    let big = "k".repeat(MAX_PROMOTED_KEY_BYTES + 1);
    let rows = props(&[(big.as_str(), Value::Int(1))]);
    let columns = derive_columns(&ids(2), |id| {
        Some(if id < 2 {
            Arc::clone(&rows)
        } else {
            props(&[])
        })
    });
    assert!(
        columns.is_empty(),
        "a key over the byte bound stays in the residual column"
    );
}

#[test]
fn mixed_flat_types_never_promote() {
    let int_row = props(&[("k", Value::Int(1))]);
    let str_row = props(&[("k", Value::String("s".into()))]);
    let columns = derive_columns(&ids(2), |id| {
        Some(if id == 0 {
            Arc::clone(&int_row)
        } else {
            Arc::clone(&str_row)
        })
    });
    assert!(columns.is_empty(), "Int and String on one key is residual");
}

#[test]
fn non_flat_values_never_promote() {
    let columns = derive_columns(&ids(3), |_| {
        Some(props(&[(
            "list",
            Value::List(Arc::new(vec![Value::Int(1)])),
        )]))
    });
    assert!(columns.is_empty(), "a list has no flat column shape");
}

#[test]
fn null_rows_are_absent_not_residual() {
    // 2 of 3 rows carry Int, the third Null: the nulls do not count,
    // so the key still meets the half threshold.
    let columns = derive_columns(&ids(3), |id| {
        Some(props(if id < 2 {
            &[("k", Value::Int(1))]
        } else {
            &[("k", Value::Null)]
        }))
    });
    assert_eq!(columns, vec![("k".to_string(), ColumnType::Int)]);
}

#[test]
fn invisible_ids_do_not_count_toward_the_denominator() {
    let columns = derive_columns(&ids(4), |id| {
        (id < 2).then(|| props(&[("k", Value::Bool(true))]))
    });
    assert_eq!(
        columns,
        vec![("k".to_string(), ColumnType::Bool)],
        "2 present of 2 visible rows promotes"
    );
}

#[test]
fn columns_come_out_in_key_order() {
    let columns = derive_columns(&ids(2), |_| {
        Some(props(&[
            ("b", Value::Int(1)),
            ("a", Value::Int(2)),
            ("c", Value::Int(3)),
        ]))
    });
    let keys: Vec<&str> = columns.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, vec!["a", "b", "c"]);
}

#[test]
fn no_rows_derive_no_columns() {
    assert!(derive_columns(&[], |_| -> Option<Arc<PropertyMap>> { None }).is_empty());
    assert!(derive_columns(&ids(3), |_| -> Option<Arc<PropertyMap>> { None }).is_empty());
}

fn node(id: i64, labels: &[&str], pairs: &[(&str, Value)]) -> Node {
    Node {
        id,
        labels: Arc::new(labels.iter().map(|l| l.to_string()).collect()),
        properties: props(pairs),
    }
}

fn field_names(schema: &Schema) -> Vec<&str> {
    schema.fields().iter().map(|f| f.name().as_str()).collect()
}

#[test]
fn wide_schema_prefixes_promoted_columns_and_keeps_the_reserved_ones() {
    let columns = vec![
        ("id".to_string(), ColumnType::Int),
        ("properties".to_string(), ColumnType::Str),
        ("name".to_string(), ColumnType::Str),
    ];
    let schema = wide_schema(Kind::Node, &columns);
    assert_eq!(
        field_names(&schema),
        vec![
            "id",
            "labels",
            "p_id",
            "p_properties",
            "p_name",
            "properties"
        ],
        "a key named id or properties is representable beside the reserved columns"
    );
    assert_eq!(schema.field(2).data_type(), &DataType::Int64);
    assert_eq!(schema.field(5).data_type(), &DataType::Utf8);
    assert_eq!(
        field_names(&wide_schema(Kind::Rel, &[])),
        vec!["id", "start_id", "end_id", "type", "properties"]
    );
}

#[test]
fn node_batch_carries_promoted_values_and_the_residual_tail() {
    let columns = vec![
        ("name".to_string(), ColumnType::Str),
        ("age".to_string(), ColumnType::Int),
    ];
    let nodes = vec![
        node(
            1,
            &["Person"],
            &[
                ("name", Value::String("a".into())),
                ("age", Value::Int(3)),
                ("odd", Value::Bool(true)),
            ],
        ),
        // `name` absent, `age` null, `odd` a non-flat list: all three
        // must land in the residual text, not in a column slot.
        node(
            2,
            &["Person", "Active"],
            &[
                ("age", Value::Null),
                ("odd", Value::List(Arc::new(vec![Value::Int(1)]))),
            ],
        ),
    ];
    let batch = node_wide_batch(&columns, &nodes).unwrap();
    assert_eq!(batch.num_rows(), 2);
    let names = batch
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(names.value(0), "a");
    assert!(names.is_null(1), "absent key is a null slot");
    let ages = batch
        .column(3)
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap();
    assert_eq!(ages.value(0), 3);
    assert!(ages.is_null(1), "a stored Null is a null slot");
    let residual = batch
        .column(4)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let residual_keys = |i: usize| -> Vec<String> {
        let text: serde_json::Value = serde_json::from_str(residual.value(i)).unwrap();
        text.as_object()
            .expect("residual is a tagged-JSON object")
            .keys()
            .cloned()
            .collect()
    };
    assert_eq!(residual_keys(0), vec!["odd".to_string()]);
    assert_eq!(
        residual_keys(1),
        vec!["odd".to_string()],
        "only unpromoted keys stay residual; a null promoted key leaves none"
    );
}

#[test]
fn labels_round_trip_in_stored_order() {
    let batch = node_wide_batch(&[], &[node(1, &["Person", "Employee", "Active"], &[])]).unwrap();
    let labels = batch
        .column(1)
        .as_any()
        .downcast_ref::<arrow::array::ListArray>()
        .unwrap();
    assert_eq!(labels.value_length(0), 3);
}

#[test]
fn a_value_of_the_wrong_shape_takes_the_residual_column() {
    // A carried column meets new data of another type: the value is
    // never coerced — its slot is null and the residual text keeps it.
    let columns = vec![("k".to_string(), ColumnType::Int)];
    let nodes = vec![
        node(1, &["L"], &[("k", Value::Int(1))]),
        node(2, &["L"], &[("k", Value::String("not an int".into()))]),
    ];
    let batch = node_wide_batch(&columns, &nodes).unwrap();
    let ints = batch
        .column(2)
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap();
    assert_eq!(ints.value(0), 1);
    assert!(ints.is_null(1), "the wrong-shaped value leaves a null slot");
    let residual = batch
        .column(3)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let keys = |i: usize| -> Vec<String> {
        let text: serde_json::Value = serde_json::from_str(residual.value(i)).unwrap();
        text.as_object()
            .expect("residual is a tagged-JSON object")
            .keys()
            .cloned()
            .collect()
    };
    assert!(
        residual.is_null(0),
        "a fully placed row carries no residual text at all"
    );
    assert_eq!(
        keys(1),
        vec!["k".to_string()],
        "the rerouted value lives in the residual text"
    );
}

#[test]
fn merge_columns_keeps_carried_keys_and_fills_with_derived() {
    let carried = vec![("z".to_string(), ColumnType::Int)];
    let derived = vec![
        ("a".to_string(), ColumnType::Bool),
        // A carried key meeting another type keeps the carried column.
        ("z".to_string(), ColumnType::Str),
    ];
    assert_eq!(
        merge_columns(&carried, derived),
        vec![
            ("a".to_string(), ColumnType::Bool),
            ("z".to_string(), ColumnType::Int),
        ]
    );
    assert_eq!(
        merge_columns(&[], vec![]),
        Vec::<(String, ColumnType)>::new()
    );
}

#[test]
fn merge_columns_never_drops_a_carried_key_at_the_cap() {
    // A carried set at the cap leaves no room; a carried set below it
    // fills with derived keys in key order, carried keys always kept.
    let full: Vec<(String, ColumnType)> = (0..MAX_PROMOTED_COLUMNS)
        .map(|i| (format!("c{i:02}"), ColumnType::Int))
        .collect();
    assert_eq!(
        merge_columns(&full, vec![("new".to_string(), ColumnType::Bool)]),
        full,
        "a derived key beyond the cap stays residual"
    );
    let carried = vec![("z".to_string(), ColumnType::Int)];
    let derived: Vec<(String, ColumnType)> = (0..MAX_PROMOTED_COLUMNS)
        .map(|i| (format!("d{i:02}"), ColumnType::Bool))
        .collect();
    let merged = merge_columns(&carried, derived);
    assert_eq!(merged.len(), MAX_PROMOTED_COLUMNS);
    assert!(
        merged.contains(&("z".to_string(), ColumnType::Int)),
        "the carried key survives the cap"
    );
}

#[test]
fn recover_columns_is_the_inverse_of_wide_schema() {
    let columns = vec![
        // The empty key too — its column is the bare prefix, and it must
        // re-open (`SET n += $map` can set one).
        ("".to_string(), ColumnType::Int),
        ("a".to_string(), ColumnType::Int),
        ("id".to_string(), ColumnType::Str),
        ("z".to_string(), ColumnType::Float),
        ("b".to_string(), ColumnType::Bool),
    ];
    for kind in [Kind::Node, Kind::Rel] {
        let schema = wide_schema(kind, &columns);
        assert_eq!(
            recover_columns(kind, &schema),
            Some(columns.clone()),
            "{kind:?} round-trips"
        );
    }
    assert_eq!(
        recover_columns(Kind::Node, &wide_schema(Kind::Node, &[])),
        Some(vec![])
    );
}

#[test]
fn recover_columns_rejects_every_off_shape() {
    let ok = wide_schema(Kind::Node, &[("a".to_string(), ColumnType::Int)]);
    // The legacy schema is not a wide one: `labels` is in the reserved
    // slot but the last field is the blob, and nothing is prefixed.
    assert_eq!(
        recover_columns(Kind::Node, &wide_schema(Kind::Rel, &[])),
        None
    );
    // The fixture helper carries the field's nullability, so `id` is
    // spelled as non-nullable the way the canonical schema builds it —
    // each shape differs from the canonical one in exactly the field its
    // case names.
    let field = |name: &str, ty: DataType, nullable: bool| Field::new(name, ty, nullable);
    let reserved = || {
        vec![
            field("id", DataType::Int64, false),
            field(
                "labels",
                DataType::List(Arc::new(field("item", DataType::Utf8, true))),
                true,
            ),
        ]
    };
    let shapes: Vec<Arc<Schema>> = vec![
        // A reserved column renamed.
        Arc::new(Schema::new({
            let mut fields = reserved();
            fields[0] = field("idx", DataType::Int64, false);
            fields.push(field("properties", DataType::Utf8, true));
            fields
        })),
        // A reserved column of the wrong type.
        Arc::new(Schema::new({
            let mut fields = reserved();
            fields[0] = field("id", DataType::Utf8, false);
            fields.push(field("properties", DataType::Utf8, true));
            fields
        })),
        // A reserved column nullable.
        Arc::new(Schema::new({
            let mut fields = reserved();
            fields[0] = field("id", DataType::Int64, true);
            fields.push(field("properties", DataType::Utf8, true));
            fields
        })),
        // A middle column with no prefix.
        Arc::new(Schema::new({
            let mut fields = reserved();
            fields.push(field("a", DataType::Int64, true));
            fields.push(field("properties", DataType::Utf8, true));
            fields
        })),
        // A middle column of a non-flat type.
        Arc::new(Schema::new({
            let mut fields = reserved();
            fields.push(field(
                "p_a",
                DataType::List(Arc::new(field("item", DataType::Utf8, true))),
                true,
            ));
            fields.push(field("properties", DataType::Utf8, true));
            fields
        })),
        // A key over the promotion byte bound.
        Arc::new(Schema::new({
            let mut fields = reserved();
            fields.push(field(
                &format!("p_{}", "k".repeat(MAX_PROMOTED_KEY_BYTES + 1)),
                DataType::Int64,
                true,
            ));
            fields.push(field("properties", DataType::Utf8, true));
            fields
        })),
        // The residual column missing.
        Arc::new(Schema::new({
            let mut fields = reserved();
            fields.push(field("p_a", DataType::Int64, true));
            fields
        })),
        // Two columns claiming one key.
        Arc::new(Schema::new({
            let mut fields = reserved();
            fields.push(field("p_a", DataType::Int64, true));
            fields.push(field("p_a", DataType::Utf8, true));
            fields.push(field("properties", DataType::Utf8, true));
            fields
        })),
    ];
    for (i, schema) in shapes.iter().enumerate() {
        assert_eq!(
            recover_columns(Kind::Node, schema),
            None,
            "shape {i} must be refused"
        );
    }
    // The writer's column cap, held on read too (ADR-0086): a part over
    // it is malformed, never a wider one to admit.
    let over_cap = {
        let mut fields = reserved();
        fields.extend(
            (0..=MAX_PROMOTED_COLUMNS).map(|i| field(&format!("p_{i}"), DataType::Int64, true)),
        );
        fields.push(field("properties", DataType::Utf8, true));
        Arc::new(Schema::new(fields))
    };
    assert_eq!(recover_columns(Kind::Node, &over_cap), None);
    assert!(
        recover_columns(Kind::Node, &ok).is_some(),
        "the valid shape still passes"
    );
}

#[test]
fn a_wide_part_round_trips_preflight_rules_and_copy() {
    use super::super::encode::with_wide_shard_writer;
    use super::super::{preflight, ShardCursor, ShardRules, MAX_FILE};

    let columns = vec![
        ("name".to_string(), ColumnType::Str),
        ("age".to_string(), ColumnType::Int),
    ];
    let nodes = vec![
        node(
            1,
            &["Person"],
            &[
                ("name", Value::String("a".into())),
                ("age", Value::Int(30)),
                ("odd", Value::Bool(true)),
            ],
        ),
        node(2, &["Person"], &[("name", Value::String("b".into()))]),
        node(3, &["Person", "Active"], &[("age", Value::Int(31))]),
    ];
    // A wide part passes the preflight a reader runs…
    let (bytes, ()) = with_wide_shard_writer(Kind::Node, &columns, MAX_FILE, |writer| {
        writer.write_rows(&node_wide_batch(&columns, &nodes)?)
    })
    .unwrap();
    // …its promoted columns read back through the rules as the same
    // set, and the kind's fixed schema reads back as none…
    let builder = preflight(bytes.clone()).unwrap();
    let rules = ShardRules::open(&builder, Kind::Node, 1..4).unwrap();
    assert_eq!(rules.columns(), columns.as_slice());
    let fixed = with_wide_shard_writer(Kind::Node, &[], MAX_FILE, |writer| {
        writer.write_rows(&node_wide_batch(&[], &nodes)?)
    })
    .unwrap()
    .0;
    let builder = preflight(fixed).unwrap();
    let rules = ShardRules::open(&builder, Kind::Node, 1..4).unwrap();
    assert!(
        rules.columns().is_empty(),
        "the fixed shape is wide with none"
    );
    // …and it serves as a clean copy source into another wide part.
    let source = ShardCursor::open_bytes("wide part", bytes.to_vec(), Kind::Node, 1..4).unwrap();
    let (copied, _cursor) = with_wide_shard_writer(Kind::Node, &columns, MAX_FILE, |writer| {
        source.take(&[1, 2, 3], writer)
    })
    .unwrap();
    assert!(!copied.is_empty());
    // …while a promoted column the target schema lacks is refused, never
    // silently dropped: its values live only in that column.
    let source = ShardCursor::open_bytes("wide part", bytes.to_vec(), Kind::Node, 1..4).unwrap();
    let refused = with_wide_shard_writer(Kind::Node, &[], MAX_FILE, |writer| {
        source.take(&[1, 2, 3], writer)
    });
    assert!(
        matches!(refused, Err(super::super::encode::ShardError::Fatal(_))),
        "a source-only column must be refused"
    );
}
