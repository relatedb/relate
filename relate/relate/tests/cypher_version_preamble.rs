//! The CIP-150 `CYPHER <version>` preamble under the one-version contract
//! (ADR-0584): `CYPHER 25` is an accepted no-op — the engine implements one
//! dialect, so an explicit `CYPHER 25` and no preamble at all execute
//! identically — while every other `CYPHER <n>` integer fails at parse with
//! the authored unsupported-version error. These pins hold the no-op at the
//! executor level — the null-argument leniencies and the `createStatement`
//! renderings the statement version used to select — for any whitespace
//! after the preamble (the historical query-text prefix sniff required a
//! literal space after the preamble and silently missed `CYPHER 25\nSHOW
//! ...`).

mod common;

use relate::Value;

use common::{connection, err_text, params};

/// Run `src` with `$x` bound to `null`: the null-argument cases below pass
/// the parameter explicitly, because an absent one fails the statement with
/// `ParameterMissing` (ADR-0308).
fn query_x_null(src: &str) -> relate::Result<relate::QueryResult> {
    connection().query_with_params(src, params(&[("x", Value::Null)]))
}

#[test]
fn show_settings_null_arg_stays_lenient_under_every_spelling() {
    for src in [
        "CYPHER 25 SHOW SETTINGS $x",
        "CYPHER 25\nSHOW SETTINGS $x",
        "CYPHER 25\tSHOW SETTINGS $x",
        "SHOW SETTINGS $x",
    ] {
        let result = query_x_null(src).unwrap_or_else(|e| panic!("{src}: {e}"));
        assert!(result.rows.is_empty(), "wrong outcome for {src:?}");
    }
}

#[test]
fn show_and_terminate_null_args_are_identical_under_every_spelling() {
    let mut conn = connection();
    // The `null`-literal spellings reference no parameter, so each runs as
    // a plain query — no binding fabricated for it.
    for (prefixed, unprefixed) in [
        (
            "CYPHER 25\nSHOW TRANSACTIONS null YIELD transactionId RETURN transactionId",
            "SHOW TRANSACTIONS null YIELD transactionId RETURN transactionId",
        ),
        (
            "CYPHER 25\nTERMINATE TRANSACTIONS null",
            "TERMINATE TRANSACTIONS null",
        ),
    ] {
        let prefixed = conn
            .query(prefixed)
            .unwrap_or_else(|e| panic!("{prefixed}: {e}"));
        let unprefixed = conn
            .query(unprefixed)
            .unwrap_or_else(|e| panic!("{unprefixed}: {e}"));
        assert_eq!(
            prefixed.rows, unprefixed.rows,
            "the preamble must not change the outcome of {unprefixed:?}"
        );
    }
    // The one real parameter case: `SHOW DATABASE $db` with `$db` bound to
    // null is the null-argument leniency under both spellings.
    let prefixed = conn
        .query_with_params(
            "CYPHER 25\nSHOW DATABASE $db YIELD name RETURN name",
            params(&[("db", Value::Null)]),
        )
        .unwrap_or_else(|e| panic!("CYPHER 25 SHOW DATABASE $db: {e}"));
    let unprefixed = conn
        .query_with_params(
            "SHOW DATABASE $db YIELD name RETURN name",
            params(&[("db", Value::Null)]),
        )
        .unwrap_or_else(|e| panic!("SHOW DATABASE $db: {e}"));
    assert_eq!(
        prefixed.rows, unprefixed.rows,
        "the preamble must not change the outcome of SHOW DATABASE $db"
    );
}

#[test]
fn nested_call_next_and_expression_subqueries_behave_identically() {
    // The preamble (or its absence) reaches nothing: nested bodies, NEXT
    // stages, and EXISTS/COUNT/COLLECT expression subqueries all keep the
    // one dialect's behavior — each prefixed spelling returns exactly what
    // its unprefixed twin returns.
    for (prefixed, unprefixed) in [
        (
            "CYPHER 25 CALL { SHOW SETTINGS $x YIELD name RETURN name } RETURN 1 AS n",
            "CALL { SHOW SETTINGS $x YIELD name RETURN name } RETURN 1 AS n",
        ),
        (
            "CYPHER 25\nRETURN 1 AS n NEXT SHOW SETTINGS $x",
            "RETURN 1 AS n NEXT SHOW SETTINGS $x",
        ),
        (
            "CYPHER 25 RETURN EXISTS { SHOW SETTINGS $x } AS e",
            "RETURN EXISTS { SHOW SETTINGS $x } AS e",
        ),
        (
            "CYPHER 25 RETURN COUNT { SHOW SETTINGS $x } AS c",
            "RETURN COUNT { SHOW SETTINGS $x } AS c",
        ),
        (
            "CYPHER 25 RETURN COLLECT { SHOW SETTINGS $x YIELD name RETURN name } AS c",
            "RETURN COLLECT { SHOW SETTINGS $x YIELD name RETURN name } AS c",
        ),
    ] {
        let prefixed = query_x_null(prefixed).unwrap_or_else(|e| panic!("{prefixed}: {e}"));
        let unprefixed = query_x_null(unprefixed).unwrap_or_else(|e| panic!("{unprefixed}: {e}"));
        assert_eq!(
            prefixed.rows, unprefixed.rows,
            "the preamble must not change the outcome of {unprefixed:?}"
        );
    }
}

#[test]
fn show_transactions_current_query_is_the_statement_text() {
    let src = "// a marker comment\nSHOW TRANSACTIONS YIELD currentQuery RETURN currentQuery";
    let result = connection().query(src).unwrap();
    assert_eq!(
        result.rows,
        vec![vec![Value::String(src.into())]],
        "currentQuery must be the full statement text"
    );
}

#[test]
fn show_constraints_create_statement_is_identical_under_every_spelling() {
    let mut conn = connection();
    conn.query("CREATE CONSTRAINT x FOR (n:L) REQUIRE n.p IS :: INT")
        .unwrap();
    let mut statements = Vec::new();
    for src in [
        "CYPHER 25 SHOW CONSTRAINTS YIELD name, createStatement RETURN name, createStatement",
        "SHOW CONSTRAINTS YIELD name, createStatement RETURN name, createStatement",
    ] {
        let result = conn.query(src).unwrap();
        assert_eq!(result.rows.len(), 1, "{src}");
        assert!(
            matches!(&result.rows[0][1], Value::String(s) if s.contains("IS :: INTEGER")),
            "the createStatement renders the same form under {src:?}: {:?}",
            result.rows
        );
        statements.push(result.rows[0][1].clone());
    }
    assert_eq!(
        statements[0], statements[1],
        "the preamble must not change the createStatement"
    );
}

/// The LABS-1405 repro: `CYPHER 5 WHEN true THEN RETURN 2 AS x` used to run
/// (the preamble was a no-op); under the one-version contract (ADR-0584) it
/// fails at parse with the authored unsupported-version error, while the
/// unprefixed and `CYPHER 25` forms return 2.
#[test]
fn the_labs_1405_repro_fails_with_the_authored_error() {
    let err = err_text(connection().query("CYPHER 5 WHEN true THEN RETURN 2 AS x"));
    assert!(
        err.contains(&relate_test_support::ce_text::unsupported_cypher_version(5)),
        "wrong error: {err}"
    );
    for src in [
        "WHEN true THEN RETURN 2 AS x",
        "CYPHER 25 WHEN true THEN RETURN 2 AS x",
    ] {
        let result = connection()
            .query(src)
            .unwrap_or_else(|e| panic!("{src}: {e}"));
        assert_eq!(result.rows, vec![vec![Value::Int(2)]], "{src}");
    }
}

/// EXPLAIN under the one admitted version plans (the preamble rides
/// along); under a rejected version it fails with the same authored error,
/// before any planning.
#[test]
fn explain_plans_under_cypher_25_and_fails_under_cypher_5() {
    let result = connection()
        .query("EXPLAIN CYPHER 25 RETURN 1 AS n")
        .unwrap();
    assert!(result.plan_root().is_some(), "EXPLAIN stamps a plan");
    let err = err_text(connection().query("EXPLAIN CYPHER 5 RETURN 1 AS n"));
    assert!(
        err.contains(&relate_test_support::ce_text::unsupported_cypher_version(5)),
        "wrong error: {err}"
    );
}
