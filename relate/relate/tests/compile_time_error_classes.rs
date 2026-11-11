//! Compile-time static rejections carry Neo4j's SyntaxError class.
//!
//! Live Neo4j (2026.10.1, probed 2026-11-10) rejects a statement whose
//! literal argument types — or literal values — are statically invalid during
//! semantic analysis and reports `Neo.ClientError.Statement.SyntaxError`. A
//! value whose type is only known per row keeps the runtime TypeError class.
//! These rows pin one statement per family the front end decides.

mod common;

use common::{connection, error_class_literal};

const SYNTAX: &str = "SyntaxError";

#[test]
fn statically_invalid_literal_arguments_are_syntax_errors() {
    let mut conn = connection();
    for (query, family) in [
        (
            "RETURN vector.similarity.cosine([true, false], [1, 0]) AS r",
            "vector similarity",
        ),
        ("RETURN vector([1, 2, 3], 3, STRING) AS r", "vector type"),
        (
            "RETURN vector([1, 2, 3], 0, INTEGER) AS r",
            "vector dimension",
        ),
        (
            "RETURN vector(\"1, 2, 3\", 3, INTEGER) AS r",
            "vector string form",
        ),
        ("RETURN vector([1, 2, 3]) AS r", "vector arity"),
        ("RETURN char_length([1, 2, 3])", "char_length"),
        ("RETURN NULLIF(1) AS x", "nullif arity"),
        (
            "RETURN format(date('2024-06-27'), 'yyyy', 'extra') AS x",
            "format arity",
        ),
        (
            "RETURN replace('woof', 'woof', 'meow', -1) AS r",
            "replace limit",
        ),
        ("RETURN date('2020-01-01', 42) AS x", "temporal pattern"),
        ("RETURN date('2022-1') AS x", "temporal parse"),
        (
            "RETURN any(x IN 5 WHERE x > 0) AS result",
            "quantifier list",
        ),
        (
            "RETURN allReduce(acc = 0, x IN 5 | acc + x, acc < 5) AS result",
            "allReduce list",
        ),
        (
            "RETURN allReduce(acc = 0, x IN [1, 2, 3] | acc + x, acc) AS result",
            "allReduce predicate",
        ),
        (
            "MATCH (p) LIMIT -1 RETURN p.name",
            "standalone page statement",
        ),
        (
            "CALL { CREATE (n) } IN null CONCURRENT TRANSACTIONS",
            "in-transactions concurrency",
        ),
        (
            "RETURN [(a)-->(b) WHERE 123 | a] AS result",
            "static predicate",
        ),
    ] {
        assert_eq!(
            error_class_literal(&mut conn, query),
            SYNTAX,
            "{family}: `{query}`"
        );
    }
}

#[test]
fn static_statement_shape_rejections_are_syntax_errors() {
    let mut conn = connection();
    for (query, family) in [
        ("", "empty query"),
        ("\n", "newline-only query"),
        (
            "USE neo4j CALL { USE foo RETURN 1 AS x } RETURN x",
            "multiple graphs",
        ),
        (
            "MATCH p = ANY 0 (a)-[r]->(b) RETURN p",
            "path selector count",
        ),
    ] {
        assert_eq!(
            error_class_literal(&mut conn, query),
            SYNTAX,
            "{family}: `{query}`"
        );
    }
}

/// A literal temporal construction failure takes the *construction error's*
/// class: a text that cannot be parsed is the compile-time SyntaxError, while
/// an out-of-range zone offset is live Neo4j's ArgumentError.
#[test]
fn literal_temporal_failures_keep_their_class() {
    let mut conn = connection();
    for (query, class) in [
        ("RETURN date('2022-1') AS x", SYNTAX),
        (
            "RETURN datetime('2018-04-01T12:45:45+45') AS x",
            "ArgumentError",
        ),
        (
            "WITH time('12:34:56[Europe/Stockholm]') AS t RETURN t",
            "ArgumentError",
        ),
        ("RETURN duration('P1.5Y1M') AS x", "ArgumentError"),
    ] {
        assert_eq!(error_class_literal(&mut conn, query), class, "`{query}`");
    }
}

/// The runtime backstop keeps the TypeError class for a value whose type is
/// only known per row — the split live Neo4j makes.
#[test]
fn row_time_type_faults_stay_type_errors() {
    let mut conn = connection();
    conn.query("CREATE (:T {p: [1]})").unwrap();
    assert_eq!(
        error_class_literal(&mut conn, "MATCH (n:T) RETURN char_length(n.p) AS x"),
        "TypeError"
    );
    assert_eq!(
        error_class_literal(
            &mut conn,
            "UNWIND [1] AS x RETURN allreduce(acc = 0, y IN x | acc + y, acc < 5) AS r"
        ),
        "TypeError"
    );
}
