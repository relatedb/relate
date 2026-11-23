//! The relationship-pair policy's tests (ADR-0668): a type declared
//! across several endpoint pairs defines no graph type, and restatements
//! resolve. Split from the converter's test module so the policy's tests
//! sit beside their subject; the node half's tests stay in the parent.

use super::*;

use crate::tests::doc;
use relate_names::SmallName;
use relate_parser::GraphTypeElementRef;

/// A document's `nodes` section giving the pair tests three endpoint
/// labels — data, interpolated directly.
const PAIR_NODES: &str =
    "\"nodes\":{\"a\":{\"label\":\"A\"},\"b\":{\"label\":\"B\"},\"c\":{\"label\":\"C\"}}";

/// One relationship entry, as the raw JSON the pair tests compose —
/// the type, the `from`/`to` node keys and the `properties` declaration
/// stated, assembled in one `json!`: no spliced fragments, no merge, so
/// nothing can be silently dropped. An empty object is the empty
/// property declaration.
fn rel_entry(
    key: &str,
    rel_type: &str,
    from: &str,
    to: &str,
    properties: serde_json::Value,
) -> String {
    let entry = json!({
        "type": rel_type,
        "from": {"node": from},
        "to": {"node": to},
        "properties": properties,
    });
    format!("\"{key}\":{entry}")
}

/// One emitted edge spec's shape for assertions: the spec's alias, its
/// identifying type, and its two endpoint references — the typed
/// reference, compared as the converter builds it, never a rendered
/// string.
#[derive(Debug, PartialEq)]
struct EdgeShape {
    alias: Option<String>,
    rel_type: String,
    source: GraphTypeElementRef,
    target: GraphTypeElementRef,
}

fn edge_shapes(body: &GraphTypeBody) -> Vec<EdgeShape> {
    body.edge_types
        .iter()
        .map(|et| EdgeShape {
            alias: et.alias.as_ref().map(|alias| alias.to_string()),
            rel_type: et.identifying_label.to_string(),
            source: et.source.clone(),
            target: et.target.clone(),
        })
        .collect()
}

/// The typed endpoint reference the converter builds for one node key.
fn endpoint_ref(node: &str) -> GraphTypeElementRef {
    GraphTypeElementRef::AliasRef(SmallName::from_name(node))
}

#[test]
fn a_multi_pair_relationship_type_defines_no_graph_type() {
    let document = doc(&format!(
        ",{PAIR_NODES},\"relationships\":{{{}, {}, {}}}",
        rel_entry("t_a_b", "T", "a", "b", json!({})),
        rel_entry("t_a_c", "T", "a", "c", json!({})),
        // A distinct type declared once keeps its spec beside the
        // skipped one.
        rel_entry("knows", "KNOWS", "a", "b", json!({})),
    ));
    let body = to_graph_type_body(&document).expect("the multi-pair document converts");
    assert_eq!(
        edge_shapes(&body),
        vec![EdgeShape {
            alias: Some("knows".into()),
            rel_type: "KNOWS".into(),
            source: endpoint_ref("a"),
            target: endpoint_ref("b"),
        }],
        "the multi-pair type contributes no spec; the singly-declared one does"
    );
    // No entry in this document declares constraints, and the
    // multi-pair type's would have ridden with its skipped spec: the
    // whole body carries none.
    assert!(body.constraints.is_empty(), "{:?}", body.constraints);
}

/// A property-bearing multi-pair type still defines no graph type
/// (ADR-0668): the skipped spec takes the entries' property and
/// constraint declarations with it — nothing of the type reaches the
/// body.
#[test]
fn a_multi_pair_type_with_properties_still_defines_no_graph_type() {
    let document = doc(&format!(
        ",{PAIR_NODES},\"relationships\":{{{}, {}}}",
        rel_entry(
            "t_a_b",
            "T",
            "a",
            "b",
            json!({"since": {"type": "STRING", "unique": true}}),
        ),
        rel_entry("t_a_c", "T", "a", "c", json!({})),
    ));
    let body = to_graph_type_body(&document).expect("the multi-pair document converts");
    assert!(edge_shapes(&body).is_empty());
    assert!(body.constraints.is_empty(), "{:?}", body.constraints);
}

/// An entry that restates an earlier entry's resolved pair identically
/// compiles once (ADR-0668): the pair is the declaration, and the type
/// stays singly-declared — its spec survives.
#[test]
fn an_identical_pair_restatement_compiles_once() {
    let document = doc(&format!(
        ",{PAIR_NODES},\"relationships\":{{{}, {}}}",
        rel_entry("t_first", "T", "a", "b", json!({})),
        rel_entry("t_again", "T", "a", "b", json!({})),
    ));
    let body = to_graph_type_body(&document).expect("the restatement converts");
    assert_eq!(
        edge_shapes(&body),
        vec![EdgeShape {
            alias: Some("t_first".into()),
            rel_type: "T".into(),
            source: endpoint_ref("a"),
            target: endpoint_ref("b"),
        }]
    );
}

/// An entry restating an earlier entry's resolved pair with a different
/// declaration is a conflict the mapping cannot express (ADR-0668): a
/// loud `Mapping` failure naming both entries, never a silent first-wins.
#[test]
fn a_same_pair_restatement_with_a_different_declaration_refuses() {
    let document = doc(&format!(
        ",{PAIR_NODES},\"relationships\":{{{}, {}}}",
        rel_entry("t_first", "T", "a", "b", json!({})),
        rel_entry(
            "t_conflicting",
            "T",
            "a",
            "b",
            json!({"since": {"type": "STRING"}}),
        ),
    ));
    let error = to_graph_type_body(&document).expect_err("a conflicting restatement refuses");
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

/// A skipped type's entries are never compiled (ADR-0668 §1): the
/// multi-pair verdict is decided from the resolved endpoints before any
/// payload compiles, so a multi-pair vocabulary whose entry carries a
/// payload the graph-type body cannot express — a token with no
/// counterpart — does not refuse the document, through `apply` and the
/// vault open alike. A type that defines a graph type still refuses the
/// same payload.
#[test]
fn a_skipped_type_is_never_policed_for_its_payload() {
    let skipped = doc(&format!(
        ",{PAIR_NODES},\"relationships\":{{{}, {}}}",
        rel_entry(
            "t_a_b",
            "T",
            "a",
            "b",
            json!({"since": {"type": "FLOAT32"}})
        ),
        rel_entry("t_a_c", "T", "a", "c", json!({})),
    ));
    let body = to_graph_type_body(&skipped).expect("the multi-pair document converts");
    assert!(edge_shapes(&body).is_empty());

    let emitting = doc(&format!(
        ",{PAIR_NODES},\"relationships\":{{{}}}",
        rel_entry(
            "t_a_b",
            "T",
            "a",
            "b",
            json!({"since": {"type": "FLOAT32"}})
        ),
    ));
    assert!(
        to_graph_type_body(&emitting).is_err(),
        "the singly-declared type's unconvertible payload still refuses"
    );
}

/// A conflicting restatement inside a multi-pair type is not refused
/// either (ADR-0668 §3): the restatement policy polices the types that
/// define a graph type, and a skipped type's declarations never compare.
#[test]
fn a_conflicting_restatement_in_a_multi_pair_type_is_skipped_not_refused() {
    let document = doc(&format!(
        ",{PAIR_NODES},\"relationships\":{{{}, {}, {}}}",
        rel_entry("t_a_b", "T", "a", "b", json!({})),
        rel_entry(
            "t_a_b_again",
            "T",
            "a",
            "b",
            json!({"since": {"type": "STRING"}}),
        ),
        rel_entry("t_a_c", "T", "a", "c", json!({})),
    ));
    let body = to_graph_type_body(&document).expect("the multi-pair document converts");
    assert!(edge_shapes(&body).is_empty());
}
