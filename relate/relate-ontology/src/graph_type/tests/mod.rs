//! The converter's test module: the concern files carry the suite —
//! `tokens` (the per-token property classification and its declaration
//! policy), `constraints` (the standalone constraint definitions),
//! `endpoints` (the relationship-entry resolution and failures),
//! `relationships` (the ADR-0668 declaration policy), `movies` (the
//! full-mapping fixture) — and this module keeps the shared fixtures
//! plus the conversion-level tests (the skip report overview,
//! determinism).

mod constraints;
mod endpoints;
mod movies;
mod relationships;
mod tokens;

use super::*;
use crate::model::Neo4jType;
use crate::tests::doc;
use relate_names::SmallName;
use relate_parser::{
    GraphTypeConstraintSpec, GraphTypeConstraintVerb, GraphTypePropertyType, OptionsForm,
    PredefinedKind, TypeExpr, VectorCoordKind,
};
use serde_json::json;

/// A document with one node `N` carrying `properties` and `constraints`.
fn node_doc(properties: serde_json::Value, constraints: serde_json::Value) -> OntologyDocument {
    let node = json!({
        "label": "N",
        "properties": properties,
        "constraints": constraints,
    });
    doc(&format!(",\"nodes\":{{\"N\":{node}}}"))
}

/// The label reference the revised endpoints and constraint targets
/// produce, in the parser's canonical `(:Label =>)` shape.
fn label_ref(label: &str, implies: bool) -> GraphTypeElementRef {
    GraphTypeElementRef::InSitu {
        alias: None,
        label: SmallName::from_name(label),
        implies,
    }
}

/// The descriptive constructs the conversion leaves out, each reported
/// in the mapper's `"<what> on <where>"` shape.
#[test]
fn descriptive_constructs_are_reported_as_skips() {
    let doc = doc(r#","nodes":{"Decorated":{"label":"Decorated",
             "description":"A decorated node.","aliases":["Dressed"],
             "reference":"https://example.com/decorated-node",
             "labels":{"identifier":"Decorated","optional":["Fancy"],
                        "implied":["Entity"]},
             "properties":{"decorated":{"type":"STRING",
                             "description":"A decorated property.",
                             "aliases":["decorated alias"],
                             "reference":"https://example.com/decorated",
                             "pattern":"^[a-z]+$",
                             "one_of":["a","b"]}}}},
             "relationships":{"R":{"type":"R",
             "from":{"node":"Decorated","count":2},
             "to":{"node":"Decorated","min_count":1}}}"#);
    let mapping = to_graph_type_body(&doc).expect("converts");
    assert_eq!(
        mapping.skipped,
        [
            "alias on Decorated.decorated",
            "aliases on Decorated",
            "cardinality on R.from",
            "cardinality on R.to",
            "description on Decorated",
            "description on Decorated.decorated",
            "one_of on Decorated.decorated",
            "optional labels on Decorated",
            "pattern on Decorated.decorated",
            "reference on Decorated",
            "reference on Decorated.decorated",
        ]
    );
}

#[test]
fn the_conversion_is_deterministic() {
    let document = doc(
        r#","nodes":{"N":{"label":"N","properties":{"id":{"type":"STRING","key":true}}}},
        "relationships":{"R":{"type":"R","from":{"node":"N"},"to":{"node":"N"}}}"#,
    );
    assert_eq!(
        to_graph_type_body(&document).expect("converts"),
        to_graph_type_body(&document).expect("converts")
    );
}
