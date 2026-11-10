//! The endpoint-anchor arms of the shortest route (ADR-0556): property-map
//! endpoints against the route-versus-GPM oracle, every arm of
//! `Route::anchor` — the pair product within the candidate bound, the
//! over-bound fallbacks (fan from the shorter side, from the end, from the
//! start scan) and the loop — across all six selectors and directions on
//! seeded random graphs; plus shortest paths inside a write statement,
//! where the scratch overlay holds uncommitted nodes and relationships.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use relate_core::Value;

use crate::exec::test_support::{exec_autocommit, exec_write_uncommitted, Builder};
use crate::graph::Graph;

use super::fixtures::{counting, gpm, run, sorted, Dir, Selector, DIRS, SELECTORS};

/// Corpus seed; a failure names it with the graph index.
const SEED: u64 = 0x0556_0001;

/// A random graph: `n` nodes named `0..n` (`id`), grouped by `g = i % groups`
/// (the property maps with several candidates), `rels` random `R`
/// relationships — self-loops and parallels included.
fn graph(rng: &mut StdRng, n: usize, groups: usize, rels: usize) -> Graph {
    let b = Builder::new();
    let nodes: Vec<_> = (0..n)
        .map(|i| {
            b.prop_node(
                &i.to_string(),
                &["N"],
                &[("g", Value::Int((i % groups) as i64))],
            )
        })
        .collect();
    for _ in 0..rels {
        let (a, z) = (rng.random_range(0..n), rng.random_range(0..n));
        b.rel(nodes[a], nodes[z], "R", &[]);
    }
    b.done()
}

/// One endpoint shape: the pattern's two node patterns, with the arm of
/// `Route::anchor` it exercises (named in a failure) and the node count and
/// group count of the graph that puts it there.
struct Shape {
    arm: &'static str,
    /// Whether the candidate product is within the pair bound — the shapes
    /// the pair arm answers (the others fall back to a fan).
    pair: bool,
    start: &'static str,
    end: &'static str,
    n: usize,
    groups: usize,
    rels: usize,
}

#[rustfmt::skip]
const SHAPES: [Shape; 6] = [
    // 1 x 1: the plain pair.
    Shape { arm: "pair 1x1", pair: true, start: "(a {id: '0'})", end: "(b {id: '5'})", n: 12, groups: 3, rels: 26 },
    // 4 x 1: several starts, one end, product within the bound.
    Shape { arm: "pair 4x1", pair: true, start: "(a {g: 0})", end: "(b {id: '5'})", n: 12, groups: 3, rels: 26 },
    // 4 x 4 = 16: exactly the bound, still the pair product.
    Shape { arm: "pair 4x4", pair: true, start: "(a {g: 0})", end: "(b {g: 1})", n: 12, groups: 3, rels: 26 },
    // 6 x 6 = 36: past the bound, equal sides — the fan from the start.
    Shape { arm: "fan 6x6", pair: false, start: "(a {g: 0})", end: "(b {g: 1})", n: 12, groups: 2, rels: 26 },
    // 20 x 1: past the bound with the end the shorter side — the fan from the end.
    Shape { arm: "fan 20x1", pair: false, start: "(a {g: 0})", end: "(b {id: '5'})", n: 40, groups: 2, rels: 44 },
    // bare start, end list past the bound: the start scan fans forward.
    Shape { arm: "scan x 20", pair: false, start: "(a)", end: "(b {g: 1})", n: 40, groups: 2, rels: 44 },
];

/// The `MATCH` of `shape` under selector `sel`, direction `dir` and a bound
/// of four hops, projecting the endpoints, the length and the node ids.
fn query(shape: &Shape, sel: Selector, dir: Dir) -> String {
    let Selector { open, close, .. } = sel;
    let (l, r) = dir.arrows();
    format!(
        "MATCH p = {open}{}{l}[:R*..4]{r}{}{close} \
         RETURN a.id, b.id, length(p), [n IN nodes(p) | n.id]",
        shape.start, shape.end
    )
}

/// The comparable form of a result (every query here returns the node-id
/// trail as its last column): a single-path selector may break a tie on a
/// different trail, so it compares everything but the trail; an all-path
/// selector compares every trail.
fn comparable(rows: Vec<Vec<Value>>, sel: Selector) -> Vec<Vec<Value>> {
    let rows = rows.into_iter().map(|mut row| {
        if !sel.all {
            row.pop();
        }
        row
    });
    let mut rows = sorted(rows.collect());
    if !sel.all {
        rows.dedup();
    }
    rows
}

/// Every anchor shape × selector × direction on a handful of seeded graphs:
/// the route answers what the enumerating GPM walk answers, and the route
/// took the pattern.
#[test]
fn property_endpoint_anchors_agree_with_the_gpm_walk() {
    let mut rng = StdRng::seed_from_u64(SEED);
    for shape in &SHAPES {
        for index in 0..3 {
            let g = graph(&mut rng, shape.n, shape.groups, shape.rels);
            for sel in SELECTORS {
                for dir in DIRS {
                    let q = query(shape, sel, dir);
                    let (rows, counts) = run(&g, &q);
                    assert!(
                        counts.route_runs > 0,
                        "{} graph {index}: the route must take {q}",
                        shape.arm
                    );
                    assert_eq!(
                        comparable(rows, sel),
                        comparable(gpm(&g, &q), sel),
                        "{} graph {index}: route and GPM disagree on {q}",
                        shape.arm
                    );
                }
            }
        }
    }
}

/// `(a {g: 0})-[*]-(a)`: one unbound variable at both ends keeps the loop
/// anchor even when the map is property-seekable.
#[test]
fn property_map_loop_anchor_agrees_with_the_gpm_walk() {
    let mut rng = StdRng::seed_from_u64(SEED ^ 1);
    for index in 0..4 {
        let g = graph(&mut rng, 12, 3, 24);
        for sel in SELECTORS {
            let Selector { open, close, .. } = sel;
            let q = format!(
                "MATCH p = {open}(a {{g: 0}})-[:R*..4]-(a){close} \
                 RETURN a.id, length(p), [n IN nodes(p) | n.id]"
            );
            let (rows, counts) = run(&g, &q);
            assert!(
                counts.route_runs > 0,
                "graph {index}: the route must take {q}"
            );
            assert_eq!(
                comparable(rows, sel),
                comparable(gpm(&g, &q), sel),
                "graph {index}: route and GPM disagree on {q}"
            );
        }
    }
}

/// Shortest paths inside a write statement: the route runs over the write
/// view, so uncommitted nodes and relationships (here the whole path) are
/// seen by both the bound-pair form and the property-endpoint seek, and a
/// committed relationship deleted earlier in the statement is not followed.
#[test]
fn shortest_paths_see_the_write_views_uncommitted_graph() {
    let g = Graph::default();
    exec_autocommit(
        &g,
        "CREATE (:N {id: 'x'})-[:R]->(:N {id: 'm'})-[:R]->(:N {id: 'y'})",
    )
    .unwrap();
    let write = |q: &str| counting(|| exec_write_uncommitted(&g, q).rows);

    // Bound pair, bound by the CREATE in the same statement.
    let bound = "CREATE (a:N {id: 's'})-[:R]->(:N {id: 't'})-[:R]->(b:N {id: 'u'}) \
                 WITH a, b MATCH p = shortestPath((a)-[:R*..4]->(b)) RETURN length(p)";
    let (rows, counts) = write(bound);
    assert!(counts.route_runs > 0, "the route must take the write form");
    assert_eq!(sorted(rows), vec![vec![Value::Int(2)]]);

    // Property endpoints seeked over uncommitted nodes.
    let seek = "CREATE (:N {id: 'p'})-[:R]->(:N {id: 'q'})-[:R]->(:N {id: 'r'}) \
                WITH count(*) AS made \
                MATCH p = ANY SHORTEST (a {id: 'p'})-[:R*..4]->(b {id: 'r'}) \
                RETURN length(p)";
    let (rows, counts) = write(seek);
    assert!(counts.route_runs > 0, "the route must take the seek form");
    assert_eq!(sorted(rows), vec![vec![Value::Int(2)]]);

    // A committed relationship deleted earlier in the statement is hidden:
    // x -> m -> y loses x -> m, so no path remains.
    let hidden = "MATCH (:N {id: 'x'})-[r:R]->(:N {id: 'm'}) DELETE r \
                  WITH count(*) AS gone \
                  MATCH p = ANY SHORTEST (a {id: 'x'})-[:R*..4]->(b {id: 'y'}) \
                  RETURN length(p)";
    let (rows, counts) = write(hidden);
    assert!(
        counts.route_runs > 0,
        "the route must take the hidden-rel form"
    );
    assert_eq!(sorted(rows), Vec::<Vec<Value>>::new());
}

/// The pair arm is identified, not just answered: within the bound the
/// property-map form takes exactly the pair partitions the row-bound form
/// takes, so the two follow the same relationships. Driven by the `pair`
/// rows of [`SHAPES`] — one table, one set of graph parameters. (Past the
/// bound the fan answers; the parity test above pins its rows.)
#[test]
fn pair_arms_follow_the_row_bound_forms_expansions() {
    let mut rng = StdRng::seed_from_u64(SEED ^ 2);
    for shape in SHAPES.iter().filter(|s| s.pair) {
        let g = graph(&mut rng, shape.n, shape.groups, shape.rels);
        let (start, end) = (shape.start, shape.end);
        let seek = format!("MATCH p = ANY SHORTEST {start}-[:R*..4]->{end} RETURN length(p)");
        let bound = format!(
            "MATCH {start}, {end} \
             MATCH p = ANY SHORTEST (a)-[:R*..4]->(b) RETURN length(p)"
        );
        let (seek_rows, seek_counts) = run(&g, &seek);
        let (bound_rows, bound_counts) = run(&g, &bound);
        assert!(
            seek_counts.route_runs > 0 && bound_counts.route_runs > 0,
            "both forms must ride the route: {seek}"
        );
        assert_eq!(sorted(seek_rows), sorted(bound_rows), "{seek}");
        assert_eq!(
            seek_counts.expands, bound_counts.expands,
            "the {} property form must take the pair partitions the bound form takes: {seek}",
            shape.arm
        );
    }
}
