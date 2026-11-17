//! End-to-end pins for redundant-paren transparency of projection items
//! (LABS-1615, LABS-1616): `(n)` and `(n.p1)` project exactly like their
//! unparenthesized forms — the WITH alias rule, the ORDER BY
//! restated-column match under DISTINCT, the rendered column name, and
//! node-safety all see the variable through the parens. The lowered IR
//! already did (lowering unwraps `Expr::Paren`); these pins hold the
//! front end to the same behavior.

mod common;

use common::{connection, err_text, rows};
use relate::Value;

#[test]
fn parenthesized_with_distinct_variable_exports_the_column() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 2}), (:L1 {p1: 1})").unwrap();
    // LABS-1616: `WITH DISTINCT (n)` needs no alias and projects the name
    // `n` — `RETURN n` resolves through it, under the column name `n`.
    let result = c.query("MATCH (n:L1) WITH DISTINCT(n) RETURN n").unwrap();
    assert_eq!(result.header.columns, ["n"]);
    assert_eq!(result.rows.len(), 2);
    // The unparenthesized spelling behaves identically.
    let result = c.query("MATCH (n:L1) WITH DISTINCT n RETURN n").unwrap();
    assert_eq!(result.header.columns, ["n"]);
    assert_eq!(result.rows.len(), 2);
    // The alias error's boundary is unchanged: parens around a
    // non-variable still require `AS`.
    let error = c.query("MATCH (n:L1) WITH DISTINCT(n.p1) RETURN n.p1");
    assert!(
        err_text(error).contains("must be aliased with AS"),
        "a parenthesized non-variable still requires AS"
    );
}

#[test]
fn parenthesized_distinct_projection_orders_by_the_restated_expression() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 2}), (:L1 {p1: 2}), (:L1 {p1: 1})")
        .unwrap();
    // LABS-1615: ORDER BY restates the projected `(n.p1)` as `n.p1`,
    // ordered ascending, distinct.
    // The column name is the verbatim spelling — Neo4j (probed live
    // 2026.10) names the parenthesized access `(n.p1)` — while ORDER BY
    // still restates the projected item semantically (P12).
    let result = c
        .query("MATCH (n:L1) RETURN DISTINCT(n.p1) ORDER BY n.p1")
        .unwrap();
    assert_eq!(result.header.columns, ["(n.p1)"]);
    let got: Vec<i64> = result
        .rows
        .iter()
        .map(|r| match &r[0] {
            Value::Int(i) => *i,
            v => panic!("expected ints, got {v:?}"),
        })
        .collect();
    assert_eq!(got, [1, 2]);
    // The unparenthesized spelling behaves identically.
    assert_eq!(
        rows(&mut c, "MATCH (n:L1) RETURN DISTINCT n.p1 ORDER BY n.p1"),
        vec![vec![Value::Int(1)], vec![Value::Int(2)]]
    );
}

#[test]
fn parenthesized_with_variable_carries_node_identity() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 1})").unwrap();
    // `WITH (n)` carries node-ness like `WITH n`: the later CREATE reuses
    // the bound node instead of binding a new one.
    c.query("MATCH (n:L1) WITH (n) CREATE (n)-[:R]->(:L2)")
        .unwrap();
    assert_eq!(
        rows(&mut c, "MATCH (:L1 {p1: 1})-[:R]->(:L2) RETURN count(*)"),
        vec![vec![Value::Int(1)]]
    );
}

#[test]
fn parenthesized_group_by_alias_key_groups_like_the_bare_spelling() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 1}), (:L1 {p1: 1}), (:L1 {p1: 2})")
        .unwrap();
    // LABS-1615: `GROUP BY (a)` resolves through the alias and groups
    // exactly like `GROUP BY a` — the validation battery and the
    // lowering read the same key (pinned end to end, not only through
    // validation).

    for q in [
        "MATCH (n:L1) RETURN n.p1 AS a, count(*) AS c GROUP BY a ORDER BY a",
        "MATCH (n:L1) RETURN n.p1 AS a, count(*) AS c GROUP BY (a) ORDER BY a",
    ] {
        assert_eq!(
            rows(&mut c, q),
            vec![
                vec![Value::Int(1), Value::Int(2)],
                vec![Value::Int(2), Value::Int(1)]
            ],
            "{q}"
        );
    }
}
