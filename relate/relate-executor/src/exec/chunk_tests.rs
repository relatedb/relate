//! ADR-0468's chunk type layer tests: the lossless round-trip (every
//! `CypherValue`, nulls included, survives `Chunk::from_rows` followed by
//! `Chunk::to_rows`, the invariant pipeline breakers rely on), the
//! typed-buffer claims and their degradation rule, the operator-built
//! shapes, and LABS-645's derived zone metadata — derivation from the
//! encoded contents, the NaN bound-voiding rule, compaction's
//! re-derivation. The generated-batch contracts run at the 1000-case
//! floor (ADR-0268 §3), the shared `config_with_floor` spelling.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::sync::Arc;

use chrono::NaiveDate;
use proptest::collection::vec;
use proptest::prelude::*;
use relate_core::{
    Node, Path, PointValue, PropertyMap, Relationship, Temporal, Value, VectorCoordKind,
};

use crate::row::{Row, RowNames};

use super::chunk::{typed_kind, typed_slot, Chunk, ColumnData};
use crate::test_support::config_with_floor;

fn rows_from(names: &[&str], cells: &[Vec<Value>]) -> Vec<Row> {
    let names: Vec<String> = names.iter().map(|name| (*name).to_string()).collect();
    let row_names = RowNames::new(&names);
    cells
        .iter()
        .map(|values| Row::from_names(&row_names, values.clone()))
        .collect()
}

fn node(id: i64) -> Node {
    Node {
        id,
        labels: Arc::new(vec!["L".to_string()]),
        properties: Arc::new(PropertyMap::new()),
    }
}

fn rel(id: i64, start: i64, end: i64) -> Relationship {
    Relationship {
        id,
        start,
        end,
        rel_type: "R".to_string(),
        properties: Arc::new(PropertyMap::new()),
    }
}

#[test]
fn typed_columns_round_trip_and_claim_their_buffers() {
    let input = rows_from(
        &["n", "f", "s", "b"],
        &[
            vec![
                Value::Int(1),
                Value::Float(2.5),
                Value::String(Arc::from("a")),
                Value::Bool(true),
            ],
            vec![
                Value::Int(-2),
                Value::Float(-0.5),
                Value::String(Arc::from("bä")),
                Value::Bool(false),
            ],
        ],
    );
    let chunk = Chunk::from_rows(&input);
    assert!(matches!(chunk.column(0).data(), ColumnData::Int(_)));
    assert!(matches!(chunk.column(1).data(), ColumnData::Float(_)));
    assert!(matches!(chunk.column(2).data(), ColumnData::Utf8(_)));
    assert!(matches!(chunk.column(3).data(), ColumnData::Bool(_)));
    // The buffer-length invariant: one entry per row in every column.
    for slot in 0..4 {
        assert_eq!(chunk.column(slot).len(), chunk.len());
    }
    assert_eq!(chunk.to_rows(), input);
}

#[test]
fn classifier_tag_and_encoder_payload_agree_on_every_variant() {
    // The two halves of the Value-variant → buffer mapping are separate
    // matches by design (the classifier must not pay the encoder's
    // per-cell payload construction — an Arc clone for every String).
    // This tie test is the pin: a variant added to one half and not the
    // other fails here, loudly, instead of classifying and encoding
    // differently at runtime.
    let typed = [
        Value::Bool(true),
        Value::Int(7),
        Value::Float(0.5),
        Value::String(Arc::from("s")),
    ];
    // ... and the untyped variants, stated with the same symmetric form
    // the proptest uses — everything else forces the fallback in both
    // halves. One loop, one spelling of the agreement.
    let untyped = [
        Value::Null,
        Value::List(Arc::new(vec![Value::Int(1)])),
        Value::Node(Arc::new(node(1))),
    ];
    for value in typed.iter().chain(&untyped) {
        assert_eq!(
            typed_kind(value),
            typed_slot(value).map(|slot| slot.kind()),
            "tag and payload halves disagree on {value:?}"
        );
    }
}

proptest! {
    /// The same pin over the whole generated Value space: `value()`
    /// reaches every variant, so a variant wired into one half of the
    /// mapping and not the other fails here even when the deterministic
    /// test above never spelled it.
    #[test]
    fn classifier_and_encoder_agree_on_every_generated_value(v in value()) {
        prop_assert_eq!(typed_kind(&v), typed_slot(&v).map(|slot| slot.kind()));
    }
}

#[test]
fn nulls_survive_in_typed_and_fallback_columns() {
    let input = rows_from(
        &["n", "s", "list", "all_null"],
        &[
            vec![
                Value::Int(1),
                Value::Null,
                Value::List(Arc::new(vec![Value::Int(1)])),
                Value::Null,
            ],
            vec![
                Value::Null,
                Value::String(Arc::from("x")),
                Value::Null,
                Value::Null,
            ],
            vec![
                Value::Int(3),
                Value::Null,
                Value::List(Arc::new(vec![])),
                Value::Null,
            ],
        ],
    );
    let chunk = Chunk::from_rows(&input);
    assert!(matches!(chunk.column(0).data(), ColumnData::Int(_)));
    assert!(chunk.column(0).is_null(1));
    assert!(!chunk.column(0).is_null(0));
    assert!(!chunk.column(0).is_null(2));
    assert!(matches!(chunk.column(1).data(), ColumnData::Utf8(_)));
    assert!(chunk.column(1).is_null(0));
    assert!(!chunk.column(1).is_null(1));
    // A column with no non-null value claims no type: the fallback.
    assert!(matches!(chunk.column(3).data(), ColumnData::Value(_)));
    assert_eq!(chunk.to_rows(), input);
}

#[test]
fn heterogeneous_columns_degrade_to_the_fallback_without_coercion() {
    let input = rows_from(
        &["int_float", "int_string", "bool_node"],
        &[
            vec![Value::Int(1), Value::Int(2), Value::Bool(true)],
            vec![
                Value::Float(1.5),
                Value::String(Arc::from("s")),
                Value::Node(Arc::new(node(7))),
            ],
        ],
    );
    let chunk = Chunk::from_rows(&input);
    for slot in 0..3 {
        assert!(
            matches!(chunk.column(slot).data(), ColumnData::Value(_)),
            "column {slot} degraded to the fallback"
        );
    }
    // No coercion: every variant comes back as it went in.
    assert_eq!(chunk.to_rows(), input);
}

#[test]
fn every_structural_variant_round_trips_through_the_fallback() {
    let variants = vec![
        Value::List(Arc::new(vec![Value::Int(1), Value::Null])),
        Value::Map(Arc::new(BTreeMap::from([(
            "k".to_string(),
            Value::Bool(true),
        )]))),
        Value::Node(Arc::new(node(7))),
        Value::Relationship(Arc::new(rel(3, 7, 9))),
        Value::Path(Arc::new(Path {
            nodes: vec![node(7)],
            rels: vec![],
        })),
        Value::Temporal(Temporal::Date(
            NaiveDate::from_ymd_opt(2026, 11, 1).expect("2026-11-01 is a valid date"),
        )),
        Value::Temporal(Temporal::Duration {
            months: 1,
            days: 2,
            nanos: 3,
        }),
        Value::Point(PointValue::cartesian_2d(1.5, -2.5)),
        Value::Uuid(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef),
        Value::Vector {
            coord: VectorCoordKind::Float64,
            values: Arc::new(vec![Value::Float(0.5)]),
        },
        Value::Bytes(Arc::from(vec![1u8, 2, 3])),
        Value::Null,
    ];
    let input = rows_from(
        &["v"],
        &variants.iter().map(|v| vec![v.clone()]).collect::<Vec<_>>(),
    );
    let chunk = Chunk::from_rows(&input);
    assert!(matches!(chunk.column(0).data(), ColumnData::Value(_)));
    assert_eq!(chunk.to_rows(), input);
}

#[test]
fn an_empty_batch_round_trips_to_no_rows() {
    let chunk = Chunk::from_rows(&[]);
    assert!(chunk.is_empty());
    assert_eq!(chunk.to_rows(), Vec::<Row>::new());
}

#[test]
#[should_panic(expected = "diverges from the batch's schema")]
fn rows_with_divergent_schemas_are_refused() {
    let mixed = vec![
        rows_from(&["a"], &[vec![Value::Int(1)]]).pop().unwrap(),
        Row::from_iter(vec![("b".to_string(), Value::Int(2))]),
    ];
    let _ = Chunk::from_rows(&mixed);
}

#[test]
fn decoded_rows_share_one_schema_allocation() {
    let input = rows_from(
        &["a", "b"],
        &[
            vec![Value::Int(1), Value::Null],
            vec![Value::Null, Value::String(Arc::from("s"))],
        ],
    );
    let decoded = Chunk::from_rows(&input).to_rows();
    assert_eq!(decoded[0].schema_ptr(), decoded[1].schema_ptr());
}

/// LABS-645: the zone is derived from the encoded contents — typed
/// columns carry min/max over their non-null slots in the engine's
/// own ordering, the fallback null-presence only, and a column with
/// no non-null value has no bounds to claim.
#[test]
fn zones_derive_from_the_encoded_contents() {
    let input = rows_from(
        &["n", "s", "b", "mixed", "none"],
        &[
            vec![
                Value::Int(5),
                Value::String(Arc::from("b")),
                Value::Bool(true),
                Value::Int(1),
                Value::Null,
            ],
            vec![
                Value::Null,
                Value::String(Arc::from("ä")),
                Value::Bool(false),
                Value::String(Arc::from("x")),
                Value::Null,
            ],
            vec![
                Value::Int(-2),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
            ],
            vec![
                Value::Int(9),
                Value::String(Arc::from("a")),
                Value::Bool(true),
                Value::Int(2),
                Value::Null,
            ],
        ],
    );
    let chunk = Chunk::from_rows(&input);
    // A typed Int column: min/max over the non-null slots, the null
    // slot noted.
    let ints = chunk.column(0).zone();
    assert_eq!(ints.min_max(), Some((&Value::Int(-2), &Value::Int(9))));
    assert!(ints.null_present());
    // A typed Utf8 column: byte order — the engine's string ordering.
    let utf8 = chunk.column(1).zone();
    assert_eq!(
        utf8.min_max(),
        Some((
            &Value::String(Arc::from("a")),
            &Value::String(Arc::from("ä"))
        ))
    );
    assert!(utf8.null_present());
    // A typed Bool column: `false` orders before `true`.
    let bools = chunk.column(2).zone();
    assert_eq!(
        bools.min_max(),
        Some((&Value::Bool(false), &Value::Bool(true)))
    );
    assert!(bools.null_present());
    // The fallback: null-presence only — no min/max over
    // heterogeneous values (the stated rule).
    let mixed = chunk.column(3).zone();
    assert_eq!(mixed.min_max(), None);
    assert!(mixed.null_present());
    // No non-null value at all: no bounds to claim.
    let none = chunk.column(4).zone();
    assert_eq!(none.min_max(), None);
    assert!(none.null_present());
}

/// LABS-645: a NaN voids a Float column's bounds — the zone stays a
/// derived fact, so it carries none rather than an estimate over
/// values the ordering cannot place. Null-presence survives: it
/// needs no ordering.
#[test]
fn a_nan_float_voids_the_bounds_but_keeps_null_presence() {
    let input = rows_from(
        &["nanful", "clean"],
        &[
            vec![Value::Float(1.5), Value::Float(-0.5)],
            vec![Value::Float(f64::NAN), Value::Float(2.5)],
            vec![Value::Null, Value::Float(0.0)],
        ],
    );
    let chunk = Chunk::from_rows(&input);
    let nanful = chunk.column(0).zone();
    assert_eq!(nanful.min_max(), None);
    assert!(nanful.null_present());
    let clean = chunk.column(1).zone();
    assert_eq!(
        clean.min_max(),
        Some((&Value::Float(-0.5), &Value::Float(2.5)))
    );
    assert!(!clean.null_present());
}

/// LABS-645: compaction re-derives — a retained column's zone
/// describes the kept rows, through the same construction every
/// column takes.
#[test]
fn retained_columns_rederive_their_zone_over_the_kept_rows() {
    let input = rows_from(
        &["n"],
        &[
            vec![Value::Int(5)],
            vec![Value::Int(-2)],
            vec![Value::Null],
            vec![Value::Int(9)],
        ],
    );
    let chunk = Chunk::from_rows(&input);
    let kept = chunk.retain_rows(&[0, 2, 3]);
    let zone = kept.column(0).zone();
    assert_eq!(zone.min_max(), Some((&Value::Int(5), &Value::Int(9))));
    assert!(zone.null_present());
    // Keeping only the null row: no non-null value, no bounds.
    let nulled = chunk.retain_rows(&[2]);
    assert_eq!(nulled.column(0).zone().min_max(), None);
    assert!(nulled.column(0).zone().null_present());
}

/// The scalar leaves — exactly the variants that encode into typed
/// buffers, plus null — so generated batches frequently claim typed
/// columns.
fn scalar() -> BoxedStrategy<Value> {
    prop_oneof![
        2 => Just(Value::Null),
        3 => any::<bool>().prop_map(Value::Bool),
        4 => any::<i64>().prop_map(Value::Int),
        3 => any::<f64>().prop_filter("finite", |f| f.is_finite()).prop_map(Value::Float),
        3 => "[a-zäöü]{0,8}".prop_map(|s| Value::String(Arc::from(s))),
    ]
    .boxed()
}

fn node_strategy() -> BoxedStrategy<Node> {
    (
        any::<i64>(),
        "[A-L]{0,2}",
        vec(("[a-z]{1,3}", scalar()), 0..3),
    )
        .prop_map(|(id, label, props)| Node {
            id,
            labels: Arc::new(vec![label]),
            properties: Arc::new(
                props
                    .into_iter()
                    .collect::<BTreeMap<String, Value>>()
                    .into(),
            ),
        })
        .boxed()
}

fn rel_strategy() -> BoxedStrategy<Relationship> {
    (
        any::<i64>(),
        any::<i64>(),
        any::<i64>(),
        "[A-Z]{1,3}",
        vec(("[a-z]{1,3}", scalar()), 0..3),
    )
        .prop_map(|(id, start, end, rel_type, props)| Relationship {
            id,
            start,
            end,
            rel_type,
            properties: Arc::new(
                props
                    .into_iter()
                    .collect::<BTreeMap<String, Value>>()
                    .into(),
            ),
        })
        .boxed()
}

/// One generated value from anywhere in the `Value` space — every
/// variant, nulls weighted.
fn value() -> BoxedStrategy<Value> {
    prop_oneof![
        5 => scalar(),
        1 => vec(scalar(), 0..4).prop_map(|v| Value::List(Arc::new(v))),
        1 => vec(("[a-z]{1,3}", scalar()), 0..4)
            .prop_map(|pairs| Value::Map(Arc::new(pairs.into_iter().collect()))),
        1 => node_strategy().prop_map(|n| Value::Node(Arc::new(n))),
        1 => rel_strategy().prop_map(|r| Value::Relationship(Arc::new(r))),
        1 => (vec(node_strategy(), 0..3), vec(rel_strategy(), 0..2))
            .prop_map(|(nodes, rels)| Value::Path(Arc::new(Path { nodes, rels }))),
        1 => (1970i32..=2100, 1u32..=12, 1u32..=28).prop_map(|(year, month, day)| {
            Value::Temporal(Temporal::Date(
                NaiveDate::from_ymd_opt(year, month, day)
                    .expect("day 1-28 of any month is a valid date"),
            ))
        }),
        1 => (any::<i64>(), any::<i64>(), any::<i128>()).prop_map(|(months, days, nanos)| {
            Value::Temporal(Temporal::Duration { months, days, nanos })
        }),
        1 => (any::<f64>(), any::<f64>())
            .prop_filter("finite", |&(x, y)| x.is_finite() && y.is_finite())
            .prop_map(|(x, y)| Value::Point(PointValue::cartesian_2d(x, y))),
        1 => any::<u128>().prop_map(Value::Uuid),
        1 => (
            prop_oneof![Just(VectorCoordKind::Int64), Just(VectorCoordKind::Float64)],
            vec(scalar(), 0..3)
        )
            .prop_map(|(coord, values)| Value::Vector { coord, values: Arc::new(values) }),
        1 => any::<Vec<u8>>().prop_map(|b| Value::Bytes(Arc::from(b))),
    ]
    .boxed()
}

/// One generated batch: a fresh schema (distinct names `c0..cn`) and
/// `rows` rows of one generated value per cell.
fn batch(rows: RangeInclusive<usize>) -> BoxedStrategy<(Vec<String>, Vec<Vec<Value>>)> {
    (1usize..=4, rows)
        .prop_flat_map(|(columns, rows)| {
            let names: Vec<String> = (0..columns).map(|i| format!("c{i}")).collect();
            vec(vec(value(), columns..=columns), rows).prop_map(move |rows| (names.clone(), rows))
        })
        .boxed()
}

fn round_trip((names, cells): (Vec<String>, Vec<Vec<Value>>)) {
    let row_names = RowNames::new(&names);
    let input: Vec<Row> = cells
        .iter()
        .map(|values| Row::from_names(&row_names, values.clone()))
        .collect();
    let chunk = Chunk::from_rows(&input);
    assert_eq!(chunk.len(), input.len());
    assert_eq!(chunk.to_rows(), input);
}

proptest! {
    #![proptest_config(config_with_floor(1000))]

    /// Random schemas, random values (every variant, nulls included):
    /// Rows -> Chunk -> Rows is exact equality, 1000+ batches.
    #[test]
    fn generated_rows_round_trip_losslessly(batch in batch(0..=8)) {
        round_trip(batch);
    }
}

proptest! {
    #![proptest_config(config_with_floor(12))]

    /// The same contract at chunk scale: each case is one batch of a
    /// thousand-plus rows.
    #[test]
    fn generated_batches_of_a_thousand_rows_round_trip(batch in batch(1000..=1200)) {
        round_trip(batch);
    }
}
