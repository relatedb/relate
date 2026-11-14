//! Patterns outside ADR-0186's routed selectors and shape keep GPM
//! enumerate-then-select: the guard rejects them, the route neither runs
//! nor expands, and the rows are those of the GPM route.

use crate::UsedSet;
use std::collections::BTreeMap;

use relate_core::Value;
use relate_front::parse_and_lower;
use relate_ir::{Clause, MatchMode, PathPattern};

use crate::exec::test_support::try_exec_cypher;
use crate::graph::Graph;
use crate::row::Row;
use crate::shortest::test_support::assert_common_end_nodes;

use super::super::GpmFilters;
use super::fixtures::{
    gpm, gpm_params, on_route, plain, run, run_params, shortcut, sorted, Counts,
};
use super::{route_shape, with_forced_gpm, Spelling};

/// The match mode and first pattern of the statement's last `MATCH` clause.
fn last_match(cypher: &str) -> (MatchMode, PathPattern) {
    let query = parse_and_lower(cypher).unwrap();
    query.parts[0]
        .clauses
        .iter()
        .rev()
        .find_map(|c| match c {
            Clause::Match {
                match_mode,
                patterns,
                ..
            } => Some((*match_mode, patterns.first()?.clone())),
            _ => None,
        })
        .unwrap()
}

fn guard_accepts(cypher: &str) -> bool {
    guard_accepts_with(cypher, &UsedSet::default(), &Row::new())
}

/// Whether the guard routes `cypher`'s last `MATCH` pattern for this input
/// row and used set, with no statement parameters.
fn guard_accepts_with(cypher: &str, used: &UsedSet, row: &Row) -> bool {
    let (mode, path) = last_match(cypher);
    let g = Graph::default();
    let view = crate::overlay::ExecGraph::read(&g);
    let filters = GpmFilters::resolve(&view, &path);
    route_shape(
        &path,
        &filters,
        mode,
        used,
        row,
        &BTreeMap::new(),
        Spelling::of(&path),
    )
    .is_some()
}

/// `q` is rejected by the guard, neither runs nor expands on the route, and
/// returns `want` lengths, the rows the GPM route gives.
fn assert_stays_on_gpm(g: &Graph, q: &str, want: &[i64]) {
    assert!(!guard_accepts(q), "guard accepted {q}");
    let (rows, counts) = run(g, q);
    assert_eq!(counts, Counts::default(), "route ran for {q}");
    let rows = sorted(rows);
    assert_eq!(rows, gpm(g, q), "{q}");
    let want: Vec<_> = want.iter().map(|&n| vec![Value::Int(n)]).collect();
    assert_eq!(rows, want, "{q}");
}

#[test]
fn routed_control_is_accepted() {
    for q in [
        "MATCH p = shortestPath((a {id: 0})-[*]->(b {id: 3})) RETURN length(p)",
        "MATCH p = allShortestPaths((a)-[:R|S*0..4]-(b:B)) RETURN length(p)",
        "MATCH p = ANY SHORTEST (a)-[*]->(b) RETURN length(p)",
        "MATCH p = SHORTEST 1 TRAIL (a)-[*1..]->(b) RETURN length(p)",
        "MATCH p = SHORTEST 1 WALK GROUP (a:A:B)-[*]->(b) RETURN length(p)",
        "MATCH p = ALL SHORTEST (a {id: 0})-[*]->(b) RETURN length(p)",
    ] {
        assert!(guard_accepts(q), "guard rejected {q}");
    }
}

#[test]
fn shortest_k_greater_than_one_stays_on_gpm() {
    let g = shortcut();
    let q = "MATCH p = SHORTEST 2 (a {id: 0})-[*]->(b {id: 3}) RETURN length(p)";
    assert_stays_on_gpm(&g, q, &[2, 3]);
}

/// One `SHORTEST $k` run: the route's rows and counters, and the rows of
/// the same statement forced onto GPM.
struct CountRun {
    rows: Vec<Vec<Value>>,
    counts: Counts,
    gpm_rows: Vec<Vec<Value>>,
}

/// `SHORTEST $k` over [`shortcut`] with `k` bound: the route's rows and
/// counters, and the rows of the same statement forced onto GPM.
fn run_count_param(k: i64) -> CountRun {
    let g = shortcut();
    let q = "MATCH p = SHORTEST $k (a {id: 0})-[*]->(b {id: 3}) RETURN length(p)";
    let params = BTreeMap::from([("k".to_string(), Value::Int(k))]);
    let (rows, counts) = run_params(&g, q, &params);
    CountRun {
        rows: sorted(rows),
        counts,
        gpm_rows: gpm_params(&g, q, &params),
    }
}

/// The plan cache's auto-parameterization hands the guard `SHORTEST $k`
/// for a literal `SHORTEST 1`: a count parameter of 1 takes the route and
/// answers what GPM answers.
#[test]
fn parameter_count_of_one_takes_the_route() {
    let counted = run_count_param(1);
    assert!(
        counted.counts.route_runs > 0,
        "expected the BFS route for $k = 1"
    );
    assert_eq!(counted.rows, counted.gpm_rows);
    assert_eq!(counted.rows, vec![vec![Value::Int(2)]]);
}

/// A count parameter other than 1 keeps GPM's enumerate-then-select.
#[test]
fn parameter_count_above_one_stays_on_gpm() {
    let counted = run_count_param(2);
    assert_eq!(counted.counts, Counts::default());
    assert_eq!(counted.rows, counted.gpm_rows);
}

/// [`shortcut`] without its 3→1 cycle: exactly two trails from 0 to 3, so
/// `ANY 2` returns both whatever order GPM enumerates them in.
fn two_trails() -> Graph {
    plain(5, &[(0, 1), (1, 2), (2, 3), (0, 4), (4, 3)])
}

#[test]
fn any_k_stays_on_gpm() {
    let g = two_trails();
    let q = "MATCH p = ANY 2 (a {id: 0})-[*]->(b {id: 3}) RETURN length(p)";
    assert_stays_on_gpm(&g, q, &[2, 3]);
}

#[test]
fn acyclic_and_simple_modes_stay_on_gpm() {
    let g = shortcut();
    for mode in ["ACYCLIC", "SIMPLE"] {
        let q = format!(
            "MATCH p = ANY SHORTEST {mode} (a {{id: 0}})-[*]->(b {{id: 3}}) RETURN length(p)"
        );
        assert_stays_on_gpm(&g, &q, &[2]);
    }
}

#[test]
fn min_hops_above_one_stays_on_gpm() {
    let g = shortcut();
    // The keyword selector: the `shortestPath()` function rejects a lower
    // bound above 1 at validation (42I08).
    let q = "MATCH p = ANY SHORTEST (a {id: 0})-[*2..5]->(b {id: 3}) RETURN length(p)";
    assert_stays_on_gpm(&g, q, &[2]);
}

/// The end node's inline WHERE rides the route's path filter: GPM checks
/// it at every arrival with the relationship variable bound, the route
/// against each bound trail, so the route runs and answers what the GPM
/// route answers.
#[test]
fn inline_end_node_where_takes_the_route() {
    let g = shortcut();
    let q = "MATCH p = ANY SHORTEST (a {id: 0})-[*]->(b WHERE b.id = 3) RETURN length(p)";
    let (rows, counts) = run(&g, q);
    assert!(counts.route_runs > 0, "route declined {q}");
    let rows = sorted(rows);
    assert_eq!(rows, gpm(&g, q), "{q}");
    assert_eq!(rows, vec![vec![Value::Int(2)]]);
}

/// The end node's inline WHERE that names the path variable keeps GPM's
/// spelling: no arrival row binds the path yet, so GPM defers the check
/// past the selector (the reduce tail's candidate pass), which the route's
/// in-search predicate would not do.
#[test]
fn inline_end_node_where_reading_the_path_name_stays_on_gpm() {
    let g = shortcut();
    let q = "MATCH p = ANY SHORTEST (a {id: 0})-[*]->(b WHERE length(p) = 2) RETURN length(p)";
    assert_stays_on_gpm(&g, q, &[2, 2]);
}

/// A clause-level `WHERE` over the path is a postfilter for every keyword
/// selector, the postfix spelling included: the selector picks the one
/// shortest path, then the predicate drops it. The route answers it as the
/// residual's post-filter and agrees with GPM's enumeration.
#[test]
fn postfix_quantified_relationship_with_outer_path_where_agrees_with_gpm() {
    let g = shortcut();
    let q = "MATCH p = ANY SHORTEST (a {id: 0})-[:R]->{1,3}(b {id: 3}) \
             WHERE none(r IN relationships(p) WHERE r.w = 1) RETURN count(*)";
    let (rows, counts) = run(&g, q);
    assert!(counts.route_runs > 0, "expected the route for {q}");
    assert_eq!(sorted(rows), gpm(&g, q), "{q}");
}

#[test]
fn dynamic_relationship_type_stays_on_gpm() {
    let g = shortcut();
    // The CIP-28 postfix quantifier keeps the type expression legal, and
    // the route declines a quantified element.
    let q = "WITH 'R' AS t MATCH (a {id: 0}), (b {id: 3}) \
             MATCH p = ANY SHORTEST (a)-[:$(t)]->*(b) RETURN length(p)";
    assert_stays_on_gpm(&g, q, &[2]);
}

/// The postfix `-[:R]->{m,n}` spelling is a quantified var-length hop like
/// any other: it takes the route, and answers what GPM answers (which of
/// several equally short paths ANY SHORTEST returns is unspecified, so the
/// comparison is by length).
#[test]
fn postfix_quantified_relationship_takes_the_route() {
    let g = shortcut();
    for q in [
        "MATCH p = ANY SHORTEST (a {id: 0})-[:R]->{1,3}(b {id: 3}) RETURN length(p)",
        "MATCH p = ALL SHORTEST (a {id: 0})-[:R]->{1,3}(b {id: 3}) RETURN length(p)",
        "MATCH p = SHORTEST 1 (a {id: 0})-[:R]->{1,3}(b {id: 3}) RETURN length(p)",
    ] {
        assert!(guard_accepts(q), "guard rejected {q}");
        assert_eq!(on_route(&g, q), gpm(&g, q), "{q}");
    }
}

#[test]
fn repeatable_elements_stays_on_gpm() {
    let g = shortcut();
    let q = "MATCH REPEATABLE ELEMENTS p = ANY SHORTEST (a {id: 0})-[*1..4]->(b {id: 3}) \
             RETURN length(p)";
    assert_eq!(last_match(q).0, MatchMode::RepeatableElements);
    assert_stays_on_gpm(&g, q, &[2]);
}

#[test]
fn non_empty_used_set_or_bound_variables_reject() {
    let q = "MATCH p = ANY SHORTEST (a)-[r*]->(b) RETURN p";
    assert_eq!(last_match(q).0, MatchMode::DifferentRelationships);
    assert!(guard_accepts(q));
    assert!(!guard_accepts_with(
        q,
        &UsedSet::from_iter([7]),
        &Row::new()
    ));
    assert!(!guard_accepts(
        "MATCH p = SHORTEST GROUP (a)-[r*]->(b) RETURN p"
    ));
    for (var, value) in [
        ("r", Value::List(Vec::new().into())),
        ("a", Value::Null),
        // LABS-863: the null-bound end — the guard declines, so the
        // route's anchor never reads it (the GPM enumeration answers).
        ("b", Value::Null),
        ("b", Value::Int(1)),
    ] {
        let row = Row::from([(var.to_string(), value)]);
        assert!(!guard_accepts_with(q, &UsedSet::default(), &row), "{var}");
    }
}

/// 51N23 (live Neo4j 2026.10.1) is a per-input-row rejection, not a route
/// fact: a shape the guard declines and the forced-GPM enumeration reject
/// exactly as the route does.
#[test]
fn same_variable_function_pattern_rejects_off_the_route() {
    let g = plain(3, &[(0, 1), (1, 2)]);
    // A label expression is not followable, so the guard declines it.
    let declined = "MATCH p = shortestPath((a:R|S)-[:R]-(a)) RETURN p";
    assert!(!guard_accepts(declined));
    let err = try_exec_cypher(&g, declined)
        .err()
        .unwrap_or_else(|| panic!("{declined}: expected 51N23"));
    assert_common_end_nodes(&err, declined);
    for q in [
        "MATCH p = shortestPath((a)-[*]-(a)) RETURN p",
        "MATCH p = allShortestPaths((a)-[*]-(a)) RETURN p",
    ] {
        let err = with_forced_gpm(|| try_exec_cypher(&g, q))
            .err()
            .unwrap_or_else(|| panic!("{q}: expected 51N23"));
        assert_common_end_nodes(&err, q);
    }
}
