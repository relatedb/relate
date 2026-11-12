//! 51N23 against the `MATCH`-level `WHERE` (live Neo4j 2026.09, probed on
//! this graph): a conjunct that reads only the endpoints drops a same-node
//! pair before the search can reject it, so the fan spelling
//! `shortestPath((p:P)-[*1..k]-(n)) WHERE n <> p` runs; a pair no such
//! conjunct drops — no `WHERE`, a path-reading conjunct, a disjunction
//! the pair satisfies — still raises. The expected counts below are the
//! live answers for the same graph.
//!
//! Nodes `id` 0..5: `0 1 4` are `P`, `2` is `K`, `3` is `L`; relationships
//! `0→1`, `1→2`, `2→3`, `3→0` and the self-loop `2→2`, all type `R`.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use relate_core::Value;

use crate::exec::test_support::{build, exec_cypher, try_exec_cypher, Builder};
use crate::graph::Graph;
use crate::shortest::test_support::assert_common_end_nodes;

use super::fixtures::{on_route, sorted};

fn graph() -> Graph {
    build(
        &[&["P"], &["P"], &["K"], &["L"], &["P"]],
        &[
            (0, 1, "R"),
            (1, 2, "R"),
            (2, 3, "R"),
            (3, 0, "R"),
            (2, 2, "R"),
        ],
    )
    .graph
}

/// `(pairs, hops)` of an aggregate over `path`, from the BFS route.
fn totals(g: &Graph, q: &str) -> (i64, i64) {
    let rows = on_route(g, q);
    match rows[..] {
        [ref row] => match row[..] {
            [Value::Int(pairs), Value::Int(hops)] => (pairs, hops),
            ref other => panic!("{q}: {other:?}"),
        },
        ref other => panic!("{q}: {other:?}"),
    }
}

/// `MATCH <pattern> <filter>` counting pairs and summing hops over `path`.
fn count_query(pattern: &str, filter: &str) -> String {
    format!(
        "MATCH {pattern} {filter} \
         RETURN count(*) AS pairs, sum(length(path)) AS hops"
    )
}

/// The fan from every `P` over the relationship spelling `rel` (`-[*1..3]-`,
/// `-[*0..3]->`), with `filter` as its `WHERE`.
fn fan_rel(rel: &str, filter: &str) -> String {
    count_query(&format!("path = shortestPath((p:P){rel}(n))"), filter)
}

/// The undirected fan over `bounds` (`1..3`), with `filter` as its `WHERE`.
fn fan(bounds: &str, filter: &str) -> String {
    fan_rel(&format!("-[*{bounds}]-"), filter)
}

/// `(pairs, hops)` of the aggregate `MATCH <pattern> <filter>` over `path`.
fn pattern_totals(g: &Graph, pattern: &str, filter: &str) -> (i64, i64) {
    totals(g, &count_query(pattern, filter))
}

#[test]
fn fan_with_an_endpoint_filter_skips_its_own_start() {
    let g = graph();
    for filter in [
        "WHERE n <> p",
        "WHERE NOT (n = p)",
        "WHERE n.id <> p.id",
        "WHERE id(n) <> id(p)",
        "WHERE n <> p AND length(path) > 0",
        "WHERE n <> p AND ALL(r IN relationships(path) WHERE type(r) = 'R')",
    ] {
        assert_eq!(totals(&g, &fan("1..3", filter)), (6, 8), "{filter}");
    }
    // The pairs themselves: (start, end, hops) in the live answer's order.
    let rows = on_route(
        &g,
        "MATCH path = shortestPath((p:P)-[*1..3]-(n)) WHERE n <> p \
         RETURN p.id, n.id, length(path)",
    );
    let want: Vec<Vec<Value>> = [
        (0, 1, 1),
        (0, 2, 2),
        (0, 3, 1),
        (1, 0, 1),
        (1, 2, 1),
        (1, 3, 2),
    ]
    .iter()
    .map(|&(p, n, len)| vec![Value::Int(p), Value::Int(n), Value::Int(len)])
    .collect();
    assert_eq!(rows, sorted(want));
}

#[test]
fn fan_filter_spellings_agree_with_the_cross_product_form() {
    let g = graph();
    let fan_q = "MATCH path = shortestPath((p:P)-[*1..3]-(n)) WHERE n <> p \
                 RETURN p.id, n.id, length(path)";
    // The form that always ran: the cross product drops the same-node pair
    // before the search sees it.
    let cross_q = "MATCH (p:P), (n) WHERE n <> p \
                   MATCH path = shortestPath((p)-[*1..3]-(n)) \
                   RETURN p.id, n.id, length(path)";
    assert_eq!(on_route(&g, fan_q), on_route(&g, cross_q));
    // The start bound by an earlier clause is the same fan.
    assert_eq!(
        on_route(
            &g,
            "MATCH (p:P) MATCH path = shortestPath((p)-[*1..3]-(n)) WHERE n <> p \
             RETURN p.id, n.id, length(path)"
        ),
        on_route(&g, fan_q)
    );
}

#[test]
fn fan_filter_over_labels_directions_and_bounds() {
    let g = graph();
    // A label test on the end drops the same-node pair as well.
    assert_eq!(totals(&g, &fan("1..3", "WHERE NOT n:P")), (4, 6));
    // Directed: the outgoing and incoming fans take the three hops the
    // cycle needs.
    assert_eq!(totals(&g, &fan_rel("-[*1..3]->", "WHERE n <> p")), (6, 12));
    assert_eq!(totals(&g, &fan_rel("<-[*1..3]-", "WHERE n <> p")), (6, 12));
    // One hop only: the cycle's closing edges are the whole answer.
    assert_eq!(totals(&g, &fan("1..1", "WHERE n <> p")), (4, 4));
    // A second conjunct narrows the answer, never widens the skip.
    assert_eq!(
        totals(&g, &fan_rel("-[*1..3]->", "WHERE n <> p AND n:K")),
        (2, 3)
    );
    assert_eq!(
        totals(&g, &fan("1..3", "WHERE n.id <> p.id AND n.id > 1")),
        (4, 6)
    );
    // A conjunct the same-node pair fails for another reason drops it too.
    assert_eq!(
        totals(&g, &fan("1..3", "WHERE n <> p AND n IS NULL")),
        (0, 0)
    );
}

#[test]
fn all_shortest_paths_fan_skips_its_own_start_too() {
    let g = graph();
    assert_eq!(
        pattern_totals(
            &g,
            "path = allShortestPaths((p:P)-[*1..3]-(n))",
            "WHERE n <> p"
        ),
        (8, 12)
    );
}

#[test]
fn a_start_with_a_self_loop_is_not_its_own_answer() {
    let g = graph();
    // Node 2 carries the loop 2→2 and the closed trail through it.
    assert_eq!(
        pattern_totals(&g, "path = shortestPath((k:K)-[*1..3]-(n))", "WHERE n <> k"),
        (3, 4)
    );
}

#[test]
fn the_other_anchors_apply_the_same_gate() {
    let g = graph();
    // Both ends bound by an earlier clause (the pair arm).
    assert_eq!(
        pattern_totals(
            &g,
            "(a:P), (b:P) MATCH path = shortestPath((a)-[*1..3]-(b))",
            "WHERE a <> b"
        ),
        (2, 2)
    );
    // The end bound and the start open (the backward fan).
    assert_eq!(
        pattern_totals(&g, "path = shortestPath((n)-[*1..3]-(p:P))", "WHERE n <> p"),
        (6, 8)
    );
    // Both ends open: every ordered pair of distinct nodes.
    let open = "path = shortestPath((a)-[*1..3]-(b))";
    assert_eq!(pattern_totals(&g, open, "WHERE a <> b"), (12, 16));
    assert_eq!(pattern_totals(&g, open, "WHERE a.id < b.id"), (6, 8));
    // An OPTIONAL MATCH keeps the start whose fan answered nothing.
    assert_eq!(
        exec_cypher(
            &g,
            "MATCH (a:P) OPTIONAL MATCH path = shortestPath((a)-[*1..3]-(b)) WHERE a <> b \
             RETURN count(*) AS rows"
        )
        .rows,
        vec![vec![Value::Int(7)]]
    );
}

#[test]
fn a_zero_lower_bound_answers_the_zero_length_path_unless_filtered() {
    let g = graph();
    // 0..k: the same-node pair is the zero-length path, never an error.
    assert_eq!(totals(&g, &fan("0..3", "")), (9, 8));
    assert_eq!(totals(&g, &fan("0..3", "WHERE n <> p")), (6, 8));
    assert_eq!(totals(&g, &fan_rel("-[*0..3]->", "")), (9, 12));
    assert_eq!(totals(&g, &fan_rel("-[*0..3]->", "WHERE n <> p")), (6, 12));
}

#[test]
fn a_same_node_pair_no_endpoint_conjunct_drops_still_raises() {
    let g = graph();
    for filter in [
        "",
        "WHERE length(path) > 0",
        "WHERE n.id >= 0",
        "WHERE n <> p OR length(path) > 0",
        "WHERE n <> p OR n.id > 0",
        "WHERE p.id >= 0 AND n:P",
    ] {
        let q = fan("1..3", filter);
        let err = try_exec_cypher(&g, &q)
            .err()
            .unwrap_or_else(|| panic!("{q}: expected 51N23"));
        assert_common_end_nodes(&err, &q);
    }
    // The pair arm keeps its rejection without the filter, as live does.
    let q = "MATCH (a:P), (b:P) MATCH path = shortestPath((a)-[*1..3]-(b)) \
             RETURN count(*) AS pairs";
    let err = try_exec_cypher(&g, q)
        .err()
        .unwrap_or_else(|| panic!("{q}: expected 51N23"));
    assert_common_end_nodes(&err, q);
    // A lower bound of 0 never raises, filter or not.
    assert!(try_exec_cypher(&g, &fan("0..3", "")).is_ok());
}

/// The fan spelling answers exactly what the cross product answers on
/// seeded random graphs — self-loops, parallels and cycles included — for
/// both selectors, three directions and bounds from 1 to 3 (and a lower
/// bound of 0, where both spellings keep the zero-length rows they filter).
#[test]
fn fan_filter_matches_the_cross_product_on_random_graphs() {
    let mut rng = StdRng::seed_from_u64(0x1454_0001);
    let mut answered = 0;
    for graph_ix in 0..12 {
        let n = rng.random_range(8..24);
        let b = Builder::new();
        let nodes: Vec<_> = (0..n)
            .map(|i| {
                let labels: &[&str] = if i % 3 == 0 { &["P"] } else { &["Q"] };
                b.prop_node(&i.to_string(), labels, &[("g", Value::Int((i % 4) as i64))])
            })
            .collect();
        for _ in 0..rng.random_range(n..3 * n) {
            let (a, z) = (rng.random_range(0..n), rng.random_range(0..n));
            b.rel(nodes[a], nodes[z], "R", &[]);
        }
        let g = b.done();
        for selector in ["shortestPath", "allShortestPaths"] {
            for (left, right) in [("-", "-"), ("-", "->"), ("<-", "-")] {
                for bounds in ["1..1", "1..2", "1..3", "0..3"] {
                    let rel = format!("{left}[*{bounds}]{right}");
                    let fan_q = format!(
                        "MATCH path = {selector}((p:P){rel}(n)) WHERE n <> p \
                         RETURN p.g, id(p), id(n), length(path)"
                    );
                    let cross_q = format!(
                        "MATCH (p:P), (n) WHERE n <> p \
                         MATCH path = {selector}((p){rel}(n)) \
                         RETURN p.g, id(p), id(n), length(path)"
                    );
                    let want = sorted(exec_cypher(&g, &cross_q).rows);
                    answered += want.len();
                    assert_eq!(
                        sorted(exec_cypher(&g, &fan_q).rows),
                        want,
                        "graph {graph_ix}: {fan_q}"
                    );
                }
            }
        }
    }
    assert!(answered > 1000, "the corpus answered only {answered} rows");
}
