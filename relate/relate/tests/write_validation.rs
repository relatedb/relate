//! Write-path runtime validation: the data exceptions a CREATE/MERGE raises
//! when the row it materializes against is wrong — as opposed to the
//! compile-time pattern-shape rules in `match_shape_validation.rs`.

mod common;

use common::{connection, err_text, error_class_literal, run, scalar_one};
use relate::Value;

fn assert_rejected(conn: &mut relate::Connection, query: &str, needle: &str) {
    let rendered = err_text(conn.query(query));
    assert!(
        rendered.contains(needle),
        "{query}\n  expected {needle:?}, got: {rendered}"
    );
}

// ---- 22N01: a CREATE/MERGE endpoint the row binds to null ----

/// Neo4j 2026.10.1 under Cypher 25 raises 22G03 under the ArgumentError
/// class ("Failed to create relationship …, node … is missing") for a
/// relationship pattern whose node position is already bound to null — in
/// CREATE and in MERGE alike. Neither spelling may silently create a fresh
/// node (and, for MERGE, a relationship) instead. Probed 2026-11-10.
#[test]
fn null_bound_create_and_merge_endpoints_is_an_argument_error() {
    let mut conn = connection();
    run(&mut conn, "CREATE ({uid: 'a'})");
    run(&mut conn, "CREATE ({uid: 'b'})");
    for query in [
        // The harness's start-point / end-point scenarios.
        "MATCH (a {uid: 'a'}), (b {uid: 'b'}) \
         OPTIONAL MATCH (b)-[:LINK_TO]->(c) \
         CREATE (b)-[:LINK_TO]->(a) \
         CREATE (c)-[r:MISSING_C]->(a)",
        "MATCH (a {uid: 'a'}), (b {uid: 'b'}) \
         OPTIONAL MATCH (b)-[:LINK_TO]->(c) \
         CREATE (b)-[:LINK_TO]->(a) \
         CREATE (a)-[r:MISSING_C]->(c)",
        "OPTIONAL MATCH (m:Missing) WITH m MERGE (m)-[:R]->(:Node)",
        "OPTIONAL MATCH (m:Missing) WITH m MERGE (:Node)-[:R]->(m)",
    ] {
        assert_rejected(
            &mut conn,
            query,
            "Expected the value NULL to be of type NODE",
        );
        assert_eq!(
            error_class_literal(&mut conn, query),
            "ArgumentError",
            "{query}"
        );
    }
    // Nothing was created by the failed statements.
    assert_eq!(
        scalar_one(&mut conn, "MATCH (n) RETURN count(*)"),
        Value::Int(2)
    );
}
