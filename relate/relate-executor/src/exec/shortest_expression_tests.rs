//! The expression form on the shared searches (ADR-0189, ADR-0568): a bound
//! end that fails its pattern, and a relationship property map that falls
//! back to enumeration. The open-endpoint, same-node closed-trail, and
//! `min_hops` > 1 shapes are not expressible: live Neo4j 2026.10.1 rejects an
//! unbound endpoint (a shortest-path expression requires bound nodes), a
//! same-node pair (51N23), and a minimum length above 1 (42I08).

use relate_core::Value;

use crate::graph::Graph;
use crate::shortest::test_support::counting_expands;

use super::test_support::{exec_cypher, Builder};

/// The single value of a one-row, one-column result, and the expansions
/// it took.
fn value(g: &Graph, q: &str) -> (Value, u64) {
    let (rows, expands) = counting_expands(|| exec_cypher(g, q).rows);
    assert_eq!(rows.len(), 1, "{q}");
    (rows[0][0].clone(), expands)
}

#[test]
fn bound_end_failing_its_pattern_returns_null() {
    // a→m→b: b is reachable, but neither `:Missing` nor `{id: 'z'}`.
    let b = Builder::new();
    let (a, end) = (b.node("a", &["N"]), b.node("b", &["N"]));
    b.chain(&[a, b.node("m", &[]), end]);
    let g = b.done();
    for (end, want) in [
        ("(b:N)", [Value::Int(2), Value::Int(1)]),
        ("(b {id: 'b'})", [Value::Int(2), Value::Int(1)]),
        ("(b:Missing)", [Value::Null, Value::Null]),
        ("(b {id: 'z'})", [Value::Null, Value::Null]),
    ] {
        let q = format!(
            "MATCH (a:N {{id: 'a'}}), (b:N {{id: 'b'}}) \
             RETURN length(shortestPath((a)-[*]->{end})), \
             size(allShortestPaths((a)-[*]->{end}))"
        );
        let (rows, expands) = counting_expands(|| exec_cypher(&g, &q).rows);
        assert_eq!(rows, vec![want.to_vec()], "{q}");
        if want[0] == Value::Null {
            assert_eq!(expands, 0, "{q} searched for an end that fails its pattern");
        } else {
            assert!(expands > 0, "{q} did not run the BFS");
        }
    }
}

#[test]
fn null_bound_end_matches_nothing_on_both_engines() {
    // a→m→b: b is reachable in 2 hops, so an unbound end would find a
    // trail. An end bound to null (or any non-node) instead matches
    // nothing — the walk's zero-row semantics for a `Binding::Mismatch`
    // (LABS-711) — and the classic walk and the shortest BFS both answer
    // from that binding: null, with no search run.
    let b = Builder::new();
    let (a, end) = (b.node("a", &["N"]), b.node("b", &["N"]));
    b.chain(&[a, b.node("m", &[]), end]);
    let g = b.done();
    // The positive control: an end bound to the real node runs the BFS
    // and finds the 2-hop trail — the fast path is live in this shape.
    let (hops, expands) = value(
        &g,
        "MATCH (a:N {id: 'a'}), (b:N {id: 'b'}) \
         RETURN length(shortestPath((a)-[*]->(b)))",
    );
    assert_eq!(hops, Value::Int(2));
    assert!(expands > 0, "the fast path did not run the BFS");
    for bound in ["null", "1"] {
        // The eval walk (the classic engine, the reference — reached by a
        // pattern comprehension): matches nothing.
        let rows = exec_cypher(
            &g,
            &format!(
                "MATCH (a:N {{id: 'a'}}) WITH a, {bound} AS b \
                 RETURN [(a)-[*]->(b) | 1] AS xs"
            ),
        )
        .rows;
        assert_eq!(
            rows,
            vec![vec![Value::List(Vec::new().into())]],
            "{bound}: the walk matched a null-bound end"
        );
        // The fast path (min_hops ≤ 1, followable — LABS-811: it used to
        // read the null binding as unbound and return the 1-hop trail to
        // m): the same null, no search.
        assert_shortest_null_bound(&g, bound);
    }
}

/// The shortest-path expression answers `null, no search` for an end bound
/// to a null or non-node value (LABS-811).
fn assert_shortest_null_bound(g: &Graph, bound: &str) {
    let (rows, expands) = counting_expands(|| {
        exec_cypher(
            g,
            &format!(
                "MATCH (a:N {{id: 'a'}}) WITH a, {bound} AS b \
                 RETURN length(shortestPath((a)-[*]->(b))), \
                 size(allShortestPaths((a)-[*]->(b)))"
            ),
        )
        .rows
    });
    assert_eq!(
        rows,
        vec![vec![Value::Null, Value::Null]],
        "{bound}: the fast path matched a null-bound end"
    );
    assert_eq!(
        expands, 0,
        "{bound}: the fast path searched for a null-bound end"
    );
}

#[test]
fn relationship_property_map_runs_on_enumeration() {
    // a→b with k: 0, and a→m→b with k: 1.
    let b = Builder::new();
    let (a, end, m) = (b.node("a", &["N"]), b.node("b", &["N"]), b.node("m", &[]));
    for (from, to, k) in [(a, end, 0), (a, m, 1), (m, end, 1)] {
        b.rel(from, to, "E", &[("k", Value::Int(k))]);
    }
    let g = b.done();
    let (hops, expands) = value(
        &g,
        "MATCH (a:N {id: 'a'}), (b:N {id: 'b'}) \
         RETURN length(shortestPath((a)-[*1..3 {k: 1}]->(b)))",
    );
    assert_eq!(hops, Value::Int(2));
    assert_eq!(expands, 0, "the BFS ignores relationship property maps");
}
