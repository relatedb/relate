//! CIP-135 `WHEN`/`ELSE` through the real execution entry points: a
//! conditional is a per-row operator, so a driving row's `WHEN` sees the
//! state its predecessor's taken branch left (the CIP-213 NEXT-chain shape
//! "WHEN as a NEXT part sees updates made by the branch of a previous row"),
//! while a stage's other clauses keep their whole-stage batch semantics.

use relate_core::Value;

use super::test_support::{column, exec_autocommit, exec_cypher, int_cell, string_cell};
use super::QueryResult;
use crate::graph::Graph;

/// The integer cells of `name`, in result order.
fn integers(result: &QueryResult, name: &str) -> Vec<i64> {
    column(result, name, int_cell)
}

/// The string cells of `name`, in result order.
fn strings(result: &QueryResult, name: &str) -> Vec<String> {
    column(result, name, string_cell)
}

/// A graph seeded by one `setup` statement — the WHEN scenarios' starting
/// graph.
fn seeded(setup: &str) -> Graph {
    let graph = Graph::default();
    exec_autocommit(&graph, setup).unwrap();
    graph
}

/// A graph holding the one `(:State {size: 0})` node the WHEN scenarios
/// start from.
fn state_graph() -> Graph {
    seeded("CREATE (:State {size: 0})")
}

/// A `WHEN` predicate must be a boolean: a numeric or string property is a
/// type error (Neo4j 22G03), never truthy-coerced.
#[test]
fn when_rejects_a_non_boolean_predicate() {
    let graph = seeded("CREATE (:Person {age: 38, name: 'Alice'})");
    for predicate in ["n.age", "n.name"] {
        let query =
            format!("MATCH (n:Person) CALL (n) {{ WHEN {predicate} THEN RETURN 1 AS x }} RETURN x");
        let error = match exec_autocommit(&graph, &query) {
            Err(error) => error,
            Ok(_) => panic!("{predicate}: a non-boolean WHEN predicate must be rejected"),
        };
        assert!(
            error.to_string().contains("requires a boolean"),
            "{predicate}: {error}"
        );
    }
}

/// The cited regression: a `WHEN` in a NEXT chain is evaluated per row
/// against the state the previous row left, so only the first of the three
/// driving rows still sees `size = 0` — one hit instead of three.
#[test]
fn when_in_a_next_chain_sees_the_previous_rows_write() {
    let graph = state_graph();
    let result = exec_autocommit(
        &graph,
        "UNWIND range(1, 3) AS i \
         MATCH (b:State) \
         RETURN i, b \
         NEXT \
         WHEN b.size = 0 THEN \
           SET b.size = i \
           RETURN i AS hit",
    )
    .unwrap();
    assert_eq!(result.columns, vec!["hit".to_string()]);
    assert_eq!(integers(&result, "hit"), vec![1]);

    // The write the single hit made stands: row 1's `i`.
    let size = exec_cypher(&graph, "MATCH (b:State) RETURN b.size AS size");
    assert_eq!(integers(&size, "size"), vec![1]);
}

/// The ELSE twin: the first row takes `ELSE` and writes, and the following
/// rows' `WHEN` sees that write.
#[test]
fn when_in_a_next_chain_sees_the_previous_rows_else_write() {
    let graph = state_graph();
    let result = exec_autocommit(
        &graph,
        "UNWIND range(1, 3) AS i \
         MATCH (b:State) \
         RETURN i, b \
         NEXT \
         WHEN b.size <> 0 THEN \
           RETURN 0 AS hit \
         ELSE \
           SET b.size = i \
           RETURN i AS hit",
    )
    .unwrap();
    // Grouped by branch in written order: the two `WHEN` hits, then the
    // `ELSE` hit (CIP-213's observed conditional output order).
    assert_eq!(integers(&result, "hit"), vec![0, 0, 1]);
    let size = exec_cypher(&graph, "MATCH (b:State) RETURN b.size AS size");
    assert_eq!(integers(&size, "size"), vec![1]);
}

/// A label predicate in the `WHEN` reads the graph, not the row's label
/// snapshot: the predecessor's `SET b:Done` hides the label from the
/// following rows — one hit, not three.
#[test]
fn when_in_a_next_chain_sees_a_label_its_predecessor_set() {
    let graph = seeded("CREATE (:State)");
    let result = exec_autocommit(
        &graph,
        "UNWIND range(1, 3) AS i \
         MATCH (b:State) \
         RETURN i, b \
         NEXT \
         WHEN NOT b:Done THEN \
           SET b:Done \
           RETURN i AS hit",
    )
    .unwrap();
    assert_eq!(integers(&result, "hit"), vec![1]);

    let done = exec_cypher(&graph, "MATCH (b:State:Done) RETURN count(*) AS c");
    assert_eq!(integers(&done, "c"), vec![1]);
}

/// The removal twin: `REMOVE b:Todo` by the first row's branch takes the
/// label away from the following rows' `WHEN`.
#[test]
fn when_in_a_next_chain_sees_a_label_its_predecessor_removed() {
    let graph = seeded("CREATE (:State:Todo)");
    let result = exec_autocommit(
        &graph,
        "UNWIND range(1, 3) AS i \
         MATCH (b:State) \
         RETURN i, b \
         NEXT \
         WHEN b:Todo THEN \
           REMOVE b:Todo \
           RETURN i AS hit",
    )
    .unwrap();
    assert_eq!(integers(&result, "hit"), vec![1]);

    let todo = exec_cypher(&graph, "MATCH (b:State:Todo) RETURN count(*) AS c");
    assert_eq!(integers(&todo, "c"), vec![0]);
}

/// A property read in the `WHEN` does not fall back to the row's stale
/// property map once a predecessor's branch removed the property — one
/// hit, not three.
#[test]
fn when_in_a_next_chain_sees_a_property_its_predecessor_removed() {
    let graph = state_graph();
    let result = exec_autocommit(
        &graph,
        "UNWIND range(1, 3) AS i \
         MATCH (b:State) \
         RETURN i, b \
         NEXT \
         WHEN b.size IS NOT NULL THEN \
           REMOVE b.size \
           RETURN i AS hit",
    )
    .unwrap();
    assert_eq!(integers(&result, "hit"), vec![1]);

    let sized = exec_cypher(
        &graph,
        "MATCH (b:State) WHERE b.size IS NOT NULL RETURN count(*) AS c",
    );
    assert_eq!(integers(&sized, "c"), vec![0]);
}

/// The conditional's output is grouped by branch in written order, not in
/// driving-row order (Neo4j's observed order).
#[test]
fn when_in_a_next_chain_groups_its_output_by_branch() {
    let graph = seeded("CREATE (:L1 {x: 1}), (:L1 {x: 2}), (:L2 {x: 3})");
    let result = exec_autocommit(
        &graph,
        "MATCH (n) RETURN n ORDER BY n.x \
         NEXT \
         WHEN n.x > 2 THEN RETURN \"large number\" AS msg \
         WHEN n.x > 1 THEN RETURN \"small number\" AS msg \
         ELSE RETURN \"tiny number\" AS msg",
    )
    .unwrap();
    // Branch-major in written order, not the ascending driving-row order
    // (Neo4j's observed conditional output order).
    assert_eq!(
        strings(&result, "msg"),
        vec!["large number", "small number", "tiny number"]
    );
}

/// A taken branch body runs once per incoming row, so an aggregate inside
/// it sees one row, not the whole stage's stream.
#[test]
fn when_in_a_next_chain_aggregates_per_incoming_row() {
    let graph = Graph::default();
    let result = exec_autocommit(
        &graph,
        "UNWIND range(1, 3) AS i RETURN i NEXT WHEN true THEN RETURN count(*) AS c",
    )
    .unwrap();
    assert_eq!(integers(&result, "c"), vec![1, 1, 1]);
}

/// A write-only branch body projects no statement result: the row it ran
/// for contributes no result row, so a unit WHEN's result is empty even
/// though its write stands.
#[test]
fn a_unit_when_branch_contributes_no_rows() {
    let graph = state_graph();
    let result = exec_autocommit(
        &graph,
        "UNWIND range(1, 3) AS i \
         MATCH (b:State) \
         RETURN i, b \
         NEXT \
         WHEN COUNT { (:Made) } = 0 THEN \
           CREATE (:Made)",
    )
    .unwrap();
    assert!(result.rows.is_empty(), "{:?}", result.rows);

    // Only the first row's branch ran: one created node.
    let made = exec_cypher(&graph, "MATCH (m:Made) RETURN count(*) AS c");
    assert_eq!(integers(&made, "c"), vec![1]);
}

/// A unit `WHEN` stage is still one unit of output for the NEXT seeding
/// policy (ADR-0287): the following stage runs once, over one row.
#[test]
fn a_unit_when_next_part_seeds_one_row() {
    let graph = Graph::default();
    let result = exec_autocommit(
        &graph,
        "UNWIND range(1, 3) AS i \
         RETURN i \
         NEXT \
         WHEN i > 0 THEN \
           CREATE (:B) \
         NEXT \
         RETURN count(*) AS c",
    )
    .unwrap();
    assert_eq!(integers(&result, "c"), vec![1]);

    // Every driving row's branch still ran.
    let created = exec_cypher(&graph, "MATCH (b:B) RETURN count(*) AS c");
    assert_eq!(integers(&created, "c"), vec![3]);
}

/// The per-row scope is the conditional, not the stage: a filtering MATCH
/// still evaluates its predicate over the whole incoming batch before any
/// later update, so every row matches the pre-stage state.
#[test]
fn a_filtering_match_in_a_next_part_still_evaluates_before_the_update() {
    let graph = state_graph();
    let result = exec_autocommit(
        &graph,
        "UNWIND range(1, 3) AS i \
         RETURN i \
         NEXT \
         MATCH (b:State) WHERE b.size = 0 \
         SET b.size = i \
         RETURN i AS hit",
    )
    .unwrap();
    assert_eq!(integers(&result, "hit"), vec![1, 2, 3]);
    let size = exec_cypher(&graph, "MATCH (b:State) RETURN b.size AS size");
    assert_eq!(integers(&size, "size"), vec![3]);
}

/// A `CALL (vars) { WHEN … / ELSE … }` whose branch bodies are all unit
/// (write-only) output is itself a unit body: the outer rows pass through
/// with the branch writes applied, exactly like a plain unit subquery,
/// instead of being dropped by the inner-join merge (gap 3 — the labels
/// were written while `c` came back 0).
#[test]
fn a_unit_conditional_call_body_passes_the_outer_rows_through() {
    let graph = seeded("CREATE (:L {m: 'S'}), (:L {m: '.'})");
    let result = exec_autocommit(
        &graph,
        "MATCH (l:L) \
         CALL (l) { \
           WHEN l.m = 'S' THEN { SET l:A } \
           ELSE { SET l:B } \
         } \
         RETURN count(l) AS c",
    )
    .unwrap();
    assert_eq!(integers(&result, "c"), vec![2]);

    // Both branches ran on their own driving row: `S` gained `A`, `.` `B`.
    let a = exec_cypher(&graph, "MATCH (l:L:A) RETURN l.m AS m ORDER BY m");
    assert_eq!(strings(&a, "m"), vec!["S"]);
    let b = exec_cypher(&graph, "MATCH (l:L:B) RETURN l.m AS m ORDER BY m");
    assert_eq!(strings(&b, "m"), vec!["."]);
}

/// The gap report's baseline: a plain unit subquery body (a bare `SET`, no
/// conditional) keeps its pass-through — the unit-conditional refinement
/// changed no other body shape.
#[test]
fn a_plain_unit_subquery_call_body_still_passes_the_outer_rows_through() {
    let graph = seeded("CREATE (:L {m: 'S'}), (:L {m: '.'})");
    let result = exec_autocommit(
        &graph,
        "MATCH (l:L) CALL (l) { SET l:C } RETURN count(l) AS c",
    )
    .unwrap();
    assert_eq!(integers(&result, "c"), vec![2]);

    let labeled = exec_cypher(&graph, "MATCH (l:L:C) RETURN count(*) AS c");
    assert_eq!(integers(&labeled, "c"), vec![2]);
}

/// A unit conditional body under `IN TRANSACTIONS` still yields the outer
/// row per iteration, so every committed batch reports its status row and
/// the failing iteration reports its own, with the live error message
/// (CIP-198 pins `'/ by zero'`) — the committed rows used to disappear
/// with the body's empty result (gap 3's status variant).
#[test]
fn a_unit_conditional_call_in_transactions_reports_every_iteration() {
    let graph = Graph::default();
    let result = exec_autocommit(
        &graph,
        "UNWIND [1, 2, 3] AS i \
         CALL (i) { \
           WHEN i = 3 THEN { CREATE (:X {e: 1/0}) } \
           ELSE { MERGE (z:Z) SET z.i = i } \
         } IN TRANSACTIONS OF 1 ROW ON ERROR BREAK REPORT STATUS AS s \
         RETURN i, s.committed AS committed, s.errorMessage AS err",
    )
    .unwrap();
    assert_eq!(
        result.columns,
        vec!["i".to_string(), "committed".to_string(), "err".to_string()]
    );
    assert_eq!(
        result.rows,
        vec![
            vec![Value::Int(1), Value::Bool(true), Value::Null],
            vec![Value::Int(2), Value::Bool(true), Value::Null],
            vec![
                Value::Int(3),
                Value::Bool(false),
                Value::String("/ by zero".into())
            ],
        ]
    );

    // The committed iterations' writes stand (one `Z`, its `i` the second
    // iteration's); the failed iteration's node never landed.
    let z = exec_cypher(&graph, "MATCH (z:Z) RETURN count(*) AS c, z.i AS i");
    assert_eq!(integers(&z, "c"), vec![1]);
    assert_eq!(integers(&z, "i"), vec![2]);
    let x = exec_cypher(&graph, "MATCH (x:X) RETURN count(*) AS c");
    assert_eq!(integers(&x, "c"), vec![0]);
}
