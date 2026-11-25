//! A whole-map parameter (`$props`) inside a pattern an *expression*
//! carries — a WHERE pattern predicate (bare or `exists()`), a pattern
//! comprehension, a `shortestPath()` argument — is not the CREATE-only
//! shorthand: live Neo4j 2026.10 rejects every spelling with "Parameter
//! maps cannot be used in `MATCH` patterns (use a literal map instead,
//! e.g. `{id: $props.id}`)", and the engine rejected only the MATCH and
//! MERGE clause patterns before, letting the expression-borne ones plan
//! and execute. They now reject the same way; a literal map with a
//! parameter *value* (`{t: $v}`) and the CREATE shorthand keep working.

use crate::common;

use common::{connection, err_text, first_column, map, params, string};
use relate::Value;

fn edge_store() -> relate::Connection {
    let mut c = connection();
    c.query("CREATE (:L1 {t: 'hello'})-[:R]->(:L2 {t: 'world'})")
        .unwrap();
    c
}

/// The gap's exact shape through the public API: the whole-map
/// parameter inside a WHERE pattern predicate rejects with the
/// parameter-use error, exactly as the MATCH clause spelling always has.
/// The exhaustive spelling corpus (the `exists()` argument, a pattern
/// comprehension, a `shortestPath()` argument, the element-pattern WHERE
/// nesting, the boolean-structured positions) is pinned once, where the
/// rule is decided, in `relate-front`'s
/// `param_property_shorthand_inside_expression_patterns_is_rejected`.
#[test]
fn a_whole_map_parameter_in_an_expression_pattern_is_rejected() {
    let mut c = connection();
    for q in [
        "MATCH (a:L1), (b) WHERE (a:L1 $props)-[:R]->(b) RETURN a",
        "MATCH (a:L1 $props) RETURN a",
    ] {
        let error = err_text(c.query(q));
        assert!(
            error.contains("InvalidParameterUse"),
            "{q}: wrong error: {error}"
        );
    }
}

/// The legal neighbors keep executing: a literal map with a parameter
/// value filters, the bare pattern predicate answers, and CREATE keeps
/// the whole-map shorthand.
#[test]
fn the_legal_spellings_around_it_keep_executing() {
    let mut c = edge_store();
    let values = params(&[("v", Value::String("hello".into()))]);
    // A literal map with a parameter value is not the whole-map
    // shorthand: the predicate filters by it.
    assert_eq!(
        first_column(
            &c.query_with_params(
                "MATCH (a:L1 {t: $v}), (b) WHERE (a)-[:R]->(b) RETURN a.t",
                values.clone()
            )
            .unwrap()
        ),
        vec![string("hello")]
    );
    // The same literal map inside the pattern predicate itself.
    assert_eq!(
        first_column(
            &c.query_with_params(
                "MATCH (a:L1), (b) WHERE (a:L1 {t: $v})-[:R]->(b) RETURN a.t",
                values.clone()
            )
            .unwrap()
        ),
        vec![string("hello")]
    );
    // The bare predicate without any map answers through the edge.
    assert_eq!(
        first_column(
            &c.query("MATCH (a:L1), (b:L2) WHERE (a)-[:R]->(b) RETURN b.t")
                .unwrap()
        ),
        vec![string("world")]
    );
    // CREATE keeps the shorthand: the node is created with the map's
    // properties.
    let props = params(&[("props", map(&[("p1", Value::Int(1))]))]);
    c.query_with_params("CREATE (n:L1 $props)", props).unwrap();
    assert_eq!(
        first_column(&c.query("MATCH (n:L1 {p1: 1}) RETURN n.p1").unwrap()),
        vec![Value::Int(1)]
    );
}
