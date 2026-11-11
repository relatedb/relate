//! The expression form on the one shortest route (ADR-0568). The front end
//! lowers `shortestPath(...)` / `allShortestPaths(...)` to the selector
//! form the `MATCH` clause uses, so the expression takes the global
//! shortest over every start and end candidate: the clause's rows
//! collapsed to one value per input row. These tests pin that contract
//! against the clause (the expression is the minimum of the clause's
//! rows), that the route and the enumeration fallback agree, and the row
//! contract the lowering must keep: one value per row, `null` when nothing
//! qualifies, a list for `allShortestPaths`, the zero-length path, closed
//! trails, and the 32-hop default for an unbounded relationship.

use relate_core::Value;

use crate::exec::test_support::{exec_cypher, Builder};
use crate::graph::Graph;

use super::fixtures::{run, shortcut, sorted};
use super::with_forced_gpm;

/// Starts in creation order `starts`, ends t1 and t2: s1 reaches t1 in 3
/// hops, s2 reaches t1 in 1 hop and t2 in 2.
fn two_starts(starts: [&str; 2]) -> Graph {
    let b = Builder::new();
    let s: Vec<_> = starts.iter().map(|id| (*id, b.node(id, &["S"]))).collect();
    let t1 = b.node("t1", &["T"]);
    let t2 = b.node("t2", &["T"]);
    let mid = |id: &str| b.node(id, &[]);
    let at = |name: &str| s.iter().find(|(id, _)| *id == name).unwrap().1;
    b.chain(&[at("s1"), mid("a"), mid("b"), t1]);
    b.edge(at("s2"), t1);
    b.chain(&[at("s2"), mid("c"), t2]);
    b.done()
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Null => "null".into(),
        other => panic!("unexpected {other:?}"),
    }
}

/// `rows` as sorted `a>b>c` strings.
fn lines(rows: &[Vec<Value>]) -> Vec<String> {
    let mut out: Vec<String> = rows
        .iter()
        .map(|r| r.iter().map(text).collect::<Vec<_>>().join(">"))
        .collect();
    out.sort();
    out
}

/// The clause's (start, end, length) rows for `selector` over `pattern`.
fn clause(g: &Graph, selector: &str, pattern: &str) -> Vec<String> {
    let q = format!(
        "MATCH p = {selector}({pattern}) \
         RETURN nodes(p)[0].id, last(nodes(p)).id, length(p)"
    );
    lines(&exec_cypher(g, &q).rows)
}

/// The expression form's (start, end, length) rows for `selector` over
/// `pattern`: one row, or one `null` row.
fn expression(g: &Graph, selector: &str, pattern: &str) -> Vec<String> {
    let q = if selector == "shortestPath" {
        format!(
            "WITH {selector}({pattern}) AS p \
             RETURN nodes(p)[0].id, last(nodes(p)).id, length(p)"
        )
    } else {
        format!(
            "UNWIND coalesce({selector}({pattern}), [null]) AS p \
             RETURN nodes(p)[0].id, last(nodes(p)).id, length(p)"
        )
    };
    lines(&exec_cypher(g, &q).rows)
}

/// The length (last field) of a `start>end>length` line.
fn length(line: &str) -> i64 {
    line.rsplit('>').next().unwrap().parse().unwrap()
}

const OPEN: &str = "(:S)-[*]->(:T)";
const BOUND_END: &str = "(:S)-[*]->(:T {id: 't1'})";
const MIN_TWO: &str = "(:S)-[*2..]->(:T)";
const SHORT: &str = "(:S)-[*..2]->(:T)";

#[test]
fn expression_is_the_minimum_of_the_clause_rows() {
    for order in [["s1", "s2"], ["s2", "s1"]] {
        let g = two_starts(order);
        for pattern in [OPEN, BOUND_END, SHORT] {
            let rows = clause(&g, "shortestPath", pattern);
            let min = rows.iter().map(|r| length(r)).min().unwrap();
            let at_min: Vec<String> = rows.iter().filter(|r| length(r) == min).cloned().collect();
            // Single path: one row, one of the shortest clause rows (the
            // witness among equals is unspecified).
            let single = expression(&g, "shortestPath", pattern);
            assert_eq!(single.len(), 1, "{pattern} {order:?}");
            assert!(
                at_min.contains(&single[0]),
                "{pattern} {order:?}: {single:?}"
            );
            // All paths: exactly the clause's rows at the global minimum.
            let every = clause(&g, "allShortestPaths", pattern);
            let every_min = every.iter().map(|r| length(r)).min().unwrap();
            let expected: Vec<String> = every
                .into_iter()
                .filter(|r| length(r) == every_min)
                .collect();
            assert_eq!(
                expression(&g, "allShortestPaths", pattern),
                expected,
                "{pattern} {order:?}"
            );
        }
    }
}

#[test]
fn candidate_order_does_not_change_the_answer() {
    // s1 reaches t1 in 3 hops, s2 in 1: whichever start is created first,
    // the expression returns the 1-hop trail (the first-candidate early
    // exit and the allShortestPaths length cap are gone).
    for order in [["s1", "s2"], ["s2", "s1"]] {
        let g = two_starts(order);
        assert_eq!(
            expression(&g, "shortestPath", OPEN),
            ["s2>t1>1"],
            "{order:?}"
        );
        assert_eq!(
            expression(&g, "allShortestPaths", OPEN),
            ["s2>t1>1"],
            "{order:?}"
        );
        assert_eq!(
            expression(&g, "shortestPath", BOUND_END),
            ["s2>t1>1"],
            "{order:?}"
        );
        assert_eq!(
            expression(&g, "allShortestPaths", BOUND_END),
            ["s2>t1>1"],
            "{order:?}"
        );
        // min_hops above one enumerates and was always order independent.
        let rows = exec_cypher(&g, &format!("RETURN length(shortestPath({MIN_TWO}))")).rows;
        assert_eq!(lines(&rows), ["2"], "{order:?}");
    }
}

#[test]
fn expression_runs_on_the_route() {
    let g = two_starts(["s1", "s2"]);
    let (rows, counts) = run(&g, &format!("RETURN length(shortestPath({OPEN}))"));
    assert_eq!(lines(&rows), ["1"]);
    assert!(
        counts.route_runs > 0,
        "the expression did not take the route"
    );
    assert!(counts.expands > 0);
    // min_hops above one declines the route and enumerates.
    let (rows, counts) = run(&g, &format!("RETURN length(shortestPath({MIN_TWO}))"));
    assert_eq!(lines(&rows), ["2"]);
    assert_eq!(counts.route_runs, 0);
}

#[test]
fn one_value_per_row_and_null_when_nothing_qualifies() {
    let g = two_starts(["s1", "s2"]);
    // A start with no way to the end: null, never a dropped row.
    let rows = exec_cypher(
        &g,
        "MATCH (t:T) RETURN t.id, length(shortestPath((t)-[*]->(:S)))",
    )
    .rows;
    assert_eq!(lines(&rows), ["t1>null", "t2>null"]);
    let rows = exec_cypher(
        &g,
        "MATCH (s:S) RETURN s.id, allShortestPaths((s)-[*]->(:Missing)) IS NULL",
    )
    .rows;
    assert_eq!(rows.len(), 2);
}

#[test]
fn zero_length_path_when_start_matches_end() {
    let b = Builder::new();
    let (s1, s2) = (b.node("s1", &["S"]), b.node("s2", &["S"]));
    let m = b.node("m", &[]);
    b.chain(&[s1, m, s2]);
    let g = b.done();
    let pat = "(:S)-[*0..]->(:S)";
    // The global minimum is the zero-length path of either start; which
    // one shortestPath returns is unspecified.
    let single = expression(&g, "shortestPath", pat);
    assert!(
        ["s1>s1>0", "s2>s2>0"].contains(&single[0].as_str()),
        "{single:?}"
    );
    // allShortestPaths: every zero-length path.
    assert_eq!(
        expression(&g, "allShortestPaths", pat),
        ["s1>s1>0", "s2>s2>0"]
    );
}

#[test]
fn closed_trails_per_input_row() {
    let b = Builder::new();
    let (s1, s2) = (b.node("s1", &["S"]), b.node("s2", &["S"]));
    let [x, y, z] = ["x", "y", "z"].map(|id| b.node(id, &[]));
    b.chain(&[s1, x, y, s1]);
    b.chain(&[s2, z, s2]);
    let g = b.done();
    let expr = exec_cypher(
        &g,
        "MATCH (s:S) RETURN s.id, length(shortestPath((s)-[*1..]->(s)))",
    )
    .rows;
    assert_eq!(lines(&expr), ["s1>3", "s2>2"]);
    let via_clause = exec_cypher(
        &g,
        "MATCH (s:S) MATCH p = shortestPath((s)-[*1..]->(s)) RETURN s.id, length(p)",
    )
    .rows;
    assert_eq!(lines(&via_clause), lines(&expr));
}

#[test]
fn bound_start_agrees_with_the_clause_per_end() {
    // The expression collapses a start's ends to the shallowest layer; the
    // clause answers one row per (start, end).
    let g = two_starts(["s1", "s2"]);
    let via_clause = exec_cypher(
        &g,
        "MATCH (s:S) MATCH p = shortestPath((s)-[*]->(:T)) RETURN s.id, length(p)",
    )
    .rows;
    let via_expr = exec_cypher(
        &g,
        "MATCH (s:S) RETURN s.id, length(shortestPath((s)-[*]->(:T)))",
    )
    .rows;
    assert_eq!(lines(&via_clause), ["s1>3", "s2>1", "s2>2"]);
    assert_eq!(lines(&via_expr), ["s1>3", "s2>1"]);
}

#[test]
fn route_and_enumeration_agree_on_every_bound_pair() {
    let g = shortcut();
    for q in [
        "MATCH (a), (b) RETURN a.id, b.id, length(shortestPath((a)-[*..4]->(b)))",
        "MATCH (a), (b) RETURN a.id, b.id, size(allShortestPaths((a)-[*..4]-(b)))",
        "MATCH (a), (b) RETURN a.id, b.id, length(shortestPath((a)-[*0..3]->(b)))",
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
