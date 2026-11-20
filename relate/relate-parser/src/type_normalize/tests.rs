use super::*;

fn p(kind: PredefinedKind, not_null: bool) -> CanonType {
    CanonType::predefined(kind, not_null)
}

#[test]
fn null_not_null_simplifies_to_nothing() {
    let c = p(PredefinedKind::Null, true);
    assert_eq!(render_canon(&c), "NOTHING");
}

#[test]
fn nothing_not_null_simplifies_to_nothing() {
    let c = p(PredefinedKind::Nothing, true);
    assert_eq!(render_canon(&c), "NOTHING");
}

#[test]
fn union_orders_by_general_type_order() {
    let c = normalize_union(vec![
        p(PredefinedKind::Float, true),
        p(PredefinedKind::Boolean, true),
        p(PredefinedKind::Integer, true),
    ]);
    assert_eq!(
        render_canon(&c),
        "BOOLEAN NOT NULL | INTEGER NOT NULL | FLOAT NOT NULL"
    );
}

/// Pin the CIP-100 "General order of types" table. The core owns the
/// twin order (`relate-core/src/typename.rs::value_type_sort_key`,
/// pinned by `value_type_sort_key_follows_the_documented_cip100_order`);
/// both must move together, so either growing apart fails here or there.
/// NULL/NOTHING lead this table (the empty/unknown ends of the order)
/// while the core sort keys never see them as union members.
#[test]
fn rank_follows_the_documented_cip100_order() {
    let predefined = [
        (PredefinedKind::Nothing, 0),
        (PredefinedKind::Null, 1),
        (PredefinedKind::Boolean, 2),
        (PredefinedKind::String, 3),
        (PredefinedKind::Uuid, 4),
        (PredefinedKind::Integer, 5),
        (PredefinedKind::Float, 6),
        (PredefinedKind::Date, 7),
        (PredefinedKind::LocalTime, 8),
        (PredefinedKind::ZonedTime, 9),
        (PredefinedKind::LocalDateTime, 10),
        (PredefinedKind::ZonedDateTime, 11),
        (PredefinedKind::Duration, 12),
        (PredefinedKind::Point, 13),
        (PredefinedKind::Node, 14),
        (PredefinedKind::Relationship, 15),
        (PredefinedKind::Map, 16),
        (PredefinedKind::Path, 19),
        (PredefinedKind::Any, 21),
    ];
    for (kind, expected) in predefined {
        assert_eq!(rank(&CanonKind::Predefined(kind)), expected, "{kind:?}");
    }
    assert_eq!(
        rank(&CanonKind::Vector {
            coord: None,
            dimension: None,
        }),
        17
    );
    assert_eq!(
        rank(&CanonKind::List(Box::new(p(PredefinedKind::Integer, true)))),
        18
    );
    assert_eq!(
        rank(&CanonKind::Union(vec![p(PredefinedKind::Integer, true)])),
        20
    );
}

#[test]
fn duplicate_and_subtype_members_collapse() {
    let c = normalize_union(vec![
        p(PredefinedKind::Boolean, true),
        p(PredefinedKind::Boolean, true),
    ]);
    assert_eq!(render_canon(&c), "BOOLEAN NOT NULL");
}

#[test]
fn single_type_union_unwraps() {
    let c = normalize_union(vec![p(PredefinedKind::Boolean, true)]);
    assert_eq!(render_canon(&c), "BOOLEAN NOT NULL");
}

#[test]
fn combine_null_and_not_null_makes_all_nullable() {
    let c = normalize_union(vec![
        p(PredefinedKind::Boolean, true),
        p(PredefinedKind::Null, false),
    ]);
    assert_eq!(render_canon(&c), "BOOLEAN");
}

#[test]
fn any_encompasses_everything() {
    let c = normalize_union(vec![
        p(PredefinedKind::Any, false),
        p(PredefinedKind::String, false),
    ]);
    assert_eq!(render_canon(&c), "ANY");
}

#[test]
fn list_of_nothing_is_subtype_of_any_list() {
    let c = normalize_union(vec![
        CanonType::list(p(PredefinedKind::Nothing, false), true),
        CanonType::list(p(PredefinedKind::Integer, true), true),
    ]);
    assert_eq!(render_canon(&c), "LIST<INTEGER NOT NULL> NOT NULL");
}

#[test]
fn list_tie_break_prefers_fewer_inner_types_then_lexicographic() {
    let list_a = CanonType::list(
        normalize_union(vec![
            p(PredefinedKind::Integer, true),
            p(PredefinedKind::Float, true),
        ]),
        true,
    );
    let list_b = CanonType::list(p(PredefinedKind::Boolean, true), true);
    let c = normalize_union(vec![list_a, list_b]);
    assert_eq!(
        render_canon(&c),
        "LIST<BOOLEAN NOT NULL> NOT NULL | LIST<INTEGER NOT NULL | FLOAT NOT NULL> NOT NULL"
    );
}

#[test]
fn validate_rejects_top_level_not_null() {
    let c = p(PredefinedKind::Float, true);
    let err = validate_property_type(&c).unwrap_err();
    assert_eq!(
        err,
        "FLOAT NOT NULL is not supported in property type constraints."
    );
}

#[test]
fn validate_rejects_union_inner_list_type() {
    let c = CanonType::list(
        normalize_union(vec![
            p(PredefinedKind::Boolean, true),
            p(PredefinedKind::Integer, true),
        ]),
        false,
    );
    let err = validate_property_type(&c).unwrap_err();
    assert!(err.starts_with("Invalid property type"));
}

#[test]
fn validate_accepts_plain_scalar_and_list() {
    assert!(validate_property_type(&p(PredefinedKind::Date, false)).is_ok());
    let list = CanonType::list(p(PredefinedKind::Integer, true), false);
    assert!(validate_property_type(&list).is_ok());
}

#[test]
fn validate_rejects_list_of_list() {
    let c = CanonType::list(
        CanonType::list(p(PredefinedKind::Integer, true), true),
        false,
    );
    let err = validate_property_type(&c).unwrap_err();
    assert!(err.starts_with("Invalid property type"));
}

#[test]
fn validate_rejects_list_of_nullable_scalar() {
    let c = CanonType::list(p(PredefinedKind::Date, false), false);
    let err = validate_property_type(&c).unwrap_err();
    assert!(err.starts_with("Invalid property type"));
}

#[test]
fn validate_rejects_list_of_unsupported_kind() {
    let c = CanonType::list(p(PredefinedKind::Map, true), false);
    let err = validate_property_type(&c).unwrap_err();
    assert_eq!(
        err,
        "LIST<MAP NOT NULL> is not supported in property type constraints."
    );
}

#[test]
fn parse_canon_type_str_round_trips_render_canon() {
    for canon in [
        p(PredefinedKind::Integer, false),
        CanonType::list(p(PredefinedKind::String, true), false),
        normalize_union(vec![
            p(PredefinedKind::Boolean, false),
            p(PredefinedKind::String, false),
        ]),
    ] {
        let rendered = render_canon(&canon);
        let parsed = parse_canon_type_str(&rendered).expect("should parse");
        assert_eq!(render_canon(&type_expr_to_canon(&parsed)), rendered);
    }
}

#[test]
fn parse_canon_type_str_rejects_garbage() {
    assert!(parse_canon_type_str("NOT A TYPE").is_none());
}

fn vector(coord: Option<VectorCoordKind>, dimension: Option<i64>, not_null: bool) -> CanonType {
    CanonType {
        kind: CanonKind::Vector { coord, dimension },
        not_null,
    }
}

#[test]
fn vector_renders_coordinate_and_dimension() {
    let c = vector(Some(VectorCoordKind::Int32), Some(42), false);
    assert_eq!(render_canon(&c), "VECTOR<INTEGER32 NOT NULL>(42)");
}

#[test]
fn vector_int64_and_float64_coordinates_render_as_plain_aliases() {
    // CIP-216 S-6: INT64/FLOAT64 double as ordinary plain-type aliases,
    // so the vector coordinate name renders as INTEGER/FLOAT, not
    // INTEGER64/FLOAT64.
    assert_eq!(
        render_canon(&vector(Some(VectorCoordKind::Int64), Some(3), false)),
        "VECTOR<INTEGER NOT NULL>(3)"
    );
    assert_eq!(
        render_canon(&vector(Some(VectorCoordKind::Float64), Some(4), false)),
        "VECTOR<FLOAT NOT NULL>(4)"
    );
}

#[test]
fn vector_missing_coordinate_or_dimension_renders_partial_form() {
    assert_eq!(render_canon(&vector(None, Some(3), false)), "VECTOR(3)");
    assert_eq!(
        render_canon(&vector(Some(VectorCoordKind::Int32), None, false)),
        "VECTOR<INTEGER32 NOT NULL>"
    );
}

#[test]
fn validate_rejects_vector_missing_coordinate_or_dimension() {
    assert!(validate_property_type(&vector(None, Some(3), false)).is_err());
    assert!(validate_property_type(&vector(Some(VectorCoordKind::Int32), None, false)).is_err());
    assert!(validate_property_type(&vector(Some(VectorCoordKind::Int32), Some(3), false)).is_ok());
}

#[test]
fn validate_rejects_outer_not_null_on_vector() {
    let c = vector(Some(VectorCoordKind::Int32), Some(3), true);
    let err = validate_property_type(&c).unwrap_err();
    assert_eq!(
        err,
        "VECTOR<INTEGER32 NOT NULL>(3) NOT NULL is not supported in property type constraints."
    );
}

#[test]
fn validate_rejects_list_of_vector() {
    let c = CanonType::list(vector(Some(VectorCoordKind::Int32), Some(42), true), false);
    let err = validate_property_type(&c).unwrap_err();
    assert!(err.contains("cannot have VECTOR as an inner type"));
}

#[test]
fn vector_sorts_before_list_in_a_union() {
    // CIP-216 01#5 (observed live-Neo4j order): a vector branch sorts
    // before a list branch in a normalized union.
    let c = normalize_union(vec![
        CanonType::list(p(PredefinedKind::Integer, true), false),
        vector(Some(VectorCoordKind::Int64), Some(3), false),
    ]);
    assert_eq!(
        render_canon(&c),
        "VECTOR<INTEGER NOT NULL>(3) | LIST<INTEGER NOT NULL>"
    );
}

#[test]
fn parse_canon_type_str_round_trips_vector() {
    for canon in [
        vector(Some(VectorCoordKind::Int8), Some(1), false),
        vector(Some(VectorCoordKind::Float32), Some(4096), false),
    ] {
        let rendered = render_canon(&canon);
        let parsed = parse_canon_type_str(&rendered).expect("should parse");
        assert_eq!(render_canon(&type_expr_to_canon(&parsed)), rendered);
    }
}

#[test]
fn from_canon_inverts_type_expr_to_canon() {
    for canon in [
        p(PredefinedKind::Integer, false),
        p(PredefinedKind::String, true),
        CanonType::list(p(PredefinedKind::String, true), true),
        normalize_union(vec![
            p(PredefinedKind::Boolean, true),
            p(PredefinedKind::String, true),
        ]),
    ] {
        assert_eq!(type_expr_to_canon(&TypeExpr::from_canon(&canon)), canon);
    }
}

#[test]
fn with_not_null_marks_every_member_of_a_union() {
    let union = TypeExpr::Union(vec![
        TypeExpr::Predefined {
            kind: PredefinedKind::Integer,
            not_null: false,
        },
        TypeExpr::Predefined {
            kind: PredefinedKind::Float,
            not_null: false,
        },
    ]);
    assert_eq!(
        render_canon(&type_expr_to_canon(&union.with_not_null())),
        "INTEGER NOT NULL | FLOAT NOT NULL"
    );
}
