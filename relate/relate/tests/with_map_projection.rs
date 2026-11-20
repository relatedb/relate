//! REL8-1866: an unaliased map projection over a variable is named
//! after its base variable — `WITH m {.*}` projects the column `m`, and
//! the projected map *shadows* the node binding, so a later `RETURN m`
//! reads the map (Neo4j 2026.10.1 live-confirmed on all four shapes
//! pinned here). The naming itself flows from the one homes
//! (`effective_name` in the front, `Projection::output_name` in the
//! IR); what the fix changed is the alias gates' implicit-name
//! vocabulary admitting the shape (a bare variable or a map projection
//! over one) in both the front (`item_needs_alias`) and the planner
//! (`relate_ir::canonical_name`).

mod common;

use common::{connection, err_text, map};
use relate::Value;

/// The issue's exact query plans, and executing it returns one row
/// whose single column is named `m` and carries the projected map —
/// the shadowing is the semantics: the following `RETURN m` reads the
/// map, not the node.
#[test]
fn with_map_projection_is_named_after_its_variable_and_shadows_it() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 'a'})").unwrap();
    c.query("EXPLAIN MATCH (m:L1) WITH m{.*} RETURN m")
        .expect("the issue's exact query plans");
    let result = c.query("MATCH (m:L1) WITH m{.*} RETURN m").unwrap();
    assert_eq!(result.header.columns, ["m"]);
    assert_eq!(
        result.rows,
        vec![vec![map(&[("p1", Value::String("a".into()))])]]
    );
}

/// RETURN's implicit name for a map projection is the same base
/// variable (not the source text) — the control that the two clauses
/// share one naming rule, with the aliased WITH form unchanged beside
/// it.
#[test]
fn return_map_projection_keeps_the_variable_name_and_alias_still_wins() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 'a'})").unwrap();
    let result = c.query("MATCH (m:L1) RETURN m{.*}").unwrap();
    assert_eq!(result.header.columns, ["m"]);
    assert_eq!(
        result.rows,
        vec![vec![map(&[("p1", Value::String("a".into()))])]]
    );
    let result = c.query("MATCH (m:L1) WITH m{.*} AS x RETURN x").unwrap();
    assert_eq!(result.header.columns, ["x"]);
    assert_eq!(
        result.rows,
        vec![vec![map(&[("p1", Value::String("a".into()))])]]
    );
}

/// The projection contributes its base variable's name to the
/// duplicate rule: `m {.*}, m` collides (Neo4j 42N38's "duplicate
/// return item name"), in WITH and in RETURN alike.
#[test]
fn with_and_return_map_projection_duplicate_the_variable_name() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 'a'})").unwrap();
    for query in [
        "MATCH (m:L1) WITH m{.*}, m RETURN m",
        "MATCH (m:L1) RETURN m{.*}, m",
    ] {
        let error = err_text(c.query(query));
        assert!(
            error.contains("ColumnNameConflict"),
            "{query}: the projection must contribute the name 'm': {error}"
        );
    }
}

/// The alias requirement's boundary is unchanged: an embedded
/// projection is an expression item like any other and still requires
/// `AS`, as does the property access beside it.
#[test]
fn with_map_projection_embedded_in_an_expression_still_requires_as() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 'a'})").unwrap();
    for query in [
        "MATCH (m:L1) WITH m{.*} + 1 RETURN m",
        "MATCH (m:L1) WITH size(m{.*}) RETURN m",
        "MATCH (m:L1) WITH m.p1 RETURN m",
    ] {
        let error = err_text(c.query(query));
        assert!(
            error.contains("must be aliased with AS"),
            "{query}: only the lone projection over a variable carries its name: {error}"
        );
    }
}
