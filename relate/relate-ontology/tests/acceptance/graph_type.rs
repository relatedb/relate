//! Graph-type derivation on 1.1.0-shape documents (ADR-0226 §2,
//! ADR-0364 §5, ADR-0487 §3, the revision of ADR-0667): a convertible
//! example derives a body, and every spec-valid construct the body
//! cannot express fails loudly.

use relate_names::SmallName;
use relate_ontology::{parse_ontology_document, to_graph_type_body, OntologyError};
use relate_parser::{PredefinedKind, TypeExpr, VectorCoordKind};

use crate::support::{json_doc, mapping_error, EXAMPLE_JSON};

// ---------------------------------------------------------------------------
// Graph-type derivation (ADR-0226 §2, ADR-0364 §5)
// ---------------------------------------------------------------------------

/// A convertible spec-shape example: keys, `must_exist` declarations, a
/// `UUID`, implied labels and a node-referenced relationship. The
/// validation-only `schema/example.json` deliberately is not
/// convertible, so `apply` coverage uses this separate document
/// (ADR-0364 §6).
const APPLY_EXAMPLE_JSON: &str = concat!(
    "{\"schema\":\"https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json\",",
    "\"id\":\"people\",\"version\":\"1\",",
    "\"nodes\":{",
    "\"person\":{\"label\":\"Person\",\"labels\":{\"identifier\":\"Person\",",
    "\"implied\":[\"Human\"]},\"properties\":{",
    "\"id\":{\"type\":\"STRING\",\"key\":true},",
    "\"name\":{\"type\":\"STRING\",\"must_exist\":true}},",
    "\"constraints\":{\"person-id\":{\"type\":\"KEY\",\"properties\":[\"id\"],",
    "\"name\":\"person_id_key\"}}},",
    "\"company\":{\"labels\":{\"identifier\":\"Company\"},\"properties\":{",
    "\"id\":{\"type\":\"UUID\",\"key\":true}}}},",
    "\"relationships\":{\"works-at\":{\"type\":\"WORKS_AT\",",
    "\"from\":{\"node\":\"person\"},\"to\":{\"node\":\"company\"},",
    "\"properties\":{\"since\":{\"type\":\"DATE\"}}}}}"
);

#[test]
fn graph_type_derivation_from_a_spec_shape_example() {
    let doc = parse_ontology_document(APPLY_EXAMPLE_JSON.as_bytes()).expect("example validates");
    let mapping = to_graph_type_body(&doc).expect("convertible example derives a graph type");
    let body = &mapping.body;

    // Node type specs, in document order, with the identifying label as
    // the node label and no aliases: references resolve by label.
    assert_eq!(body.node_types.len(), 2);
    let person = &body.node_types[0];
    assert_eq!(person.alias, None);
    assert_eq!(person.identifying_label, "Person");
    assert_eq!(person.implied_labels, ["Human"]);
    assert_eq!(person.properties.len(), 2);
    assert_eq!(person.properties[0].name, "id");
    assert_eq!(
        person.properties[0].value_type,
        TypeExpr::Predefined {
            kind: PredefinedKind::String,
            not_null: false
        }
    );
    // The `must_exist` flag compiles into the declaration's NOT NULL.
    assert_eq!(
        person.properties[1].value_type,
        TypeExpr::Predefined {
            kind: PredefinedKind::String,
            not_null: true
        }
    );
    let company = &body.node_types[1];
    assert_eq!(company.identifying_label, "Company");
    assert_eq!(
        company.properties[0].value_type,
        TypeExpr::Predefined {
            kind: PredefinedKind::Uuid,
            not_null: false
        }
    );

    // The edge type resolves both endpoints to the referenced nodes'
    // identifying labels.
    assert_eq!(body.edge_types.len(), 1);
    let works_at = &body.edge_types[0];
    assert_eq!(works_at.alias, None);
    assert_eq!(works_at.identifying_label, "WORKS_AT");
    assert_eq!(
        works_at.source,
        relate_parser::GraphTypeElementRef::InSitu {
            alias: None,
            label: SmallName::from_name("Person"),
            implies: true,
        }
    );
    assert_eq!(
        works_at.target,
        relate_parser::GraphTypeElementRef::InSitu {
            alias: None,
            label: SmallName::from_name("Company"),
            implies: true,
        }
    );

    // The `key` flags become named single-property definitions and the
    // explicit object keeps its own name: three definitions, none
    // dropped.
    assert_eq!(body.constraints.len(), 3);
    assert_eq!(
        body.constraints[0].name.as_ref(),
        Some(&SmallName::from_name("id_person_key"))
    );
    assert_eq!(body.constraints[0].properties, ["id"]);
    assert_eq!(
        body.constraints[0].kind,
        relate_parser::GraphTypeConstraintVerb::Key
    );
    assert_eq!(
        body.constraints[1].name.as_ref(),
        Some(&SmallName::from_name("person_id_key"))
    );
    assert_eq!(
        body.constraints[2].name.as_ref(),
        Some(&SmallName::from_name("id_company_key"))
    );
    assert!(body.drop_constraint_names.is_empty());
    assert!(mapping.skipped.is_empty());
}

#[test]
fn the_validation_only_example_is_not_silently_converted() {
    let doc = parse_ontology_document(EXAMPLE_JSON.as_bytes()).unwrap();
    let err = to_graph_type_body(&doc).expect_err("the validation-only example is not convertible");
    assert!(matches!(err, OntologyError::Mapping(_)), "{err}");
    // The first failure names the relationship entry whose `from`
    // endpoint carries neither a node nor a label.
    assert!(
        err.to_string()
            .contains("Ontology relationship 'empty-targets' references unknown 'from' node ''."),
        "{err}"
    );
}

#[test]
fn property_type_constraints_fail_loudly_at_apply() {
    let err = mapping_error(
        ",\"nodes\":{\"N\":{\"label\":\"N\",\"properties\":{\"p\":{\"type\":\"STRING\"}},\
         \"constraints\":{\"c\":{\"type\":\"PROPERTY_TYPE\",\"properties\":[\"p\"]}}}}",
    );
    assert!(err.to_string().contains("constraint 'c'"), "{err}");
    assert!(err.to_string().contains("PROPERTY_TYPE"), "{err}");
}

/// The narrow numerics widen for scalars and lists, and keep their
/// exact coordinate kinds inside vectors (ADR-0667): each base token
/// pins its widened `PredefinedKind` and its exact `VectorCoordKind`,
/// so a converter that widened or dropped a vector's coordinates —
/// mapping `VECTOR<INTEGER8>` to `Float` coordinates, say — fails the
/// row.
#[test]
fn narrow_numeric_tokens_widen_with_exact_vector_kinds() {
    for (token, scalar_kind, coord) in [
        ("FLOAT32", PredefinedKind::Float, VectorCoordKind::Float32),
        ("INTEGER8", PredefinedKind::Integer, VectorCoordKind::Int8),
        ("INTEGER16", PredefinedKind::Integer, VectorCoordKind::Int16),
        ("INTEGER32", PredefinedKind::Integer, VectorCoordKind::Int32),
    ] {
        // The scalar and list forms widen to the row's kind.
        for form in [token.to_owned(), format!("LIST<{token}>")] {
            let doc = parse_ontology_document(
                json_doc(&format!(
                    ",\"nodes\":{{\"N\":{{\"label\":\"N\",\"properties\":{{\"p\":{{\"type\":\"{form}\"}}}}}}}}"
                ))
                .as_bytes(),
            )
            .unwrap_or_else(|e| panic!("{form} is spec-valid: {e}"));
            let mapping = to_graph_type_body(&doc).unwrap_or_else(|e| panic!("{form}: {e}"));
            let declared = match &mapping.body.node_types[0].properties[0].value_type {
                TypeExpr::Predefined { kind, .. } => *kind,
                TypeExpr::List { element, .. } => match **element {
                    TypeExpr::Predefined { kind, .. } => kind,
                    _ => panic!("{form}: a list of a predefined kind"),
                },
                other => panic!("{form}: expected a scalar or list, got {other:?}"),
            };
            assert_eq!(declared, scalar_kind, "{form}");
        }

        // The vector form keeps the exact inner kind (CIP-216) with the
        // declared dimension — including the 8/16/32-bit element types
        // that widen as plain scalars.
        let doc = parse_ontology_document(
            json_doc(&format!(
                ",\"nodes\":{{\"N\":{{\"label\":\"N\",\"properties\":{{\"p\":{{\"type\":\"VECTOR<{token}>\",\"dimension\":3}}}}}}}}"
            ))
            .as_bytes(),
        )
        .unwrap();
        let mapping = to_graph_type_body(&doc).unwrap_or_else(|e| panic!("VECTOR<{token}>: {e}"));
        match &mapping.body.node_types[0].properties[0].value_type {
            TypeExpr::Vector {
                coord: Some(actual),
                dimension: Some(3),
                ..
            } => assert_eq!(*actual, coord, "VECTOR<{token}>"),
            other => panic!("VECTOR<{token}>: expected a dimension-3 vector, got {other:?}"),
        }
    }
}

/// A label-only endpoint converts to a plain label reference; an
/// endpoint with neither a node nor a label fails naming the
/// relationship, the end and the empty node.
#[test]
fn label_only_endpoints_convert_and_bare_endpoints_fail() {
    let doc = parse_ontology_document(
        json_doc(
            ",\"nodes\":{\"N\":{\"label\":\"N\"}},\"relationships\":{\"R\":{\"type\":\"R\",\
             \"from\":{\"label\":\"N\"},\"to\":{\"node\":\"N\"}}}",
        )
        .as_bytes(),
    )
    .expect("a label-only from endpoint is spec-valid");
    let mapping = to_graph_type_body(&doc).expect("a label-only endpoint converts");
    assert_eq!(
        mapping.body.edge_types[0].source,
        relate_parser::GraphTypeElementRef::InSitu {
            alias: None,
            label: SmallName::from_name("N"),
            implies: false,
        }
    );

    let err = mapping_error(
        ",\"nodes\":{\"N\":{\"label\":\"N\"}},\"relationships\":{\
         \"empty\":{\"type\":\"E\",\"from\":{\"node\":\"N\"},\"to\":{}}}",
    );
    assert!(
        err.to_string()
            .contains("Ontology relationship 'empty' references unknown 'to' node ''."),
        "{err}"
    );
}

#[test]
fn nullable_and_empty_main_forms_validate_but_fail_loudly_at_apply() {
    // No identifying label: accepted at validation, loud at apply. Both the
    // null and the absent form are spec-valid; the error names the entry.
    let doc = parse_ontology_document(json_doc(",\"nodes\":{\"N\":{\"label\":null}}").as_bytes())
        .expect("a null label is spec-valid");
    let err = to_graph_type_body(&doc).expect_err("a node needs a label to be convertible");
    assert!(
        err.to_string()
            .contains("node 'N' has no identifying label"),
        "{err}"
    );
    let doc = parse_ontology_document(
        json_doc(",\"nodes\":{\"meaningless\":{\"description\":\"no label\"}}").as_bytes(),
    )
    .expect("a label-less node entry is spec-valid");
    let err = to_graph_type_body(&doc).expect_err("a node needs a label to be convertible");
    assert!(
        err.to_string()
            .contains("node 'meaningless' has no identifying label"),
        "{err}"
    );

    // An empty constraint property list: accepted at validation, loud at
    // apply.
    let doc = parse_ontology_document(
        json_doc(
            ",\"nodes\":{\"N\":{\"label\":\"N\",\"constraints\":{\"c\":{\"type\":\"EXISTS\",\"properties\":[]}}}}",
        )
        .as_bytes(),
    )
    .expect("an empty constraint property list is spec-valid");
    let err = to_graph_type_body(&doc).expect_err("a graph-type constraint needs a property");
    assert!(err.to_string().contains("constraint 'c'"), "{err}");
    assert!(err.to_string().contains("no properties"), "{err}");

    // A purely descriptive empty-named property is carried and reported
    // skipped; one that declares a type fails loudly.
    let doc = parse_ontology_document(
        json_doc(",\"nodes\":{\"N\":{\"label\":\"N\",\"properties\":{\"\":{}}}}").as_bytes(),
    )
    .expect("a descriptive empty-named property is spec-valid");
    let mapping = to_graph_type_body(&doc).unwrap();
    assert!(mapping.body.node_types.is_empty());
    let doc = parse_ontology_document(
        json_doc(
            ",\"nodes\":{\"N\":{\"label\":\"N\",\"properties\":{\"\":{\"type\":\"STRING\"}}}}",
        )
        .as_bytes(),
    )
    .expect("a typed empty-named property is spec-valid");
    let err = to_graph_type_body(&doc).expect_err("an empty property name cannot be declared");
    assert!(err.to_string().contains("non-empty"), "{err}");
}
