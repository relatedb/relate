//! ADR-0669's batch pass (one clause-engine pass per batch instead of one
//! nested invocation per row), end to end: a 100 001-row bulk create past
//! the old 100 000-pass budget, the pass's results matching the per-row
//! path's, the pipeline-order read-after-write semantics pinned both
//! ways, a pattern-comprehension body evaluating the batch, per-batch
//! rollback with `REPORT STATUS` on the pass, and the plan details
//! carrying the strategy (`batch pass` / `per-row`). Split from
//! `call_in_transactions.rs` (the clause forms, batch boundaries, ON
//! ERROR and the modifier-argument coverage stay there) while that file
//! approached the 1000-line bar.

mod common;

use relate::Value;

use common::{connection, find_node, plan, scalar};

/// The batch pass end-to-end (ADR-0669): the exact bulk-create shape
/// the WAL payload cap's remedy names — 100 001 rows through
/// `CALL (id) { CREATE (…) } IN TRANSACTIONS OF 25000 ROWS` — completes
/// with every node landed. (The budget-crossing regression itself is
/// pinned at the unit level: `the_counting_rule_ticks_each_event_once`
/// asserts the per-batch tick count, and the bombs in
/// `nested_eval_tests` cross the raised 1 000 000-pass budget,
/// ADR-0670.)
#[test]
fn bulk_create_batches_end_to_end() {
    const ROWS: i64 = 100_001;
    let mut c = connection();
    c.query(&format!(
        "UNWIND range(1, {ROWS}) AS id \
         CALL (id) {{ CREATE (:Person {{name: 'Hakan ' + id, id: id, age: id % 100}}) }} \
         IN TRANSACTIONS OF 25000 ROWS"
    ))
    .unwrap();
    assert_eq!(
        scalar(&mut c, "MATCH (n:Person) RETURN count(n)"),
        Value::Int(ROWS)
    );
    assert_eq!(
        scalar(
            &mut c,
            &format!("MATCH (n:Person {{id: {ROWS}}}) RETURN n.age")
        ),
        Value::Int(ROWS % 100)
    );
}

/// The batch pass produces the per-row path's results: every row's
/// writes land with its own bindings, in input order, for both the
/// scope-clause and the naked form.
#[test]
fn batch_pass_matches_per_row_results() {
    let mut c = connection();
    c.query(
        "UNWIND [1, 2, 3] AS id \
         CALL (id) { CREATE (:B {i: id, doubled: id * 2}) } \
         IN TRANSACTIONS OF 2 ROWS",
    )
    .unwrap();
    assert_eq!(
        scalar(&mut c, "MATCH (n:B) RETURN count(DISTINCT n.doubled)"),
        Value::Int(3)
    );
    assert_eq!(
        c.query("MATCH (n:B) RETURN n.i AS i ORDER BY i")
            .unwrap()
            .rows,
        vec![
            vec![Value::Int(1)],
            vec![Value::Int(2)],
            vec![Value::Int(3)]
        ]
    );
    // The naked form: a unit body runs per outer row (its seed is the
    // whole outer row), batched the same — two more creates.
    c.query("UNWIND [4, 5] AS x CALL { CREATE (:B {i: 99}) } IN TRANSACTIONS OF 1 ROW")
        .unwrap();
    assert_eq!(scalar(&mut c, "MATCH (n:B) RETURN count(n)"), Value::Int(5));
    assert_eq!(
        scalar(&mut c, "MATCH (n:B {i: 99}) RETURN count(n)"),
        Value::Int(2)
    );
}

/// A read-after-write body takes the batch pass and evaluates the batch
/// as a unit (ADR-0669): the clause engine's pipeline order lets row 1's
/// `MATCH` see row 2's `CREATE` — `hi` is set on the `v: 2` node — where
/// Neo4j's per-row nested loop would find nothing. That interleaving is
/// deliberately not preserved: the statement names the batch, and a
/// caller needing per-row observation runs smaller batches.
#[test]
fn read_after_write_body_evaluates_the_batch_as_a_unit() {
    let mut c = connection();
    c.query(
        "UNWIND [1, 2] AS id \
         CALL (id) { CREATE (:R {v: id}) MATCH (x:R {v: id + 1}) SET x.hi = true } \
         IN TRANSACTIONS OF 2 ROWS",
    )
    .unwrap();
    assert_eq!(
        scalar(&mut c, "MATCH (n:R) RETURN count(n)"),
        Value::Int(2),
        "both rows' creates land"
    );
    assert_eq!(
        scalar(&mut c, "MATCH (n:R {v: 2}) RETURN n.hi"),
        Value::Bool(true),
        "row 1's MATCH sees row 2's create: the batch is the unit"
    );
}

/// A `WITH`-carrying body keeps the per-row path (the importing-scope
/// re-attach is per-row by construction, ADR-0669), and with it the
/// per-row interleaving: row `j`'s `MATCH` sees only rows `< j`'s
/// writes — no `hi` anywhere. The creates land first (the positive
/// anchor, the twin of
/// `read_after_write_body_evaluates_the_batch_as_a_unit`), so the
/// missing `hi` reads as per-row semantics held, not as the body doing
/// nothing.
#[test]
fn with_body_keeps_per_row_semantics() {
    let mut c = connection();
    c.query(
        "UNWIND [1, 2] AS id \
         CALL (id) { WITH id CREATE (:R2 {v: id}) MATCH (x:R2 {v: id + 1}) SET x.hi = true } \
         IN TRANSACTIONS OF 2 ROWS",
    )
    .unwrap();
    assert_eq!(
        scalar(&mut c, "MATCH (n:R2) RETURN count(n)"),
        Value::Int(2),
        "both rows' creates land"
    );
    assert_eq!(
        scalar(
            &mut c,
            "MATCH (n:R2) WHERE n.hi IS NOT NULL RETURN count(n)"
        ),
        Value::Int(0),
        "the per-row path never lets a row's MATCH see a later row's create"
    );
}

/// Per-batch rollback and `REPORT STATUS` work on the batch pass: a
/// uniqueness violation inside one multi-row batch rolls the whole batch
/// back and reports it, the next batch still commits.
#[test]
fn batch_pass_rolls_back_a_failed_batch_and_reports_status() {
    let mut c = connection();
    c.query("CREATE CONSTRAINT s_i FOR (n:S) REQUIRE n.i IS UNIQUE")
        .unwrap();
    let rows = c
        .query(
            "UNWIND [1, 1, 3] AS id \
             CALL (id) { CREATE (:S {i: id}) } \
             IN TRANSACTIONS OF 2 ROWS ON ERROR CONTINUE REPORT STATUS AS s \
             RETURN id, s.committed AS committed",
        )
        .unwrap()
        .rows;
    assert_eq!(
        rows,
        vec![
            vec![Value::Int(1), Value::Bool(false)],
            vec![Value::Int(1), Value::Bool(false)],
            vec![Value::Int(3), Value::Bool(true)],
        ],
        "the duplicate pair fails as one batch, the singleton commits"
    );
    assert_eq!(
        scalar(&mut c, "MATCH (n:S) RETURN count(n)"),
        Value::Int(1),
        "the failed batch's first create is rolled back with it"
    );
}

/// A graph-reading expression inside a write clause also takes the
/// batch pass (ADR-0669): the pattern comprehension in row 1's `SET`
/// sees the whole batch's self-loops, so both rows report the same peer
/// count — the per-row path would count 1, then 2.
#[test]
fn scope_bearing_expression_body_evaluates_the_batch_as_a_unit() {
    let mut c = connection();
    c.query(
        "UNWIND [1, 2] AS id \
         CALL (id) { CREATE (n:PE {v: id})-[:SELF]->(n) SET n.peers = size([(x:PE)-[:SELF]->(y:PE) | y]) } \
         IN TRANSACTIONS OF 2 ROWS",
    )
    .unwrap();
    assert_eq!(
        c.query("MATCH (n:PE) RETURN n.peers AS p ORDER BY p")
            .unwrap()
            .rows,
        vec![vec![Value::Int(2)], vec![Value::Int(2)]]
    );
}

/// ADR-0669 + the Neo4j-aligned details (probed against a local Neo4j
/// 2026.10.1): EXPLAIN carries the batch fact, the failure policy and
/// the execution strategy on the `CallSubquery` operator — all
/// plan-time, so they show before any row runs, and the strategy names
/// which path the executor will take.
#[test]
fn explain_carries_the_batch_facts_and_the_strategy() {
    let mut c = connection();
    let result = c
        .query(
            "EXPLAIN UNWIND range(1, 10) AS id \
             CALL (id) { CREATE (:E {i: id}) } \
             IN TRANSACTIONS OF 3 ROWS",
        )
        .unwrap();
    let node = find_node(plan(&result), "CallSubquery").expect("the batched call");
    assert!(
        node.details.contains("IN TRANSACTIONS OF 3 ROWS"),
        "{node:?}"
    );
    assert!(node.details.contains("ON ERROR FAIL"), "{node:?}");
    assert!(node.details.contains("batch pass"), "{node:?}");

    let result = c
        .query(
            "EXPLAIN UNWIND range(1, 10) AS id \
             CALL (id) { WITH id CREATE (:E {i: id}) } \
             IN TRANSACTIONS OF 3 ROWS ON ERROR CONTINUE",
        )
        .unwrap();
    let node = find_node(plan(&result), "CallSubquery").expect("the batched call");
    assert!(node.details.contains("ON ERROR CONTINUE"), "{node:?}");
    assert!(node.details.contains("per-row"), "{node:?}");
}
