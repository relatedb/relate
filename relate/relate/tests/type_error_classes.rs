//! The boolean-requiring sites' error *classes* split on whether the type is
//! statically known, which is how live Neo4j splits them (probed
//! 2026-11-10 on Neo4j 2026.10.1 through the Bolt driver):
//!
//! - a statically non-boolean predicate (`WHERE 1`) and an entity variable in
//!   a predicate position (`WHERE n`) are compile-time rejections —
//!   `Neo.ClientError.Statement.SyntaxError`, GQLSTATUS `42N06`;
//! - a value whose type is only known per row (the list-quantifier's
//!   non-boolean `WHERE` predicate) stays the runtime argument-type
//!   rejection — `Neo.ClientError.Statement.TypeError`, GQLSTATUS `22G03`.

mod common;

use common::{connection, error_class_literal};

/// The planner's static predicate rejections are front-end rejections and
/// carry Neo4j's SyntaxError class. Each row carries the exact message
/// fragment its own site produces, so the statements are not
/// interchangeable — a rejection from the wrong site fails its row.
#[test]
fn static_boolean_rejections_classify_as_syntax_error() {
    let mut conn = connection();
    for (query, fragment) in [
        ("MATCH (n) WHERE 1 RETURN n", "predicate requires a boolean"),
        ("MATCH (n) WHERE n RETURN n", "not a boolean predicate"),
    ] {
        assert_eq!(
            error_class_literal(&mut conn, query),
            "SyntaxError",
            "{query}"
        );
        let error = conn.query(query).unwrap_err();
        assert!(error.to_string().contains(fragment), "{query}: {error}");
        let info = relate::error_info(&error);
        assert_eq!(
            info.neo4j_code, "Neo.ClientError.Statement.SyntaxError",
            "{query}: {error}"
        );
        assert_eq!(info.gql_status, "42N06", "{query}: {error}");
    }
}

/// A predicate whose value is only known per row keeps the runtime
/// TypeError class (the executor's `bool_or_null`).
#[test]
fn runtime_boolean_rejection_stays_type_error() {
    let mut conn = connection();
    let query = "WITH [1, 2] AS xs RETURN all(x IN [1, 2] WHERE xs) AS r";
    assert_eq!(
        error_class_literal(&mut conn, query),
        "TypeError",
        "{query}"
    );
    let error = conn.query(query).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("quantifier WHERE requires a boolean"),
        "{query}: {error}"
    );
    let info = relate::error_info(&error);
    assert_eq!(
        info.neo4j_code, "Neo.ClientError.Statement.TypeError",
        "{query}: {error}"
    );
    assert_eq!(info.gql_status, "22G03", "{query}: {error}");
}
