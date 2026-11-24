//! The relationship-declaration policy's tests (ADR-0668, composed
//! with the revised contract of ADR-0667): a type declared across
//! several distinct endpoint pairs defines no graph type and is
//! reported as skipped, restatements resolve, and one identifying
//! label names one node type. Split from the converter's test module
//! so the policy's tests sit beside their subject; the token, property
//! and constraint tests stay in the parent.

use super::*;

/// A document's `nodes` section giving the declaration tests three
/// endpoint labels, each node carrying one representable property —
/// data, interpolated directly, so no test below also trips over the
/// dropped-element report a bare label node would add.
const PAIR_NODES: &str = concat!(
    "\"nodes\":{\"a\":{\"label\":\"A\",\"properties\":{\"p\":{\"type\":\"STRING\"}}},",
    "\"b\":{\"label\":\"B\",\"properties\":{\"p\":{\"type\":\"STRING\"}}},",
    "\"c\":{\"label\":\"C\",\"properties\":{\"p\":{\"type\":\"STRING\"}}}}"
);

/// One relationship entry, as the raw JSON the declaration tests
/// compose — the type, the `from`/`to` node keys, and the properties
/// and constraints the entry carries, assembled in one `json!`: no
/// spliced fragments, no merge, so nothing can be silently dropped. An
/// empty object is the empty property (or constraint) declaration.
fn rel_entry(
    key: &str,
    rel_type: &str,
    from: &str,
    to: &str,
    properties: serde_json::Value,
    constraints: serde_json::Value,
) -> String {
    let entry = json!({
        "type": rel_type,
        "from": {"node": from},
        "to": {"node": to},
        "properties": properties,
        "constraints": constraints,
    });
    format!("\"{key}\":{entry}")
}

/// One emitted edge spec's shape for assertions: the spec's alias (the
/// canonical form carries none), its identifying type, and its two
/// endpoint references — typed references, compared as the converter
/// builds them, never a rendered string.
#[derive(Debug, PartialEq)]
struct EdgeShape {
    alias: Option<String>,
    rel_type: String,
    source: GraphTypeElementRef,
    target: GraphTypeElementRef,
}

/// The edge specs of one conversion, in the body's order, as shapes.
fn edge_shapes(mapping: &GraphTypeMapping) -> Vec<EdgeShape> {
    mapping
        .body
        .edge_types
        .iter()
        .map(|edge| EdgeShape {
            alias: edge.alias.as_ref().map(|alias| alias.to_string()),
            rel_type: edge.identifying_label.to_string(),
            source: edge.source.clone(),
            target: edge.target.clone(),
        })
        .collect()
}

/// The type-level skip report a type declared across several distinct
/// pairs answers with — one spelling, the pinned format.
fn multi_pair_skip(rel_type: &str) -> String {
    format!(
        "relationship type {rel_type} is declared by multiple entries, none of which is applied"
    )
}

/// A type declared across several distinct endpoint pairs defines no
/// graph type (ADR-0668): no spec of it reaches the body, a distinct
/// type declared once keeps its spec beside it, and the type-level
/// report is the type's one skip entry.
#[test]
fn a_multi_pair_relationship_type_defines_no_graph_type() {
    let document = doc(&format!(
        ",{PAIR_NODES},\"relationships\":{{{}, {}, {}}}",
        rel_entry("t_a_b", "T", "a", "b", json!({}), json!({})),
        rel_entry("t_a_c", "T", "a", "c", json!({}), json!({})),
        // A distinct type declared once keeps its spec beside the
        // skipped one.
        rel_entry("knows", "KNOWS", "a", "b", json!({}), json!({})),
    ));
    let mapping = to_graph_type_body(&document).expect("the multi-pair document converts");
    assert_eq!(
        edge_shapes(&mapping),
        vec![EdgeShape {
            alias: None,
            rel_type: "KNOWS".into(),
            source: label_ref("A", true),
            target: label_ref("B", true),
        }],
        "the multi-pair type contributes no spec; the singly-declared one does"
    );
    // No entry in this document declares constraints, and the
    // multi-pair type's would have ridden with its skipped spec: the
    // whole body carries none.
    assert!(
        mapping.body.constraints.is_empty(),
        "{:?}",
        mapping.body.constraints
    );
    assert_eq!(mapping.skipped, [multi_pair_skip("T")]);
}

/// A property-bearing multi-pair type still defines no graph type
/// (ADR-0668): the skipped spec takes the entries' property and
/// constraint declarations with it — nothing of the type reaches the
/// body, and the type's entries contribute no skip entries of their
/// own, only the type-level report.
#[test]
fn a_multi_pair_type_with_properties_still_defines_no_graph_type() {
    let relationships = json!({
        "t_a_b": {"type": "T", "from": {"node": "a"}, "to": {"node": "b"},
                  "properties": {"since": {"type": "STRING", "unique": true}},
                  "description": "One of the pairs."},
        "t_a_c": {"type": "T", "from": {"node": "a"}, "to": {"node": "c"}},
    });
    let document = doc(&format!(",{PAIR_NODES},\"relationships\":{relationships}"));
    let mapping = to_graph_type_body(&document).expect("the multi-pair document converts");
    assert!(edge_shapes(&mapping).is_empty());
    assert!(
        mapping.body.constraints.is_empty(),
        "{:?}",
        mapping.body.constraints
    );
    assert_eq!(mapping.skipped, [multi_pair_skip("T")]);
}

/// An entry that restates an earlier entry's resolved pair identically
/// compiles once (ADR-0668): the pair is the declaration, and the type
/// stays singly-declared — its spec survives, nothing is skipped. The
/// restatement may spell an endpoint the other way: a label-only
/// target naming the same label resolves to the same endpoint, and the
/// first entry's spelling is the one the spec carries.
#[test]
fn an_identical_pair_restatement_compiles_once() {
    let document = doc(&format!(
        ",{PAIR_NODES},\"relationships\":{{{}, {}}}",
        rel_entry("t_first", "T", "a", "b", json!({}), json!({})),
        rel_entry("t_again", "T", "a", "b", json!({}), json!({})),
    ));
    let mapping = to_graph_type_body(&document).expect("the restatement converts");
    assert_eq!(
        edge_shapes(&mapping),
        vec![EdgeShape {
            alias: None,
            rel_type: "T".into(),
            source: label_ref("A", true),
            target: label_ref("B", true),
        }]
    );
    assert!(
        mapping.skipped.is_empty(),
        "the restatement applies once and carries nothing descriptive: {:?}",
        mapping.skipped
    );

    // The other spelling of one endpoint: a label-only `from` naming
    // the first entry's resolved label restates the same pair.
    let relationships = json!({
        "t_first": {"type": "T", "from": {"node": "a"}, "to": {"node": "b"}},
        "t_again": {"type": "T", "from": {"label": "A"}, "to": {"node": "b"}},
    });
    let respelled = doc(&format!(",{PAIR_NODES},\"relationships\":{relationships}"));
    let mapping = to_graph_type_body(&respelled).expect("the respelled restatement converts");
    assert_eq!(
        edge_shapes(&mapping),
        vec![EdgeShape {
            alias: None,
            rel_type: "T".into(),
            source: label_ref("A", true),
            target: label_ref("B", true),
        }],
        "one pair, one spec — the first entry's spelling"
    );
}

/// An entry restating an earlier entry's resolved pair with a different
/// declaration is a conflict the mapping cannot express (ADR-0668): a
/// loud `Mapping` failure naming both entries, never a silent first-wins.
#[test]
fn a_same_pair_restatement_with_a_different_declaration_refuses() {
    let document = doc(&format!(
        ",{PAIR_NODES},\"relationships\":{{{}, {}}}",
        rel_entry("t_first", "T", "a", "b", json!({}), json!({})),
        rel_entry(
            "t_conflicting",
            "T",
            "a",
            "b",
            json!({"since": {"type": "STRING"}}),
            json!({})
        ),
    ));
    let error = to_graph_type_body(&document).expect_err("a conflicting restatement refuses");
    assert!(matches!(error, OntologyError::Mapping(_)), "{error}");
    let rendered = error.to_string();
    assert!(
        rendered.contains("t_conflicting") && rendered.contains("t_first"),
        "the refusal names both entries, got {rendered}"
    );
    assert!(
        rendered.contains("one endpoint pair, one declaration"),
        "the refusal names the policy, got {rendered}"
    );
}

/// A skipped type's entries are never compiled (ADR-0668): the
/// multi-pair verdict is decided from the resolved endpoints before any
/// payload compiles, so a multi-pair vocabulary whose entry carries a
/// payload the graph-type body cannot express does not refuse the
/// document, through `apply` and the vault open alike. A type that
/// defines a graph type still refuses the same payload.
#[test]
fn a_skipped_type_is_never_policed_for_its_payload() {
    let unconvertible = |key: &str, from: &str, to: &str| {
        rel_entry(
            key,
            "T",
            from,
            to,
            json!({"since": {"type": "STRING"}}),
            json!({"since_type": {"type": "PROPERTY_TYPE", "properties": ["since"]}}),
        )
    };
    let skipped = doc(&format!(
        ",{PAIR_NODES},\"relationships\":{{{}, {}}}",
        unconvertible("t_a_b", "a", "b"),
        rel_entry("t_a_c", "T", "a", "c", json!({}), json!({})),
    ));
    let mapping = to_graph_type_body(&skipped).expect("the multi-pair document converts");
    assert!(edge_shapes(&mapping).is_empty());
    assert_eq!(mapping.skipped, [multi_pair_skip("T")]);

    let emitting = doc(&format!(
        ",{PAIR_NODES},\"relationships\":{{{}}}",
        unconvertible("t_a_b", "a", "b"),
    ));
    let error =
        to_graph_type_body(&emitting).expect_err("the singly-declared type's payload refuses");
    assert!(
        matches!(error, OntologyError::Mapping(_)) && error.to_string().contains("PROPERTY_TYPE"),
        "{error}"
    );
}

/// A conflicting restatement inside a multi-pair type is not refused
/// either (ADR-0668): the restatement policy polices the types that
/// define a graph type, and a skipped type's declarations never compare.
#[test]
fn a_conflicting_restatement_in_a_multi_pair_type_is_skipped_not_refused() {
    let document = doc(&format!(
        ",{PAIR_NODES},\"relationships\":{{{}, {}, {}}}",
        rel_entry("t_a_b", "T", "a", "b", json!({}), json!({})),
        rel_entry(
            "t_a_b_again",
            "T",
            "a",
            "b",
            json!({"since": {"type": "STRING"}}),
            json!({})
        ),
        rel_entry("t_a_c", "T", "a", "c", json!({}), json!({})),
    ));
    let mapping = to_graph_type_body(&document).expect("the multi-pair document converts");
    assert!(edge_shapes(&mapping).is_empty());
    assert_eq!(mapping.skipped, [multi_pair_skip("T")]);
}

/// An identical restatement's entries each report their own descriptive
/// fields: the result row states what was not applied, and neither the
/// declaration's nor the restatement's decorations are.
#[test]
fn an_identical_restatement_reports_each_entrys_decorations() {
    let relationships = json!({
        "t_first": {"type": "T", "from": {"node": "a"}, "to": {"node": "b"}},
        "t_again": {"type": "T", "from": {"node": "a"}, "to": {"node": "b"},
                    "description": "The restated declaration."},
    });
    let document = doc(&format!(",{PAIR_NODES},\"relationships\":{relationships}"));
    let mapping = to_graph_type_body(&document).expect("the restatement converts");
    assert_eq!(mapping.skipped, ["description on t_again".to_owned()]);
}

/// One label, one node type (ADR-0668): two node entries declaring the
/// same identifying label — whatever their keys — are a duplicate the
/// mapping refuses by name, never a body the graph-type validation
/// would refuse late and generically.
#[test]
fn same_label_node_entries_refuse() {
    let document = doc(
        r#","nodes":{"a":{"label":"A"},"a2":{"label":"A"},"b":{"label":"B"}},"relationships":{}"#,
    );
    let error = to_graph_type_body(&document).expect_err("a shared identifying label refuses");
    assert!(matches!(error, OntologyError::Mapping(_)), "{error}");
    let rendered = error.to_string();
    assert!(
        rendered.contains("a2") && rendered.contains("'a'") && rendered.contains("A"),
        "the refusal names both entries and the label, got {rendered}"
    );
    assert!(
        rendered.contains("one label, one node type"),
        "the refusal names the policy, got {rendered}"
    );
}
