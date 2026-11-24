//! The endpoint-resolution concern's tests (ADR-0667): relationship
//! endpoints resolve by label (a node reference to the node's
//! identifying label, a label-only target a plain label reference),
//! the three endpoint failures name the relationship and the end, and
//! the entry failures — a type-less relationship, a node without an
//! identifying label, a type declared by several entries — fail or
//! report in the mapper's exact words. The declaration policy's own
//! tests (restatements, the multi-pair verdict) live in
//! `relationships.rs`.

use super::*;

/// Endpoints resolve by label: a node reference resolves to the node's
/// identifying label (an explicit label must spell it exactly), and a
/// label-only endpoint is a plain label reference with no
/// declared-type implication.
#[test]
fn endpoints_resolve_by_label() {
    let doc = doc(r#","nodes":{"a":{"label":"A"},
             "b":{"labels":{"identifier":"B"}}},
             "relationships":{
             "node-only":{"type":"R1","from":{"node":"a"},"to":{"node":"b"}},
             "labeled":{"type":"R2","from":{"node":"a","label":"A"},
                                  "to":{"node":"b","label":"B"}},
             "label-only":{"type":"R3","from":{"label":"Trailer"},"to":{"node":"a"}}}"#);
    let mapping = to_graph_type_body(&doc).expect("converts");
    let endpoints: Vec<(GraphTypeElementRef, GraphTypeElementRef)> = mapping
        .body
        .edge_types
        .iter()
        .map(|edge| (edge.source.clone(), edge.target.clone()))
        .collect();
    assert_eq!(
        endpoints,
        [
            (label_ref("A", true), label_ref("B", true)),
            (label_ref("A", true), label_ref("B", true)),
            (label_ref("Trailer", false), label_ref("A", true)),
        ]
    );
}

/// The three endpoint failures, in the mapper's exact words.
#[test]
fn endpoint_failures_name_the_relationship_and_end() {
    let cases: &[(&str, &str)] = &[
        // An endpoint label that is not the referenced node's
        // identifying label.
        (
            r#""mislabeled":{"type":"R","from":{"node":"A","label":"Wrong"},
                             "to":{"node":"A"}}"#,
            "Ontology relationship 'mislabeled' references 'from' node 'A' with label \
             'Wrong', which the node does not have.",
        ),
        // An endpoint naming no node entry.
        (
            r#""unknown":{"type":"R","from":{"node":"Missing"},"to":{"node":"A"}}"#,
            "Ontology relationship 'unknown' references unknown 'from' node 'Missing'.",
        ),
        // An endpoint with neither a node nor a label (empty, or
        // whitespace-only spellings of either).
        (
            r#""bare":{"type":"R","from":{},"to":{"node":"A"}}"#,
            "Ontology relationship 'bare' references unknown 'from' node ''.",
        ),
    ];
    for (entry, message) in cases {
        let doc = doc(&format!(
            r#","nodes":{{"A":{{"label":"A"}}}},"relationships":{{{entry}}}"#
        ));
        let err = to_graph_type_body(&doc).expect_err("the endpoint fails");
        let text = err.to_string();
        assert!(matches!(err, OntologyError::Mapping(_)), "{text}");
        assert!(text.contains(message), "{text}");
    }
    let whitespace = doc(r#","nodes":{"A":{"label":"A"}},
             "relationships":{"R":{"type":"R","from":{"node":" "},"to":{"node":"A"}}}"#);
    let err = to_graph_type_body(&whitespace).expect_err("a whitespace node is absent");
    assert!(
        err.to_string()
            .contains("Ontology relationship 'R' references unknown 'from' node ''."),
        "{err}"
    );
}

/// A relationship entry without a type, a node without an identifying
/// label, and a type declared by several relationship entries.
#[test]
fn the_three_entry_failures_and_the_duplicated_type() {
    let typeless = doc(r#","nodes":{"A":{"label":"A"}},
        "relationships":{"R":{"from":{"node":"A"},"to":{"node":"A"}}}"#);
    let err = to_graph_type_body(&typeless).expect_err("a type-less relationship fails");
    assert!(
        err.to_string()
            .contains("Ontology relationship 'R' has no type."),
        "{err}"
    );

    let unlabeled = doc(r#","nodes":{"N":{"label":null,"properties":{"p":{"type":"STRING"}}}}"#);
    let err = to_graph_type_body(&unlabeled).expect_err("a node needs a label to be convertible");
    assert!(
        err.to_string()
            .contains("Ontology node 'N' has no identifying label."),
        "{err}"
    );

    // A type declared by several entries: none of the entries is
    // applied, and the type is reported rather than merged.
    let located_in = doc(
        r#","nodes":{"A":{"label":"A","properties":{"p":{"type":"STRING"}}},
             "B":{"label":"B","properties":{"p":{"type":"STRING"}}}},
             "relationships":{
             "first":{"type":"LOCATED_IN","from":{"node":"A"},"to":{"node":"B"}},
             "second":{"type":"LOCATED_IN","from":{"node":"B"},"to":{"node":"A"}}}"#,
    );
    let mapping = to_graph_type_body(&located_in).expect("converts");
    assert!(mapping
        .body
        .edge_types
        .iter()
        .all(|edge| edge.identifying_label != "LOCATED_IN"));
    assert!(mapping.skipped.contains(
        &"relationship type LOCATED_IN is declared by multiple entries, none of which is applied"
            .to_owned()
    ));
}
