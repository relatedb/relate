//! The hub join's cost gate (REL8-2253): the join enumerates side B once
//! from the base row, so a selective side A — few rows, filtered by the
//! planner's order — leaves the nested loop's per-hub expansions the
//! cheaper plan, while a side A that covers most hubs keeps the join.

use super::graph_stats::StartEstimate;
use super::hub_join::{
    is_guess, join_beats_nested, take_hub_join_engagements, with_hub_join_disabled,
};
use super::test_support::exec_cypher;
use super::*;
use crate::graph::Graph;
use relate_planner::Basis;

const PERSONS: usize = 2_000;
const INTERESTS: usize = 10;
const CITIES: usize = 200;

/// The graph-benchmark Q6 shape, shrunk: persons each have one interest
/// (round robin over a few) and one city (round robin over many).
fn interest_graph() -> Graph {
    let g = Graph::default();
    g.begin_write().unwrap();
    let node = |label: &str, key: &str, value: Value| {
        g.create_node(vec![label.into()], BTreeMap::from([(key.into(), value)]))
            .unwrap()
    };
    let interests: Vec<_> = (0..INTERESTS)
        .map(|i| node("Int", "name", Value::String(format!("int{i}").into())))
        .collect();
    let cities: Vec<_> = (0..CITIES)
        .map(|i| node("Cit", "name", Value::String(format!("city{i}").into())))
        .collect();
    for i in 0..PERSONS {
        let p = node("Per", "id", Value::Int(i as i64));
        g.create_rel(p, interests[i % INTERESTS], "HAS".into(), BTreeMap::new())
            .unwrap();
        g.create_rel(p, cities[i % CITIES], "LIVES".into(), BTreeMap::new())
            .unwrap();
    }
    g.commit_write();
    g
}

/// `sum` keeps the pair on the matcher (the path fold would take a bare
/// count).
fn query(interest_filter: &str) -> String {
    format!(
        "MATCH (p:Per)-[:HAS]->(i:Int {interest_filter}), (p)-[:LIVES]->(c:Cit) \
         RETURN c.name AS city, sum(p.id) AS total ORDER BY city"
    )
}

/// Run `cypher`, assert the hub join engaged `engagements` times (0 = the
/// nested loop took the pair), and assert the rows equal the hub-join-
/// disabled run's: the A/B is real because the hook disengages the join.
fn assert_engagements_and_rows(g: &Graph, cypher: &str, engagements: u32) -> Vec<Vec<Value>> {
    let _ = take_hub_join_engagements();
    let ran = exec_cypher(g, cypher);
    assert_eq!(take_hub_join_engagements(), engagements);
    with_hub_join_disabled(|| assert_eq!(exec_cypher(g, cypher).rows, ran.rows));
    ran.rows
}

#[test]
fn a_selective_side_a_declines_the_hub_join() {
    let g = interest_graph();
    // 200 side-A rows (one interest of ten): above the row-count floor, yet
    // expanding side B from 200 bound persons beats enumerating all 2,000
    // person-city pairs.
    let rows = assert_engagements_and_rows(&g, &query("{name: 'int0'}"), 0);
    // persons 0, 10, 20, ... live in cities 0, 10, 20, ... (mod 200)
    assert_eq!(rows.len(), CITIES / INTERESTS);
}

#[test]
fn a_side_a_covering_every_hub_keeps_the_hub_join() {
    // every person is a side-A row, so the one full enumeration of side B
    // is no dearer than the per-person expansions.
    assert_engagements_and_rows(&interest_graph(), &query(""), 1);
}

fn priced(rows: u64, basis: Basis) -> StartEstimate {
    StartEstimate { rows, basis }
}

/// Every rung of the basis vocabulary, in trust order, with whether it
/// is a guess: the margin threshold is asserted once, here.
fn every_basis() -> [(Basis, bool); 7] {
    use relate_planner::Provenance;
    [
        (Basis::Rank, true),
        (Basis::Default, true),
        (Basis::Sampled, false),
        (Basis::Statistic(Provenance::Sketched), false),
        (Basis::Statistic(Provenance::UpperBound), false),
        (Basis::Statistic(Provenance::Exact), false),
        (Basis::Measured, false),
    ]
}

#[test]
fn the_join_ties_unless_the_price_is_a_guess_and_then_needs_the_margin() {
    for (basis, guess) in every_basis() {
        assert_eq!(is_guess(priced(100, basis)), guess, "{basis:?}");
        assert!(!join_beats_nested(priced(100, basis), 99), "{basis:?}");
        assert_eq!(
            join_beats_nested(priced(100, basis), 100),
            !guess,
            "{basis:?}"
        );
        assert_eq!(
            join_beats_nested(priced(100, basis), 199),
            !guess,
            "{basis:?}"
        );
        assert!(join_beats_nested(priced(100, basis), 200), "{basis:?}");
    }
}

#[test]
fn a_strictly_dearer_join_never_wins_on_basis() {
    assert!(!join_beats_nested(priced(101, Basis::Measured), 100));
}

#[test]
fn an_equal_price_keeps_the_more_trustworthy_basis_and_fewer_rows_still_win() {
    let guess = priced(10, Basis::Default);
    let measured = priced(10, Basis::Measured);
    assert_eq!(guess.cheaper(measured), measured);
    assert_eq!(measured.cheaper(guess), measured);
    assert_eq!(
        measured.cheaper(priced(9, Basis::Rank)),
        priced(9, Basis::Rank)
    );
}
