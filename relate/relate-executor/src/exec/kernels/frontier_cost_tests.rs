//! Unit tests for the frontier cost gate's estimate and break-even.

use relate_ir::{Direction, NodePattern, RelPattern};
use relate_names::NamedToken;
use relate_planner::{ContainsCount, DegreeMoments, PatternStats};

use super::frontier_cost::{estimate, excess_degree, mean_degree, CostEstimate};
use crate::ctx::KernelRung;
use crate::exec::test_support::{
    build, exec_cypher, frontier_decline_of, named_node, one_hop_rel, profiled, served,
};
use crate::frontier_decline::{FrontierDecline, FrontierDeclineReason};
use crate::graph::Graph;

/// One uniform label: `nodes` nodes whose out-degree moments are fixed.
struct Uniform {
    nodes: u64,
    moments: Option<DegreeMoments>,
    /// Degree-moments consults made, each a histogram scan on the live graph.
    consults: std::cell::Cell<usize>,
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
    assert_eq!(mean_degree(moments, 100), 6.0);
    assert_eq!(mean_degree(moments, 0), 0.0);
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
    let stats = Uniform {
        nodes: 100,
        moments: Some(DegreeMoments {
            sum: 300,
            sum_sq: 1200,
        }),
        consults: Default::default(),
    };
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

/// Pairs never exceed seeds x endpoint population; probes never exceed
/// the trails.
#[test]
fn pairs_and_probes_are_capped() {
    let stats = Uniform {
        nodes: 10,
        moments: Some(DegreeMoments {
            sum: 80,
            sum_sq: 800,
        }),
        consults: Default::default(),
    };
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
    let stats = Uniform {
        nodes: 10,
        moments: None,
        consults: Default::default(),
    };
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
    for (pattern, dense_rides) in [
        ("(a:File)-[:T*1..4]-(b:File)", Some(true)),
        ("(a:File)-[*1..4]-(b)", Some(true)),
        ("(a:File)-[*1..4]->(b)", Some(true)),
        ("(a:File)-[:T*1..2]-(b:File)", None),
        ("(a:File)-[*1..2]-(b)", None),
    ] {
        let cypher =
            format!("MATCH {pattern} WHERE a <> b WITH DISTINCT a, b RETURN count(*) AS c");
        if let Some(rides) = dense_rides {
            assert_eq!(engaged(&dense, &cypher), rides, "dense {cypher}");
        }
        assert!(!engaged(&sparse, &cypher), "sparse {cypher}");
        assert_eq!(
            frontier_decline_of(&sparse, &cypher),
            Some(FrontierDeclineReason::Plan(FrontierDecline::Cost)),
            "sparse {cypher}"
        );
        // The routing never changes the answer.
        for g in [&mut dense, &mut sparse] {
            let auto = exec_cypher(g, &cypher).rows;
            g.configure_pattern_engine(crate::pattern_engine::PatternEngine::Walk);
            let walk = exec_cypher(g, &cypher).rows;
            g.configure_pattern_engine(crate::pattern_engine::PatternEngine::Auto);
            assert_eq!(auto, walk, "{cypher}");
        }
    }
}

/// Untyped and typed, directed and undirected, bounded and unbounded
/// distinct-endpoint statements answer the walker's rows under every
/// routing (the walker, the cost-gated default, the forced rung).
#[test]
fn every_routing_agrees_on_untyped_and_typed_reach() {
    use crate::pattern_engine::PatternEngine;
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
        let cypher =
            format!("MATCH {pattern} WHERE a <> b WITH DISTINCT a, b RETURN count(*) AS c");
        let mut answers = Vec::new();
        for engine in [PatternEngine::Walk, PatternEngine::Auto, PatternEngine::Nfa] {
            g.configure_pattern_engine(engine);
            answers.push(exec_cypher(&g, &cypher).rows);
        }
        g.configure_pattern_engine(PatternEngine::Auto);
        assert_eq!(answers[0], answers[1], "auto: {cypher}");
        assert_eq!(answers[0], answers[2], "nfa: {cypher}");
    }
}

/// A quantifier repeats one rel up to the hop ceiling; the estimate
/// consults the histograms once per distinct rel, never per expanded
/// hop, so a crafted bounded pattern cannot inflate plan-time scans.
#[test]
fn the_estimate_consults_per_distinct_rel_not_per_hop() {
    let stats = Uniform {
        nodes: 100,
        moments: Some(DegreeMoments {
            sum: 300,
            sum_sq: 1200,
        }),
        consults: Default::default(),
    };
    let both = rel(Direction::Both);
    let hops: Vec<&RelPattern> = vec![&both; 1000];
    let cost = estimate(&stats, 100, 100, 100, &start(), &hops).unwrap();
    assert!(!cost.trails.is_nan());
    // First hop, one landing price for every later hop, the adjacency.
    assert_eq!(stats.consults.get(), 3);
}
