//! The endpoint-anchor arms of the shortest route (ADR-0556): property-map
//! endpoints against the route-versus-GPM oracle, every arm of
//! `Route::anchor` — the pair product within the candidate bound, the
//! over-bound fallbacks (fan from the shorter side, from the end, from the
//! start scan) and the loop — across all six selectors and directions on
//! seeded random graphs; plus shortest paths inside a write statement,
//! where the scratch overlay holds uncommitted nodes and relationships.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use relate_core::{CapacityKind, Error, Value};
use relate_ir::Direction;

use crate::exec::test_support::{
    exec_autocommit, exec_write_uncommitted, try_exec_cypher, Builder,
};
use crate::graph::Graph;

use super::fixtures::{counting, gpm, run, sorted, DirectionExt, Selector, DIRS, SELECTORS};
use super::route::{with_list_limit, AnchorArm};

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

/// The arm's display text, leading a shape's failure label. The arm
/// vocabulary is [`AnchorArm`], the one home of `Route::anchor`'s arms; a
/// shape names its row with the typed variant, never a display string, so
/// the pair-arm identification test selects with
/// `shape.arm == AnchorArm::Pair` and a reworded label cannot shrink
/// coverage.
fn arm_label(arm: AnchorArm) -> &'static str {
    match arm {
        AnchorArm::Pair => "pair",
        AnchorArm::FanForward => "fan forward",
        AnchorArm::FanBackward => "fan backward",
        AnchorArm::Loop => "loop",
    }
}

/// One endpoint shape: the pattern's two node patterns, the `RETURN`
/// columns naming its endpoints, the arm of `Route::anchor` it exercises
/// and the node count and group count of the graph that puts it there.
struct Shape {
    arm: AnchorArm,
    /// The row's short name within its arm's family, for failure messages.
    name: &'static str,
    /// The `RETURN` columns before the trail, naming the pattern's
    /// endpoints (one name for the loop, two otherwise).
    cols: &'static str,
    start: &'static str,
    end: &'static str,
    n: usize,
    groups: usize,
    rels: usize,
}

impl Shape {
    /// The shape's failure label: its arm's family plus its row name.
    fn label(&self) -> String {
        format!("{} {}", arm_label(self.arm), self.name)
    }
}

#[rustfmt::skip]
const SHAPES: [Shape; 11] = [
    // 1 x 1: the plain pair.
    Shape { arm: AnchorArm::Pair, name: "1x1", cols: "a.id, b.id, length(p)", start: "(a {id: '0'})", end: "(b {id: '5'})", n: 12, groups: 3, rels: 26 },
    // 4 x 1: several starts, one end, product within the bound.
    Shape { arm: AnchorArm::Pair, name: "4x1", cols: "a.id, b.id, length(p)", start: "(a {g: 0})", end: "(b {id: '5'})", n: 12, groups: 3, rels: 26 },
    // 4 x 4 = 16: exactly the bound, still the pair product.
    Shape { arm: AnchorArm::Pair, name: "4x4", cols: "a.id, b.id, length(p)", start: "(a {g: 0})", end: "(b {g: 1})", n: 12, groups: 3, rels: 26 },
    // 6 x 6 = 36: past the bound, equal sides — the fan from the start.
    Shape { arm: AnchorArm::FanForward, name: "6x6", cols: "a.id, b.id, length(p)", start: "(a {g: 0})", end: "(b {g: 1})", n: 12, groups: 2, rels: 26 },
    // 20 x 1: past the bound with the end the shorter side — the fan from the end.
    Shape { arm: AnchorArm::FanBackward, name: "20x1", cols: "a.id, b.id, length(p)", start: "(a {g: 0})", end: "(b {id: '5'})", n: 40, groups: 2, rels: 44 },
    // bare start, end list past the bound: the start scan fans forward.
    Shape { arm: AnchorArm::FanForward, name: "start scan", cols: "a.id, b.id, length(p)", start: "(a)", end: "(b {g: 1})", n: 40, groups: 2, rels: 44 },
    // 1 x 20: the end list is past the bound and never built — the fan
    // from the one start.
    Shape { arm: AnchorArm::FanForward, name: "fan 1x20", cols: "a.id, b.id, length(p)", start: "(a {id: '0'})", end: "(b {g: 1})", n: 40, groups: 2, rels: 44 },
    // 20 x 20: both lists past the bound — each built in full for the
    // shorter-side choice.
    Shape { arm: AnchorArm::FanForward, name: "fan 20x20", cols: "a.id, b.id, length(p)", start: "(a {g: 0})", end: "(b {g: 1})", n: 40, groups: 2, rels: 44 },
    // start list past the bound, bare end: the start list fans forward.
    Shape { arm: AnchorArm::FanForward, name: "20 x scan", cols: "a.id, b.id, length(p)", start: "(a {g: 0})", end: "(b)", n: 40, groups: 2, rels: 44 },
    // bare start, one end: the end fans backward.
    Shape { arm: AnchorArm::FanBackward, name: "scan x 1", cols: "a.id, b.id, length(p)", start: "(a)", end: "(b {id: '5'})", n: 12, groups: 3, rels: 26 },
    // one unbound variable at both ends: the loop anchor, even though the
    // map is property-seekable.
    Shape { arm: AnchorArm::Loop, name: "same variable", cols: "a.id, length(p)", start: "(a {g: 0})", end: "(a)", n: 12, groups: 3, rels: 24 },
];

/// The `MATCH` of `shape` under selector `sel`, direction `dir` and a bound
/// of four hops, projecting the endpoints, the length and the node ids.
fn query(shape: &Shape, sel: Selector, dir: Direction) -> String {
    let Selector { open, close, .. } = sel;
    let (l, r) = dir.arrows();
    format!(
        "MATCH p = {open}{}{l}[:R*..4]{r}{}{close} \
         RETURN {}, [n IN nodes(p) | n.id]",
        shape.start, shape.end, shape.cols
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
/// the route answers what the enumerating GPM walk answers, the route took
/// the pattern, and `Route::anchor` took the arm the shape names.
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
                        shape.label()
                    );
                    assert_eq!(
                        counts.arms,
                        vec![shape.arm],
                        "{} graph {index}: Route::anchor took the wrong arm for {q}",
                        shape.label()
                    );
                    assert_eq!(
                        comparable(rows, sel),
                        comparable(gpm(&g, &q), sel),
                        "{} graph {index}: route and GPM disagree on {q}",
                        shape.label()
                    );
                }
            }
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

/// The pair bound is probed, not enumerated: a seek past the bound is
/// answered with the same rows, and the funnel stops at the first match
/// past it — the node reads stay at the bound however many nodes match.
/// An empty side beside an over-bound side is an empty product.
#[test]
fn an_over_bound_seek_is_not_materialized() {
    let g = Graph::default();
    exec_autocommit(
        &g,
        "UNWIND range(0, 199) AS i CREATE (:N {id: toString(i), g: i % 2})",
    )
    .unwrap();
    exec_autocommit(
        &g,
        "MATCH (a:N {id: '0'}), (b:N) WHERE b.g = 1 CREATE (a)-[:R]->(b)",
    )
    .unwrap();
    let q = "MATCH p = ANY SHORTEST (a {id: '0'})-[:R*..2]->(b {g: 1}) RETURN b.id, length(p)";
    let _ = crate::ctx::take_node_reads();
    let (rows, counts) = run(&g, q);
    let reads = crate::ctx::take_node_reads();
    assert!(counts.route_runs > 0, "the route must take {q}");
    assert_eq!(rows.len(), 100);
    assert_eq!(sorted(rows), gpm(&g, q));
    // Db Hits count the nodes a scan examines (ADR-0567): the unlabeled
    // start map reads all 200 nodes to find its one match, and the end
    // probe examines 34 to find its 17 (every other node matches `g = 1`)
    // and stops; the 100-node end list is never listed. Before examined
    // nodes counted, this was 1 + 17 reads.
    assert_eq!(reads, 200 + 34, "the end seek was materialized");

    // An empty start beside an over-bound end: no pair, no row.
    let none = "MATCH p = ANY SHORTEST (a {id: 'nope'})-[:R*..2]->(b {g: 1}) RETURN length(p)";
    let (rows, counts) = run(&g, none);
    assert!(counts.route_runs > 0, "the route must take {none}");
    assert!(rows.is_empty());
    let none = "MATCH p = ANY SHORTEST (a {g: 1})-[:R*..2]->(b {id: 'nope'}) RETURN length(p)";
    assert!(run(&g, none).0.is_empty());
}

/// A write view's scratch deletions can hide base matches, so a probe
/// that stops at the bound must not trust a truncated base answer: 20
/// committed ends, 10 deleted earlier in the statement, leaves 10 — a
/// within-bound list the pair arm answers in full.
#[test]
fn a_capped_seek_over_a_write_view_counts_what_survives_the_scratch() {
    let g = Graph::default();
    exec_autocommit(
        &g,
        "CREATE (s:N {id: 's'}) \
         WITH s UNWIND range(0, 19) AS i \
         CREATE (s)-[:R]->(:N {id: toString(i), g: 0, k: i})",
    )
    .unwrap();
    let q = "MATCH (n:N {g: 0}) WHERE n.k < 10 DETACH DELETE n \
             WITH count(*) AS gone \
             MATCH p = ANY SHORTEST (a {id: 's'})-[:R*..2]->(b {g: 0}) \
             RETURN b.id, length(p)";
    let (rows, counts) = counting(|| exec_write_uncommitted(&g, q).rows);
    assert!(counts.route_runs > 0, "the route must take the write form");
    assert_eq!(rows.len(), 10, "{rows:?}");
}

/// The pair arm is identified, not just answered: within the bound the
/// property-map form takes exactly the pair partitions the row-bound form
/// takes, so the two follow the same relationships. Driven by the `AnchorArm::Pair`
/// rows of [`SHAPES`] — one table, one set of graph parameters. (Past the
/// bound the fan answers; the parity test above pins its rows.)
#[test]
fn pair_arms_follow_the_row_bound_forms_expansions() {
    let mut rng = StdRng::seed_from_u64(SEED ^ 2);
    let pairs: Vec<&Shape> = SHAPES.iter().filter(|s| s.arm == AnchorArm::Pair).collect();
    assert!(
        !pairs.is_empty(),
        "the pair arm must have shapes to identify"
    );
    for shape in pairs {
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
        // The property form's plan may feed one route run per bound
        // endpoint row; every such run must take the pair arm, never a fan.
        assert!(
            !seek_counts.arms.is_empty() && seek_counts.arms.iter().all(|a| *a == AnchorArm::Pair),
            "the property form must take the pair arm: {seek} (got {:?})",
            seek_counts.arms
        );
        assert!(
            !bound_counts.arms.is_empty()
                && bound_counts.arms.iter().all(|a| *a == AnchorArm::Pair),
            "the bound form must take the pair arm: {seek} (got {:?})",
            bound_counts.arms
        );
        assert_eq!(sorted(seek_rows), sorted(bound_rows), "{seek}");
        assert_eq!(
            seek_counts.expands,
            bound_counts.expands,
            "the {} property form must take the pair partitions the bound form takes: {seek}",
            shape.label()
        );
    }
}

/// ADR-0567: both sides past the pair bound are listed in full for the
/// shorter-side choice, through the capped funnel. A side past the listing
/// limit is the typed capacity refusal — no truncation, no unbounded
/// fallback — and the funnel's node reads stay at the limit plus one per
/// side instead of every match. Within the limit the rows are unchanged.
#[test]
fn an_over_limit_listing_is_a_typed_capacity_refusal() {
    let g = Graph::default();
    exec_autocommit(
        &g,
        "UNWIND range(0, 199) AS i CREATE (:N {id: toString(i), g: i % 2})",
    )
    .unwrap();
    let q = "MATCH p = ANY SHORTEST (a {g: 0})-[:R*..2]->(b {g: 1}) RETURN length(p)";
    let _ = crate::ctx::take_node_reads();
    let err = with_list_limit(40, || try_exec_cypher(&g, q))
        .err()
        .expect("100 matches past a 40-node listing limit must refuse");
    assert!(
        matches!(
            err,
            Error::Capacity {
                used: 41,
                limit: 40,
                kind: CapacityKind::SeekCandidates,
            }
        ),
        "{err}"
    );
    // Examined nodes: the two pair probes, each stopped at 17 matches among
    // alternating nodes, then the first side's listing stopped at 41; the
    // refusal comes before the second side is listed, so 200 are never all
    // read per side. Before examined nodes counted: 75.
    assert_eq!(crate::ctx::take_node_reads(), 148);
    // The same shape within the limit answers.
    assert!(with_list_limit(200, || try_exec_cypher(&g, q)).is_ok());
}

/// A negated-label endpoint is not probed: the funnel answers it with the
/// all-nodes superset, which would read as an over-limit match count. Even
/// with the listing limit lowered under the node count, the statement
/// answers, with the rows of the GPM oracle (ADR-0567).
#[test]
fn a_negated_label_endpoint_is_never_refused_by_the_listing_limit() {
    let g = Graph::default();
    exec_autocommit(
        &g,
        "UNWIND range(0, 99) AS i CREATE (:N {id: toString(i), g: i % 50}), (:M {id: 'm', g: 1})",
    )
    .unwrap();
    exec_autocommit(
        &g,
        "MATCH (a:N {id: '0'}), (b:N {g: 1}) CREATE (a)-[:R]->(b)",
    )
    .unwrap();
    let q =
        "MATCH p = ANY SHORTEST (a:!M {g: 0})-[:R*..2]->(b:!M {g: 1}) RETURN a.id, b.id, length(p)";
    let rows = with_list_limit(4, || try_exec_cypher(&g, q))
        .expect("a superset list must not trip the listing refusal")
        .rows;
    assert_eq!(sorted(rows), gpm(&g, q));
}
