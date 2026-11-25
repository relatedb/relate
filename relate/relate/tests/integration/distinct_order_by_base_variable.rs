//! REL8-1868: a DISTINCT projection's ORDER BY resolves the base variable
//! of a bare-variable item — `RETURN DISTINCT m AS node ORDER BY m.p1`
//! reads the property off the pre-projection `m`, and the DISTINCT pass
//! then collapses the projected value (verified live Neo4j 2026.10.1).
//! Every row a distinct group collapses shares the projected value — and
//! with it the base variable's value — so the key is well-defined per
//! group and sort-then-dedup-keep-first is the engine's whole model.
//!
//! The accessible set under DISTINCT is exactly the output names, the
//! projected items' own expressions, and those base variables: a variable
//! the projection never mentions (`b`), a key mixing one in, and a
//! variable the projection only derives from (`m{.p1} AS node`) all stay
//! inaccessible; without DISTINCT every input name stays readable.

use crate::common;

use common::{connection, err_text};
use relate::{Connection, QueryResult, Value};

/// The three-path fixture: two paths share the target `y`, the third
/// ends at `x` — three rows in, two distinct projected values out, so
/// both the dedup and the surviving order are observable in `p1`.
fn fixture() -> Connection {
    let mut conn = connection();
    conn.query(
        "CREATE (:L1 {p1:'a'})-[:R]->(y:L2 {p1:'y'}),
               (:L1 {p1:'b'})-[:R]->(y),
               (:L1 {p1:'c'})-[:R]->(:L2 {p1:'x'})",
    )
    .unwrap();
    conn
}

/// Column `column`'s `p1` values in row order — a node column reads its
/// property, a map projection its entry, a string column is the value
/// itself. [`p1s`] is the first-column spelling every other test reads.
fn p1_column(result: &QueryResult, column: usize) -> Vec<String> {
    let text = |value: &Value| match value {
        Value::String(s) => s.to_string(),
        other => panic!("expected a string p1, got {other:?}"),
    };
    result
        .rows
        .iter()
        .map(|row| match &row[column] {
            Value::Node(node) => text(
                node.properties
                    .get("p1")
                    .unwrap_or_else(|| panic!("node without p1: {:?}", row[column])),
            ),
            Value::Map(m) => text(&m["p1"]),
            value @ Value::String(_) => text(value),
            other => panic!("expected a node, map or string column, got {other:?}"),
        })
        .collect()
}

/// [`p1_column`] over the result's first column.
fn p1s(result: &QueryResult) -> Vec<String> {
    p1_column(result, 0)
}

/// The issue's statement: the alias hides `m` from the output names, but
/// the item projects `m` itself, so ORDER BY reads through it —
/// descending and ascending, the EXPLAIN form, and the unaliased sibling
/// whose output name is the variable.
#[test]
fn distinct_alias_orders_by_the_base_variable_property() {
    let mut conn = fixture();
    let result = conn
        .query("MATCH (b:L1)-[:R]->(m:L2) RETURN DISTINCT m AS node ORDER BY m.p1 DESC LIMIT 10")
        .unwrap();
    assert_eq!(p1s(&result), ["y", "x"]);
    let result = conn
        .query("MATCH (b:L1)-[:R]->(m:L2) RETURN DISTINCT m AS node ORDER BY m.p1 ASC")
        .unwrap();
    assert_eq!(p1s(&result), ["x", "y"]);
    conn.query(
        "EXPLAIN MATCH (b:L1)-[:R]->(m:L2) \
         RETURN DISTINCT m AS node ORDER BY m.p1 DESC LIMIT 10",
    )
    .unwrap();
    let result = conn
        .query("MATCH (b:L1)-[:R]->(m:L2) RETURN DISTINCT m ORDER BY m.p1 DESC")
        .unwrap();
    assert_eq!(p1s(&result), ["y", "x"]);
}

/// The base variable's access is granted wholesale, not per property: a
/// property the store never interned sorts on its per-row null key.
#[test]
fn distinct_alias_orders_by_an_absent_property_of_the_base_variable() {
    let mut conn = fixture();
    let result = conn
        .query("MATCH (b:L1)-[:R]->(m:L2) RETURN DISTINCT m AS node ORDER BY m.p2 DESC")
        .unwrap();
    // Every key is null, so membership is the pin, not the order.
    let mut got = p1s(&result);
    got.sort_unstable();
    assert_eq!(got, ["x", "y"]);
}

/// The sibling paths beside the base variable: the output name, and the
/// restated projected item — both order the same two rows.
#[test]
fn distinct_alias_orders_by_the_output_name_and_the_restated_item() {
    let mut conn = fixture();
    let result = conn
        .query("MATCH (b:L1)-[:R]->(m:L2) RETURN DISTINCT m AS node ORDER BY node.p1 DESC")
        .unwrap();
    assert_eq!(p1s(&result), ["y", "x"]);
    let result = conn
        .query("MATCH (b:L1)-[:R]->(m:L2) RETURN DISTINCT m.p1 AS p ORDER BY m.p1 DESC")
        .unwrap();
    assert_eq!(p1s(&result), ["y", "x"]);
}

/// The WITH twin of the issue's shape — the scope-carrying kind carries
/// the same policy, and the RETURN fix keeps the two level.
#[test]
fn with_distinct_alias_orders_by_the_base_variable_property() {
    let mut conn = fixture();
    let result = conn
        .query("MATCH (b:L1)-[:R]->(m:L2) WITH DISTINCT m AS node ORDER BY m.p1 DESC RETURN node")
        .unwrap();
    assert_eq!(p1s(&result), ["y", "x"]);
}

/// The inaccessibility DISTINCT keeps: a variable the projection never
/// mentions, a key that mixes one in, and a variable the projection only
/// derives from all stay undefined (Relate's UndefinedVariable family;
/// Neo4j refuses the same shapes with 42N44).
#[test]
fn distinct_alias_keeps_other_and_derived_variables_inaccessible() {
    let mut conn = fixture();
    for src in [
        "MATCH (b:L1)-[:R]->(m:L2) RETURN DISTINCT m AS node ORDER BY b.p1 DESC",
        "MATCH (b:L1)-[:R]->(m:L2) RETURN DISTINCT m AS node ORDER BY m.p1 + b.p1 DESC",
        "MATCH (b:L1)-[:R]->(m:L2) RETURN DISTINCT m{.p1} AS node ORDER BY m.p1 DESC",
    ] {
        let err = err_text(conn.query(src));
        assert!(
            err.contains("UndefinedVariable"),
            "{src}: expected UndefinedVariable, got {err}"
        );
    }
}

/// The alias-collision shape (REL8-1868): an output name that
/// repeats a base variable's name is the projected column — alias scope
/// wins over the base carry, exactly as the executor writes the items
/// over the kept bindings — so the ORDER BY key `m.p1` reads the
/// column's value (x's), never the incoming `m` the item also names.
#[test]
fn alias_output_repeating_a_base_name_orders_by_the_column() {
    let mut conn = connection();
    conn.query("CREATE (:L2 {p1:'m0'}), (:L2 {p1:'x1'}), (:L2 {p1:'x2'})")
        .unwrap();
    let result = conn
        .query(
            "MATCH (m:L2 {p1:'m0'}), (x:L2) WHERE x.p1 IN ['x1','x2'] \
             RETURN DISTINCT m AS node, x AS m ORDER BY m.p1 DESC",
        )
        .unwrap();
    // Reading the column orders by x's p1 (x2 first); reading the
    // incoming m ('m0' on both rows) would tie and keep input order.
    assert_eq!(p1_column(&result, 1), ["x2", "x1"]);
}

/// Without DISTINCT the projection narrows nothing: the derived-from
/// shape DISTINCT refuses executes and keeps all three rows.
#[test]
fn without_distinct_the_derived_variable_stays_readable() {
    let mut conn = fixture();
    let result = conn
        .query("MATCH (b:L1)-[:R]->(m:L2) RETURN m{.p1} AS node ORDER BY m.p1 DESC")
        .unwrap();
    assert_eq!(p1s(&result), ["y", "y", "x"]);
}
