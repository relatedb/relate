//! The schema-rule coexistence ADR-0372 narrowed to serving pairs, pinned
//! against live Neo4j 2026.10.1 (2026-11-22, recorded as ADR-0372's Later
//! references note): a FULLTEXT index coexists with existence, type and
//! uniqueness constraints and a graph type's `STRING NOT NULL` property —
//! every pair in both creation orders; the one conflicting pair — a RANGE
//! index and an index-backed constraint on the same
//! entity/label/properties — rejects in both orders on both engines, each
//! order under its own error class. The pins document the parity; the
//! behavior itself landed with the serving-pair narrowing.

use crate::common;

use common::{assert_error_class, connection, run};

/// The one FULLTEXT statement every coexistence pair shares.
const FULLTEXT: &str = "CREATE FULLTEXT INDEX ft FOR (n:Pt) ON EACH [n.title]";

/// The coexistence pairs, each pinned in both creation orders: the second
/// statement of either order would fail on the stale 22N70/22N65
/// conflict.
#[test]
fn fulltext_coexists_with_non_serving_rules_in_both_orders() {
    let others: &[&str] = &[
        "CREATE CONSTRAINT c FOR (n:Pt) REQUIRE n.title IS NOT NULL",
        "CREATE CONSTRAINT c FOR (n:Pt) REQUIRE n.title IS TYPED STRING",
        "CREATE CONSTRAINT c FOR (n:Pt) REQUIRE n.title IS UNIQUE",
        "ALTER CURRENT GRAPH TYPE SET { (:Pt => {title :: STRING NOT NULL}) }",
    ];
    for other in others {
        for (first, second) in [(FULLTEXT, *other), (*other, FULLTEXT)] {
            let mut c = connection();
            run(&mut c, first);
            run(&mut c, second);
        }
    }
}

/// The serving pair keeps its single conflict rule in both creation
/// orders — on both engines, each order under its own binding class.
#[test]
fn a_range_index_and_an_index_backed_constraint_conflict_in_both_orders() {
    const RANGE: &str = "CREATE RANGE INDEX ri FOR (n:Pt) ON (n.title)";
    const UNIQUE: &str = "CREATE CONSTRAINT c FOR (n:Pt) REQUIRE n.title IS UNIQUE";
    // Both orders refuse with the engine's binding class; the rendered
    // kinds differ — `AlreadyExists` index-first, the equivalent-rule
    // `EquivalentSchemaRuleAlreadyExists` constraint-first — so the kind
    // token rides beside the class assertion.
    for (a, b, expected_kind) in [
        (RANGE, UNIQUE, "AlreadyExists"),
        (UNIQUE, RANGE, "EquivalentSchemaRuleAlreadyExists"),
    ] {
        let mut c = connection();
        run(&mut c, a);
        let Err(error) = c.query(b) else {
            panic!("{b}: expected the serving-pair conflict");
        };
        assert_error_class(&error, "SemanticError", b);
        assert!(
            error.to_string().contains(expected_kind),
            "{b}: wrong conflict kind: {error}"
        );
    }
}
