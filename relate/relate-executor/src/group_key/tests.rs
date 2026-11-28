//! The key-construction unit tests — the normalized tree's equivalence
//! classes, the property-index key's identity reading, and the ADR-0614
//! decline predicate. A child module of `group_key` (the
//! `constraint_keys` precedent): the white-box pins construct the
//! private tree directly, so the tests live where the representation is
//! visible and the normalization types stay private to their module.

use super::*;
use relate_core::{Node, PointValue};
use relate_model::VectorCoords;
use relate_test_support::temporal::dst_overlap_half;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

fn key(v: Value) -> GroupKey {
    GroupKey::new(&v)
}

fn nan() -> Value {
    Value::Float(f64::NAN)
}

#[test]
fn int_and_integral_float_share_a_key() {
    assert_eq!(key(Value::Int(1)), key(Value::Float(1.0)));
    assert_eq!(key(Value::Int(0)), key(Value::Float(0.0)));
}

#[test]
fn keys_agree_with_eq_non_null_past_2_pow_53() {
    // The equivalence key and `=` must give the same verdict on the
    // boundary where a lossy `as f64` cast goes wrong.
    let big = Value::Int(9_007_199_254_740_993);
    let near = Value::Float(9_007_199_254_740_992.0);
    assert!(!big.eq_non_null(&near));
    assert_ne!(key(big.clone()), key(near.clone()));
    // ... and the float DOES share a key with the integer it denotes.
    let exact = Value::Int(9_007_199_254_740_992);
    assert!(exact.eq_non_null(&near));
    assert_eq!(key(exact), key(near));
}

#[test]
fn vector_keys_agree_with_vector_eq_both_directions() {
    // `=` and the equivalence key must give the same verdict on vectors:
    // numerically equal elements collapse (equal), different values or
    // coordinate kinds do not (unequal).
    let int_backed = Value::vector(VectorCoordKind::Float64, VectorCoords::from_int(vec![1]));
    let float_backed = Value::vector(VectorCoordKind::Float64, VectorCoords::from_f64(vec![1.0]));
    assert!(int_backed.eq_non_null(&float_backed));
    assert_eq!(key(int_backed.clone()), key(float_backed));
    let two = Value::vector(VectorCoordKind::Float64, VectorCoords::from_f64(vec![2.0]));
    assert!(!int_backed.eq_non_null(&two));
    assert_ne!(key(int_backed.clone()), key(two));
    let other_kind = Value::vector(VectorCoordKind::Int32, VectorCoords::from_int(vec![1]));
    assert!(!int_backed.eq_non_null(&other_kind));
    assert_ne!(key(int_backed), key(other_kind));
}

#[test]
fn positive_and_negative_zero_share_a_key() {
    assert_eq!(key(Value::Float(0.0)), key(Value::Float(-0.0)));
    assert_eq!(key(Value::Int(0)), key(Value::Float(-0.0)));
}

#[test]
fn non_integral_floats_key_by_value() {
    assert_eq!(key(Value::Float(1.5)), key(Value::Float(1.5)));
    assert_ne!(key(Value::Float(1.5)), key(Value::Float(2.5)));
    assert_ne!(key(Value::Int(1)), key(Value::Float(1.5)));
}

#[test]
fn every_nan_is_its_own_key() {
    assert_ne!(key(nan()), key(nan()));
    // Two lists each holding a NaN are also never equivalent.
    assert_ne!(
        key(Value::List(vec![nan()].into())),
        key(Value::List(vec![nan()].into()))
    );
}

#[test]
fn float_beyond_i64_range_does_not_collide_with_i64_extremes() {
    // 2^63 is i64::MAX + 1; it must not collapse onto Int(i64::MAX).
    let two_pow_63 = Value::Float(9_223_372_036_854_775_808.0);
    assert_ne!(key(two_pow_63.clone()), key(Value::Int(i64::MAX)));
    // But an in-range integral float (2^62, exactly representable) still
    // collapses onto its integer.
    let two_pow_62: i64 = 4_611_686_018_427_387_904;
    assert_eq!(
        key(Value::Float(two_pow_62 as f64)),
        key(Value::Int(two_pow_62))
    );
}

#[test]
fn strings_never_collide_with_lists() {
    let s = Value::String("a,b".into());
    let l = Value::List(vec![Value::String("a".into()), Value::String("b".into())].into());
    assert_ne!(key(s), key(l));
}

#[test]
fn strings_with_quotes_and_escapes_stay_distinct() {
    // The old to_cypher_literal key rendered these ambiguously; the
    // canonical key keeps the byte content verbatim.
    assert_ne!(
        key(Value::String("a\"b".into())),
        key(Value::String("a'b".into()))
    );
    assert_ne!(
        key(Value::String("a\\b".into())),
        key(Value::String("ab".into()))
    );
}

#[test]
fn bools_never_collide_with_ints() {
    assert_ne!(key(Value::Bool(true)), key(Value::Int(1)));
    assert_ne!(key(Value::Bool(false)), key(Value::Int(0)));
}

#[test]
fn nested_numeric_equivalence_in_lists_and_maps() {
    let a = Value::List(vec![Value::Int(1)].into());
    let b = Value::List(vec![Value::Float(1.0)].into());
    assert_eq!(key(a), key(b));

    let mut m1 = BTreeMap::new();
    m1.insert("a".to_string(), Value::List(vec![Value::Int(1)].into()));
    let mut m2 = BTreeMap::new();
    m2.insert("a".to_string(), Value::List(vec![Value::Float(1.0)].into()));
    assert_eq!(key(Value::Map(m1.into())), key(Value::Map(m2.into())));
}

#[test]
fn map_key_order_does_not_matter() {
    let mut m1 = BTreeMap::new();
    m1.insert("a".to_string(), Value::Int(1));
    m1.insert("b".to_string(), Value::Int(2));
    let mut m2 = BTreeMap::new();
    m2.insert("b".to_string(), Value::Int(2));
    m2.insert("a".to_string(), Value::Int(1));
    assert_eq!(key(Value::Map(m1.into())), key(Value::Map(m2.into())));
}

#[test]
fn nodes_key_by_id() {
    let n1 = Node {
        id: 1,
        labels: Arc::new(vec!["P".to_string()]),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    };
    let n2 = Node {
        id: 2,
        labels: Arc::new(vec!["P".to_string()]),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    };
    // Structurally identical but different ids -> distinct.
    assert_ne!(key(Value::from_node(n1.clone())), key(Value::from_node(n2)));
    assert_eq!(key(Value::from_node(n1.clone())), key(Value::from_node(n1)));
}

#[test]
fn time_keys_by_local_time_and_offset() {
    // `Temporal::equals`: 12:00+01:00 and 13:00+02:00 share an instant
    // but are different times, so they key apart.
    let a = Temporal::Time {
        time: NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
        offset: 3600,
    };
    let b = Temporal::Time {
        time: NaiveTime::from_hms_opt(13, 0, 0).unwrap(),
        offset: 7200,
    };
    assert_ne!(
        key(Value::Temporal(a.clone())),
        key(Value::Temporal(b.clone()))
    );
    assert_ne!(a.compare(&b), Some(std::cmp::Ordering::Equal));
}

#[test]
fn datetime_keys_by_instant_and_zone_identity() {
    let utc = NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap();
    let a = Temporal::DateTime {
        utc,
        offset: 0,
        zone: Some("UTC".to_string()),
    };
    let b = Temporal::DateTime {
        utc,
        offset: 0,
        zone: Some("Europe/Stockholm".to_string()),
    };
    // A named zone is never the offset it denotes.
    assert_ne!(key(Value::Temporal(a)), key(Value::Temporal(b)));
}

#[test]
fn datetime_dst_overlap_halves_key_apart_but_stay_equal() {
    // The centralized fixture: 02:30+02:00 and 02:30+01:00 on the
    // fall-back day — equal local date-time and zone, different
    // offsets/instants. `=` holds between the halves, yet they are two
    // grouping keys (TCK TemporalCompare).
    let first = dst_overlap_half(false);
    let second = dst_overlap_half(true);
    assert!(first.eq_non_null(&second), "the halves are `=`-equal");
    assert_ne!(key(first.clone()), key(second.clone()));
    // The property index keys the same identity, not `=`: the pair stays
    // apart for an indexed seek and a uniqueness probe, exactly as
    // Neo4j's range index and constraints keep it apart.
    assert_ne!(
        GroupKey::for_property_index(&first),
        GroupKey::for_property_index(&second)
    );
}

#[test]
fn property_index_key_shares_the_grouping_tree() {
    // Times the two relations agree on: equal times share the bucket,
    // the same instant in a different offset does not.
    let time = |h, m, offset| {
        Value::Temporal(Temporal::Time {
            time: NaiveTime::from_hms_opt(h, m, 0).unwrap(),
            offset,
        })
    };
    // The concrete bucket: UTC nanos of day plus the offset.
    assert_eq!(
        GroupKey::for_property_index(&time(12, 0, 0)),
        Some(GroupKey(Norm::Temporal(NormTemporal::Time {
            utc_nanos: 12 * 3_600 * 1_000_000_000,
            offset: 0,
        })))
    );
    assert_ne!(
        GroupKey::for_property_index(&time(12, 0, 0)),
        GroupKey::for_property_index(&time(12, 0, 3600))
    );
    // A local date-time is the same value in both readings.
    let local = NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap();
    let value = Value::Temporal(Temporal::LocalDateTime(local));
    assert_eq!(
        key(value.clone()),
        GroupKey::for_property_index(&value).unwrap()
    );
}

#[test]
fn vector_elements_normalize_numerically() {
    let a = Value::vector(VectorCoordKind::Float64, VectorCoords::from_int(vec![1]));
    let b = Value::vector(VectorCoordKind::Float64, VectorCoords::from_f64(vec![1.0]));
    assert_eq!(key(a), key(b.clone()));
    // The coordinate kind stays part of the key.
    let c = Value::vector(VectorCoordKind::Int32, VectorCoords::from_int(vec![1]));
    assert_ne!(key(b), key(c));
}

#[test]
fn points_equivalent_by_coordinate_value() {
    let p1 = Value::Point(PointValue {
        srid: 7203,
        x: 1.0,
        y: 2.0,
        z: None,
    });
    let p2 = Value::Point(PointValue {
        srid: 7203,
        x: 1.0,
        y: 2.0,
        z: None,
    });
    assert_eq!(key(p1), key(p2.clone()));
    let p3 = Value::Point(PointValue {
        srid: 7203,
        x: 1.0,
        y: 2.5,
        z: None,
    });
    assert_ne!(key(p2), key(p3));
}

#[test]
fn null_is_reflexive() {
    assert_eq!(key(Value::Null), key(Value::Null));
}

#[test]
fn hashset_dedups_by_equivalence() {
    let mut set: HashSet<GroupKey> = HashSet::new();
    set.insert(key(Value::Int(1)));
    set.insert(key(Value::Float(1.0)));
    set.insert(key(Value::Int(2)));
    // 1 and 1.0 collapse; 2 stands alone.
    assert_eq!(set.len(), 2);
}

#[test]
fn row_key_compares_columnwise_by_equivalence() {
    let r1 = vec![Value::Int(1), Value::Float(2.0)];
    let r2 = vec![Value::Float(1.0), Value::Int(2)];
    assert_eq!(row_key(&r1), row_key(&r2));
    let r3 = vec![Value::Int(1), Value::Float(3.0)];
    assert_ne!(row_key(&r1), row_key(&r3));
}

#[test]
fn property_index_key_shares_the_numeric_normalization() {
    // The one canonical numeric key: Int(1) and Float(1.0) collide.
    assert_eq!(
        GroupKey::for_property_index(&Value::Int(1)),
        GroupKey::for_property_index(&Value::Float(1.0))
    );
    assert_eq!(
        GroupKey::for_property_index(&Value::Float(-0.0)),
        GroupKey::for_property_index(&Value::Int(0))
    );
    // ... and it is the grouping key's tree, for every value.
    assert_eq!(
        GroupKey::for_property_index(&Value::Float(1.0)),
        Some(GroupKey::new(&Value::Int(1)))
    );
}

#[test]
fn property_index_rejects_nan_anywhere() {
    assert_eq!(GroupKey::for_property_index(&nan()), None);
    assert_eq!(
        GroupKey::for_property_index(&Value::List(vec![Value::Int(1), nan()].into())),
        None
    );
    let mut m = BTreeMap::new();
    m.insert("a".to_string(), nan());
    assert_eq!(GroupKey::for_property_index(&Value::Map(m.into())), None);
    let p = Value::Point(PointValue {
        srid: 7203,
        x: f64::NAN,
        y: 2.0,
        z: None,
    });
    assert_eq!(GroupKey::for_property_index(&p), None);
    // A packed VECTOR (ADR-0706) cannot hold a NaN — its coordinates
    // are finite by construction — so the vector case this walk used to
    // decline is unconstructible; the NaN decline stays pinned by the
    // scalar and list cases above.
    assert_eq!(GroupKey::for_property_index(&Value::Float(f64::NAN)), None);
    // NaN-free values stay indexable.
    assert!(GroupKey::for_property_index(&Value::Int(1)).is_some());
}

#[test]
fn identity_narrower_than_eq_exactly_for_named_zone_datetimes() {
    let zoned = Value::Temporal(Temporal::DateTime {
        utc: NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap(),
        offset: 3600,
        zone: Some("Europe/Stockholm".to_string()),
    });
    // The decline class: a named-zone datetime, bare or nested.
    assert!(identity_narrower_than_eq(&zoned));
    assert!(identity_narrower_than_eq(&Value::List(
        vec![zoned.clone()].into()
    )));
    let mut m = BTreeMap::new();
    m.insert("at".to_string(), zoned.clone());
    assert!(identity_narrower_than_eq(&Value::Map(m.into())));
    // A packed VECTOR (ADR-0706) holds only finite numbers, so a
    // narrow-identity temporal can no longer nest inside one; the bare
    // and list nestings above keep the decline pinned.
    // An offset-only datetime never declines: `ZoneId::Offset`'s
    // (instant, offset) identity is a bijection of its `=` class
    // (wall clock, offset) — the identity bucket answers `=` exactly.
    let offset_only = |zone: Option<&str>, offset: i32| {
        Value::Temporal(Temporal::DateTime {
            utc: NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
            offset,
            zone: zone.map(str::to_string),
        })
    };
    for clean in [
        offset_only(None, 3600),
        offset_only(Some("Z"), 0),
        offset_only(Some("+00:00"), 0),
        Value::List(vec![offset_only(None, 0)].into()),
    ] {
        assert!(
            !identity_narrower_than_eq(&clean),
            "{clean:?} must not decline"
        );
    }
    // Everything else: identity is the `=` class, so an identity
    // bucket answers a `=` lookup exactly.
    let local = Value::Temporal(Temporal::LocalDateTime(
        NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap(),
    ));
    let time = Value::Temporal(Temporal::Time {
        time: NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
        offset: 3600,
    });
    for clean in [
        Value::Null,
        Value::Int(1),
        Value::Float(1.5),
        Value::String("s".into()),
        local,
        time,
        Value::List(vec![Value::Int(1), Value::Float(1.0)].into()),
    ] {
        assert!(
            !identity_narrower_than_eq(&clean),
            "{clean:?} must not decline"
        );
    }
}

#[test]
fn logical_bytes_tracks_payload() {
    let small = GroupKey::new(&Value::Int(1)).logical_bytes();
    let big = GroupKey::new(&Value::String("x".repeat(100).into())).logical_bytes();
    assert!(small > 0);
    assert!(big >= 100);
}
