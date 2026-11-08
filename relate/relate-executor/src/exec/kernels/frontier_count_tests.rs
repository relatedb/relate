//! The walk-count arm's suite: the contraction's answers on hand-built
//! multi-path graphs (closed-form counts), the crash pin (a branching
//! graph whose walk count the DFS would enumerate one row at a time),
//! the generated-multigraph differential against the stood-down DFS,
//! and one decline per equivalence guard — each decline answering the
//! DFS's own rows with no kernel engaged.

use proptest::prelude::*;
use relate_core::Value;

use crate::exec::test_support::{build, exec_cypher, Built};
use crate::graph::Graph;
use crate::test_support::config_with_floor;

use super::super::take_frontier_counters;
use super::rearival_tests::{multigraph, multigraph_case, pattern_body};
use super::{take_kernel_engagements, with_frontier_disabled};

/// A layered graph: `layers + 1` layers of `width` nodes, every node of
/// one layer linked `R` to every node of the next — `width^k` walks of
/// length `k` from any layer-0 seed, every one a distinct path to one
/// of only `width` endpoints per layer.
fn layered(layers: usize, width: usize) -> Built {
    let labels: Vec<&[&str]> = (0..(layers + 1) * width).map(|_| &["N"][..]).collect();
    let mut rels = Vec::new();
    for layer in 0..layers {
        for a in 0..width {
            for b in 0..width {
                rels.push((layer * width + a, (layer + 1) * width + b, "R"));
            }
        }
    }
    build(&labels, &rels)
}

/// The single answer cell of a one-row, one-column result.
fn scalar(g: &Graph, cypher: &str) -> Value {
    let result = exec_cypher(g, cypher);
    assert_eq!(result.rows.len(), 1, "{cypher}: one row");
    result.rows[0][0].clone()
}

/// The arm engaged for `cypher`: the answer, the one engagement, and the
/// frontier counters' engagement marker.
fn engaged(g: &Graph, cypher: &str) -> Value {
    let _ = take_kernel_engagements();
    let _ = take_frontier_counters();
    let answer = scalar(g, cypher);
    assert_eq!(take_kernel_engagements(), 1, "the walk count took {cypher}");
    assert!(
        take_frontier_counters().levels > 0,
        "the walk count ran for {cypher}"
    );
    answer
}

/// The engaged answer equals the stood-down DFS's — rows and columns;
/// returns the engaged run's kernel engagements.
fn differential(g: &Graph, cypher: &str) -> u32 {
    let _ = take_kernel_engagements();
    let engaged = exec_cypher(g, cypher);
    let engagements = take_kernel_engagements();
    let _ = take_frontier_counters();
    let dfs = with_frontier_disabled(|| exec_cypher(g, cypher));
    assert_eq!(
        take_kernel_engagements(),
        0,
        "the stand-down holds: {cypher}"
    );
    assert_eq!(engaged.columns, dfs.columns, "{cypher}");
    assert_eq!(engaged.rows, dfs.rows, "{cypher}");
    engagements
}

/// A declined shape: no kernel engaged, and the rows are the DFS's own.
fn declines(g: &Graph, cypher: &str) {
    let _ = take_kernel_engagements();
    let _ = take_frontier_counters();
    let rows = exec_cypher(g, cypher);
    assert_eq!(take_kernel_engagements(), 0, "nothing takes {cypher}");
    assert_eq!(
        take_frontier_counters().levels,
        0,
        "the frontier never ran for {cypher}"
    );
    let dfs = with_frontier_disabled(|| exec_cypher(g, cypher));
    assert_eq!(
        rows.rows, dfs.rows,
        "the decline is the DFS's rows: {cypher}"
    );
}

// The contraction's count is the walk count: on the 2-wide layered
// graph, 2 + 4 + 8 walks of length 1..3 from one seed, though only 6
// distinct endpoints exist — and each spelling of the count (the
// star, the target, the seed) is the same walk count, matching the DFS.
#[test]
fn counts_walks_not_endpoints_on_a_multi_path_graph() {
    let built = layered(3, 2);
    let g = &built.graph;
    for counted in ["count(*)", "count(d)", "count(s)"] {
        let cypher = format!(
            "MATCH REPEATABLE ELEMENTS (s:N {{id: 0}})-[:R*1..3]->(d) RETURN {counted} AS n"
        );
        assert_eq!(engaged(g, &cypher), Value::Int(14), "{cypher}");
        differential(g, &cypher);
    }
    // The zero-length walk counts once per seed; the lower bound drops
    // the short walks; the reversed spelling walks back from layer 3.
    for (cypher, walks) in [
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*0..2]->(d) RETURN count(*) AS n",
            7,
        ),
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*2..3]->(d) RETURN count(*) AS n",
            12,
        ),
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 6})<-[:R*1..3]-(d) RETURN count(*) AS n",
            14,
        ),
        // Every layer-0 node seeds (no property map): twice the walks.
        (
            "MATCH REPEATABLE ELEMENTS (s:N)-[:R*3..3]->(d) RETURN count(*) AS n",
            16,
        ),
    ] {
        assert_eq!(engaged(g, cypher), Value::Int(walks), "{cypher}");
        differential(g, cypher);
    }
}

// The crash pin: 12 layers of 4, fully linked — Σ 4^k for k in 1..=12 =
// 22,369,620 walks the DFS would emit one row each; the contraction
// touches 4 cells per level and answers in milliseconds.
#[test]
fn branching_twelve_level_count_answers_without_enumeration() {
    let built = layered(12, 4);
    let started = std::time::Instant::now();
    let answer = engaged(
        &built.graph,
        "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..12]->(d) RETURN count(d) AS walks",
    );
    assert_eq!(answer, Value::Int(22_369_620));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the contraction never enumerates: {:?}",
        started.elapsed()
    );
}

// Every equivalence guard declines to the unchanged plan, answers the
// DFS's own: the default pool (trail rel-uniqueness does not contract),
// a filtered target (label, property map, MATCH WHERE, inline WHERE), a
// DISTINCT count (the endpoint set's arm, not this one), a second item,
// grouping, paging, a WITH between, and a non-count aggregate.
#[test]
fn every_guard_declines_to_the_dfs() {
    let built = layered(3, 2);
    let g = &built.graph;
    for cypher in [
        "MATCH (s:N {id: 0})-[:R*1..3]->(d) RETURN count(*) AS n",
        "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d:N) RETURN count(*) AS n",
        "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d {id: 5}) RETURN count(*) AS n",
        "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d) WHERE d.id > 3 \
         RETURN count(*) AS n",
        "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d WHERE d.id > 3) \
         RETURN count(*) AS n",
        "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d) RETURN count(DISTINCT d) AS n",
        "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d) RETURN sum(d.id) AS n",
        "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d) RETURN count(d.id) AS n",
        "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d) RETURN s.id AS s, count(*) AS n",
        "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d) RETURN count(*) AS n SKIP 1",
        "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d) WITH d RETURN count(*) AS n",
    ] {
        if cypher.contains("count(DISTINCT d)") {
            // The endpoint-set arm may take this one on the row rung;
            // the walk count must not — the DFS's answer either way.
            differential(g, cypher);
            continue;
        }
        declines(g, cypher);
    }
}

// The generated differential: the rearival suite's multigraphs (parallel
// rels, self-loops, mixed and undirected directions, both types) under
// the walk pool, every vocabulary pattern with its target unfiltered —
// the walk count equals the DFS's row count exactly, and the default
// pool's spelling declines. (A plain comment: rustdoc does not document
// macro invocations.)
proptest! {
    #![proptest_config(config_with_floor(256))]
    #[test]
    fn walk_count_matches_the_dfs_on_generated_multigraphs(
        (n, edges, seed, pattern) in multigraph_case(),
    ) {
        let body = pattern_body(pattern, seed).replace("(y:N)", "(y)");
        let built = multigraph(n, &edges);
        let g = &built.graph;
        let walk = format!("MATCH REPEATABLE ELEMENTS {body} RETURN count(*) AS n");
        prop_assert_eq!(differential(g, &walk), 1, "the walk count takes {}", walk);
        declines(g, &format!("MATCH {body} RETURN count(*) AS n"));
    }
}
