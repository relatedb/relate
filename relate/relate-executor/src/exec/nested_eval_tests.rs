//! ADR-0395: the per-statement nested-evaluation budget. A nested
//! clause-engine part pass — a `CALL { … }` body part per imported row
//! (or one shared execution per part of an import-invariant body), a
//! conditional's taken branch bodies, an EXISTS/COUNT/COLLECT expression
//! subquery's body, a FOREACH clause list per list item, a NEXT stage —
//! is one tick on the statement's single counter (owned by the execution
//! view; every nested entry ticks it, so no nested context can reset
//! it). A UNION body ticks once per part, so a wide body driven by many
//! rows spends rows × parts on the budget and crosses it instead of
//! hiding its width inside one invocation tick. A statement whose
//! passes exceed the fixed 1 000 000 budget fails with the typed
//! `NestedEvaluationBudgetExceeded` execution error instead of running
//! the exponentially compounding fan.
//!
//! The bomb tests drive each counting site past the budget: each
//! configures its store with `BOMB_BUDGET` and fails one past it,
//! before its full theoretical fan runs — whatever the published
//! constant (ADR-0670) says, the crossings stay cheap. Every bomb
//! asserts through the shared
//! [`assert_nested_eval_budget_error`](relate_test_support::nested_eval::assert_nested_eval_budget_error)
//! helper over `error_of`'s extraction, so the enforced value and the
//! asserted text read the same number.
//! The counting-rule test pins each event's exact tick count — and, by
//! running every case on one reused view, that the counter resets per
//! statement and compounds only within one.

use std::collections::BTreeMap;

use relate_core::Value;
use relate_test_support::nested_eval::assert_nested_eval_budget_error;

use super::kernels::{take_kernel_engagements, with_kernels_disabled};
use super::test_support::{
    error_of, exec_write_unvalidated, try_exec_cypher, try_exec_cypher_on, write_scratch,
};
use crate::graph::Graph;
use crate::overlay::ExecGraph;

/// The bomb budget: small enough that five crossings cost nothing,
/// large enough that the drivers' shapes (rows x parts, list items)
/// still mean something.
const BOMB_BUDGET: u64 = 1_000;

/// The driver rows that spend one pass past the bomb budget, given the
/// statement's per-row tick factor (1 for a single body, 2 for a
/// two-part UNION or a two-subquery projection) — the one derivation
/// every crossing driver reads.
fn rows_past_the_budget(ticks_per_row: u64) -> u64 {
    BOMB_BUDGET / ticks_per_row + 1
}

/// The doubling nested-CALL bomb (the LABS-416 shape): each level's body
/// `UNWIND`s a two-element list feeding the next level's naked
/// `CALL { … }`, so every body above the leaf contains a nested CALL —
/// the import-invariant shared fast path never applies to them — and the
/// body passes double per level: 2^(depth+1) - 2 per-row body passes
/// plus 2^depth shared executions of the RETURN-only leaf clause,
/// 196 606 at depth 16.
fn nested_call_bomb(depth: usize) -> String {
    let mut query = String::from("UNWIND [0, 1] AS v\n");
    for k in 1..=depth {
        query.push_str(&format!("CALL {{\nUNWIND [0, 1] AS a{k}\n"));
    }
    query.push_str("CALL {\nRETURN 1 AS leaf\n}\n");
    for _ in 0..depth {
        query.push_str("}\n");
    }
    query.push_str("RETURN count(*) AS c");
    query
}

/// A store with four `L0` and four `L1` nodes and two `(:L0)-[:R]->(:L1)`
/// relationships for the expression-subquery bombs.
fn label_graph() -> Graph {
    let g = Graph::default();
    g.begin_write().unwrap();
    let mut l0 = Vec::new();
    let mut l1 = Vec::new();
    for _ in 0..4 {
        l0.push(g.create_node(vec!["L0".into()], BTreeMap::new()).unwrap());
        l1.push(g.create_node(vec!["L1".into()], BTreeMap::new()).unwrap());
    }
    for k in 0..2 {
        g.create_rel(l0[k], l1[k], "R".into(), BTreeMap::new())
            .unwrap();
    }
    g.commit_write();
    g
}

/// The EXISTS-chain bomb: every level carries a node-pattern `WHERE`, so
/// no level is probe-eligible and each runs the clause engine — one pass
/// per part per candidate row. Each level binds its own variable (`a1`,
/// `a2`, …), so an inner `MATCH` never sees its variable pre-bound from
/// the outer row: every level's label scan runs fresh over the four
/// candidates and the passes compound 4× per level — (4^(d+1) - 1) / 3 at
/// depth d, 349 525 at depth 9.
fn exists_chain_bomb(depth: usize) -> String {
    let mut body = "MATCH (z:Z)".to_string();
    for k in (1..=depth).rev() {
        let label = if k % 2 == 1 { "L1" } else { "L0" };
        body = format!("MATCH (a{k}:{label} WHERE EXISTS {{ {body} }})");
    }
    format!("RETURN EXISTS {{ {body} }} AS bomb")
}

/// The nested-CALL counting site: one `CALL (i) { … }` body pass per
/// imported row — under the seam's budget, a driver one row past it
/// crosses at the next pass with the typed kind and the exact message.
/// The doubling fan form (`nested_call_bomb`, driven by the
/// counting-rule test below) pins the compounding-per-level counts
/// exactly; past half a million passes its retained row product meets
/// the ADR-0463 expression cap first — the two guards are coupled
/// through the retained rows — so the budget's own crossing is shown by
/// this flat read-only form: a `RETURN`-body `CALL` per row, ticking per
/// row while retaining no graph writes.
#[test]
fn per_row_call_body_exceeds_the_budget() {
    let mut g = Graph::default();
    g.set_nested_eval_budget(BOMB_BUDGET);
    assert_nested_eval_budget_error(
        &error_of(try_exec_cypher(
            &g,
            &format!(
                "UNWIND range(1, {}) AS i CALL (i) {{ RETURN i AS x }} RETURN count(*) AS c",
                rows_past_the_budget(1)
            ),
        )),
        BOMB_BUDGET,
    )
}

/// The EXISTS clause-engine counting site (`subquery_eval`): the
/// predicate-bearing chain never takes the probes, and its passes cross
/// the budget with the same typed error.
#[test]
fn exists_chain_bomb_exceeds_the_budget() {
    let mut g = label_graph();
    g.set_nested_eval_budget(BOMB_BUDGET);
    // Four candidates per level: depth 5's 1 + 4 + ... + 4^5 = 1 365
    // passes cross a thousand.
    assert_nested_eval_budget_error(
        &error_of(try_exec_cypher(&g, &exists_chain_bomb(5))),
        BOMB_BUDGET,
    )
}

/// The wide-UNION counting rule (the granularity the security review
/// pinned): a body's UNION parts each tick, so a two-part body driven
/// by half-the-budget-plus-one rows spends one-past-budget part passes
/// on the counter and crosses — the rows × parts product lands on the
/// budget, not just the row axis.
#[test]
fn wide_union_body_driven_by_rows_exceeds_the_budget() {
    let mut g = Graph::default();
    g.set_nested_eval_budget(BOMB_BUDGET);
    assert_nested_eval_budget_error(
        &error_of(try_exec_cypher(
            &g,
            &format!(
                "UNWIND range(1, {}) AS i \
             CALL (i) {{ RETURN i AS x UNION ALL RETURN i + 1 AS x }} \
             RETURN count(*) AS c",
                rows_past_the_budget(2)
            ),
        )),
        BOMB_BUDGET,
    )
}

/// The omitted-RETURN COUNT counting site (`subquery_eval`): a COUNT
/// body with no RETURN runs the clause engine directly, once per
/// subquery per row — two per row, so a half-the-budget-plus-one-row
/// driver crosses.
#[test]
fn omitted_return_count_bomb_exceeds_the_budget() {
    let mut g = label_graph();
    g.set_nested_eval_budget(BOMB_BUDGET);
    assert_nested_eval_budget_error(
        &error_of(try_exec_cypher(
            &g,
            &format!(
                "UNWIND range(1, {}) AS i \
             RETURN COUNT {{ MATCH (n:L0) }} + COUNT {{ MATCH (n:L1) }} AS x",
                rows_past_the_budget(2)
            ),
        )),
        BOMB_BUDGET,
    )
}

/// The FOREACH counting site (`dispatch`): the clause list runs once per
/// list item per row, so a one-past-budget-item list crosses before its
/// last pass.
#[test]
fn foreach_bomb_exceeds_the_budget() {
    let mut g = Graph::default();
    g.set_nested_eval_budget(BOMB_BUDGET);
    let scratch = write_scratch(&g);
    assert_nested_eval_budget_error(
        &error_of(exec_write_unvalidated(
            &g,
            &scratch,
            &format!(
                "FOREACH (i IN range(1, {}) | CREATE ())",
                rows_past_the_budget(1)
            ),
        )),
        BOMB_BUDGET,
    )
}

/// The counting rule (ADR-0395), pinned per event on one reused view: each
/// nested statement-engine pass ticks exactly once, the counter compounds
/// across nesting levels within a statement, each statement starts from
/// zero (the counts stay exact in sequence on the same view), and the
/// pattern-probe fast path answers without ticking.
#[test]
fn the_counting_rule_ticks_each_event_once() {
    let g = label_graph();
    let view = ExecGraph::read(&g);
    let cases: &[(&str, u64)] = &[
        // A pattern-probe shape (one plain 2-node-1-rel MATCH from the
        // row-bound endpoint `a` to an anonymous endpoint, no WHERE)
        // answers through the LABS-193 anchored probe: zero engine
        // passes, zero ticks.
        (
            "MATCH (a:L0) RETURN EXISTS { MATCH (a)-[:R]->() } AS b",
            0,
        ),
        // An import-invariant CALL body shares one execution across its
        // two driving rows: one tick for the clause.
        (
            "UNWIND [0, 1] AS v CALL { RETURN 1 AS leaf } RETURN count(*) AS c",
            1,
        ),
        // A body computing from its import runs per imported row.
        (
            "UNWIND [0, 1] AS v CALL (v) { RETURN v AS out } RETURN count(*) AS c",
            2,
        ),
        // A UNION body ticks once per part per row: the wide-body
        // granularity (the rows × parts product lands on the budget,
        // not just the row axis).
        (
            "UNWIND [0, 1] AS v CALL (v) { RETURN v AS x \
             UNION ALL RETURN v + 1 AS x } RETURN count(*) AS c",
            4,
        ),
        // A conditional's taken branch body is one nested pass.
        ("WHEN true THEN { RETURN 1 AS x } ELSE { RETURN 2 AS x }", 1),
        // A nested body plus its NEXT-stage recursion: two passes.
        (
            "CALL { RETURN 1 AS a NEXT RETURN a + 1 AS b } RETURN b",
            2,
        ),
        // The COLLECT expression-subquery engine plus its NEXT stage.
        (
            "RETURN COLLECT { RETURN 1 AS a NEXT RETURN a + 1 AS b } AS c",
            2,
        ),
        // The EXISTS clause-engine path compounds across nesting levels:
        // 1 + 4 + 16 + 64 = 85 passes at depth 3 (four candidates per
        // level), one tick per part per row — the shared counter never
        // resets at a nested context.
        ("RETURN EXISTS { MATCH (a1:L1 WHERE EXISTS { MATCH (a2:L0 WHERE EXISTS { MATCH (a3:L1 WHERE EXISTS { MATCH (z:Z) }) }) }) } AS b", 85),
    ];
    for (query, expected) in cases {
        try_exec_cypher_on(&view, query).unwrap();
        assert_eq!(view.nested_evals().count(), *expected, "{query}");
    }
    // The nested-CALL doubling bomb below the budget, both CALL events of
    // the counting rule: the per-row bodies (a nested CALL in every one,
    // so the import-invariant fast path never applies) double per level —
    // 2^(depth+1) - 2 = 16 382 passes at depth 13 — plus one shared
    // execution per import-invariant leaf clause per body pass — 2^depth
    // = 8 192 more: 24 574 on the one statement's counter.
    try_exec_cypher_on(&view, &nested_call_bomb(13)).unwrap();
    assert_eq!(view.nested_evals().count(), 24_574);
    // The FOREACH arm: one pass per list item per row, on the write view.
    let scratch = write_scratch(&g);
    let wview = ExecGraph::write(&g, &scratch);
    try_exec_cypher_on(&wview, "FOREACH (i IN range(1, 5) | CREATE ())").unwrap();
    assert_eq!(wview.nested_evals().count(), 5);
    // `CALL { … } IN TRANSACTIONS` (ADR-0669): an admissible body —
    // reads included, the body runs in pipeline order over the batch —
    // costs one pass per batch, 10 rows at OF 3 ROWS being four batches;
    // a `WITH`-carrying body keeps the per-row path's one pass per row
    // (10 here).
    try_exec_cypher_on(
        &wview,
        "UNWIND range(1, 10) AS id CALL (id) { CREATE (:P {i: id}) } \
         IN TRANSACTIONS OF 3 ROWS",
    )
    .unwrap();
    assert_eq!(wview.nested_evals().count(), 4);
    try_exec_cypher_on(
        &wview,
        "UNWIND range(1, 10) AS id \
         CALL (id) { CREATE (:Q {i: id}) MATCH (x:Q) DELETE x } \
         IN TRANSACTIONS OF 3 ROWS",
    )
    .unwrap();
    assert_eq!(wview.nested_evals().count(), 4);
    try_exec_cypher_on(
        &wview,
        "UNWIND range(1, 10) AS id \
         CALL (id) { WITH id CREATE (:W {i: id}) } \
         IN TRANSACTIONS OF 3 ROWS",
    )
    .unwrap();
    assert_eq!(wview.nested_evals().count(), 10);
}

/// A chain corridor for the kernel arm's EXISTS shape: two
/// `(:A)-[:R]->(:B)-[:S]->(:C)` trails — the labeled 2-hop chain the
/// ADR-0473 chain kernel engages on (the families suite's corridor).
fn corridor_graph() -> Graph {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a0 = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let a1 = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b0 = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let b1 = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let c0 = g.create_node(vec!["C".into()], BTreeMap::new()).unwrap();
    let c1 = g.create_node(vec!["C".into()], BTreeMap::new()).unwrap();
    g.create_rel(a0, b0, "R".into(), BTreeMap::new()).unwrap();
    g.create_rel(a1, b1, "R".into(), BTreeMap::new()).unwrap();
    g.create_rel(b0, c0, "S".into(), BTreeMap::new()).unwrap();
    g.create_rel(b1, c1, "S".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    g
}

/// The kernel arm's accounting symmetry (ADR-0395 over ADR-0473): a
/// kernel-served EXISTS full-pattern walk is the same nested evaluation
/// the seek arm serves, so it advances the budget exactly once — no arm
/// of `patterns_exist` is the evaluation route that escapes the counter.
/// The kernels-disabled run pins the other arm on the same shape: one
/// evaluation, one tick, either arm.
#[test]
fn kernel_served_exists_walk_ticks_the_budget_once() {
    let g = corridor_graph();
    let view = ExecGraph::read(&g);
    let query = "RETURN EXISTS { MATCH (:A)-[:R]->(:B)-[:S]->(:C) } AS hit";
    let kernaled = try_exec_cypher_on(&view, query).unwrap();
    // The kernel took the walk (not the seek): one engagement, one tick.
    assert_eq!(take_kernel_engagements(), 1);
    assert_eq!(kernaled.rows, vec![vec![Value::Bool(true)]]);
    assert_eq!(view.nested_evals().count(), 1);
    // The same shape on the seek arm (kernels forced off): the counter
    // restarts per statement, so the same single tick.
    let probed = with_kernels_disabled(|| try_exec_cypher_on(&view, query).unwrap());
    assert_eq!(take_kernel_engagements(), 0);
    assert_eq!(probed.rows, vec![vec![Value::Bool(true)]]);
    assert_eq!(view.nested_evals().count(), 1);
}

/// The local-callable counting site (LABS-416's residue): a procedure
/// body is a nested statement-engine pass, once per calling row. Under
/// the seam's budget a flat driver crosses it — the cheapest shape that
/// pins the site's crossing; the relate-crate twin this replaces drove
/// the real million-pass budget at a per-pass cost no suite should pay
/// on every run.
#[test]
fn local_callable_bomb_exceeds_the_budget() {
    let mut g = Graph::default();
    g.set_nested_eval_budget(BOMB_BUDGET);
    assert_nested_eval_budget_error(
        &error_of(try_exec_cypher(
            &g,
            &format!(
                "DEFINE PROCEDURE p1() {{ RETURN 1 AS v }} \
                 UNWIND range(1, {}) AS i CALL p1() YIELD v RETURN count(v) AS c",
                rows_past_the_budget(1)
            ),
        )),
        BOMB_BUDGET,
    );
}
