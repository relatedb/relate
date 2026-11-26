//! The hub join's cost gate (REL8-2253): the join enumerates side B once
//! from the base row, so a selective side A — few rows, filtered by the
//! planner's order — leaves the nested loop's per-hub expansions the
//! cheaper plan, while a side A that covers most hubs keeps the join.

use super::hub_join::{take_hub_join_engagements, with_hub_join_disabled};
use super::test_support::exec_cypher;
use super::*;
use crate::graph::Graph;

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
