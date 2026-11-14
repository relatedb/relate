//! The bidirectional all-shortest pair search against the one-sided layered
//! search it replaces: same trails, same multiplicities, on named shapes
//! and on random small graphs.

use relate_core::{NodeId, RelId};
use relate_ir::Direction;

use crate::exec::test_support::{build, Built};
use crate::overlay::ExecGraph;
use crate::store::RelTypes;

use super::test_support::counting_expands;
use super::Search;

fn search<'a, 'g>(graph: &'a ExecGraph<'g>, dir: Direction, max: u32) -> Search<'a, 'g> {
    Search {
        graph,
        types: RelTypes::Any,
        dir,
        min: 1,
        max,
    }
}

fn sorted(mut trails: Vec<Vec<RelId>>) -> Vec<Vec<RelId>> {
    trails.sort();
    trails
}

/// The one-sided answer: the layered DAG stopped at `end`.
fn one_sided(s: &Search<'_, '_>, a: NodeId, b: NodeId) -> Vec<Vec<RelId>> {
    sorted(
        s.dag(a, true, None, |n, _| Ok(n == b))
            .unwrap()
            .trails(s.graph, b)
            .unwrap(),
    )
}

fn bidirectional(s: &Search<'_, '_>, a: NodeId, b: NodeId) -> Vec<Vec<RelId>> {
    sorted(s.all_shortest_trails(a, b).unwrap())
}

/// Every ordered pair of distinct nodes, both directions of search, equal.
fn assert_equivalent(edges: &[(usize, usize, &str)], n: usize) {
    let Built {
        graph: g,
        nodes: ids,
        ..
    } = build(&vec![&[][..]; n], edges);
    let view = ExecGraph::read(&g);
    for dir in [Direction::Outgoing, Direction::Incoming, Direction::Both] {
        for max in [1, 2, 3, 5, u32::MAX] {
            let s = search(&view, dir, max);
            for &a in &ids {
                for &b in ids.iter().filter(|&&b| b != a) {
                    assert_eq!(
                        bidirectional(&s, a, b),
                        one_sided(&s, a, b),
                        "{dir:?} max {max} {a}->{b} edges {edges:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn diamond_and_ties_keep_every_path() {
    // Two diamonds in a row: 0→{1,2}→3→{4,5}→6, four shortest trails.
    let edges = [
        (0, 1, "R"),
        (0, 2, "R"),
        (1, 3, "R"),
        (2, 3, "R"),
        (3, 4, "R"),
        (3, 5, "R"),
        (4, 6, "R"),
        (5, 6, "R"),
    ];
    assert_equivalent(&edges, 7);
    let Built {
        graph: g, nodes: n, ..
    } = build(&[&[][..]; 7], &edges);
    let view = ExecGraph::read(&g);
    let s = search(&view, Direction::Outgoing, 8);
    assert_eq!(bidirectional(&s, n[0], n[6]).len(), 4);
}

#[test]
fn multi_edges_multiply_paths() {
    // 0⇉1⇉2: two relationships per hop, four trails; plus a parallel
    // chain through 3 of the same length.
    let edges = [
        (0, 1, "R"),
        (0, 1, "R"),
        (1, 2, "R"),
        (1, 2, "R"),
        (0, 3, "R"),
        (3, 2, "R"),
    ];
    assert_equivalent(&edges, 4);
    let Built {
        graph: g, nodes: n, ..
    } = build(&[&[][..]; 4], &edges);
    let view = ExecGraph::read(&g);
    let s = search(&view, Direction::Outgoing, 8);
    assert_eq!(bidirectional(&s, n[0], n[2]).len(), 5);
}

#[test]
fn undirected_cycles_self_loops_and_unreachable() {
    // A triangle with a tail, a self-loop, an isolated node and a
    // component apart.
    let edges = [
        (0, 1, "R"),
        (1, 2, "R"),
        (2, 0, "R"),
        (2, 3, "R"),
        (3, 3, "R"),
        (5, 6, "R"),
    ];
    assert_equivalent(&edges, 8);
}

#[test]
fn distances_one_to_six_on_a_chain_with_shortcuts() {
    // 0-1-2-3-4-5-6 plus a shortcut 0-3 and a long way round 6-7-0.
    let edges = [
        (0, 1, "R"),
        (1, 2, "R"),
        (2, 3, "R"),
        (3, 4, "R"),
        (4, 5, "R"),
        (5, 6, "R"),
        (0, 3, "R"),
        (6, 7, "R"),
        (7, 0, "R"),
    ];
    assert_equivalent(&edges, 8);
    let chain: Vec<_> = (0..6).map(|i| (i, i + 1, "R")).collect();
    assert_equivalent(&chain, 7);
}

/// A small xorshift generator: random graphs without a dependency's API.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

#[test]
fn random_small_graphs_agree_with_the_one_sided_search() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..150 {
        let n = 2 + rng.below(8);
        let m = rng.below(3 * n + 1);
        let edges: Vec<(usize, usize, &str)> =
            (0..m).map(|_| (rng.below(n), rng.below(n), "R")).collect();
        assert_equivalent(&edges, n);
    }
}

#[test]
fn two_hub_pair_expands_far_fewer_relationships_than_one_side() {
    // Distance 4 along a chain whose middle node fans out 600 ways: the
    // one-sided search expands that fan on its way to the end; meeting in
    // the middle never expands the middle node.
    const FAN: usize = 200;
    // Nodes: 0 = start, 2 = the fanning middle, 4 = end, then leaves.
    let mut edges = vec![(0, 1, "R"), (1, 2, "R"), (2, 3, "R"), (3, 4, "R")];
    let mut n = 5;
    for _ in 0..3 * FAN {
        edges.push((2, n, "R"));
        n += 1;
    }
    let Built {
        graph: g,
        nodes: ids,
        ..
    } = build(&vec![&[][..]; n], &edges);
    let view = ExecGraph::read(&g);
    let s = search(&view, Direction::Outgoing, 8);
    let (bi, bi_work) = counting_expands(|| bidirectional(&s, ids[0], ids[4]));
    let (one, one_work) = counting_expands(|| one_sided(&s, ids[0], ids[4]));
    assert_eq!(bi, one);
    assert!(
        bi_work * 10 < one_work,
        "bidirectional {bi_work} vs one-sided {one_work}"
    );
}
