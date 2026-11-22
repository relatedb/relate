use super::*;
use relate_model::CypherValue;

#[test]
fn unordered_rows_compare_as_a_bag() {
    let (left, right) = swapped_rows();
    assert_eq!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Exact
    );
}

#[test]
fn ordered_rows_compare_positionally() {
    let left = row_json(Json::Number(serde_json::Number::from(1)));
    let right = row_cypher(CypherValue::Int(2));
    assert!(matches!(
        compare_rows(&left, &right, StatementOrder::Ordered),
        Relation::Different(_)
    ));
}

#[test]
fn floats_compare_with_epsilon() {
    let left = row_json(serde_json::json!(0.30000000000000004));
    let right = row_cypher(CypherValue::Float(0.3));
    assert_eq!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Exact
    );
}

#[test]
fn neo4j_typed_dates_fold_to_strings() {
    // Both tag spellings fold; the real v2 wire shape is
    // `{"$type", "_value"}` (cf. relate-load's extract.rs).
    for typed in [
        serde_json::json!({"$type": "Date", "_value": "2024-03-14"}),
        serde_json::json!({"__type": "date", "_value": "2024-03-14"}),
    ] {
        let left = row_json(typed);
        let right = row_cypher(CypherValue::Str("2024-03-14".into()));
        assert_eq!(
            compare_rows(&left, &right, StatementOrder::Unordered),
            Relation::Exact
        );
    }
}

#[test]
fn unknown_type_tags_stay_objects_and_diff_visibly() {
    // An unknown `$type` must not collapse to a string: it stays an
    // object and diffs against a plain string on the engine side.
    let v = serde_json::json!({"$type": "Bogus", "_value": "2024-03-14"});
    assert!(normalize_neo4j(&v).is_object());
    let left = row_json(v);
    let right = row_cypher(CypherValue::Str("2024-03-14".into()));
    assert!(matches!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Different(_)
    ));
}

#[test]
fn non_finite_floats_fold_to_strings_not_null() {
    assert_eq!(
        normalize_cypher(&CypherValue::Float(f64::NAN)),
        Json::String("NaN".into())
    );
    assert_eq!(
        normalize_cypher(&CypherValue::Float(f64::INFINITY)),
        Json::String("Inf".into())
    );
    assert_eq!(
        normalize_cypher(&CypherValue::Float(f64::NEG_INFINITY)),
        Json::String("-Inf".into())
    );
    // NaN no longer conflates with a genuine null.
    let left = row_json(Json::Null);
    let right = row_cypher(CypherValue::Float(f64::NAN));
    assert!(matches!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Different(_)
    ));
}

#[test]
fn integers_compare_exactly_above_the_f64_mantissa() {
    // 2^53 and 2^53+1 are distinct integers but the same f64.
    let left = row_json(serde_json::json!(9007199254740992i64));
    let right = row_cypher(CypherValue::Int(9007199254740993));
    assert!(matches!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Different(_)
    ));
    // …and equal large integers still match.
    let right = row_cypher(CypherValue::Int(9007199254740992));
    assert_eq!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Exact
    );
}

#[test]
fn float_epsilon_applies_only_with_a_genuine_float() {
    // 0.1 + 0.2 vs 0.3: classic f64 drift, inside the epsilon.
    let left = row_json(serde_json::json!(0.1f64 + 0.2f64));
    let right = row_cypher(CypherValue::Float(0.3));
    assert_eq!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Exact
    );
}

#[test]
fn point_normalizes_to_the_engine_display_spelling() {
    let p = relate_model::value::PointValue::wgs84_2d(-73.9857, 40.7484);
    assert_eq!(
        normalize_cypher(&CypherValue::Point(p)),
        Json::String("point({srid: 4326, x: -73.9857, y: 40.7484})".into())
    );
}

/// F-1-1: the typed-JSON wire point
/// (`{"$type": "Point", "_value": "SRID=…;POINT …"}`) and the engine
/// `Point` canonicalize to the same `Display` string — 2D and 3D.
#[test]
fn typed_wire_points_compare_equal_to_engine_points() {
    let left = row_json(serde_json::json!({
        "$type": "Point",
        "_value": "SRID=7203;POINT (1.0 2.0)"
    }));
    let right = row_cypher(CypherValue::Point(PointValue::cartesian_2d(1.0, 2.0)));
    assert_eq!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Exact
    );

    let left = row_json(serde_json::json!({
        "$type": "Point",
        "_value": "SRID=9157;POINT Z (1.0 2.0 3.0)"
    }));
    let right = row_cypher(CypherValue::Point(PointValue::cartesian_3d(1.0, 2.0, 3.0)));
    assert_eq!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Exact
    );
}

/// F-1-1: the suite's reference client requests plain JSON
/// (`Accept: application/json`), where a POINT arrives as a bare EWKT
/// string — that spelling canonicalizes identically, 2D and 3D.
#[test]
fn plain_json_wire_points_compare_equal_to_engine_points() {
    let left = row_json(serde_json::json!("SRID=4326;POINT (-73.9857 40.7484)"));
    let right = row_cypher(CypherValue::Point(PointValue::wgs84_2d(-73.9857, 40.7484)));
    assert_eq!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Exact
    );

    let left = row_json(serde_json::json!(
        "SRID=4979;POINT Z (-73.9857 40.7484 100.5)"
    ));
    let right = row_cypher(CypherValue::Point(PointValue::wgs84_3d(
        -73.9857, 40.7484, 100.5,
    )));
    assert_eq!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Exact
    );
}

#[test]
fn differing_or_unparseable_points_still_diff() {
    // Same CRS, different coordinate.
    let left = row_json(serde_json::json!("SRID=7203;POINT (1.0 2.0)"));
    let right = row_cypher(CypherValue::Point(PointValue::cartesian_2d(1.0, 3.0)));
    assert!(matches!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Different(_)
    ));
    // Different SRID, same coordinates.
    let left = row_json(serde_json::json!("SRID=4326;POINT (1.0 2.0)"));
    assert!(matches!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Different(_)
    ));
    // SRID-less WKT cannot recover the CRS: it stays as-is and diffs
    // visibly rather than guessing a coordinate space.
    let left = row_json(serde_json::json!("POINT (1.0 2.0)"));
    assert!(matches!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Different(_)
    ));
    // A plain string that merely mentions POINT is not EWKT.
    let left = row_json(serde_json::json!("a POINT (1.0 2.0) of interest"));
    assert!(matches!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Different(_)
    ));
}
