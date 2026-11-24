//! The constraint-definition concern's tests (ADR-0667): the
//! `key`/`unique` flags and `KEY`/`UNIQUE` objects compile into named
//! standalone definitions over the resolved names, `EXISTS` compiles
//! into the declarations' `NOT NULL`, a dropped element's definitions
//! stay aimed at the label, and the loud failures name the owner and
//! the construct. Split from the converter's test module so the
//! definitions' tests sit beside their subject.

use super::*;

/// One expected standalone constraint definition.
fn expected_constraint(
    name: Option<&str>,
    owner: &str,
    is_edge: bool,
    properties: &[&str],
    kind: GraphTypeConstraintVerb,
    implies: bool,
) -> GraphTypeConstraintSpec {
    GraphTypeConstraintSpec {
        name: name.map(SmallName::from_name),
        target: label_ref(owner, implies),
        is_edge,
        properties: properties.iter().map(|p| SmallName::from_name(p)).collect(),
        kind,
        property_type: None,
        options: OptionsForm::Absent,
    }
}

/// An `EXISTS` constraint object makes its property required exactly as
/// the `must_exist` flag does: the declaration is `NOT NULL`, and no
/// standalone existence constraint is emitted.
#[test]
fn exists_constraints_require_their_properties() {
    let doc = node_doc(
        json!({"a": {"type": "STRING"}, "b": {"type": "ANY"}}),
        json!({"a_exists": {"type": "EXISTS", "properties": ["a"]},
               "b_exists": {"type": "EXISTS", "properties": ["b"]}}),
    );
    let mapping = to_graph_type_body(&doc).expect("converts");
    assert_eq!(
        mapping.body.node_types[0].properties,
        [
            GraphTypePropertyType {
                name: SmallName::from_name("a"),
                value_type: TypeExpr::Predefined {
                    kind: PredefinedKind::String,
                    not_null: true,
                },
                default_value: None,
                inline_constraint: None,
            },
            GraphTypePropertyType {
                name: SmallName::from_name("b"),
                value_type: TypeExpr::Predefined {
                    kind: PredefinedKind::Any,
                    not_null: true,
                },
                default_value: None,
                inline_constraint: None,
            },
        ]
    );
    assert!(mapping.body.constraints.is_empty());
    assert_eq!(
        mapping.skipped,
        ["untyped property N.b (not representable)"]
    );
}

/// An `EXISTS` constraint covering several properties, or naming a
/// property the owner does not declare, fails loudly naming the entry.
#[test]
fn exists_constraints_are_single_property_and_resolvable() {
    let multi = node_doc(
        json!({"a": {"type": "STRING"}, "b": {"type": "STRING"}}),
        json!({"multi": {"type": "EXISTS", "properties": ["a", "b"]}}),
    );
    let err = to_graph_type_body(&multi).expect_err("a multi-property EXISTS fails");
    assert!(matches!(err, OntologyError::Mapping(_)), "{err}");
    assert!(
        err.to_string().contains(
            "Ontology node 'N' has an existence constraint covering multiple properties, \
             which is not representable in a graph type."
        ),
        "{err}"
    );

    let missing = node_doc(
        json!({"a": {"type": "STRING"}}),
        json!({"ghost": {"type": "EXISTS", "properties": ["missing"]}}),
    );
    let err = to_graph_type_body(&missing).expect_err("an unknown EXISTS property fails");
    assert!(
        err.to_string()
            .contains("Ontology node 'N' references unknown property 'missing'."),
        "{err}"
    );
}

/// A node whose content is all unrepresentable is dropped with a skip
/// report, and its constraints stay as definitions on the label —
/// aimed without implying a declared node type.
#[test]
fn an_element_with_no_representable_content_is_dropped_keeping_constraints() {
    let doc = doc(r#","nodes":{"Empty":{"label":"Empty",
             "properties":{"id":{"type":"ANY"}},
             "constraints":{"id_key":{"type":"KEY","properties":["id"]}}}}"#);
    let mapping = to_graph_type_body(&doc).expect("converts");
    assert!(mapping.body.node_types.is_empty());
    assert!(mapping
        .skipped
        .contains(&"element type Empty has no representable content".to_owned()));
    assert_eq!(
        mapping.body.constraints,
        vec![expected_constraint(
            None,
            "Empty",
            false,
            &["id"],
            GraphTypeConstraintVerb::Key,
            false,
        )]
    );
}

/// `key`/`unique` flags become named single-property definitions —
/// `<name>_<element>_<key|uniq>` from the property's resolved name —
/// and `KEY`/`UNIQUE` objects become definitions over the listed
/// properties' resolved names, sorted, keeping the object's name.
#[test]
fn flags_and_constraint_objects_become_named_definitions() {
    let doc = doc(r#","nodes":{"N":{"label":"N",
             "properties":{
                 "a":{"type":"STRING"},
                 "zeta":{"type":"STRING","name":"zed","unique":true},
                 "id":{"type":"STRING","key":true}},
             "constraints":{
                 "n_ba":{"type":"KEY","properties":["a","zeta"],"name":"n_ba"},
                 "n_a":{"type":"UNIQUE","properties":["a"]}}}}"#);
    let mapping = to_graph_type_body(&doc).expect("converts");
    assert_eq!(
        mapping.body.constraints,
        vec![
            // The flag definitions first, in property order.
            expected_constraint(
                Some("zed_N_uniq"),
                "N",
                false,
                &["zed"],
                GraphTypeConstraintVerb::Unique,
                true,
            ),
            expected_constraint(
                Some("id_N_key"),
                "N",
                false,
                &["id"],
                GraphTypeConstraintVerb::Key,
                true,
            ),
            // Then the objects, in document order, their properties
            // sorted by resolved name.
            expected_constraint(
                Some("n_ba"),
                "N",
                false,
                &["a", "zed"],
                GraphTypeConstraintVerb::Key,
                true,
            ),
            expected_constraint(
                None,
                "N",
                false,
                &["a"],
                GraphTypeConstraintVerb::Unique,
                true
            ),
        ]
    );
}

/// A relationship's own constraints target the edge type by label.
#[test]
fn relationship_constraint_definitions_target_the_edge_type() {
    let doc = doc(
        r#","nodes":{"A":{"label":"A","properties":{"p":{"type":"STRING"}}},
             "B":{"label":"B","properties":{"p":{"type":"STRING"}}}},
             "relationships":{"R":{"type":"REL",
             "from":{"node":"A"},"to":{"node":"B"},
             "properties":{"since":{"type":"DATE","must_exist":true}},
             "constraints":{"r_since":{"type":"UNIQUE","properties":["since"],
                                        "name":"r_since"},
                            "r_flag":{"type":"KEY","properties":["since"]}}}}"#,
    );
    let mapping = to_graph_type_body(&doc).expect("converts");
    assert_eq!(
        mapping.body.edge_types[0].properties,
        [GraphTypePropertyType {
            name: SmallName::from_name("since"),
            value_type: TypeExpr::Predefined {
                kind: PredefinedKind::Date,
                not_null: true,
            },
            default_value: None,
            inline_constraint: None,
        }]
    );
    assert_eq!(
        mapping.body.constraints,
        vec![
            expected_constraint(
                Some("r_since"),
                "REL",
                true,
                &["since"],
                GraphTypeConstraintVerb::Unique,
                true,
            ),
            expected_constraint(
                None,
                "REL",
                true,
                &["since"],
                GraphTypeConstraintVerb::Key,
                true
            ),
        ]
    );
}

/// A `PROPERTY_TYPE` constraint object fails loudly: the body cannot
/// express a property-type assertion (ADR-0364 §5.4).
#[test]
fn property_type_constraints_fail_loudly() {
    let doc = node_doc(
        json!({"p": {"type": "STRING"}}),
        json!({"c": {"type": "PROPERTY_TYPE", "properties": ["p"]}}),
    );
    let err = to_graph_type_body(&doc).expect_err("a PROPERTY_TYPE object fails");
    let text = err.to_string();
    assert!(text.contains("constraint 'c'"), "{text}");
    assert!(text.contains("PROPERTY_TYPE"), "{text}");
}

/// An empty constraint property list fails loudly, and a constraint
/// naming an undeclared property is a dangling reference.
#[test]
fn constraint_lists_must_be_non_empty_and_resolvable() {
    let empty = node_doc(
        json!({"a": {"type": "STRING"}}),
        json!({"c": {"type": "EXISTS", "properties": []}}),
    );
    let err = to_graph_type_body(&empty).expect_err("an empty list fails");
    let text = err.to_string();
    assert!(text.contains("constraint 'c'"), "{text}");
    assert!(text.contains("no properties"), "{text}");

    let dangling = node_doc(
        json!({"a": {"type": "STRING"}}),
        json!({"c": {"type": "UNIQUE", "properties": ["ghost"]}}),
    );
    let err = to_graph_type_body(&dangling).expect_err("a dangling constraint fails");
    let text = err.to_string();
    assert!(text.contains("Ontology node 'N'"), "{text}");
    assert!(
        text.contains("references unknown property 'ghost'."),
        "{text}"
    );
}
