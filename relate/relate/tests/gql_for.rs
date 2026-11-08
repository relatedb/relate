//! CIP-250 (GQ10): the `FOR <variable> IN <expression>` clause — the
//! GQL-compliant successor spelling of `UNWIND <expression> AS
//! <variable>`, normalized onto the existing UNWIND infrastructure
//! (identical validation, planning, and execution). Every expectation is
//! doc-derived from the CIP-250 example table. Default dialect since its
//! graduation from the `gql-for` preview row (verified unlocked in live
//! Neo4j 2026.09 and 2026.10.1) — no `RELATE_PREVIEW` opt-in needed.

mod common;

use relate::Value;

/// The single-column `x` rows of a FOR query, in order.
fn x_rows(c: &mut relate::Connection, query: &str) -> Vec<Value> {
    common::rows(c, query)
        .into_iter()
        .map(|row| row.into_iter().next().expect("one column"))
        .collect()
}

fn int(v: i64) -> Value {
    Value::Int(v)
}

#[test]
fn for_matches_unwind_on_lists_nulls_and_non_lists() {
    // The CIP-250 test-case table: FOR produces identical results to UNWIND
    // for list, null, and non-list values (a non-list is coerced to a
    // singleton list, matching UNWIND).
    let mut c = common::connection();
    for (for_query, unwind_query, want) in [
        (
            "FOR x IN [1, 2, 3] RETURN x",
            "UNWIND [1, 2, 3] AS x RETURN x",
            vec![int(1), int(2), int(3)],
        ),
        (
            "FOR x IN null RETURN x",
            "UNWIND null AS x RETURN x",
            vec![],
        ),
        (
            "FOR x IN 42 RETURN x",
            "UNWIND 42 AS x RETURN x",
            vec![int(42)],
        ),
    ] {
        assert_eq!(x_rows(&mut c, for_query), want, "{for_query}");
        // Identical to the UNWIND spelling of the same statement.
        assert_eq!(x_rows(&mut c, unwind_query), want, "{unwind_query}");
    }
}

#[test]
fn for_chains_and_nested_lists() {
    // The CIP-250 examples: a null element in an intermediate row produces
    // no rows for it; nested lists unnest per level.
    let mut c = common::connection();
    let got = x_rows(&mut c, "FOR x IN [1, null, 2] FOR y IN x RETURN y");
    assert_eq!(got, vec![int(1), int(2)]);
    let got = x_rows(&mut c, "FOR x IN [[1, 2], [3, 4]] FOR y IN x RETURN y");
    assert_eq!(got, vec![int(1), int(2), int(3), int(4)]);
    // Unnested one level only: the outer rows are the inner lists.
    let got = x_rows(&mut c, "FOR x IN [[1, 2], [3, 4]] RETURN x");
    assert_eq!(
        got,
        vec![
            common::list(vec![int(1), int(2)]),
            common::list(vec![int(3), int(4)]),
        ]
    );
}

#[test]
fn for_in_every_unwind_clause_position() {
    // The CIP-250 test-case table: variable scoping and interaction with
    // every clause position where UNWIND is valid — after MATCH, after
    // WITH, inside a CALL subquery, with aggregation, and inside FOREACH.
    let mut c = common::connection();
    c.query("CREATE (:Person {name: 'Ada', friends: ['Bob', 'Cyd']})")
        .unwrap();

    let rows = common::rows(
        &mut c,
        "MATCH (n:Person) FOR friend IN n.friends RETURN friend",
    );
    assert_eq!(
        rows,
        vec![vec![common::string("Bob")], vec![common::string("Cyd")]]
    );

    let rows = common::rows(&mut c, "WITH 3 AS n FOR x IN range(1, n) RETURN x");
    assert_eq!(rows, vec![vec![int(1)], vec![int(2)], vec![int(3)]]);

    let rows = common::rows(&mut c, "WITH [1, 2, 3] AS list FOR x IN list RETURN x");
    assert_eq!(rows.len(), 3);

    let rows = common::rows(&mut c, "CALL { FOR x IN [1, 2, 3] RETURN x } RETURN x");
    assert_eq!(rows.len(), 3);

    let rows = common::rows(
        &mut c,
        "MATCH (n:Person) FOR tag IN n.friends WITH tag, count(*) AS cnt RETURN tag, cnt",
    );
    assert_eq!(rows.len(), 2);

    // A FOREACH body admits only updating clauses, so the CIP-250 `FOR`
    // spelling is invalid there in every dialect — Neo4j 2026.09 rejects it
    // with 42I01 "invalid FOREACH. Invalid use of FOR inside 'FOREACH'.",
    // and the FOREACH rule reports it.
    let err = c
        .query("FOREACH (i IN [1] | FOR x IN [10, 20] CREATE (:Tagged {v: x}))")
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("invalid use of FOR inside 'FOREACH'"),
        "{err}"
    );
    let rows = common::rows(&mut c, "MATCH (t:Tagged) RETURN t.v ORDER BY t.v");
    assert!(rows.is_empty(), "{rows:?}");
}

#[test]
fn for_forwards_variables_into_subqueries() {
    // The CIP-250 interaction table: variables introduced by FOR reach a
    // CALL subquery body through its importing WITH — a naked reference is
    // Neo4j's 42N62, same as for any other outer variable.
    let mut c = common::connection();
    let rows = common::rows(
        &mut c,
        "FOR x IN [1, 2] CALL { WITH x RETURN x AS inner } RETURN inner",
    );
    assert_eq!(rows, vec![vec![int(1)], vec![int(2)]]);
    // The FOR variable is visible inside an importing subquery body too.
    // (The *importing* WITH may only name outside variables — an alias like
    // `WITH x AS carried` is 42I28, live Neo4j — so the rename happens in
    // the body's RETURN.)
    let rows = common::rows(
        &mut c,
        "FOR x IN [1, 2] CALL { WITH x RETURN x AS carried } RETURN carried",
    );
    assert_eq!(rows, vec![vec![int(1)], vec![int(2)]]);
}

#[test]
fn for_shows_the_unwind_plan_operator() {
    // The CIP-250 interaction table: the query plan shows the Unwind
    // operator, identical to UNWIND — no new operator exists.
    let mut c = common::connection();
    let result = c.query("EXPLAIN FOR x IN [1, 2, 3] RETURN x").unwrap();
    let unwind = c.query("EXPLAIN UNWIND [1, 2, 3] AS x RETURN x").unwrap();
    // The operator shape is identical (the plan's `statement_echo` field
    // alone carries the differing query text).
    let for_unwind = common::find_node(common::plan(&result), "Unwind")
        .expect("the FOR plan shows the Unwind operator");
    assert!(common::find_node(common::plan(&unwind), "Unwind").is_some());
    assert_eq!(
        format!("{for_unwind:?}"),
        format!(
            "{:?}",
            common::find_node(common::plan(&unwind), "Unwind").expect("the UNWIND operator")
        )
    );
}

#[test]
fn for_variable_binding_follows_the_unwind_rules() {
    // The CIP-250 interaction table: the binding variable follows the same
    // scoping rules as UNWIND — a binding clause introduces a NEW name, so
    // re-using one the enclosing scope already bound is 42N59 (verified
    // live on Neo4j 2026.09 under CYPHER 25; ForStatementAcceptance
    // [16]/[19]). The two spellings behave identically.
    let mut c = common::connection();
    let for_error = common::err_text(c.query("WITH 1 AS x FOR x IN [1, 2] RETURN x"));
    let unwind_error = common::err_text(c.query("WITH 1 AS x UNWIND [1, 2] AS x RETURN x"));
    assert_eq!(for_error, unwind_error);
    assert!(
        for_error.contains("VariableAlreadyBound"),
        "wrong error: {for_error}"
    );
    // A genuinely unknown variable after the clause is the same error
    // either way.
    let for_error = common::err_text(c.query("FOR x IN [1, 2] RETURN y"));
    let unwind_error = common::err_text(c.query("UNWIND [1, 2] AS x RETURN y"));
    assert_eq!(for_error, unwind_error);
    assert!(for_error.contains("Variable"), "wrong error: {for_error}");
}

#[test]
fn for_error_shapes_are_parse_errors() {
    // The CIP-250 example table's error shapes: missing IN, missing
    // variable, and the UNWIND-shaped AS form all fail the parse in the
    // default dialect (the grammar admits well-formed FOR only).
    let mut c = common::connection();
    for query in [
        "FOR x [1, 2, 3] RETURN x",
        "FOR IN [1, 2, 3] RETURN x",
        "FOR [1, 2, 3] AS x RETURN x",
    ] {
        let error = common::err_text(c.query(query));
        assert!(!error.is_empty(), "{query} parsed");
    }
}
