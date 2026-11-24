//! The per-token property classification's tests (ADR-0667): every
//! token of the closed 39-token `Neo4jType` enum maps to a declaration
//! or is reported skipped, the untyped and dimensionless forms keep a
//! requirement as `ANY NOT NULL`, vectors resolve their coordinate
//! kinds and dimensions, and the empty resolved name never declares.
//! Split from the converter's test module so the classification's
//! tests sit beside their subject.

use super::*;

/// A document with one node `N` whose only property is `p : <token>`.
fn token_doc(token: &str) -> OntologyDocument {
    node_doc(json!({"p": {"type": token}}), json!({}))
}

/// The one typed property declaration the token tests expect — property
/// `p` with no inline constraint — so each mapping arm states only its
/// `TypeExpr`.
fn expected_props(value_type: TypeExpr) -> Vec<GraphTypePropertyType> {
    vec![GraphTypePropertyType {
        name: SmallName::from_name("p"),
        value_type,
        default_value: None,
        inline_constraint: None,
    }]
}

/// The mapping the test expects for one token, written out independently
/// of the implementation: a declaration of the stated shape, or the
/// skipped unrepresentable form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    Predefined(PredefinedKind),
    List(PredefinedKind),
    Vector(VectorCoordKind),
    /// No graph-type property type expresses the token: the property
    /// contributes a skip entry and no declaration.
    Skipped,
}

fn expected_mapping(ty: Neo4jType) -> Expected {
    use Neo4jType as T;
    match ty {
        T::Any | T::ListAny => Expected::Skipped,
        T::Boolean => Expected::Predefined(PredefinedKind::Boolean),
        T::ListBoolean => Expected::List(PredefinedKind::Boolean),
        T::Date => Expected::Predefined(PredefinedKind::Date),
        T::ListDate => Expected::List(PredefinedKind::Date),
        T::Duration => Expected::Predefined(PredefinedKind::Duration),
        T::ListDuration => Expected::List(PredefinedKind::Duration),
        // The narrow numerics widen for scalars and lists (ADR-0667).
        T::Float32 => Expected::Predefined(PredefinedKind::Float),
        T::ListFloat32 => Expected::List(PredefinedKind::Float),
        T::Float => Expected::Predefined(PredefinedKind::Float),
        T::ListFloat => Expected::List(PredefinedKind::Float),
        T::Integer8 | T::Integer16 | T::Integer32 => Expected::Predefined(PredefinedKind::Integer),
        T::ListInteger8 | T::ListInteger16 | T::ListInteger32 => {
            Expected::List(PredefinedKind::Integer)
        }
        T::Integer => Expected::Predefined(PredefinedKind::Integer),
        T::ListInteger => Expected::List(PredefinedKind::Integer),
        T::LocalDatetime => Expected::Predefined(PredefinedKind::LocalDateTime),
        T::ListLocalDatetime => Expected::List(PredefinedKind::LocalDateTime),
        T::LocalTime => Expected::Predefined(PredefinedKind::LocalTime),
        T::ListLocalTime => Expected::List(PredefinedKind::LocalTime),
        T::Point => Expected::Predefined(PredefinedKind::Point),
        T::ListPoint => Expected::List(PredefinedKind::Point),
        T::String => Expected::Predefined(PredefinedKind::String),
        T::ListString => Expected::List(PredefinedKind::String),
        T::VectorFloat => Expected::Vector(VectorCoordKind::Float64),
        T::VectorFloat32 => Expected::Vector(VectorCoordKind::Float32),
        T::VectorInteger => Expected::Vector(VectorCoordKind::Int64),
        T::VectorInteger32 => Expected::Vector(VectorCoordKind::Int32),
        T::VectorInteger16 => Expected::Vector(VectorCoordKind::Int16),
        T::VectorInteger8 => Expected::Vector(VectorCoordKind::Int8),
        T::ZonedDatetime => Expected::Predefined(PredefinedKind::ZonedDateTime),
        T::ListZonedDatetime => Expected::List(PredefinedKind::ZonedDateTime),
        T::ZonedTime => Expected::Predefined(PredefinedKind::ZonedTime),
        T::ListZonedTime => Expected::List(PredefinedKind::ZonedTime),
        T::Uuid => Expected::Predefined(PredefinedKind::Uuid),
    }
}

/// The skip entry a token test expects for `p` on node `N`.
fn expected_skip(what: &str) -> String {
    format!("{what} N.p (not representable)")
}

/// Every token of the closed 39-token `Neo4jType` enum, each mapped and
/// reported independently of `Neo4jType::mapping`'s implementation.
#[test]
fn every_neo4j_token_maps_or_is_reported_skipped() {
    let tokens: Vec<Neo4jType> = crate::model::NEO4J_TYPES
        .iter()
        .map(|(_, ty)| *ty)
        .collect();
    assert_eq!(
        tokens.len(),
        39,
        "main's closed Neo4jType enum has 39 tokens"
    );
    for ty in tokens {
        assert_eq!(
            Neo4jType::parse(ty.as_str()),
            Some(ty),
            "{} is not in the closed vocabulary",
            ty.as_str()
        );
        let mapping = to_graph_type_body(&token_doc(ty.as_str()))
            .unwrap_or_else(|e| panic!("{} maps: {e}", ty.as_str()));
        match expected_mapping(ty) {
            Expected::Predefined(kind) => {
                assert_eq!(
                    mapping.body.node_types[0].properties,
                    expected_props(TypeExpr::Predefined {
                        kind,
                        not_null: false,
                    }),
                    "{}",
                    ty.as_str()
                );
                assert!(mapping.skipped.is_empty(), "{}", ty.as_str());
            }
            Expected::List(kind) => {
                assert_eq!(
                    mapping.body.node_types[0].properties,
                    expected_props(TypeExpr::List {
                        element: Box::new(TypeExpr::Predefined {
                            kind,
                            not_null: true,
                        }),
                        not_null: false,
                    }),
                    "{}",
                    ty.as_str()
                );
                assert!(mapping.skipped.is_empty(), "{}", ty.as_str());
            }
            // The token test declares no dimension, so every vector form
            // is the dimensionless one: skipped, the element dropped.
            // The vector coordinate kinds with dimensions are pinned by
            // `vector_tokens_map_coordinate_kinds_and_dimensions`.
            Expected::Vector(_) => {
                assert!(
                    mapping.body.node_types.is_empty(),
                    "{}: the dimensionless vector leaves nothing representable",
                    ty.as_str()
                );
                assert_eq!(
                    mapping.skipped,
                    [
                        expected_skip("dimensionless vector property"),
                        "element type N has no representable content".to_owned(),
                    ],
                    "{}",
                    ty.as_str()
                );
            }
            Expected::Skipped => {
                let what = match ty {
                    Neo4jType::Any => "untyped property",
                    Neo4jType::ListAny => "untyped list property",
                    _ => unreachable!("only the untyped tokens are skipped"),
                };
                assert!(
                    mapping.body.node_types.is_empty(),
                    "{}: the element is dropped with no representable content",
                    ty.as_str()
                );
                assert_eq!(
                    mapping.skipped,
                    [
                        "element type N has no representable content".to_owned(),
                        expected_skip(what),
                    ],
                    "{}",
                    ty.as_str()
                );
            }
        }
    }
}

/// A token with no `type` member at all is the untyped form too: a skip
/// entry, no declaration, and — required — `ANY NOT NULL`.
#[test]
fn untyped_and_required_properties() {
    let doc = node_doc(
        json!({
            "misc": {},
            "any": {"type": "ANY"},
            "required_any": {"type": "ANY", "must_exist": true},
            "required_misc": {"must_exist": true},
            "required_list_any": {"type": "LIST<ANY>", "must_exist": true},
        }),
        json!({}),
    );
    let mapping = to_graph_type_body(&doc).expect("converts");
    let any_not_null = || GraphTypePropertyType {
        name: SmallName::from_name(""),
        value_type: TypeExpr::Predefined {
            kind: PredefinedKind::Any,
            not_null: true,
        },
        default_value: None,
        inline_constraint: None,
    };
    let mut required_any = any_not_null();
    required_any.name = SmallName::from_name("required_any");
    let mut required_misc = any_not_null();
    required_misc.name = SmallName::from_name("required_misc");
    let mut required_list_any = any_not_null();
    required_list_any.name = SmallName::from_name("required_list_any");
    assert_eq!(
        mapping.body.node_types[0].properties,
        [required_any, required_misc, required_list_any]
    );
    assert_eq!(
        mapping.skipped,
        [
            "untyped list property N.required_list_any (not representable)".to_owned(),
            "untyped property N.any (not representable)".to_owned(),
            "untyped property N.misc (not representable)".to_owned(),
            "untyped property N.required_any (not representable)".to_owned(),
            "untyped property N.required_misc (not representable)".to_owned(),
        ]
    );
}

/// A required dimensionless vector keeps its requirement as `ANY NOT
/// NULL`; an optional one declares nothing.
#[test]
fn dimensionless_vectors() {
    let doc = node_doc(
        json!({
            "optional": {"type": "VECTOR<FLOAT32>"},
            "required": {"type": "VECTOR<INTEGER8>", "must_exist": true},
        }),
        json!({}),
    );
    let mapping = to_graph_type_body(&doc).expect("converts");
    assert_eq!(
        mapping.body.node_types[0].properties,
        [GraphTypePropertyType {
            name: SmallName::from_name("required"),
            value_type: TypeExpr::Predefined {
                kind: PredefinedKind::Any,
                not_null: true,
            },
            default_value: None,
            inline_constraint: None,
        }]
    );
    assert_eq!(
        mapping.skipped,
        [
            "dimensionless vector property N.optional (not representable)".to_owned(),
            "dimensionless vector property N.required (not representable)".to_owned(),
        ]
    );
}

/// A graph-type vector needs a positive dimension (ADR-0364 §5.4).
#[test]
fn a_non_positive_vector_dimension_fails_loudly() {
    for dimension in [0, -1] {
        let doc = node_doc(
            json!({"p": {"type": "VECTOR<FLOAT>", "dimension": dimension}}),
            json!({}),
        );
        let err = to_graph_type_body(&doc).expect_err("a non-positive dimension fails");
        let text = err.to_string();
        assert!(matches!(&err, OntologyError::Mapping(_)), "{text}");
        assert!(text.contains("property 'p'"), "{text}");
        assert!(text.contains(&format!("dimension {dimension}")), "{text}");
        assert!(text.contains("positive"), "{text}");
    }
}

/// The exact inner coordinate kinds inside vectors, with dimensions.
#[test]
fn vector_tokens_map_coordinate_kinds_and_dimensions() {
    let doc = node_doc(
        json!({
            "vf":   {"type": "VECTOR<FLOAT>", "dimension": 3},
            "vf32": {"type": "VECTOR<FLOAT32>", "dimension": 4},
            "vi":   {"type": "VECTOR<INTEGER>", "dimension": 5},
            "vi32": {"type": "VECTOR<INTEGER32>", "dimension": 6},
            "vi16": {"type": "VECTOR<INTEGER16>", "dimension": 7},
            "vi8":  {"type": "VECTOR<INTEGER8>", "dimension": 8},
        }),
        json!({}),
    );
    let mapping = to_graph_type_body(&doc).expect("vectors convert");
    let vectors: Vec<(Option<VectorCoordKind>, Option<i64>)> = mapping.body.node_types[0]
        .properties
        .iter()
        .map(|p| match p.value_type {
            TypeExpr::Vector {
                coord, dimension, ..
            } => (coord, dimension),
            ref other => panic!("expected a vector type, got {other:?}"),
        })
        .collect();
    assert_eq!(
        vectors,
        [
            (Some(VectorCoordKind::Float64), Some(3)),
            (Some(VectorCoordKind::Float32), Some(4)),
            (Some(VectorCoordKind::Int64), Some(5)),
            (Some(VectorCoordKind::Int32), Some(6)),
            (Some(VectorCoordKind::Int16), Some(7)),
            (Some(VectorCoordKind::Int8), Some(8)),
        ]
    );
}

/// Empty names never declare or assert; a descriptive empty-named
/// property is carried and reported skipped.
#[test]
fn empty_named_properties_fail_when_declared_or_asserted() {
    let declared = node_doc(json!({"": {"type": "STRING"}}), json!({}));
    let err = to_graph_type_body(&declared).expect_err("an empty declared name fails");
    let text = err.to_string();
    assert!(matches!(&err, OntologyError::Mapping(_)), "{text}");
    assert!(text.contains("Ontology node 'N'"), "{text}");
    assert!(text.contains("property ''"), "{text}");
    assert!(text.contains("non-empty"), "{text}");

    let flagged = node_doc(json!({"": {"must_exist": true}}), json!({}));
    let err = to_graph_type_body(&flagged).expect_err("an empty asserted name fails");
    let text = err.to_string();
    assert!(text.contains("property ''"), "{text}");
    assert!(text.contains("non-empty"), "{text}");

    let listed = node_doc(
        json!({"": {}}),
        json!({"c": {"type": "EXISTS", "properties": [""]}}),
    );
    let err = to_graph_type_body(&listed).expect_err("an empty listed name fails");
    let text = err.to_string();
    // The requirement the object places on the empty-named property is
    // what fails: an empty name cannot declare.
    assert!(text.contains("property ''"), "{text}");
    assert!(text.contains("non-empty"), "{text}");

    // A purely descriptive empty-named property reports its untyped
    // skip and declares nothing.
    let descriptive = node_doc(json!({"": {}}), json!({}));
    let mapping = to_graph_type_body(&descriptive).expect("a descriptive property converts");
    assert!(mapping.body.node_types.is_empty());
    assert!(mapping
        .skipped
        .contains(&"untyped property N. (not representable)".to_owned()));
}

/// Two ids resolving to one name would declare one graph-type property
/// twice; the collision fails loudly.
#[test]
fn duplicate_resolved_property_names_fail() {
    let doc = node_doc(
        json!({"a": {"type": "STRING"}, "b": {"type": "STRING", "name": "a"}}),
        json!({}),
    );
    let err = to_graph_type_body(&doc).expect_err("a duplicate resolved name fails");
    assert!(
        err.to_string()
            .contains("Ontology node 'N' has multiple properties resolving to name 'a'."),
        "{err}"
    );
}
