//! The total-order property tests and per-kind order regressions of
//! `value::ordering` (REL8-2030): the recorded prevention for the comparator
//! class, where a pair of values with no ordering arm compared `Equal`.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use super::*;
use crate::value::PropertyMap;
use crate::{Node, NodeId, Path, RelId, Relationship};

fn node(id: NodeId) -> Value {
    Value::Node(Arc::new(Node {
        id,
        labels: Arc::new(Vec::new()),
        properties: Arc::new(PropertyMap::new()),
    }))
}

fn relationship(id: RelId) -> Value {
    Value::Relationship(Arc::new(Relationship {
        id,
        start: 1,
        end: 2,
        rel_type: "R".into(),
        properties: Arc::new(PropertyMap::new()),
    }))
}

fn path(node_ids: &[NodeId]) -> Value {
    let nodes: Vec<Node> = node_ids
        .iter()
        .map(|&id| Node {
            id,
            labels: Arc::new(Vec::new()),
            properties: Arc::new(PropertyMap::new()),
        })
        .collect();
    let start = node_ids[0];
    let rels = nodes
        .iter()
        .skip(1)
        .map(|n| Relationship {
            id: n.id,
            start,
            end: n.id,
            rel_type: "R".into(),
            properties: Arc::new(PropertyMap::new()),
        })
        .collect();
    Value::Path(Arc::new(Path { nodes, rels }))
}

fn map(entries: &[(&str, Value)]) -> Value {
    let map: BTreeMap<String, Value> = entries
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect();
    Value::Map(Arc::new(map))
}

fn list(values: &[Value]) -> Value {
    Value::List(Arc::new(values.to_vec()))
}

fn temporal(t: Temporal) -> Value {
    Value::Temporal(t)
}

/// `a` sorts before `b` under the one total order the tests read.
fn less(a: &Value, b: &Value) -> bool {
    a.order_by_cmp(b) == Ordering::Less
}

/// Two representatives of every value kind (one where the kind carries a
/// single spelling), so the same-kind arms and every cross-kind pair are
/// exercised.
fn sample() -> Vec<Value> {
    let date = NaiveDate::from_ymd_opt(2024, 6, 1).unwrap();
    let later = NaiveDate::from_ymd_opt(2024, 6, 2).unwrap();
    let time = NaiveTime::from_hms_opt(1, 2, 3).unwrap();
    let dt = NaiveDateTime::new(date, time);
    let nanos = 3_600 * 1_000_000_000;
    let point = || PointValue::cartesian_2d(0.0, 0.0);
    vec![
        Value::Null,
        Value::Bool(false),
        Value::Bool(true),
        Value::Int(-1),
        Value::Int(1),
        Value::Int(i64::MAX),
        Value::Float(-1.5),
        Value::Float(1.0),
        Value::Float(f64::NAN),
        Value::Float(f64::INFINITY),
        Value::String("a".into()),
        Value::String("b".into()),
        Value::Bytes(Arc::from([0u8].as_slice())),
        Value::Bytes(Arc::from([1u8].as_slice())),
        Value::Uuid(1),
        Value::Uuid(2),
        list(&[]),
        list(&[Value::Int(1)]),
        list(&[Value::String("a".into())]),
        list(&[Value::Int(1), Value::Int(2)]),
        list(&[Value::Null]),
        map(&[]),
        map(&[("a", Value::Int(1))]),
        map(&[("a", Value::String("x".into()))]),
        map(&[("a", Value::Null)]),
        map(&[("a", Value::Int(1)), ("b", Value::Int(2))]),
        map(&[("a", Value::Point(point()))]),
        node(1),
        node(2),
        relationship(1),
        relationship(2),
        path(&[1]),
        path(&[1, 2]),
        temporal(Temporal::Date(date)),
        temporal(Temporal::Date(later)),
        temporal(Temporal::CivilDate {
            year: 300_000,
            month: 6,
            day: 15,
        }),
        temporal(Temporal::LocalDateTime(dt)),
        temporal(Temporal::CivilDateTime {
            year: 300_000,
            month: 6,
            day: 15,
            time,
        }),
        temporal(Temporal::Time { time, offset: 0 }),
        temporal(Temporal::LocalTime(time)),
        temporal(Temporal::DateTime {
            utc: dt,
            offset: 0,
            zone: None,
        }),
        temporal(Temporal::Duration {
            months: 0,
            days: 0,
            nanos,
        }),
        temporal(Temporal::Duration {
            months: 1,
            days: 0,
            nanos: 0,
        }),
        Value::Point(PointValue::cartesian_2d(1.0, 0.0)),
        Value::Point(PointValue::cartesian_2d(2.0, 0.0)),
        Value::Point(PointValue::wgs84_2d(1.0, 0.0)),
        Value::Point(PointValue::cartesian_3d(1.0, 0.0, 0.0)),
        Value::vector(VectorCoordKind::Float64, VectorCoords::from_f64(vec![1.0])),
        Value::vector(
            VectorCoordKind::Float64,
            VectorCoords::from_f64(vec![1.0, 2.0]),
        ),
        Value::vector(VectorCoordKind::Int32, VectorCoords::from_int(vec![1])),
    ]
}

/// The recorded prevention for the comparator class: `ORDER BY` /
/// `min` / `max` read one total order, so every pair and triple of a
/// sample spanning every value kind must be antisymmetric and
/// transitive. Corpus issues 46 and 54 are the two failures this
/// prevents — unsorted output, and the standard library's panic on a
/// comparator that is not a total order.
#[test]
fn order_by_cmp_is_a_total_order_over_every_value_kind() {
    let values = sample();
    for a in &values {
        for b in &values {
            assert_eq!(
                a.order_by_cmp(b),
                b.order_by_cmp(a).reverse(),
                "antisymmetry: {a:?} vs {b:?}"
            );
            for c in &values {
                if a.order_by_cmp(b) != Ordering::Greater && b.order_by_cmp(c) != Ordering::Greater
                {
                    assert_ne!(
                        a.order_by_cmp(c),
                        Ordering::Greater,
                        "transitivity: {a:?} <= {b:?} <= {c:?}"
                    );
                }
            }
        }
    }
}

/// The second half of the class: two values of different kinds are never
/// `Equal`, so a cross-kind pair cannot vanish into a stable sort's
/// "equal" bucket (and `min`/`max` cannot answer the first input).
#[test]
fn order_by_cmp_never_equals_across_kinds() {
    let values = sample();
    for a in &values {
        for b in &values {
            if a.order_type_rank() != b.order_type_rank() {
                assert_ne!(
                    a.order_by_cmp(b),
                    Ordering::Equal,
                    "cross-kind equality: {a:?} vs {b:?}"
                );
            }
        }
    }
}

/// The sort that reads the comparator: Rust's `sort_by` panics above 20
/// elements when the relation is not a total order (issue 54's input,
/// INTEGER-or-null under one key).
#[test]
fn sorting_more_than_twenty_maps_does_not_panic() {
    let mut maps: Vec<Value> = [
        68, 39, 60, 0, 40, 0, 0, 7, 0, 90, 0, 42, 0, 0, 90, 0, 69, 0, 56, 0, 40,
    ]
    .into_iter()
    .map(|a| map(&[("a", if a == 0 { Value::Null } else { Value::Int(a) })]))
    .collect();
    maps.sort_by(Value::order_by_cmp);
    assert_eq!(maps.len(), 21);
}

/// Issue 46's map and point orders, as Neo4j 2026.10 answers them: the
/// kind rank decides a cross-type pair under a key, a null sorts last,
/// and points order by SRID then coordinates.
#[test]
fn maps_and_points_order_by_their_parts() {
    let mut maps = [
        map(&[("a", Value::Int(1))]),
        map(&[("a", Value::String("x".into()))]),
        map(&[("a", Value::Null)]),
    ];
    maps.sort_by(Value::order_by_cmp);
    assert_eq!(
        maps,
        [
            map(&[("a", Value::String("x".into()))]),
            map(&[("a", Value::Int(1))]),
            map(&[("a", Value::Null)]),
        ]
    );
    // Same key, a null against a number: the two values decide.
    assert!(less(
        &map(&[("a", Value::Null), ("b", Value::Int(1))]),
        &map(&[("a", Value::Null), ("b", Value::Int(2))])
    ));
    // Size, then keys.
    assert!(less(&map(&[]), &map(&[("a", Value::Int(1))])));
    // A map nested in a list reaches the map arm.
    let mut lists = [
        list(&[map(&[("a", Value::Int(1))])]),
        list(&[map(&[("a", Value::String("x".into()))])]),
    ];
    lists.sort_by(Value::order_by_cmp);
    assert_eq!(lists[0], list(&[map(&[("a", Value::String("x".into()))])]));

    let points = || {
        vec![
            Value::Point(PointValue::cartesian_2d(2.0, 0.0)),
            Value::Point(PointValue::cartesian_2d(1.0, 0.0)),
            Value::Point(PointValue::cartesian_2d(0.0, 5.0)),
        ]
    };
    let mut sorted = points();
    sorted.sort_by(Value::order_by_cmp);
    assert_eq!(
        sorted,
        vec![
            Value::Point(PointValue::cartesian_2d(0.0, 5.0)),
            Value::Point(PointValue::cartesian_2d(1.0, 0.0)),
            Value::Point(PointValue::cartesian_2d(2.0, 0.0)),
        ]
    );
    // The SRID decides before the coordinates (wgs-84 4326 < cartesian
    // 7203 < cartesian-3d 9157), whatever the x values.
    assert!(less(
        &Value::Point(PointValue::wgs84_2d(9.0, 9.0)),
        &Value::Point(PointValue::cartesian_2d(0.0, 0.0))
    ));
    // And `min`/`max` read the same relation.
    let points = points();
    assert_eq!(
        points
            .iter()
            .min_by(|x, y| x.order_by_cmp(y))
            .unwrap()
            .clone(),
        Value::Point(PointValue::cartesian_2d(0.0, 5.0))
    );
}

/// Issue 46's entity variants: a node, a relationship, a path and a
/// list of nodes order by entity id, which is the creation order.
#[test]
fn entities_order_by_id() {
    assert!(less(&node(1), &node(2)));
    assert!(less(&relationship(1), &relationship(2)));
    assert!(
        less(&path(&[1]), &path(&[1, 2])),
        "a shorter path sorts first"
    );
    assert!(
        less(&path(&[1, 9]), &path(&[2, 1])),
        "then the node sequence"
    );
    let mut lists = [list(&[node(2)]), list(&[node(1)])];
    lists.sort_by(Value::order_by_cmp);
    assert_eq!(lists, [list(&[node(1)]), list(&[node(2)])]);
}

/// Issue 26: the two internal representations of DATE (and of LOCAL
/// DATETIME) share one order, so a far-year value sorts among in-range
/// ones and the comparison operators answer instead of null.
#[test]
fn far_year_dates_order_with_in_range_dates() {
    let in_range = |y, m, d| temporal(Temporal::Date(NaiveDate::from_ymd_opt(y, m, d).unwrap()));
    let far = || {
        temporal(Temporal::CivilDate {
            year: 300_000,
            month: 6,
            day: 15,
        })
    };
    let mut dates = [in_range(2000, 1, 1), far(), in_range(1990, 1, 1)];
    dates.sort_by(Value::order_by_cmp);
    assert_eq!(dates, [in_range(1990, 1, 1), in_range(2000, 1, 1), far()]);
    assert_eq!(
        far().compare(&in_range(2000, 1, 1)),
        Some(Ordering::Greater)
    );
    assert_eq!(in_range(2000, 1, 1).compare(&far()), Some(Ordering::Less));

    let dt = NaiveDateTime::new(
        NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(),
        NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
    );
    let far_dt = temporal(Temporal::CivilDateTime {
        year: 300_000,
        month: 6,
        day: 15,
        time: NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
    });
    assert_eq!(
        temporal(Temporal::LocalDateTime(dt)).compare(&far_dt),
        Some(Ordering::Less)
    );
    assert_eq!(
        far_dt.compare(&temporal(Temporal::LocalDateTime(dt))),
        Some(Ordering::Greater)
    );
}
