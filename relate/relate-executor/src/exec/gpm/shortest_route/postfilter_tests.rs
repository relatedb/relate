//! A `MATCH`-clause `WHERE` that reads the path is a post-filter of the
//! selected paths for the keyword selectors, and a search filter for the
//! function spellings and the inline `WHERE`s — as in live Neo4j (the
//! differential matrix recorded with the ADR that fixed this). The
//! triangle 0-1, 0-2, 2-1 is the minimal witness: the one shortest path
//! 0-1 violates `length(p) >= 2`, so a post-filter yields no row while a
//! search filter would promote the longer path 0-2-1.

use relate_core::Value;

use super::fixtures::{gpm, on_route, plain, run, sorted};

/// The triangle `0-1`, `0-2`, `2-1` (`R` edges, queried undirected).
fn triangle() -> crate::graph::Graph {
    plain(3, &[(0, 1), (0, 2), (2, 1)])
}

/// The triangle plus a second two-hop route `0-3-1`: the direct edge is the
/// only shortest path, two equal two-hop paths follow it.
fn ties() -> crate::graph::Graph {
    plain(4, &[(0, 1), (0, 2), (2, 1), (0, 3), (3, 1)])
}

const KEYWORD_SELECTORS: [&str; 4] = [
    "SHORTEST 1",
    "ANY SHORTEST",
    "ALL SHORTEST",
    "SHORTEST 1 GROUPS",
];

/// Every path-reading clause predicate spelling the matrix covered.
const PATH_READERS: [(&str, &str); 3] = [
    ("-[:R*]-", "length(p) >= 2"),
    (
        "-[:R*]-",
        "all(x IN relationships(p) WHERE true) AND length(p) >= 2",
    ),
    ("-[r:R*]-", "size(r) >= 2"),
];

#[test]
fn keyword_selectors_filter_the_selected_path_afterwards() {
    for graph in [triangle(), ties()] {
        for selector in KEYWORD_SELECTORS {
            for (rel, predicate) in PATH_READERS {
                let q = format!(
                    "MATCH p = {selector} (a {{id: 0}}){rel}(b {{id: 1}}) \
                     WHERE {predicate} RETURN length(p)"
                );
                let (rows, counts) = run(&graph, &q);
                assert!(counts.route_runs > 0, "expected the route for {q}");
                assert!(rows.is_empty(), "the one shortest path is dropped: {q}");
                assert_eq!(sorted(rows), gpm(&graph, &q), "route vs GPM: {q}");
            }
        }
    }
}

#[test]
fn keyword_selector_keeps_a_selected_path_that_satisfies_the_predicate() {
    let g = triangle();
    for selector in KEYWORD_SELECTORS {
        // The shortest path 0-2 is length 1; `<= 1` keeps it.
        let q = format!(
            "MATCH p = {selector} (a {{id: 0}})-[:R*]-(b {{id: 2}}) \
             WHERE length(p) <= 1 RETURN length(p)"
        );
        assert_eq!(on_route(&g, &q), vec![vec![Value::Int(1)]], "{q}");
    }
}

#[test]
fn fan_post_filters_every_reached_node() {
    // 0-1, 1-2, 0-2 closing a triangle: from 0 the shortest paths to 1 and
    // 2 are both single edges, so `length(p) >= 2` leaves no row.
    let g = triangle();
    let q = "MATCH p = SHORTEST 1 (a {id: 0})-[:R*..2]-(b) \
             WHERE length(p) >= 2 RETURN b.id";
    let (rows, counts) = run(&g, q);
    assert!(counts.route_runs > 0, "expected the route for {q}");
    assert!(rows.is_empty(), "{q}");
    // A chain keeps exactly the far node whose shortest path is long enough.
    let chain = plain(4, &[(0, 1), (1, 2), (2, 3)]);
    assert_eq!(
        on_route(&chain, q),
        vec![vec![Value::Int(2)]],
        "only node 2 is two hops away within the bound"
    );
}

#[test]
fn function_spellings_keep_the_predicate_in_the_search() {
    for graph in [triangle(), ties()] {
        let q = "MATCH p = shortestPath((a {id: 0})-[:R*]-(b {id: 1})) \
                 WHERE length(p) >= 2 RETURN length(p)";
        assert_eq!(on_route(&graph, q), vec![vec![Value::Int(2)]], "{q}");
    }
    let q = "MATCH p = allShortestPaths((a {id: 0})-[:R*]-(b {id: 1})) \
             WHERE length(p) >= 2 RETURN length(p)";
    assert_eq!(
        on_route(&ties(), q),
        vec![vec![Value::Int(2)], vec![Value::Int(2)]],
        "{q}"
    );
}

#[test]
fn inline_where_keeps_filtering_the_search() {
    let g = triangle();
    let q = "MATCH p = SHORTEST 1 ((a {id: 0})-[r:R*]-(b {id: 1}) WHERE size(r) >= 2) \
             RETURN length(p)";
    assert_eq!(sorted(run(&g, q).0), vec![vec![Value::Int(2)]], "{q}");
}
