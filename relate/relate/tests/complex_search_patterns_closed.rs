//! The `complex-search-patterns` preview gate, CLOSED side (ADR-0371):
//! without the opt-in a SEARCH clause's MATCH statement
//! keeps the floor's pattern-shape rejections — the detailed
//! `SearchClauseTooComplexPattern` / `SearchClauseMultipleBoundVariables` /
//! `SearchClauseInvalidPredicates` errors `FulltextSearchAcceptance`
//! [11]-[13] pin.
//!
//! This file is its own test binary: `RELATE_PREVIEW` is process-global
//! and read once per `Database::open` (ADR-0371), so it must never share a
//! process with the enabled side (`complex_search_patterns_gate.rs`).

mod common;

use common::{connection, err_text};

/// The ruled-out shapes and the closed-form detail each keeps: the gate
/// widens
/// acceptance, so the closed form stays these rejections rather than the
/// authored preview error.
const RULE_0_REJECTIONS: &[(&str, &str)] = &[
    (
        "MATCH (m:Movie), () SEARCH m IN (FULLTEXT INDEX ft FOR 'magic' LIMIT 5) RETURN m",
        "SearchClauseTooComplexPattern",
    ),
    (
        "MATCH (m:Movie)-[:ACTED_IN*1..2]->() \
         SEARCH m IN (FULLTEXT INDEX ft FOR 'magic' LIMIT 5) RETURN m",
        "SearchClauseTooComplexPattern",
    ),
    (
        "MATCH (m:Movie)-[r]->() SEARCH m IN (FULLTEXT INDEX ft FOR 'magic' LIMIT 5) RETURN m",
        "SearchClauseMultipleBoundVariables",
    ),
    (
        "MATCH (m:Movie)-[]->(:Book) \
         SEARCH m IN (FULLTEXT INDEX ft FOR 'magic' LIMIT 5) RETURN m",
        "SearchClauseInvalidPredicates",
    ),
    (
        "MATCH (seed:Person {name: 'Alice'}), (p:Person) \
         SEARCH p IN (FULLTEXT INDEX ft FOR 'brave' LIMIT 5) RETURN p",
        "SearchClauseTooComplexPattern",
    ),
];

#[test]
fn every_rule_0_shape_keeps_its_rejection_ungated() {
    let mut c = connection();
    for (query, detail) in RULE_0_REJECTIONS {
        let error = err_text(c.query(query));
        assert!(
            error.contains(detail),
            "{query}: expected {detail}, got {error}"
        );
        assert!(
            !error.contains("preview feature"),
            "{query}: the closed form is the pattern-shape rejection, not the authored \
             preview error: {error}"
        );
    }
}

#[test]
fn the_plain_rule_0_shape_stays_open() {
    let mut c = connection();
    c.query("CREATE FULLTEXT INDEX ft IF NOT EXISTS FOR (n:Person) ON EACH [n.bio]")
        .unwrap();
    c.query("CREATE (:Person {bio: 'brave explorer'})").unwrap();
    let result = c
        .query(
            "MATCH (p:Person) SEARCH p IN (FULLTEXT INDEX ft FOR 'brave' LIMIT 5) \
             RETURN count(p) AS n",
        )
        .unwrap();
    assert_eq!(result.rows[0][0], relate::Value::Int(1));
}
