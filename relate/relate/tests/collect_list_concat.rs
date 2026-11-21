//! LABS-199 / ADR-0198: `WITH *` grouping keys and aggregates in CASE arms.
//!
//! Two dialect rulings ground-truthed against live Neo4j CE 2026.09.0:
//!
//! 1. When a `WITH`/`RETURN` projection combines `*` with aggregating
//!    expressions, every variable carried by `*` is a grouping key (one
//!    group per carried row, exactly as when the variable is projected
//!    explicitly).
//! 2. An aggregate call in any ordinary expression position of an
//!    aggregating projection — including a CASE arm — belongs to that
//!    projection.
//!
//! The agreed rejections (unprojected carried variable, same-clause alias,
//! unprojected property leaf) stay rejections with unchanged error kinds.

mod common;

use relate::Value;

use common::{connection, err_text, list, refused_literal, run};

fn int(value: i64) -> Value {
    Value::Int(value)
}

fn seed() -> relate::Connection {
    let mut c = connection();
    run(&mut c, "CREATE (a {x: 1, y: 10}), (b {x: 2, y: 10})");
    c
}

/// The one negative assertion the rejection pins share (stellar std-6):
/// the query is refused with the `AmbiguousAggregationExpression` token.
/// The error comes from the canonical refusal runner, so a shape that gets
/// *admitted* fails with the statement named instead of `err_text`'s bare
/// "expected an error". The token is matched in the rendered text because
/// the public `relate::Error` carries no kind — the rejection lowers to
/// `Error::Syntax(String)` at this boundary; the typed
/// `ErrorKind::AmbiguousAggregationExpression` is pinned one layer down, by
/// the front-end validation tests.
fn assert_ambiguous_aggregation(c: &mut relate::Connection, query: &str) {
    let err = refused_literal(c, query).to_string();
    assert!(
        err.contains("AmbiguousAggregationExpression"),
        "{query} must stay rejected, got: {err}"
    );
}

// s20: `WITH *` expands to all in-scope variables; they become the grouping
// keys, so the carried `z` inside the aggregate-containing expression
// resolves — one group per carried row.
#[test]
fn labs_199_with_star_registers_grouping_keys() {
    let mut c = seed();

    let r = run(
        &mut c,
        "WITH 99 AS z MATCH (n) WITH *, collect(n.x) + [z] AS xs RETURN xs",
    );
    let mut xs: Vec<Value> = r.rows.into_iter().map(|row| row[0].clone()).collect();
    xs.sort_by_key(|v| format!("{v:?}"));
    assert_eq!(
        xs,
        vec![list(vec![int(1), int(99)]), list(vec![int(2), int(99)])]
    );
}

// The carried variables ride through to the next projection: `n` stays in
// scope after the aggregating `WITH *`, one row per group.
#[test]
fn labs_199_with_star_count_star_keeps_carried_variables() {
    let mut c = seed();

    let r = run(&mut c, "MATCH (n) WITH *, count(*) AS c RETURN n.x, c");
    let mut got: Vec<Vec<Value>> = r.rows;
    got.sort_by_key(|row| format!("{:?}", row[0]));
    assert_eq!(
        got,
        vec![vec![int(1), int(1)], vec![int(2), int(1)]],
        "one group per carried row"
    );
}

// A bare (non-list-wrapped) variable leaf next to the aggregate resolves
// against the `*` grouping keys too — the front-end and planner checks agree.
#[test]
fn labs_199_with_star_bare_variable_leaf() {
    let mut c = seed();

    let r = run(
        &mut c,
        "WITH 99 AS z MATCH (n) WITH *, count(n.x) + z AS v RETURN v",
    );
    assert_eq!(r.rows, vec![vec![int(100)], vec![int(100)]]);
}

// `RETURN *` with an aggregate follows the same ruling: the carried
// variables are the grouping keys and ride through to the result.
#[test]
fn labs_199_return_star_with_aggregate() {
    let mut c = seed();

    let r = run(
        &mut c,
        "WITH 99 AS z MATCH (n) RETURN *, collect(n.x) + [z] AS xs",
    );
    assert_eq!(r.header.columns, vec!["n", "xs", "z"]);
    let mut xs: Vec<Value> = r.rows.iter().map(|row| row[1].clone()).collect();
    xs.sort_by_key(|v| format!("{v:?}"));
    assert_eq!(
        xs,
        vec![list(vec![int(1), int(99)]), list(vec![int(2), int(99)])]
    );
}

// p-case: an aggregate call inside a CASE arm belongs to the enclosing
// aggregating projection.
#[test]
fn labs_199_aggregate_in_case_arm() {
    let mut c = seed();

    let r = run(
        &mut c,
        "MATCH (n) WITH CASE WHEN true THEN collect(n.x) + [3] ELSE [] END AS xs RETURN xs",
    );
    assert_eq!(r.rows, vec![vec![list(vec![int(1), int(2), int(3)])]]);

    // The not-taken arm's aggregate still belongs to the projection.
    let r = run(
        &mut c,
        "MATCH (n) WITH CASE WHEN false THEN [] ELSE collect(n.x) + [3] END AS xs RETURN xs",
    );
    assert_eq!(r.rows, vec![vec![list(vec![int(1), int(2), int(3)])]]);
}

// `WITH *` must not make an unprojected property leaf legal: `*` projects
// the variables, not their property expressions (ADR-0198 keeps the i03
// rejection).
#[test]
fn labs_199_with_star_still_rejects_property_leaf() {
    let mut c = seed();

    assert_ambiguous_aggregation(
        &mut c,
        "WITH 99 AS z MATCH (n) WITH *, collect(n.x) + [n.y] AS xs RETURN xs",
    );
}

// The agreed rejections keep their semantics: an unprojected carried
// variable (i01), a same-clause alias reference (i02), and an unprojected
// property leaf (i03) all stay errors.
#[test]
fn labs_199_agreed_rejections_unchanged() {
    let mut c = seed();

    assert_ambiguous_aggregation(
        &mut c,
        "WITH 10 AS y MATCH (n) WITH collect(n.x) + [y] AS xs RETURN xs",
    );

    // i02 keeps its own tolerance: the same-clause alias reference may
    // surface as either message, so it does not ride the strict helper.
    let err = err_text(c.query("MATCH (n) WITH n.y AS y, collect(n.x) + [y] AS xs RETURN xs"));
    assert!(
        err.contains("AmbiguousAggregationExpression") || err.contains("Variable"),
        "i02 must stay rejected, got: {err}"
    );

    let mut c = seed();
    assert_ambiguous_aggregation(
        &mut c,
        "MATCH (n) WITH collect(n.x) + [n.y] AS xs RETURN xs",
    );
}

// LABS-199's Comparison arm keeps Binary parity (stellar F-1-11): both arms
// judge a mixed side by the same policy — the planner's grouping-key leaf
// discipline, which admits a non-aggregate part built from a grouping key
// and refuses one reading a non-key leaf. Live Neo4j 2026.10.1 (probed
// 2026-11-21) returns the rows below for both arms and refuses both `n.q`
// spellings.
//
// One table row per case and one assertion site: both arms are generated
// from the row's own tail, so the parity the test claims is a row
// invariant, not two lists kept in step by index.
#[test]
fn labs_199_comparison_mixed_sides_follow_binary_rules() {
    /// What one row's mixed side is: the ambiguous shape both arms refuse,
    /// or the key-based shape both arms run — with each arm's rows.
    enum Expect {
        Ambiguous,
        Rows(Vec<Vec<Value>>, Vec<Vec<Value>>),
    }

    let mut c = seed();
    let false_rows = vec![vec![Value::Bool(false)], vec![Value::Bool(false)]];
    for (tail, expect) in [
        // Key-based arithmetic over the projected key: admitted by both
        // arms (the comparison is false for every group; the sum + key
        // value is the group's own).
        (
            "sum(n.y) + (n.x + 1)",
            Expect::Rows(
                false_rows.clone(),
                vec![vec![Value::Int(13)], vec![Value::Int(14)]],
            ),
        ),
        (
            "sum(n.y) + n.x",
            Expect::Rows(
                false_rows.clone(),
                vec![vec![Value::Int(12)], vec![Value::Int(13)]],
            ),
        ),
        // A pure aggregate beside the comparison, no non-aggregate part.
        (
            "sum(n.y)",
            Expect::Rows(
                false_rows.clone(),
                vec![vec![Value::Int(11)], vec![Value::Int(11)]],
            ),
        ),
        // `n.q` is no grouping reference: the key is the property `n.x`.
        ("sum(n.y) + n.q", Expect::Ambiguous),
    ] {
        let (comparison, binary) = match expect {
            Expect::Ambiguous => (None, None),
            Expect::Rows(comparison, binary) => (Some(comparison), Some(binary)),
        };
        for (arm, expected) in [("count(*) > ", comparison), ("count(*) + ", binary)] {
            let q = format!("MATCH (n) WITH n.x AS x, {arm}({tail}) AS c RETURN c");
            match expected {
                None => assert_ambiguous_aggregation(&mut c, &q),
                Some(rows) => assert_eq!(run(&mut c, &q).rows, rows, "{q}"),
            }
        }
    }
}
