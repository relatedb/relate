//! `SHORTEST k` and `SHORTEST k GROUPS` on the length-class route
//! (ADR-0607) against the enumeration it replaces: a seeded corpus of small
//! random graphs (parallel relationships, self-loops, two types) run the
//! selector x direction x bound x type matrix on both, and named witnesses
//! pin the declines, the post-filter, the halt and the bounded work.

use std::collections::BTreeMap;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use relate_core::Value;
use relate_ir::Direction;

use crate::exec::test_support::build;
use crate::graph::Graph;

use super::fixtures::{gpm, gpm_params, run, run_params, sorted, DirectionExt, DIRS};

const SEED: u64 = 0x0607_0607;
const GRAPHS: usize = 60;

/// The count selectors the route answers.
const SELECTORS: [&str; 6] = [
    "SHORTEST 2 ",
    "SHORTEST 3 ",
    "SHORTEST 5 ",
    "SHORTEST 2 GROUPS ",
    "SHORTEST 3 GROUPS ",
    "ANY 2 ",
];

const BOUNDS: [&str; 4] = ["*..3", "*1..4", "*0..3", "*"];
const TYPES: [&str; 3] = ["", ":A", ":A|B"];

/// The result columns: the endpoints, the length, the node-id sequence and
/// the relationship-type sequence. Which of several parallel relationships a
/// tied pick binds is unspecified, so relationship ids are not compared.
const COLUMNS: &str = "a.id AS s, b.id AS e, length(p) AS len, \
                       [n IN nodes(p) | n.id] AS ns, [r IN relationships(p) | type(r)] AS ts";

fn graph(n: usize, rels: &[(usize, usize, &str)]) -> Graph {
    build(&vec![&["X"][..]; n], rels).graph
}

fn random_graph(rng: &mut StdRng) -> (usize, Vec<(usize, usize, &'static str)>) {
    let n = rng.random_range(3..=6);
    // Few enough relationships for the unbounded enumeration to finish.
    let m = rng.random_range(2..=8);
    let mut rels: Vec<(usize, usize, &'static str)> = Vec::new();
    for _ in 0..m {
        let t = if rng.random_bool(0.6) { "A" } else { "B" };
        let (a, b) = if !rels.is_empty() && rng.random_bool(0.25) {
            let (a, b, _) = rels[rng.random_range(0..rels.len())];
            if rng.random_bool(0.5) {
                (a, b)
            } else {
                (b, a)
            }
        } else {
            (rng.random_range(0..n), rng.random_range(0..n))
        };
        rels.push((a, b, t));
    }
    (n, rels)
}

fn query(selector: &str, dir: Direction, types: &str, bound: &str, s: usize, e: usize) -> String {
    let (l, r) = dir.arrows();
    format!(
        "MATCH p = {selector}(a {{id: {s}}}){l}[r{types}{bound}]{r}(b {{id: {e}}}) \
         RETURN {COLUMNS}"
    )
}

#[test]
fn class_route_matches_enumeration_on_random_graphs() {
    let mut rng = StdRng::seed_from_u64(SEED);
    let (mut routed, mut total) = (0usize, 0usize);
    for index in 0..GRAPHS {
        let (n, rels) = random_graph(&mut rng);
        let g = graph(n, &rels);
        let unbounded_ok = rels.len() <= 7;
        for selector in SELECTORS {
            for dir in DIRS {
                for (bi, bound) in BOUNDS.iter().enumerate() {
                    if *bound == "*" && !unbounded_ok {
                        continue;
                    }
                    let types = TYPES[(index + bi) % TYPES.len()];
                    let s = rng.random_range(0..n);
                    // One in four pairs is a closed one (start = end).
                    let e = if rng.random_bool(0.25) {
                        s
                    } else {
                        rng.random_range(0..n)
                    };
                    let q = query(selector, dir, types, bound, s, e);
                    let (rows, counts) = run(&g, &q);
                    let expected = gpm(&g, &q);
                    assert_eq!(
                        sorted(rows),
                        expected,
                        "graph #{index} (n={n}, rels={rels:?}): {q}"
                    );
                    routed += usize::from(counts.route_runs > 0);
                    total += 1;
                }
            }
        }
    }
    assert_eq!(
        routed, total,
        "every pair-anchored count selector takes the class route"
    );
}

/// Two routes `0-1-3`, `0-2-3` and a longer `0-1-2-3`, `0-2-1-3`, plus a
/// parallel `0-1`: ties at every length, multi-edges, and a cycle.
fn tied() -> Graph {
    graph(
        4,
        &[
            (0, 1, "A"),
            (0, 1, "A"),
            (1, 3, "A"),
            (0, 2, "B"),
            (2, 3, "A"),
            (1, 2, "A"),
            (3, 0, "B"),
        ],
    )
}

#[test]
fn ties_and_multi_edges_pick_the_enumerations_trails() {
    let g = tied();
    for selector in SELECTORS {
        for dir in DIRS {
            for k in 0..4 {
                let q = query(selector, dir, "", "*..5", k, 3);
                let (rows, counts) = run(&g, &q);
                assert!(counts.route_runs > 0, "declined: {q}");
                assert_eq!(sorted(rows), gpm(&g, &q), "{q}");
            }
        }
    }
}

#[test]
fn a_post_filter_applies_after_the_selection() {
    let g = tied();
    for selector in ["SHORTEST 3 ", "SHORTEST 2 GROUPS "] {
        for predicate in ["length(p) >= 3", "length(p) <= 2", "size(r) = 2"] {
            let q = format!(
                "MATCH p = {selector}(a {{id: 0}})-[r:A|B*..5]-(b {{id: 3}}) \
                 WHERE {predicate} RETURN {COLUMNS}"
            );
            let (rows, counts) = run(&g, &q);
            assert!(counts.route_runs > 0, "declined: {q}");
            assert_eq!(sorted(rows), gpm(&g, &q), "{q}");
        }
    }
}

#[test]
fn a_count_parameter_and_a_limit_ride_the_route() {
    let g = tied();
    let params = BTreeMap::from([("k".to_string(), Value::Int(3))]);
    for selector in ["SHORTEST $k ", "SHORTEST $k GROUPS "] {
        let q = format!("MATCH p = {selector}(a {{id: 0}})-[*..5]-(b {{id: 3}}) RETURN {COLUMNS}");
        let (rows, counts) = run_params(&g, &q, &params);
        assert!(counts.route_runs > 0, "declined: {q}");
        assert_eq!(sorted(rows), gpm_params(&g, &q, &params), "{q}");
    }
    // A halting consumer stops the walk with the rows it asked for.
    let q = "MATCH p = SHORTEST 4 (a {id: 0})-[*..5]-(b {id: 3}) RETURN length(p) AS len LIMIT 2";
    let (rows, counts) = run(&g, q);
    assert!(counts.route_runs > 0);
    assert_eq!(rows.len(), 2);
}

#[test]
fn shapes_the_class_walk_does_not_answer_stay_on_enumeration() {
    let g = tied();
    for q in [
        // a fan: one end open
        "MATCH p = SHORTEST 2 (a {id: 0})-[*..3]-(b) RETURN length(p) AS len",
        // an explicit WALK mode
        "MATCH p = SHORTEST 2 WALK (a {id: 0})-[*..3]-(b {id: 3}) RETURN length(p) AS len",
        // an inline predicate filters the search
        "MATCH p = SHORTEST 2 (a {id: 0})-[*..3]-(b {id: 3} WHERE b.id = 3) RETURN length(p) AS len",
    ] {
        let (rows, counts) = run(&g, q);
        assert_eq!(counts.route_runs, 0, "{q} took the route");
        assert_eq!(sorted(rows), gpm(&g, q), "{q}");
    }
}

/// The complete graph on `n` nodes (one relationship per ordered pair
/// `i < j`): the trails between two nodes within `n - 1` hops number in the
/// millions, which the enumeration would have to list.
fn complete(n: usize) -> Graph {
    let mut rels = Vec::new();
    for i in 0..n {
        for j in i + 1..n {
            rels.push((i, j, "R"));
        }
    }
    graph(n, &rels)
}

#[test]
fn the_work_follows_the_class_not_the_trail_space() {
    let g = complete(11);
    // The direct edge, then the nine two-hop trails: k = 4 keeps three of
    // them (4 rows); two groups keep all nine (10 rows).
    for (selector, expected) in [("SHORTEST 4 ", 4), ("SHORTEST 2 GROUPS ", 10)] {
        let q = format!(
            "MATCH p = {selector}(a {{id: 0}})-[:R*..10]-(b {{id: 10}}) RETURN length(p) AS len"
        );
        let (rows, counts) = run(&g, &q);
        assert!(counts.route_runs > 0, "declined: {q}");
        assert_eq!(rows.len(), expected, "{q}");
        assert!(
            counts.expands < 5_000,
            "{q} followed {} relationships",
            counts.expands
        );
    }
}

/// An unbounded pattern with `k` above the available trails stops at the
/// real longest trail, not at the graph's relationship count: the small
/// component here has a handful of trails while a 300-relationship path
/// elsewhere in the graph sets the live count the sweep used to walk to.
#[test]
fn an_unbounded_sweep_stops_at_the_real_longest_trail() {
    let mut rels = vec![
        (0, 1, "A"),
        (0, 1, "A"),
        (1, 3, "A"),
        (0, 2, "B"),
        (2, 3, "A"),
    ];
    rels.extend((4..304).map(|i| (i, i + 1, "A")));
    let g = graph(305, &rels);
    for selector in ["SHORTEST 500 ", "SHORTEST 500 GROUPS "] {
        let q = query(selector, Direction::Both, "", "*", 0, 3);
        let (rows, counts) = run(&g, &q);
        assert!(counts.route_runs > 0, "declined: {q}");
        assert_eq!(sorted(rows), gpm(&g, &q), "{q}");
        assert!(
            counts.expands < 300,
            "{q} followed {} relationships",
            counts.expands
        );
    }
}

/// The hub fixture's hubs: neighbours of the start.
const HUBS: usize = 60;
/// Dead-end leaves each hub carries.
const LEAVES_PER_HUB: usize = 200;
/// Every leaf of the hub fixture.
const LEAF_COUNT: usize = HUBS * LEAVES_PER_HUB;

/// A hub start: `HUBS` neighbours of the start, each with `LEAVES_PER_HUB`
/// dead-end leaves and one relationship to `m`, which reaches the end in two
/// more hops. The shortest trails are the `HUBS` routes
/// `0 - hub - m - y - end`.
fn hub_start() -> (Graph, usize) {
    let (m, y, end) = (
        HUBS + LEAF_COUNT + 1,
        HUBS + LEAF_COUNT + 2,
        HUBS + LEAF_COUNT + 3,
    );
    let mut rels = Vec::new();
    for hub in 1..=HUBS {
        rels.push((0, hub, "R"));
        rels.push((hub, m, "R"));
        for leaf in 0..LEAVES_PER_HUB {
            rels.push((hub, HUBS + 1 + (hub - 1) * LEAVES_PER_HUB + leaf, "R"));
        }
    }
    rels.extend([(m, y, "R"), (y, end, "R")]);
    (graph(end + 1, &rels), end)
}

#[test]
fn a_hub_start_does_not_scan_the_neighbours_it_cannot_leave_through() {
    let (g, end) = hub_start();
    for (selector, rows) in [("SHORTEST 2 ", 2), ("SHORTEST 2 GROUPS ", HUBS)] {
        let q = format!(
            "MATCH p = {selector}(a {{id: 0}})-[:R*..4]-(b {{id: {end}}}) RETURN length(p) AS len"
        );
        let (got, counts) = run(&g, &q);
        assert!(counts.route_runs > 0, "declined: {q}");
        assert_eq!(got.len(), rows, "{q}");
        // The hubs hold `LEAF_COUNT` leaf hops. The walk reads each hub's one hop
        // towards the end from the end ball instead of scanning the hub.
        assert!(
            counts.expands < 1_000,
            "{q} followed {} relationships",
            counts.expands
        );
    }
}
