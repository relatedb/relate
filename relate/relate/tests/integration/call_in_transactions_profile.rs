//! ADR-0681: the `CALL { … } IN TRANSACTIONS` observability tests — the
//! body's operators as the `CallSubquery` node's plan children (a plain
//! `CALL { … }` stays opaque) and PROFILE's batch facts on the operator
//! (the committed counters of a clean run, the rolled-back count of a
//! failed batch under `ON ERROR CONTINUE`). Split from
//! `call_in_transactions.rs` (its statement-semantics tests) so neither
//! file carries the whole in-transactions corpus across the 1 000-line
//! split line.

use crate::common;

use relate::Value;

use common::{connection, find_node, plan, scalar};

/// REL8-2047: the batched call's body renders as the `CallSubquery`
/// operator's plan children — what each batch runs, visible in EXPLAIN
/// before any row runs — behind the pipeline's data-flow child, while a
/// plain `CALL { … }` keeps the opaque node (its body is not a batch the
/// plan names). The pre-order id rule holds across the new subtree: the
/// operator precedes every body child.
#[test]
fn explain_renders_the_batched_body_as_plan_children() {
    let mut c = connection();
    let result = c
        .query(
            "EXPLAIN UNWIND range(1, 10) AS id \
             CALL (id) { CREATE (:BC {i: id}) } \
             IN TRANSACTIONS OF 3 ROWS",
        )
        .unwrap();
    let node = find_node(plan(&result), "CallSubquery").expect("the batched call");
    let operators: Vec<&str> = node
        .children
        .iter()
        .map(|child| child.operator.as_str())
        .collect();
    let body = node
        .children
        .iter()
        .find(|child| child.operator == "Create")
        .expect("the body's CREATE renders as a plan child");
    assert!(
        node.id < body.id,
        "the operator precedes its body children: {node:?}"
    );
    assert!(
        operators.first() == Some(&"Unwind"),
        "the pipeline's data-flow child stays first: {operators:?}"
    );
    assert_eq!(
        body.details, "(:BC)",
        "the body child carries its own details: {body:?}"
    );
    // REL8-2047's role marker: the body subtree names its `CallSubquery`
    // parent, so a walk tells it from the data-flow child — the body
    // operators run inside the batch, so they are not executed children
    // of the statement's own pipeline.
    assert_eq!(
        body.body_of(),
        Some(node.id),
        "the body child names its parent operator"
    );
    assert!(
        node.executed_children()
            .all(|child| child.operator != "Create"),
        "the body subtree is not an executed child"
    );

    // A plain `CALL { … }` stays opaque: no body operators among its
    // children.
    let result = c
        .query("EXPLAIN UNWIND range(1, 3) AS id CALL (id) { CREATE (:PC {i: id}) }")
        .unwrap();
    let node = find_node(plan(&result), "CallSubquery").expect("the plain call");
    assert!(
        node.children.iter().all(|child| child.operator != "Create"),
        "the plain call renders no body operators: {node:?}"
    );
}

/// REL8-2047: PROFILE reports the batch facts on the `CallSubquery`
/// operator — the counters and the per-batch peaks. A clean run commits
/// every batch and peaks above zero wherever the batch did work.
#[test]
fn profile_reports_the_batch_facts() {
    let mut c = connection();
    let result = c
        .query(
            "PROFILE UNWIND range(1, 9) AS id \
             CALL (id) { CREATE (:PBF {i: id}) } \
             IN TRANSACTIONS OF 3 ROWS",
        )
        .unwrap();
    let node = find_node(plan(&result), "CallSubquery").expect("the batched call");
    let metrics = node.metrics.as_ref().expect("PROFILE stamps the operator");
    assert_eq!(metrics.intx.batches, 3, "nine rows in batches of three");
    assert_eq!(metrics.intx.committed, 3);
    assert_eq!(metrics.intx.rolled_back, 0);
    assert!(metrics.intx.peak_input_rows_bytes > 0, "{metrics:?}");
    assert!(metrics.intx.peak_output_rows_bytes > 0, "{metrics:?}");
    assert!(metrics.intx.peak_scratch_bytes > 0, "{metrics:?}");
    assert!(metrics.intx.peak_wal_bytes > 0, "{metrics:?}");
    assert_eq!(
        scalar(&mut c, "MATCH (n:PBF) RETURN count(n)"),
        Value::Int(9),
        "the profiled run created every row"
    );
}

/// REL8-2047: a batch that fails under `ON ERROR CONTINUE` reports as
/// rolled back while the batches around it commit — the counters name the
/// per-batch failure the `REPORT STATUS` rows describe.
#[test]
fn profile_reports_rolled_back_batches() {
    let mut c = connection();
    let result = c
        .query(
            "PROFILE UNWIND range(1, 9) AS id \
             CALL (id) { CREATE (:RBF {i: id, boom: 1 / (5 - id)}) } \
             IN TRANSACTIONS OF 3 ROWS ON ERROR CONTINUE",
        )
        .unwrap();
    let node = find_node(plan(&result), "CallSubquery").expect("the batched call");
    let metrics = node.metrics.as_ref().expect("PROFILE stamps the operator");
    assert_eq!(metrics.intx.batches, 3);
    assert_eq!(metrics.intx.committed, 2, "the two clean batches commit");
    assert_eq!(metrics.intx.rolled_back, 1, "the id=5 batch rolls back");
    // The rolled-back batch's rows never landed; the committed ones did.
    assert_eq!(
        scalar(&mut c, "MATCH (n:RBF) RETURN count(n)"),
        Value::Int(6)
    );
}
