//! The chain kernel's cost gate on hops the walk model cannot price
//! (LABS-991): an untyped rel, or a hop leaving an unlabeled node, rides
//! the any-label out-degree tail instead of passing the row estimate
//! through — so a graph with a high-degree node declines the chain to
//! the streaming walker (with the walker's own answer), while a graph
//! without one keeps the kernel.

use relate_core::Value;

use super::super::kernels::take_kernel_engagements;
use super::super::test_support::{build, exec_cypher, Built};
use super::differential;

/// A five-node `:N` chain (`0 -> 1 -> 2 -> 3 -> 4`, rel type `R`), plus —
/// when `hub_leaves > 0` — a separate hub node fanning out to that many
/// leaves. The chain is the only 4-hop path, so the answer is one row
/// either way; the hub only moves the any-label out-degree tail (a
/// hub of 64+ leaves puts four unlabeled hops past the gate's 2^20
/// stage ceiling).
fn chain_with_hub(hub_leaves: usize) -> Built {
    let total = 5 + usize::from(hub_leaves > 0) + hub_leaves;
    let labels: Vec<&[&str]> = (0..total).map(|_| &["N"][..]).collect();
    let mut rels: Vec<(usize, usize, &str)> = (0..4).map(|i| (i, i + 1, "R")).collect();
    rels.extend((0..hub_leaves).map(|leaf| (5, 6 + leaf, "R")));
    build(&labels, &rels)
}

/// Every spelling the walk model leaves unpriced: untyped, typed through
/// unlabeled interior nodes, and untyped through labeled ones. Spelled
/// with `LIMIT` — a plain `RETURN count` rides the count fold
/// (LABS-1293), which never lands the last stage and so prices one hop
/// fewer; the `LIMIT` keeps these on the rows kernel's gate.
const SHAPES: [&str; 3] = [
    "MATCH (s:N {id: 0})-->()-->()-->()-->(n:N) RETURN count(n) LIMIT 5",
    "MATCH (s:N {id: 0})-[:R]->()-[:R]->()-[:R]->()-[:R]->(n:N) RETURN count(n) LIMIT 5",
    "MATCH (s:N {id: 0})-->(:N)-->(:N)-->(:N)-->(n:N) RETURN count(n) LIMIT 5",
];

#[test]
fn a_high_degree_node_declines_the_unpriced_chain_to_the_walker() {
    let built = chain_with_hub(64);
    for cypher in SHAPES {
        // Engagement 0 is the differential fact here — the query ran the
        // walker's own path, so re-running it stood-down would compare
        // the walker with itself.
        let _ = take_kernel_engagements();
        let answer = exec_cypher(&built.graph, cypher).rows;
        assert_eq!(take_kernel_engagements(), 0, "declined: {cypher}");
        assert_eq!(answer, vec![vec![Value::Int(1)]], "{cypher}");
    }
}

#[test]
fn a_thin_graph_keeps_the_kernel_on_the_unpriced_chain() {
    // No node out-degree above 1: the any-label tail is 1, the bound
    // never grows, and the kernel stays engaged.
    let built = chain_with_hub(0);
    for cypher in SHAPES {
        let result = differential(&built.graph, cypher, true);
        assert_eq!(result.rows, vec![vec![Value::Int(1)]], "{cypher}");
    }
}
