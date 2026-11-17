//! The memoised admission's wiring pins (LABS-1478), the sites past the
//! chain and hub-star kernels: the frontier BFS's per-state guards, the
//! re-arrival search's guards and the frontier rung's target admission
//! (hoisted above the seed loop). Each runs engaged against the stood-down
//! walker on the wide graph, where nodes repeat across seeds and the
//! guarded states pass the memo's warm-up.

use crate::exec::test_support::{exec_autocommit, exec_cypher};
use crate::graph::Graph;
use crate::pattern_engine::PatternEngine;

use super::{take_kernel_engagements, with_kernels_disabled};
use crate::exec::test_support::Builder;

/// Run every query engaged and stood down, assert equal rows, and return
/// how many engaged the kernels.
fn assert_engaged_equals_walker(g: &Graph, queries: &[String]) -> usize {
    let mut engaged_queries = 0;
    for q in queries {
        let _ = take_kernel_engagements();
        let engaged = exec_cypher(g, q).rows;
        let hits = take_kernel_engagements();
        let walker = with_kernels_disabled(|| exec_cypher(g, q).rows);
        let mut a: Vec<_> = engaged.iter().map(|r| format!("{r:?}")).collect();
        let mut b: Vec<_> = walker.iter().map(|r| format!("{r:?}")).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "{q}");
        engaged_queries += usize::from(hits > 0);
    }
    engaged_queries
}

const WIDTH: usize = 300;
const LAYERS: usize = 8;

/// A layered DAG: `LAYERS` layers of `WIDTH` nodes, each node linked to
/// three nodes of the next layer, so a seed's walk re-reaches the same
/// nodes along many routes (and across seeds), the stood-down walker
/// stays finite (no cycles), and one run makes thousands of guard checks.
/// Labels mix hits, misses and overlaps; every seventh layer-0 node is a
/// `Hub` seed.
fn layered_graph() -> Graph {
    let b = Builder::new();
    let ids: Vec<Vec<_>> = (0..LAYERS)
        .map(|layer| {
            (0..WIDTH)
                .map(|i| {
                    let labels: &[&str] = match (i + layer) % 6 {
                        0 => &["V"],
                        1 => &["W"],
                        2 => &["U", "V"],
                        _ => &["U"],
                    };
                    let labels: Vec<&str> = if layer == 0 && i % 7 == 0 {
                        labels.iter().copied().chain(["Hub"]).collect()
                    } else {
                        labels.to_vec()
                    };
                    b.node(&format!("l{layer}n{i}"), &labels)
                })
                .collect()
        })
        .collect();
    for layer in 0..LAYERS - 1 {
        for i in 0..WIDTH {
            for step in [1, 17, 101] {
                b.rel(
                    ids[layer][i],
                    ids[layer + 1][(i * 3 + step) % WIDTH],
                    "E",
                    &[],
                );
            }
        }
    }
    let mut g = b.done();
    g.configure_pattern_engine(PatternEngine::Nfa);
    g
}

/// Guarded quantified bodies (per-state label guards), a repeated target
/// admission across many seeds, and EXISTS subqueries (the hit pass and the
/// all-miss pass).
fn guarded_queries() -> Vec<String> {
    let mut qs = Vec::new();
    for body in [
        "(a:Hub)((m)-[:E]->(n:U)){1,}(y:V)",
        "(a:Hub)((m)-[:E]->(n:U:V)){1,}(y:U)",
        "(a:Hub)((m)-[:E]->(n:Missing)){1,}(y:U)",
        "(a:Hub)((m)-[:E]->(n:U|W)){2,}(y:W)",
    ] {
        qs.push(format!("MATCH {body} RETURN count(DISTINCT y)"));
        qs.push(format!("MATCH {body} RETURN DISTINCT y.id AS y"));
    }
    qs.push("MATCH (a:Hub)-[:E*]->(y:V) RETURN count(DISTINCT y)".into());
    for target in ["V", "Missing"] {
        qs.push(format!(
            "RETURN EXISTS {{ MATCH (a:Hub)((m)-[:E]->(n:U)){{1,}}(y:{target}) }} AS hit"
        ));
    }
    qs
}

/// A small dense cyclic graph, where the re-arrival search runs (the walk
/// revisits nodes, so the rule consults its guarded states).
fn cyclic_graph() -> Graph {
    let b = Builder::new();
    let ids: Vec<_> = (0..8)
        .map(|i| {
            let labels: &[&str] = if i % 3 == 0 { &["U", "Hub"] } else { &["U"] };
            b.node(&format!("c{i}"), labels)
        })
        .collect();
    for i in 0..8 {
        b.rel(ids[i], ids[(i + 1) % 8], "E", &[]);
        b.rel(ids[i], ids[(i + 3) % 8], "E", &[]);
    }
    b.rel(ids[2], ids[2], "E", &[]);
    b.rel(ids[2], ids[3], "E", &[]);
    let mut g = b.done();
    g.configure_pattern_engine(PatternEngine::Nfa);
    g
}

#[test]
fn memoised_frontier_guards_and_targets_match_the_walker() {
    let g = layered_graph();
    let engaged = assert_engaged_equals_walker(&g, &guarded_queries());
    assert!(engaged > 0, "no query engaged the kernel");
}

#[test]
fn guarded_search_over_a_cyclic_graph_matches_the_walker() {
    let g = cyclic_graph();
    let qs: Vec<String> = [
        "MATCH (a:Hub)((m)-[:E]->(n:U)){1,}(y:Hub) RETURN count(DISTINCT y)",
        "MATCH (a:U)((m)-[:E]->(n:Hub)){1,}(y:U) RETURN DISTINCT y.id AS y",
        "MATCH (a:Hub)((m)-[:E]->(n:U)){2,4}(y) RETURN DISTINCT y.id AS y",
        "MATCH REPEATABLE ELEMENTS (a:Hub)((m)-[:E]->(n:U)){1,4}(y:Hub) RETURN DISTINCT y.id AS y",
    ]
    .map(String::from)
    .into();
    assert!(assert_engaged_equals_walker(&g, &qs) > 0);
}

#[test]
fn memoised_guards_see_a_label_mutation_between_statements() {
    let g = layered_graph();
    let qs = guarded_queries();
    let before: Vec<_> = qs.iter().map(|q| exec_cypher(&g, q).rows).collect();
    assert!(assert_engaged_equals_walker(&g, &qs) > 0);
    exec_autocommit(&g, "MATCH (n:V) SET n:U REMOVE n:V").unwrap();
    exec_autocommit(&g, "MATCH (n:W) SET n:V").unwrap();
    let after: Vec<_> = qs.iter().map(|q| exec_cypher(&g, q).rows).collect();
    assert_ne!(before, after, "the mutation changed no answer");
    assert!(assert_engaged_equals_walker(&g, &qs) > 0);
}
