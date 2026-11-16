//! Fixtures the route tests share: `plain` index-keyed graphs over the
//! shared builder, query runners over the counting combinator, and
//! order-insensitive rows.

use std::collections::BTreeMap;

use relate_core::Value;
use relate_ir::Direction;

use crate::exec::test_support::{
    build, exec_autocommit, try_exec_cypher, try_exec_cypher_params, Builder,
};
use crate::graph::Graph;
use crate::shortest::test_support::counting_expands;

use super::anchor::{take_anchor_arms, AnchorArm};
use super::{take_route_runs, with_forced_gpm};

/// Relationships shortest-path searches followed, (pattern, input row)
/// evaluations the shortest-path route accepted, and the anchor arms those
/// runs took (a run whose bounds are empty — min above max — matches
/// nothing before it anchors, so it records no arm).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Counts {
    pub expands: u64,
    pub route_runs: u64,
    /// The [`AnchorArm`] each anchored route run took, in order.
    pub arms: Vec<AnchorArm>,
}

/// `f`'s result and the [`Counts`] it caused.
pub(super) fn counting<T>(f: impl FnOnce() -> T) -> (T, Counts) {
    let _ = take_route_runs();
    let _ = take_anchor_arms();
    let (out, expands) = counting_expands(f);
    let counts = Counts {
        expands,
        route_runs: take_route_runs(),
        arms: take_anchor_arms(),
    };
    (out, counts)
}

/// [`build`] with `n` unlabelled nodes and `(from, to)` relationships of
/// type `R`.
pub(super) fn plain(n: usize, rels: &[(usize, usize)]) -> Graph {
    let rels: Vec<_> = rels.iter().map(|&(a, b)| (a, b, "R")).collect();
    build(&vec![&[][..]; n], &rels).graph
}

/// `n` nodes, the first `x` labelled `X` and the rest `Y`, and `(from, to)`
/// relationships of type `R`: the cross-product shape of an all-pairs
/// `MATCH (a:X), (b:Y)`.
pub(super) fn labelled(x: usize, n: usize, rels: &[(usize, usize)]) -> Graph {
    let labels: Vec<&[&str]> = (0..n)
        .map(|i| if i < x { &["X"][..] } else { &["Y"][..] })
        .collect();
    let rels: Vec<_> = rels.iter().map(|&(a, b)| (a, b, "R")).collect();
    build(&labels, &rels).graph
}

/// 0→1→2→3, the shortcut 0→4→3, and 3→1 closing a cycle.
pub(super) fn shortcut() -> Graph {
    plain(5, &[(0, 1), (1, 2), (2, 3), (0, 4), (4, 3), (3, 1)])
}

/// A `:M` node with `loops` parallel `:B` self-loops (`i` = 1..=loops): the
/// self-loop shape a `SHORTEST` selector answers over, whose trail count —
/// every ordering of every subset of the loops — multiplies by ~`loops`
/// per loop enumeration adds.
pub(super) fn self_loops(loops: usize) -> Graph {
    let b = Builder::new();
    let m = b.node("m", &["M"]);
    for i in 1..=loops {
        b.rel(m, m, "B", &[("i", Value::Int(i as i64))]);
    }
    b.done()
}

/// 200 `:N` nodes (`id` text, `g` parity) on a ring with `*3` chords;
/// `indexed` declares the RANGE index the point seeks need.
fn ring_graph(indexed: bool) -> Graph {
    let g = Graph::default();
    if indexed {
        exec_autocommit(&g, "CREATE INDEX FOR (n:N) ON (n.id)").unwrap();
    }
    exec_autocommit(
        &g,
        "UNWIND range(0, 199) AS i CREATE (:N {id: toString(i), g: i % 2})",
    )
    .unwrap();
    exec_autocommit(
        &g,
        "MATCH (a:N), (b:N) WHERE (toInteger(a.id) + 1) % 200 = toInteger(b.id) \
         OR (toInteger(a.id) * 3) % 200 = toInteger(b.id) CREATE (a)-[:R]->(b)",
    )
    .unwrap();
    g
}

/// The ring with the RANGE index on `id` the point seeks need.
pub(super) fn ring() -> Graph {
    ring_graph(true)
}

/// The ring with no index: no seek serves an endpoint.
pub(super) fn ring_unindexed() -> Graph {
    ring_graph(false)
}

/// The variable-length relationship the ring tests walk.
pub(super) const BODY: &str = "-[:R*..6]->";

/// Rows of `q` and the [`Counts`] it caused.
pub(super) fn run(g: &Graph, q: &str) -> (Vec<Vec<Value>>, Counts) {
    run_params(g, q, &BTreeMap::new())
}

/// [`run`] with statement parameters.
pub(super) fn run_params(
    g: &Graph,
    q: &str,
    params: &BTreeMap<String, Value>,
) -> (Vec<Vec<Value>>, Counts) {
    counting(|| try_exec_cypher_params(g, q, params).unwrap().rows)
}

/// Rows of `q`, sorted; asserts the route ran and expanded.
pub(super) fn on_route(g: &Graph, q: &str) -> Vec<Vec<Value>> {
    let (rows, counts) = run(g, q);
    assert!(counts.route_runs > 0, "expected the BFS route for {q}");
    assert!(counts.expands > 0, "the route expanded nothing for {q}");
    sorted(rows)
}

/// Rows of `q` with every routed pattern forced onto GPM, sorted.
pub(super) fn gpm(g: &Graph, q: &str) -> Vec<Vec<Value>> {
    gpm_params(g, q, &BTreeMap::new())
}

/// [`gpm`] with statement parameters.
pub(super) fn gpm_params(g: &Graph, q: &str, params: &BTreeMap<String, Value>) -> Vec<Vec<Value>> {
    sorted(with_forced_gpm(|| {
        try_exec_cypher_params(g, q, params).unwrap().rows
    }))
}

/// The route's answer to `q` is the GPM walk's — rows, or the same error —
/// and returns the run's [`Counts`].
pub(super) fn parity(g: &Graph, q: &str) -> Counts {
    let (routed, counts) = counting(|| try_exec_cypher(g, q));
    let (oracle, oracle_counts) = counting(|| with_forced_gpm(|| try_exec_cypher(g, q)));
    assert_eq!(
        oracle_counts.route_runs, 0,
        "the oracle must bypass the route for {q}"
    );
    // One equality over both answers: the rows, or the same typed error.
    assert_eq!(
        routed.map(|r| sorted(r.rows)),
        oracle.map(|o| sorted(o.rows)),
        "route and gpm disagree for {q}"
    );
    assert!(counts.route_runs > 0, "the route must take {q}");
    counts
}

/// `rows` in [`Value::order_by_cmp`] order, for order-insensitive
/// comparison.
pub(super) fn sorted(mut rows: Vec<Vec<Value>>) -> Vec<Vec<Value>> {
    rows.sort_by(|a, b| {
        let mut cells = a.iter().zip(b).map(|(x, y)| x.order_by_cmp(y));
        cells.find(|o| o.is_ne()).unwrap_or(a.len().cmp(&b.len()))
    });
    rows
}

/// A routed selector: its text before and after the pattern, and whether it
/// returns every shortest trail rather than one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Selector {
    pub(super) open: &'static str,
    pub(super) close: &'static str,
    pub(super) all: bool,
}

/// The keyword selectors only: the legacy `shortestPath()`/`allShortestPaths()`
/// functions reject a pattern whose start and end can be the same node
/// (51N23) and one whose endpoints are not both bound variables in an
/// expression, so the open-endpoint matrices below run the keyword
/// spellings, which keep closed walks.
#[rustfmt::skip]
pub(super) const SELECTORS: [Selector; 4] = [
    Selector { open: "ANY SHORTEST ", close: "", all: false },
    Selector { open: "SHORTEST 1 ", close: "", all: false },
    Selector { open: "SHORTEST 1 GROUP ", close: "", all: true },
    Selector { open: "ALL SHORTEST ", close: "", all: true },
];

/// The three directions the matrix runs.
pub(super) const DIRS: [Direction; 3] = [Direction::Outgoing, Direction::Incoming, Direction::Both];

/// The query builders' direction spelling helpers, over the canonical
/// [`Direction`] vocabulary: the arrow pair a direction renders as, and
/// whether it may follow a relationship `from → to` from `at`. `step` is
/// deliberately independent of the production hop arithmetic
/// (`crate::store::hop_other`) so the oracle cannot inherit a kernel bug.
pub(super) trait DirectionExt {
    fn arrows(self) -> (&'static str, &'static str);
    fn step(self, at: usize, rel: (usize, usize)) -> Option<usize>;
}

impl DirectionExt for Direction {
    fn arrows(self) -> (&'static str, &'static str) {
        match self {
            Direction::Outgoing => ("-", "->"),
            Direction::Incoming => ("<-", "-"),
            Direction::Both => ("-", "-"),
        }
    }

    /// The node reached from `at` over relationship `from → to`, if this
    /// direction may follow it from there.
    fn step(self, at: usize, rel: (usize, usize)) -> Option<usize> {
        let (from, to) = rel;
        match self {
            Direction::Outgoing => (from == at).then_some(to),
            Direction::Incoming => (to == at).then_some(from),
            Direction::Both if from == at => Some(to),
            Direction::Both => (to == at).then_some(from),
        }
    }
}
