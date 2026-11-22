//! The CIP-275 graph-type display-variant suites' shared fixtures.

use super::{connection, scalar_one, set_preview_once};

/// The CIP-275 graph-type display-variant suites' shared gated connection:
/// the graph-type preview features those suites declare their fixtures
/// with, set before any store opens.
pub fn show_variants_connection() -> relate::Connection {
    set_preview_once(
        "graph-type-opt-props,graph-type-default-values,graph-type-implied-labels,\
         graph-type-show-variants",
    );
    connection()
}

/// The printed shape of an engine graph-type specification: no backticks,
/// no generated `constraint_<hash>` names, no line breaks.
pub fn printed_spec(spec: &str) -> String {
    let flat = spec.replace(['`', '\n'], "");
    let mut out = String::new();
    let mut rest = flat.as_str();
    while let Some(at) = rest.find("CONSTRAINT constraint_") {
        out.push_str(&rest[..at]);
        out.push_str("CONSTRAINT ");
        let name = &rest[at + "CONSTRAINT constraint_".len()..];
        let end = name.find(' ').expect("a generated name ends in a space");
        rest = &name[end + 1..];
    }
    out.push_str(rest);
    out
}

/// `SHOW CURRENT GRAPH TYPE <variant>`'s specification, [`printed_spec`]-shaped.
pub fn show_spec(c: &mut relate::Connection, variant: &str) -> String {
    let relate::Value::String(spec) = scalar_one(c, &format!("SHOW CURRENT GRAPH TYPE {variant}"))
    else {
        panic!("expected a specification string");
    };
    printed_spec(&spec)
}
