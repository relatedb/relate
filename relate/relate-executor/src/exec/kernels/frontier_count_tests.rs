//! The walk-count arm's suite: the contraction's answers on hand-built
//! multi-path graphs (closed-form counts), the crash pin (a branching
//! graph whose walk count the DFS would enumerate one row at a time),
//! the generated-multigraph differential against the stood-down DFS,
//! and one verdict per equivalence guard — each guard either declines
//! with no kernel engaged or only ties with the stood-down DFS's own
//! rows.

use proptest::prelude::*;
use relate_core::Value;

use crate::exec::test_support::{exec_cypher, layered, reset_frontier_facts};
use crate::graph::Graph;
use crate::test_support::config_with_floor;
use crate::QueryResult;

use super::super::take_frontier_counters;
use super::rearival_tests::{multigraph, multigraph_case, pattern_body};
use super::{take_kernel_engagements, with_frontier_disabled};

/// The single answer cell of a one-row, one-column result.
fn scalar(g: &Graph, cypher: &str) -> Value {
    let result = exec_cypher(g, cypher);
    assert_eq!(result.rows.len(), 1, "{cypher}: one row");
    result.rows[0][0].clone()
}

/// The arm engaged for `cypher`: the answer, the one engagement, and the
/// frontier counters' engagement marker.
fn engaged(g: &Graph, cypher: &str) -> Value {
    reset_frontier_facts();
    let answer = scalar(g, cypher);
    assert_eq!(take_kernel_engagements(), 1, "the walk count took {cypher}");
    assert!(
        take_frontier_counters().levels > 0,
        "the walk count ran for {cypher}"
    );
    answer
}

/// The answer ties with the stood-down DFS's: the same statement with the
/// frontier disabled answers the same columns and rows.
fn assert_matches_dfs(g: &Graph, cypher: &str, rows: &QueryResult) {
    let dfs = with_frontier_disabled(|| exec_cypher(g, cypher));
    assert_eq!(rows.columns, dfs.columns, "{cypher}");
    assert_eq!(rows.rows, dfs.rows, "{cypher}");
}

/// The engaged answer equals the stood-down DFS's — rows and columns;
/// returns the engaged run's kernel engagements.
fn differential(g: &Graph, cypher: &str) -> u32 {
    let _ = take_kernel_engagements();
    let engaged = exec_cypher(g, cypher);
    let engagements = take_kernel_engagements();
    let _ = take_frontier_counters();
    assert_matches_dfs(g, cypher, &engaged);
    assert_eq!(
        take_kernel_engagements(),
        0,
        "the stand-down holds: {cypher}"
    );
    engagements
}

/// How a guard row is pinned: the walk count must decline, or — the
/// endpoint-set arm may take the row on the row rung — the answer must
/// only tie with the stood-down DFS.
#[derive(Clone, Copy)]
enum Expect {
    Declines,
    TiesWithDfs,
}

/// The stand-down pin over an already-run statement: the frontier never
/// ran, and the answer is the stood-down DFS's own (columns and rows).
fn assert_stood_down(g: &Graph, cypher: &str, rows: &QueryResult) {
    assert_eq!(
        take_frontier_counters().levels,
        0,
        "no frontier run: {cypher}"
    );
    assert_matches_dfs(g, cypher, rows);
}

/// One measured run: the canonical reset, then the statement.
fn run(g: &Graph, cypher: &str) -> QueryResult {
    reset_frontier_facts();
    exec_cypher(g, cypher)
}

/// A shape the frontier stood down from: no frontier run, and the rows
/// are the DFS's own — whichever other kernel (or none) then serves the
/// clause.
fn stood_down(g: &Graph, cypher: &str) {
    let rows = run(g, cypher);
    assert_stood_down(g, cypher, &rows);
}

/// A declined shape: no kernel engaged, and the rows are the DFS's own.
fn declines(g: &Graph, cypher: &str) {
    let rows = run(g, cypher);
    assert_eq!(take_kernel_engagements(), 0, "nothing takes {cypher}");
    assert_stood_down(g, cypher, &rows);
}

// The contraction's count is the walk count: on the 2-wide layered
// graph, 2 + 4 + 8 walks of length 1..3 from one seed, though only 6
// distinct endpoints exist — and each spelling of the count (the
// star, the target, the seed) is the same walk count, matching the DFS.
#[test]
fn counts_walks_not_endpoints_on_a_multi_path_graph() {
    let g = &layered(3, 2);
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
    let g = layered(12, 4);
    let started = std::time::Instant::now();
    let answer = engaged(
        &g,
        "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..12]->(d) RETURN count(d) AS walks",
    );
    assert_eq!(answer, Value::Int(22_369_620));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the contraction never enumerates: {:?}",
        started.elapsed()
    );
}

// Every guard shape pins its verdict: the default pool (trail
// rel-uniqueness does not contract), a target predicate that reads
// another binding, a second item, grouping, paging, a WITH between and a
// non-count aggregate decline to the unchanged plan with the DFS's own
// answers; the DISTINCT count is only tied to the DFS's answer, since
// the endpoint set's arm may serve it.
#[test]
fn every_guard_shape_pins_its_verdict() {
    let g = &layered(3, 2);
    for (cypher, expect) in [
        (
            "MATCH (s:N {id: 0})-[:R*1..3]->(d) RETURN count(*) AS n",
            Expect::Declines,
        ),
        // A target predicate that is not local to the arrival node (it
        // reads the seed) is outside the closed vocabulary: it declines.
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d) WHERE d.id > s.id \
             RETURN count(*) AS n",
            Expect::Declines,
        ),
        // The endpoint-set arm may take this one on the row rung; the
        // walk count must not — the DFS's answer either way.
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d) \
             RETURN count(DISTINCT d) AS n",
            Expect::TiesWithDfs,
        ),
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d) RETURN sum(d.id) AS n",
            Expect::Declines,
        ),
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d) RETURN count(d.id) AS n",
            Expect::Declines,
        ),
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d) \
             RETURN s.id AS s, count(*) AS n",
            Expect::Declines,
        ),
        // An inline element WHERE is GPM: outside the count fold's shapes.
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d WHERE d.id > 3) \
             RETURN count(*) AS n",
            Expect::Declines,
        ),
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d) RETURN count(*) AS n SKIP 1",
            Expect::Declines,
        ),
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d) WITH d RETURN count(*) AS n",
            Expect::Declines,
        ),
    ] {
        match expect {
            Expect::Declines => declines(g, cypher),
            Expect::TiesWithDfs => {
                differential(g, cypher);
            }
        }
    }
}

// A filter on the target depends only on the arrival node, so the walk
// count stays exact under it: the last leg counts the admitted
// out-neighbours per reached pair. Every spelling of the filter (label,
// property map, MATCH WHERE) engages the arm and matches the DFS; the
// inline element WHERE is GPM and declines (pinned in the guard table).
// Closed forms on
// `layered(3, 2)` from id 0: 2, 4 and 8 walks reach layers 1, 2 and 3
// (ids 2-3, 4-5, 6-7), 14 in all, 4 per layer-3 node. The zero-length
// arrival is the seed itself: it counts only when it passes the filter.
#[test]
fn a_target_filter_counts_the_admitted_arrivals() {
    let g = &layered(3, 2);
    for (query, walks) in [
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d:N) RETURN count(*) AS n",
            14,
        ),
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d:Missing) RETURN count(*) AS n",
            0,
        ),
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d {id: 6}) RETURN count(*) AS n",
            4,
        ),
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d) WHERE d.id > 3 \
             RETURN count(*) AS n",
            12,
        ),
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..3]->(d) WHERE d.id IS NULL \
             RETURN count(*) AS n",
            0,
        ),
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*0..0]->(d {id: 0}) RETURN count(*) AS n",
            1,
        ),
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*0..0]->(d {id: 1}) RETURN count(*) AS n",
            0,
        ),
    ] {
        assert_eq!(engaged(g, query), Value::Int(walks), "{query}");
        differential(g, query);
    }
}

// The generated differential: the rearival suite's multigraphs (parallel
// rels, self-loops, mixed and undirected directions, both types) under
// the walk pool, every vocabulary pattern with its target unfiltered —
// the filtered target has its own test above — the walk count equals the
// DFS's row count exactly, and the default pool's spelling never takes
// it: the frontier stands down and another kernel (or none) serves the
// clause. (A plain comment: rustdoc does not document macro invocations.)
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
        // The default pool's spelling never takes the walk count; an
        // exact-`*k` undirected hop rides the chain count instead
        // (LABS-1384), so the pin is the DFS's answer, not a decline.
        let default = format!("MATCH {body} RETURN count(*) AS n");
        stood_down(g, &default);
    }

    // The same multigraphs with a property filter on the target: the
    // admitted-arrival count equals the DFS's row count exactly (the
    // generator's graph has no filter-sensitive structure beyond the
    // ids, so the filter selects one node per case).
    #[test]
    fn filtered_walk_count_matches_the_dfs_on_generated_multigraphs(
        (n, edges, seed, pattern) in multigraph_case(),
    ) {
        let target = (seed + 1) % n;
        let filtered = format!("(y:N {{id: {target}}})");
        let body = pattern_body(pattern, seed).replace("(y:N)", &filtered);
        prop_assert!(
            body.contains(&filtered),
            "the target filter lands in the pattern: {body}"
        );
        let built = multigraph(n, &edges);
        let walk = format!("MATCH REPEATABLE ELEMENTS {body} RETURN count(*) AS n");
        prop_assert_eq!(
            differential(&built.graph, &walk),
            1,
            "the walk count takes {}",
            walk
        );
    }
}
