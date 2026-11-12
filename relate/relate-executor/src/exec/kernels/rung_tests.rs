//! LABS-1117: the PROFILE record names the traversal rung that served a
//! `MATCH` clause — the fact EXPLAIN cannot show, because the rungs are
//! runtime choices inside an operator, not plan nodes.
//!
//! Two distinct shapes pin the record as the *actual* rung, never a
//! constant: a 3-hop fixed chain (the chain kernel, with its hop count)
//! and an unbounded quantifier (the frontier-NFA rung). A record that
//! always returned one value fails one of the two.

use relate_core::Value;

use crate::ctx::KernelRung;

use super::super::test_support::{profiled, served, Builder};
use super::frontier_rider_tests::ring_graph;

/// A 3-hop fixed chain in the chain kernel's closed vocabulary (typed
/// hops, static label conjunctions, one terminal predicate) — the
/// corridor's longer sibling, so the recorded hop count is the shape's
/// own three, not a constant.
fn three_hop_corridor() -> crate::graph::Graph {
    let b = Builder::new();
    let a1 = b.prop_node("a1", &["Alpha"], &[("k", Value::Int(1))]);
    let a2 = b.prop_node("a2", &["Alpha"], &[("k", Value::Int(2))]);
    let b1 = b.node("b1", &["Beta"]);
    let b2 = b.node("b2", &["Beta"]);
    let g1 = b.node("g1", &["Gamma"]);
    let g2 = b.node("g2", &["Gamma"]);
    let d1 = b.prop_node("d1", &["Delta"], &[("name", Value::String("one".into()))]);
    let d2 = b.prop_node("d2", &["Delta"], &[("name", Value::String("two".into()))]);
    b.rel(a1, b1, "R", &[]);
    b.rel(a2, b2, "R", &[]);
    b.rel(b1, g1, "S", &[]);
    b.rel(b2, g2, "S", &[]);
    b.rel(g1, d1, "T", &[]);
    b.rel(g2, d2, "T", &[]);
    b.done()
}

/// The unrolled fixed-k chain engages on a 3-hop typed chain, and the
/// record names it with its hop count — not the walker, not the frontier.
#[test]
fn profile_names_the_chain_kernel_for_a_fixed_chain() {
    let graph = three_hop_corridor();
    let records = profiled(&graph, relate_test_support::plan_rung::THREE_HOP_MATCH);
    assert_eq!(
        served(&records).kernel,
        Some(KernelRung::Chain { hops: 3 }),
        "the fixed 3-hop chain runs the chain kernel"
    );
}

/// The frontier-NFA rung engages on an unbounded quantifier whose answer
/// is the distinct endpoint set, and the record names it — never the
/// chain, never the walker.
#[test]
fn profile_names_the_frontier_rung_for_a_varlen_pattern() {
    let graph = ring_graph();
    let records = profiled(
        &graph,
        "MATCH (x:Geo {id:'n0'})-[:LINK*]-(y:Geo) RETURN count(DISTINCT y.id) AS c",
    );
    assert_eq!(
        served(&records).kernel,
        Some(KernelRung::FrontierNfa),
        "the unbounded distinct-endpoint shape runs the frontier rung"
    );
}

/// The walker is a recorded rung too, not an absent fact: a `MATCH` the
/// traversal gates decline (a pattern outside the kernels' closed
/// vocabulary) reports `Walker`, and no other clause of the statement
/// reports a rung at all.
#[test]
fn profile_names_the_walker_for_a_declined_pattern() {
    let built = super::super::test_support::build(
        &[&["N"], &["N"], &["N"], &["N"]],
        &[(0, 1, "E"), (1, 2, "E"), (2, 3, "E")],
    );
    // An untyped rel keeps the recursive walker (the matcher's static
    // type-list proof), so no kernel engages.
    let records = profiled(&built.graph, "MATCH (a:N)-->(b:N) RETURN b");
    let served = served(&records);
    assert_eq!(served.kernel, Some(KernelRung::Walker));
    assert_eq!(
        records
            .iter()
            .filter(|record| record.kernel.is_some())
            .count(),
        1,
        "only the MATCH clause carries a rung"
    );
}

/// A nested evaluation's rung never lands on the enclosing clause
/// (LABS-1117): a `CALL { … }` subquery and a `RETURN EXISTS { … }` have
/// no `MATCH` ladder of their own, so their records carry no rung even
/// when the inner `MATCH` runs the chain kernel. The rung is the
/// ladder's return value, not a thread-local the nesting could inherit.
#[test]
fn a_nested_match_rung_does_not_leak_onto_the_enclosing_clause() {
    let graph = three_hop_corridor();
    let chain = "MATCH (a:Alpha)-[:R]->(b:Beta)-[:S]->(g:Gamma)-[:T]->(d:Delta)";
    for cypher in [
        format!(
            "CALL {{ {chain} WHERE a.k >= 1 AND d.name CONTAINS 'o' \
             RETURN a.k AS ak, d.name AS dn LIMIT 99 }} RETURN ak LIMIT 10"
        ),
        format!("RETURN EXISTS {{ {chain} }} AS hit"),
    ] {
        let records = profiled(&graph, &cypher);
        let rungs: Vec<_> = records.iter().map(|record| record.kernel).collect();
        assert!(
            rungs.iter().all(Option::is_none),
            "no clause reports the nested MATCH's rung for {cypher}: {rungs:?}"
        );
    }
}

/// A 40-node `R` chain: every node has one successor, so a `*1..2`
/// recursion from every node has two paths per interior endpoint pool.
fn r_chain() -> crate::graph::Graph {
    let b = Builder::new();
    let n: Vec<_> = (0..40)
        .map(|i| b.node(&format!("c{i}"), &["Chain"]))
        .collect();
    for i in 0..39 {
        b.rel(n[i], n[i + 1], "R", &[]);
    }
    b.done()
}

/// The rung and the single-column integer answer of one statement.
fn rung_and_count(graph: &crate::graph::Graph, cypher: &str) -> (Option<KernelRung>, Value) {
    let records = profiled(graph, cypher);
    let rung = served(&records).kernel;
    let result = super::super::test_support::exec_cypher(graph, cypher);
    (rung, result.rows[0][0].clone())
}

/// A bounded recursion whose consumer needs only the distinct endpoints
/// is served by the frontier rung in auto mode (the cost gate prices an
/// unlabeled endpoint as the whole store, `frontier_engages`).
#[test]
fn count_distinct_end_over_a_bounded_path_takes_the_frontier() {
    let (rung, count) = rung_and_count(
        &r_chain(),
        "MATCH (h)-[:R*1..2]->(e) RETURN count(DISTINCT e) AS c",
    );
    assert_eq!(rung, Some(KernelRung::FrontierNfa));
    assert_eq!(count, Value::Int(39));
}

/// Shapes that need the paths themselves decline the frontier rung and
/// the walker answers: same counts, never `FrontierNfa`.
#[test]
fn path_needing_shapes_stay_on_the_walker() {
    let graph = r_chain();
    for (cypher, expected) in [
        (
            "MATCH p = (h)-[:R*1..2]->(e) RETURN count(DISTINCT p) AS c",
            77,
        ),
        ("MATCH (h)-[:R*1..2]->(e) RETURN count(*) AS c", 77),
        (
            "MATCH (h)-[r:R*1..2]->(e) RETURN count(DISTINCT r) AS c",
            77,
        ),
        (
            "MATCH p = (h)-[:R*1..2]->(e) WHERE length(p) = 2 \
             RETURN count(DISTINCT e) AS c",
            38,
        ),
    ] {
        let (rung, count) = rung_and_count(&graph, cypher);
        assert_eq!(rung, Some(KernelRung::Walker), "{cypher}");
        assert_eq!(count, Value::Int(expected), "{cypher}");
    }
}
