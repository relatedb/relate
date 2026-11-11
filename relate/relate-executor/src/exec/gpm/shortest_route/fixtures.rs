//! Fixtures the route tests share: `plain` index-keyed graphs over the
//! shared builder, query runners over the counting combinator, and
//! order-insensitive rows.

use relate_core::Value;
use relate_ir::Direction;

use crate::exec::test_support::{build, exec_cypher};
use crate::graph::Graph;
use crate::shortest::test_support::counting_expands;

use super::route::{take_anchor_arms, AnchorArm};
use super::{take_route_runs, with_forced_gpm};

/// Relationships shortest-path searches followed, (pattern, input row)
/// evaluations the shortest-path route accepted, and the anchor arms those
/// runs took.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Counts {
    pub expands: u64,
    pub route_runs: u64,
    /// The [`AnchorArm`] each route run took, in order.
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

/// 0→1→2→3, the shortcut 0→4→3, and 3→1 closing a cycle.
pub(super) fn shortcut() -> Graph {
    plain(5, &[(0, 1), (1, 2), (2, 3), (0, 4), (4, 3), (3, 1)])
}

/// Rows of `q` and the [`Counts`] it caused.
pub(super) fn run(g: &Graph, q: &str) -> (Vec<Vec<Value>>, Counts) {
    counting(|| exec_cypher(g, q).rows)
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
    sorted(with_forced_gpm(|| exec_cypher(g, q).rows))
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
