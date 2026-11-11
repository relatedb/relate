//! The W5 semantic-error corpus rules the planner owns.

use relate_front::parse_and_lower;
use relate_ir::Clause;

use super::*;
use crate::binding::Binding;

fn accepted(src: &str) {
    let q = parse_and_lower(src).unwrap_or_else(|e| panic!("{src}: {e}"));
    plan(q, &mut |_| false).unwrap_or_else(|e| panic!("{src}: {e}"));
}

/// The static binding a projected expression carries into the next stage,
/// from its lowered IR.
fn static_binding(expr_src: &str) -> Option<Binding> {
    let q = parse_and_lower(&format!("RETURN {expr_src} AS n")).unwrap();
    match q.parts[0].clauses.last() {
        Some(Clause::Return { items, .. }) => Binding::of_static_expr(&items[0].expr),
        other => panic!("expected a RETURN, got {other:?}"),
    }
}

/// A projected subquery expression carries its static result type into the
/// enclosing scope (`EXISTS` → BOOLEAN, `COUNT` → INTEGER, `COLLECT` →
/// LIST), so a *following-clause* pattern that reuses the name as a node is
/// the authored `VariableTypeConflict` — live Neo4j 2026.10.1 raises it at
/// compile time. (A pattern inside an expression-subquery body is not this
/// rule: those non-entity imports are shadowable by design, and live reports
/// a value-level fault there instead.)
#[test]
fn a_projected_subquery_expression_keeps_its_static_type() {
    // The classification itself, per case: a wrong mapping (e.g. `COLLECT`
    // read as a scalar) must fail here, not merely produce some conflict.
    for (expr, expected) in [
        ("exists{ RETURN true }", Binding::Scalar),
        ("count{ RETURN 1 }", Binding::Scalar),
        ("collect{ RETURN 1 }", Binding::List),
    ] {
        assert_eq!(static_binding(expr), Some(expected), "{expr}");
    }
    // Each reused as a following-clause pattern node is the authored
    // VariableTypeConflict.
    for src in [
        "WITH exists{ RETURN true } AS n MATCH (n) RETURN n",
        "WITH count{ RETURN 1 } AS n MATCH (n) RETURN n",
        "WITH collect{ RETURN 1 } AS n MATCH (n) RETURN n",
    ] {
        let error = rejection(src);
        assert!(error.contains("VariableTypeConflict"), "{src}: {error}");
    }
    // The same result used as a plain value stays legal.
    accepted("WITH count{ RETURN 1 } AS n RETURN n");
}

#[test]
fn importing_with_may_only_name_outside_variables() {
    for src in [
        "WITH 1 AS n CALL { WITH n AS x RETURN x } RETURN x",
        "WITH 1 AS n CALL { WITH n, 2 AS m RETURN n AS x } RETURN x",
        "WITH 1 AS n CALL { WITH n+1 AS x RETURN x } RETURN x",
        "WITH 1 AS n CALL { WITH collect(n) AS ns RETURN ns } RETURN ns",
        "WITH 1 AS n CALL { WITH *, 1 AS x RETURN x } RETURN x",
        "WITH 1 AS n CALL { WITH n ORDER BY n.prop RETURN x } RETURN x",
        "WITH 1 AS n CALL { WITH DISTINCT n RETURN n+1 AS x } RETURN x",
        "WITH 1 AS n CALL { WITH n LIMIT 1 RETURN n+1 AS x } RETURN x",
        "WITH 1 AS n CALL { WITH n AS m RETURN m+1 AS x UNION WITH n RETURN n-1 AS x } RETURN x",
    ] {
        assert!(
            rejection(src).contains("InvalidImportingWith"),
            "{src}: {}",
            rejection(src)
        );
    }
}

#[test]
fn non_importing_with_and_scope_clauses_stay_legal() {
    for src in [
        "WITH 1 AS n CALL { WITH 42 AS fourtytwo RETURN 1 AS x } RETURN x",
        "WITH 1 AS n CALL { WITH EXISTS { () } AS a RETURN 1 AS x } RETURN x",
        "WITH 1 AS n CALL { WITH rand() AS r RETURN 1 AS x } RETURN x",
        "WITH 1 AS n CALL (n) { WITH n AS x RETURN x } RETURN x",
        "WITH 1 AS n CALL { WITH n RETURN n+1 AS x } RETURN x",
        // A comprehension's bound identifier is not a reference to the outer
        // name of the same spelling, so this WITH is not importing.
        "WITH 1 AS x CALL { WITH [x IN [1,2] | x] AS y RETURN y } RETURN y",
        "WITH 1 AS x CALL { WITH reduce(a = 0, x IN [1] | a + x) AS y RETURN y } RETURN y",
    ] {
        accepted(src);
    }
}

#[test]
fn removed_exists_property_forms_are_rejected() {
    for src in [
        "MATCH (n) WHERE exists(n.prop) RETURN n",
        "MATCH (n) WHERE NOT exists(n.prop) RETURN n",
        "MATCH (n) RETURN exists(n['prop'])",
        "WITH {key: 'blah'} AS map RETURN exists(map.key)",
    ] {
        let err = rejection(src);
        assert!(err.contains("InvalidPropertyExistence"), "{src}: {err}");
    }
    // `exists(<pattern>)` stays the legacy pattern-existence form.
    accepted("MATCH (n) WHERE exists((n)-->()) RETURN n");
}

#[test]
fn literal_point_maps_need_a_coordinate_family() {
    let err = rejection("RETURN point({xxx: 2.3, yyy: 4.5})");
    assert!(err.contains("InvalidPoint"), "{err}");
    accepted("RETURN point({x: 1, y: 2})");
    accepted("RETURN point({latitude: 1, longitude: 2})");
    accepted("RETURN point($m)");
}

#[test]
fn graph_selector_arity_is_checked_before_the_composite_refusal() {
    let err = rejection("USE graph.byElementId() RETURN 1");
    assert!(err.contains("InvalidNumberOfArguments"), "{err}");
    let err = rejection("RETURN graph.byElementId() AS g");
    assert!(err.contains("InvalidNumberOfArguments"), "{err}");
}

#[test]
fn bound_relationships_are_rejected_inside_shortest_path() {
    for src in [
        "WITH [] AS r LIMIT 1 MATCH p = shortestPath((src)-[r*]->(dst)) RETURN src, dst",
        "WITH [] AS r LIMIT 1 MATCH p = allShortestPaths((src)-[r*]->(dst)) RETURN src, dst",
    ] {
        let err = rejection(src);
        assert!(err.contains("VariableAlreadyBound"), "{src}: {err}");
    }
    accepted("MATCH p = shortestPath((src)-[r*]->(dst)) RETURN src, dst");
}

#[test]
fn subquery_pattern_may_shadow_a_non_entity_import() {
    // The source's "subquery may open with a WITH of an EXISTS
    // containing a pattern and a WHERE": the pattern's `n` shadows the
    // imported scalar at compile time (Neo4j raises only when the
    // value is actually evaluated).
    accepted(
        "WITH 1 AS n CALL { WITH EXISTS { (n) WHERE n.prop = 42 } AS n RETURN 1 AS x } RETURN x",
    );
    // An entity import keeps its binding: reusing an outer node as a
    // relationship is still a type conflict.
    assert!(rejection("MATCH (n) RETURN EXISTS { (n)-[n]->() }").contains("VariableTypeConflict"));
}

/// A `WITH`-bearing subquery-expression body may not shadow an outer name
/// through `UNWIND`/`LOAD CSV`: the shadowing test reads the ORIGINAL body,
/// not the persistent-imports rewrite (which injects a bare `Variable` item
/// into every `WITH` and would make any two-outer-name body count as
/// correlated, silently disabling the rejection).
#[test]
fn a_with_bearing_expression_body_still_rejects_unwind_shadowing() {
    let src = "MATCH (n) MATCH (x) RETURN EXISTS { MATCH (a) WITH a UNWIND [1] AS n RETURN n }";
    let err = rejection(src);
    assert!(err.contains("VariableAlreadyBound"), "{src}: {err}");
}

/// A `USE` after a transactions-exempt CALL is still misplaced: the
/// placement rule asks the clause's *position*, not the running write flag
/// (which the `CALL … IN TRANSACTIONS` exemption suppresses).
#[test]
fn a_use_after_a_transactions_call_is_still_misplaced() {
    let err = crate::rejection("MATCH (n) CALL { CREATE (:X) } IN TRANSACTIONS USE neo4j RETURN n");
    assert!(err.contains("InvalidPlacementOfUseClause"), "{err}");
}
