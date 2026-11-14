//! Unit tests for the frontier cost gate's estimate and break-even.

use relate_ir::{Direction, NodePattern, RelPattern};
use relate_names::NamedToken;
use relate_planner::{ContainsCount, DegreeMoments, PatternStats};

use super::frontier_cost::{estimate, excess_degree, mean_degree, CostEstimate};
use crate::exec::test_support::{
    build, exec_cypher, frontier_decline_of, named_node, one_hop_rel, profiled, served,
};
use crate::frontier_decline::{FrontierDecline, FrontierDeclineReason};
use crate::graph::Graph;
use crate::metrics::KernelRung;
use crate::pattern_engine::PatternEngine;

/// One uniform label: `nodes` nodes whose out-degree moments are fixed.
struct Uniform {
    nodes: u64,
    moments: Option<DegreeMoments>,
    /// The in-degree sum the pair counts keep (`None`: no pair counts).
    in_sum: Option<u128>,
    /// Degree-moments consults made, each a histogram scan on the live graph.
    consults: std::cell::Cell<usize>,
}

impl Uniform {
    /// `nodes` nodes whose degree moments are `sum` and `sum_sq`.
    fn new(nodes: u64, sum: u128, sum_sq: u128) -> Self {
        Self {
            nodes,
            moments: Some(DegreeMoments { sum, sum_sq }),
            // A symmetric label: as many rels arrive as leave.
            in_sum: Some(sum),
            consults: Default::default(),
        }
    }

    /// A label with `out` rels leaving and `inbound` arriving.
    fn directed(nodes: u64, out: u128, inbound: u128) -> Self {
        Self {
            in_sum: Some(inbound),
            ..Self::new(nodes, out, out * 4)
        }
    }

    /// No histogram: nothing to price with.
    fn unpriceable(nodes: u64) -> Self {
        Self {
            nodes,
            moments: None,
            in_sum: None,
            consults: Default::default(),
        }
    }
}

impl PatternStats for Uniform {
    fn label_rows(&self, _: &NamedToken) -> Option<u64> {
        Some(self.nodes)
    }
    fn equality_rows(&self, _: &NamedToken, _: &NamedToken, _: &relate_ir::Expr) -> Option<u64> {
        None
    }
    fn contains_rows(
        &self,
        _: &NamedToken,
        _: &NamedToken,
        _: &relate_ir::Expr,
    ) -> Option<ContainsCount> {
        None
    }
    fn rel_type_rows(&self, _: &NamedToken) -> Option<u64> {
        None
    }
    fn degree_moments(&self, _: &[NamedToken], _: &[NamedToken]) -> Option<DegreeMoments> {
        self.consults.set(self.consults.get() + 1);
        self.moments
    }
    fn in_degree_sum(&self, _: &[NamedToken], _: &[NamedToken]) -> Option<u128> {
        self.in_sum
    }
}

fn rel(direction: Direction) -> RelPattern {
    let mut rel = one_hop_rel(&[]);
    rel.direction = direction;
    rel
}

fn start() -> NodePattern {
    named_node("a")
}

#[test]
fn the_break_even_is_trails_against_pairs_and_probes() {
    // Trails ~ 1.5 x pairs is the measured break-even: well above it the
    // frontier wins, at or below it the walker keeps the clause.
    let wins = CostEstimate {
        trails: 4000.0,
        pairs: 1000.0,
        probes: 1000.0,
    };
    assert!(wins.frontier_wins());
    let loses = CostEstimate {
        trails: 1000.0,
        pairs: 1000.0,
        probes: 1000.0,
    };
    assert!(!loses.frontier_wins());
    let empty = CostEstimate {
        trails: 0.0,
        pairs: 0.0,
        probes: 0.0,
    };
    assert!(!empty.frontier_wins());
}

#[test]
fn degree_moments_price_mean_and_excess() {
    let moments = DegreeMoments {
        sum: 600,
        sum_sq: 3000,
    };
    assert_eq!(mean_degree(moments.sum, 100), 6.0);
    assert_eq!(mean_degree(moments.sum, 0), 0.0);
    assert_eq!(excess_degree(moments), 4.0);
    assert_eq!(excess_degree(DegreeMoments::default()), 0.0);
    // Degree-1 nodes have no excess.
    assert_eq!(
        excess_degree(DegreeMoments {
            sum: 10,
            sum_sq: 10
        }),
        0.0
    );
}

/// Undirected hops count every rel twice: the trails of an undirected
/// chain exceed the directed chain's by the doubling on each hop.
#[test]
fn an_undirected_hop_counts_twice() {
    let stats = Uniform::new(100, 300, 1200);
    let directed = rel(Direction::Outgoing);
    let undirected = rel(Direction::Both);
    let one = |rel: &RelPattern| estimate(&stats, 100, 100, 100, &start(), &[rel]).unwrap();
    // First hop: mean 3 per node (x2 undirected) from 100 seeds.
    assert_eq!(one(&directed).trails, 300.0);
    assert_eq!(one(&undirected).trails, 600.0);
    // Second hop rides the excess degree 1200/300 - 1 = 3 (x2).
    let two_directed = estimate(&stats, 100, 100, 100, &start(), &[&directed, &directed]).unwrap();
    assert_eq!(two_directed.trails, 300.0 + 900.0);
    let two_undirected =
        estimate(&stats, 100, 100, 100, &start(), &[&undirected, &undirected]).unwrap();
    assert_eq!(two_undirected.trails, 600.0 + 3600.0);
}

/// The first hop prices the side it leaves the start label by: a label
/// whose rels all point at it (the co-change `File`: zero out-degree) has
/// no outgoing fanout but a full incoming and undirected one — never the
/// zero a doubled out-sum produced.
#[test]
fn the_first_hop_prices_the_side_it_leaves_by() {
    let sink = Uniform::directed(100, 0, 400);
    let one = |stats: &Uniform, direction| {
        estimate(stats, 100, 100, 100, &start(), &[&rel(direction)])
            .unwrap()
            .trails
    };
    assert_eq!(one(&sink, Direction::Outgoing), 0.0);
    assert_eq!(one(&sink, Direction::Incoming), 400.0);
    assert_eq!(one(&sink, Direction::Both), 400.0);
    // A source label is the mirror image.
    let source = Uniform::directed(100, 400, 0);
    assert_eq!(one(&source, Direction::Outgoing), 400.0);
    assert_eq!(one(&source, Direction::Incoming), 0.0);
    assert_eq!(one(&source, Direction::Both), 400.0);
    // Mixed: undirected is the sum of both sides, not twice either.
    let mixed = Uniform::directed(100, 100, 300);
    assert_eq!(one(&mixed, Direction::Both), 400.0);
}

/// An undirected or incoming first hop needs the in side: without pair
/// counts there is no estimate, while an outgoing one still prices.
#[test]
fn a_missing_in_degree_side_leaves_only_outgoing_priceable() {
    let stats = Uniform {
        in_sum: None,
        ..Uniform::new(10, 30, 120)
    };
    let one = |direction| estimate(&stats, 10, 10, 10, &start(), &[&rel(direction)]);
    assert!(one(Direction::Outgoing).is_some());
    assert!(one(Direction::Incoming).is_none());
    assert!(one(Direction::Both).is_none());
}

/// Pairs never exceed seeds x endpoint population; probes never exceed
/// the trails.
#[test]
fn pairs_and_probes_are_capped() {
    let stats = Uniform::new(10, 80, 800);
    let both = rel(Direction::Both);
    let hops = [&both, &both, &both, &both];
    let cost = estimate(&stats, 10, 10, 10, &start(), &hops).unwrap();
    assert!(cost.trails > 10_000.0);
    assert_eq!(cost.pairs, 100.0);
    assert_eq!(cost.probes, 10.0 * 160.0);
    assert!(cost.frontier_wins());
}

/// No histogram: nothing to price with.
#[test]
fn unpriceable_statistics_have_no_estimate() {
    let stats = Uniform::unpriceable(10);
    let both = rel(Direction::Both);
    assert!(estimate(&stats, 10, 10, 10, &start(), &[&both]).is_none());
}

/// `n` File nodes, each joined to the next `reach` nodes around a ring
/// (`T` rels): degree `2 * reach` undirected.
fn ring(n: usize, reach: usize) -> Graph {
    let labels: Vec<&[&str]> = vec![&["File"]; n];
    let rels: Vec<(usize, usize, &str)> = (0..n)
        .flat_map(|i| (1..=reach).map(move |step| (i, (i + step) % n, "T")))
        .collect();
    build(&labels, &rels).graph
}

/// `commits` Commit nodes each touching `reach` of `files` File nodes
/// (`TOUCHES` rels point at File, so File has no out-edges — the
/// co-change shape).
fn co_change(commits: usize, files: usize, reach: usize) -> Graph {
    let labels: Vec<&[&str]> = (0..files)
        .map(|_| &["File"][..])
        .chain((0..commits).map(|_| &["Commit"][..]))
        .collect();
    let rels: Vec<(usize, usize, &str)> = (0..commits)
        .flat_map(|c| (0..reach).map(move |k| (files + c, (c + k) % files, "TOUCHES")))
        .collect();
    build(&labels, &rels).graph
}

/// Run `cypher` under `engine`, leaving the graph on the default routing.
fn rows_under(g: &mut Graph, engine: PatternEngine, cypher: &str) -> Vec<Vec<crate::exec::Value>> {
    g.configure_pattern_engine(engine);
    let rows = exec_cypher(g, cypher).rows;
    g.configure_pattern_engine(PatternEngine::Auto);
    rows
}

/// The distinct-endpoint count statement over `pattern`.
fn distinct_reach(pattern: &str) -> String {
    format!("MATCH {pattern} WHERE a <> b WITH DISTINCT a, b RETURN count(*) AS c")
}

/// The auto gate end to end: a dense graph's bounded distinct-endpoint
/// recursion rides the frontier (typed and untyped, directed or not),
/// where a short bound over a sparse ring keeps the walker with the typed
/// `Cost` decline in PROFILE — and the answers agree.
#[test]
fn the_auto_gate_engages_dense_distinct_reach_and_holds_short_bounds() {
    let mut dense = ring(40, 4);
    let mut sparse = ring(40, 1);
    let engaged = |g: &Graph, cypher: &str| {
        let rung = served(&profiled(g, cypher)).kernel;
        rung == Some(KernelRung::FrontierNfa)
    };
    // Dense rides the frontier on the long bounds; the short bounds sit
    // near the break-even, so only the sparse side is pinned for them.
    let long_bounds = [
        "(a:File)-[:T*1..4]-(b:File)",
        "(a:File)-[*1..4]-(b)",
        "(a:File)-[*1..4]->(b)",
    ];
    let short_bounds = ["(a:File)-[:T*1..2]-(b:File)", "(a:File)-[*1..2]-(b)"];
    for pattern in long_bounds {
        assert!(engaged(&dense, &distinct_reach(pattern)), "dense {pattern}");
    }
    for pattern in long_bounds.into_iter().chain(short_bounds) {
        let cypher = distinct_reach(pattern);
        assert!(!engaged(&sparse, &cypher), "sparse {cypher}");
        assert_eq!(
            frontier_decline_of(&sparse, &cypher),
            Some(FrontierDeclineReason::Plan(FrontierDecline::Cost)),
            "sparse {cypher}"
        );
        // The routing never changes the answer.
        for g in [&mut dense, &mut sparse] {
            let auto = rows_under(g, PatternEngine::Auto, &cypher);
            let walk = rows_under(g, PatternEngine::Walk, &cypher);
            assert_eq!(auto, walk, "{cypher}");
        }
    }
}

/// Untyped and typed, directed and undirected, bounded and unbounded
/// distinct-endpoint statements answer the walker's rows under every
/// routing (the walker, the cost-gated default, the forced rung).
#[test]
fn every_routing_agrees_on_untyped_and_typed_reach() {
    let mut g = ring(7, 2);
    for pattern in [
        "(a:File)-[*]-(b)",
        "(a:File)-[*2..]->(b:File)",
        "(a:File)<-[*..3]-(b)",
        "(a:File)-[:T*]-(b:File)",
        "(a:File)-[*1..3]-(b)",
        "(a:File)-[:T*1..3]->(b)",
        "(a:File)-[:T|U*1..3]-(b)",
    ] {
        let cypher = distinct_reach(pattern);
        let walk = rows_under(&mut g, PatternEngine::Walk, &cypher);
        let auto = rows_under(&mut g, PatternEngine::Auto, &cypher);
        let nfa = rows_under(&mut g, PatternEngine::Nfa, &cypher);
        assert_eq!(walk, auto, "auto: {cypher}");
        assert_eq!(walk, nfa, "nfa: {cypher}");
    }
}

/// A quantifier repeats one rel up to the hop ceiling; the estimate
/// consults the histograms once per distinct rel, never per expanded
/// hop, so a crafted bounded pattern cannot inflate plan-time scans.
#[test]
fn the_estimate_consults_per_distinct_rel_not_per_hop() {
    let stats = Uniform::new(100, 300, 1200);
    let both = rel(Direction::Both);
    let hops: Vec<&RelPattern> = vec![&both; 1000];
    let cost = estimate(&stats, 100, 100, 100, &start(), &hops).unwrap();
    assert!(!cost.trails.is_nan());
    // First hop, one landing price for every later hop, the adjacency.
    assert_eq!(stats.consults.get(), 3);
}

/// LABS-1661: the co-change shape (undirected over rels that all point at
/// the start label) priced zero trails and declined on every bound. The
/// dense long bound now rides the frontier and the sparse short bound
/// keeps the walker, and the routings agree on the answer.
#[test]
fn the_auto_gate_prices_a_sink_label_from_its_in_degree() {
    let mut dense = co_change(24, 6, 4);
    let pattern = "(a:File)-[:TOUCHES*1..4]-(b:File)";
    let cypher = distinct_reach(pattern);
    assert_eq!(
        served(&profiled(&dense, &cypher)).kernel,
        Some(KernelRung::FrontierNfa)
    );
    let sparse = co_change(8, 8, 1);
    let short = distinct_reach("(a:File)-[:TOUCHES*1..2]-(b:File)");
    assert_eq!(
        frontier_decline_of(&sparse, &short),
        Some(FrontierDeclineReason::Plan(FrontierDecline::Cost))
    );
    let walk = rows_under(&mut dense, PatternEngine::Walk, &cypher);
    for engine in [PatternEngine::Auto, PatternEngine::Nfa] {
        assert_eq!(rows_under(&mut dense, engine, &cypher), walk, "{engine:?}");
    }
}
