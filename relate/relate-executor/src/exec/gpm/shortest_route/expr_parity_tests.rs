//! The expression form on the one shortest route (ADR-0568). The front end
//! lowers `shortestPath(...)` / `allShortestPaths(...)` to the selector form
//! the `MATCH` clause uses; live Neo4j 2026.10.1 requires both endpoint node
//! patterns to name a bound variable, so the expression answers one value per
//! input row for that bound pair. These tests pin the bound-pair contract
//! against the clause, the route/enumeration parity, the zero-length path,
//! the 51N23 same-node rejection, and the 32-hop default ceiling.

use relate_core::Value;

use crate::exec::test_support::{exec_cypher, try_exec_cypher, Builder};
use crate::graph::Graph;
use crate::shortest::test_support::assert_common_end_nodes;

use super::fixtures::{plain, run, shortcut, sorted};
use super::with_forced_gpm;

/// `rows` as sorted `a>b>c` strings.
fn lines(rows: &[Vec<Value>]) -> Vec<String> {
    let mut out: Vec<String> = rows
        .iter()
        .map(|r| {
            r.iter()
                .map(|v| match v {
                    Value::String(s) => s.to_string(),
                    Value::Int(i) => i.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Null => "null".into(),
                    other => panic!("unexpected {other:?}"),
                })
                .collect::<Vec<_>>()
                .join(">")
        })
        .collect();
    out.sort();
    out
}

/// Every pair `a < b` of an undirected chain, all reachable.
fn chain() -> Graph {
    plain(4, &[(0, 1), (1, 2), (2, 3)])
}

#[test]
fn bound_pair_agrees_with_the_clause() {
    let g = chain();
    let clause = sorted(
        exec_cypher(
            &g,
            "MATCH (a), (b) WHERE a.id < b.id \
             MATCH p = shortestPath((a)-[*..3]-(b)) \
             RETURN a.id, b.id, length(p)",
        )
        .rows,
    );
    let expr = sorted(
        exec_cypher(
            &g,
            "MATCH (a), (b) WHERE a.id < b.id \
             RETURN a.id, b.id, length(shortestPath((a)-[*..3]-(b)))",
        )
        .rows,
    );
    assert_eq!(expr, clause);

    let clause_all = sorted(
        exec_cypher(
            &g,
            "MATCH (a), (b) WHERE a.id < b.id \
             MATCH p = allShortestPaths((a)-[*..3]-(b)) \
             RETURN a.id, b.id, count(*)",
        )
        .rows,
    );
    let expr_all = sorted(
        exec_cypher(
            &g,
            "MATCH (a), (b) WHERE a.id < b.id \
             RETURN a.id, b.id, size(allShortestPaths((a)-[*..3]-(b)))",
        )
        .rows,
    );
    assert_eq!(expr_all, clause_all);
}

#[test]
fn expression_runs_on_the_route() {
    let (rows, counts) = run(
        &chain(),
        "MATCH (a {id: 0}), (b {id: 3}) \
         RETURN length(shortestPath((a)-[*..3]-(b)))",
    );
    assert_eq!(lines(&rows), ["3"]);
    assert!(
        counts.route_runs > 0,
        "the expression did not take the route"
    );
    assert!(counts.expands > 0);
}

#[test]
fn one_value_per_row_and_null_when_nothing_qualifies() {
    // 0 reaches 2 but not 3: the expression keeps the row and answers null.
    let g = plain(4, &[(0, 1), (1, 2)]);
    let rows = exec_cypher(
        &g,
        "MATCH (a {id: 0}), (b {id: 3}) \
         RETURN length(shortestPath((a)-[*]->(b))), \
         allShortestPaths((a)-[*]->(b)) IS NULL",
    )
    .rows;
    assert_eq!(lines(&rows), ["null>true"]);
}

#[test]
fn zero_length_path_when_start_matches_end() {
    // The min-length-zero pair keeps its zero-length path, whether the
    // endpoints share a variable or two variables bind one node (live).
    let g = chain();
    for q in [
        "MATCH (a {id: 0}) RETURN length(shortestPath((a)-[*0..2]-(a)))",
        "MATCH (a {id: 0}), (b {id: 0}) RETURN length(shortestPath((a)-[*0..2]-(b)))",
    ] {
        assert_eq!(lines(&exec_cypher(&g, q).rows), ["0"], "{q}");
    }
}

#[test]
fn same_node_pair_with_a_positive_bound_raises_51n23() {
    let g = chain();
    for q in [
        // One variable at both ends.
        "MATCH (a {id: 0}) RETURN length(shortestPath((a)-[*]-(a)))",
        // Two variables the row binds to one node.
        "MATCH (a {id: 0}), (b {id: 0}) RETURN length(shortestPath((a)-[*]-(b)))",
    ] {
        let err = try_exec_cypher(&g, q)
            .err()
            .unwrap_or_else(|| panic!("{q}: expected 51N23"));
        assert_common_end_nodes(&err, q);
    }
}

#[test]
fn route_and_enumeration_agree_on_every_bound_pair() {
    let g = shortcut();
    for q in [
        "MATCH (a), (b) WHERE a.id < b.id \
         RETURN a.id, b.id, length(shortestPath((a)-[*..4]-(b)))",
        "MATCH (a), (b) WHERE a.id < b.id \
         RETURN a.id, b.id, size(allShortestPaths((a)-[*..4]-(b)))",
        "MATCH (a), (b) WHERE a.id < b.id \
         RETURN a.id, b.id, length(shortestPath((a)-[*0..3]->(b)))",
        // The end node's inline WHERE rides the route's path filter for
        // the expression form too, so the one value per row is the
        // shortest trail the predicate accepts.
        "MATCH (a), (b) WHERE a.id < b.id \
         RETURN a.id, b.id, length(shortestPath((a)-[*..4]->(b WHERE b.id = 3)))",
    ] {
        let routed = sorted(exec_cypher(&g, q).rows);
        let enumerated = sorted(with_forced_gpm(|| exec_cypher(&g, q).rows));
        assert_eq!(routed, enumerated, "{q}");
    }
}

#[test]
fn unbounded_relationship_stops_at_the_default_hop_ceiling() {
    // A 40-node chain: n0 reaches n32 in 32 hops (the ceiling) but not n33.
    let b = Builder::new();
    let nodes: Vec<_> = (0..40).map(|i| b.node(&format!("n{i}"), &[])).collect();
    b.chain(&nodes);
    let g = b.done();
    let hops = |end: &str| {
        let q = format!(
            "MATCH (a {{id: 'n0'}}), (b {{id: '{end}'}}) \
             RETURN length(shortestPath((a)-[*]->(b)))"
        );
        lines(&exec_cypher(&g, &q).rows)
    };
    assert_eq!(hops("n32"), ["32"]);
    assert_eq!(hops("n33"), ["null"]);
}
