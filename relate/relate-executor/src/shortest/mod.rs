//! Shortest-path graph search behind the one shortest route (ADR-0186,
//! ADR-0189), which serves the `MATCH` clause and, lowered to the same
//! selector form, the expression form (ADR-0568).
//!
//! - [`pair`]: one shortest trail between two distinct nodes, by the
//!   ADR-0037 shallow intersection or the ADR-0034 bidirectional BFS.
//! - [`Search::dag`]: a layered single-source BFS that keeps shortest-path
//!   predecessors, with [`layered::Dag::trails`] enumerating them, and
//!   [`Search::closed`] for the shortest closed trails through one node.
//! - [`Search::sweep_levels`]: a multi-source bit-parallel level sweep that
//!   answers distance only, for a fan with a listed far side.
//! - [`class_walk::ClassWalk`]: the trails of one exact length between two
//!   nodes, the class enumeration of `SHORTEST k` and `SHORTEST k GROUPS`
//!   (ADR-0607).
//! - [`Search::pair_trails`]: the pair answer as trails; an all-path
//!   search grows a layered DAG from each end and joins them where they
//!   meet (ADR-0601), sharing the layer step with [`Search::dag`].
//!
//! Searches take the graph and the pattern's direction, types and hop
//! bounds, and return relationship-id trails; callers build paths with
//! [`path_from_trail`]. Every node entry is one cancellation check
//! (ADR-0111) and every followed relationship one counted expansion.

use std::cell::RefCell;
use std::collections::HashSet;

use relate_core::{Error, NodeId, Path, RelId, Result};
use relate_ir::{Direction, NodePattern, PathPattern, RelPattern};

use crate::graph::HopBuf;
use crate::overlay::ExecGraph;
use crate::store::RelTypes;

pub(crate) mod class_walk;
pub(crate) mod layered;
pub(crate) mod multi_source;
mod pair;
mod reach;

/// 51N23: what a shortest-path route does when a start and end pair
/// resolves to one node.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommonEndNodes {
    /// The keyword selectors keep their closed trails.
    Allowed,
    /// The `shortestPath()`/`allShortestPaths()` function and expression
    /// forms reject the pair, unless the lower bound is 0: the zero-length
    /// path then answers it.
    Forbidden,
}

impl CommonEndNodes {
    /// The runtime half of 51N23 — the one predicate every engine arm that
    /// resolves a pair reads, beside the static half
    /// ([`reject_static_common_end_nodes`]).
    pub(crate) fn reject(self, start: NodeId, end: NodeId, min: u32) -> Result<()> {
        match self {
            CommonEndNodes::Allowed => Ok(()),
            CommonEndNodes::Forbidden if start == end && min > 0 => {
                Err(common_end_nodes_forbidden())
            }
            CommonEndNodes::Forbidden => Ok(()),
        }
    }
}

/// 51N23: the common-end-node rejection text (Neo4j's
/// `ShortestPathCommonEndNodesForbiddenException`, live Neo4j 2026.10.1).
pub(crate) fn common_end_nodes_forbidden() -> Error {
    Error::execution(
        "ShortestPathCommonEndNodes: The shortest path algorithm does not work when the start and end nodes are the same.",
    )
}

/// 51N23, statically: a `shortestPath()`/`allShortestPaths()` pattern that
/// names one node variable at both ends is rejected on every input row —
/// whatever the lower bound, before either engine looks at the graph (live
/// Neo4j 2026.10.1 raises on an empty graph). With no input row there is no
/// rejection, as live evaluates per row. A pair that distinct bindings
/// resolve to one node at run time is the other half
/// ([`CommonEndNodes::reject`]).
pub(crate) fn reject_static_common_end_nodes(path: &PathPattern) -> Result<()> {
    let same = path.is_shortest_path_function
        && path
            .boundary_nodes()
            .is_some_and(|(start, end)| start.variable.is_some() && start.variable == end.variable);
    if same {
        Err(common_end_nodes_forbidden())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod all_pair_tests;
#[cfg(test)]
mod hop_reader_tests;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

#[cfg(test)]
thread_local! {
    static EXPANDS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Count `n` relationships a shortest-path search followed. The counter
/// exists only in tests, for the bounded-expansion regression tests.
#[cfg(test)]
fn note_expands(n: usize) {
    EXPANDS.with(|c| c.set(c.get() + n as u64));
}

#[cfg(not(test))]
fn note_expands(_: usize) {}

/// Take and reset this thread's count of followed relationships.
#[cfg(test)]
pub(crate) fn take_shortest_expands() -> u64 {
    EXPANDS.with(|c| c.replace(0))
}

/// What one pattern's searches follow. Every search returns trails of at
/// least one relationship and assumes `min` ≤ 1 (ADR-0189 sends larger
/// minimums to enumeration): callers answer `min` = 0 with the zero-length
/// path themselves, and `min` only picks the pair search's strategy.
#[derive(Clone)]
pub(crate) struct Search<'a, 'g> {
    pub graph: &'a ExecGraph<'g>,
    /// Relationship types to follow, resolved to interned ids once at the
    /// search's construction (LABS-801); [`RelTypes::Any`] follows every
    /// type.
    pub types: RelTypes,
    pub dir: Direction,
    pub min: u32,
    /// Longest trail in relationships; `u32::MAX` for no cap.
    pub max: u32,
    /// Keep every shortest predecessor of a node (all-path selectors)
    /// instead of the first one only.
    pub all: bool,
    /// A relationship the layered search never follows: the first hop of a
    /// closed trail, which the way back must not reuse.
    pub skip: Option<RelId>,
    /// The batched hop reader's reusable buffers, held for the search's
    /// life instead of allocated per node entered.
    pub scratch: RefCell<HopBuf>,
}

impl Search<'_, '_> {
    /// The same search along the reversed direction.
    pub(crate) fn reversed(&self) -> Self {
        Self {
            dir: self.dir.flip(),
            ..self.clone()
        }
    }

    /// Call `f` with each relationship followed out of `node` and the node
    /// it reaches.
    fn each_hop(&self, node: NodeId, f: impl FnMut(NodeId, RelId) -> Result<()>) -> Result<()> {
        self.each_hop_along(self.dir, node, f)
    }

    /// [`Self::each_hop`] along `dir` instead of the search's own direction:
    /// one search value serves a walk and the balls grown against it.
    fn each_hop_along(
        &self,
        dir: Direction,
        node: NodeId,
        mut f: impl FnMut(NodeId, RelId) -> Result<()>,
    ) -> Result<()> {
        self.each_hop_batch_along(dir, node, |hops| {
            hops.iter().try_for_each(|&(next, rid)| f(next, rid))
        })
    }

    /// Call `f` once per batch of at most `HOP_BATCH` relationships
    /// followed out of `node` along `dir`, each with the node it reaches,
    /// resolved at the snapshot. This is the one place a node entry is
    /// polled for cancellation: the sampled tick runs once per entry, never
    /// per relationship, so callers do not tick again before calling it.
    fn each_hop_batch_along(
        &self,
        dir: Direction,
        node: NodeId,
        mut f: impl FnMut(&[(NodeId, RelId)]) -> Result<()>,
    ) -> Result<()> {
        self.graph.cancel_tick()?;
        // Taken out of the cell so a re-entrant walk from `f` gets its own
        // buffers instead of a double borrow.
        let mut buf = std::mem::take(&mut *self.scratch.borrow_mut());
        let result = self
            .graph
            .try_for_each_hop_batch(node, dir, &self.types, &mut buf, |hops| {
                note_expands(hops.len());
                f(hops)
            });
        *self.scratch.borrow_mut() = buf;
        result
    }

    /// Every relationship followed out of `node`, with the node it reaches.
    pub(crate) fn hops(&self, node: NodeId) -> Result<Vec<(NodeId, RelId)>> {
        self.hops_along(self.dir, node)
    }

    /// [`Self::hops`] along `dir`.
    fn hops_along(&self, dir: Direction, node: NodeId) -> Result<Vec<(NodeId, RelId)>> {
        let mut out = Vec::new();
        self.each_hop_batch_along(dir, node, |hops| {
            out.extend_from_slice(hops);
            Ok(())
        })?;
        Ok(out)
    }
}

/// Whether the searches follow `start-rel-end` exactly: relationship types
/// absent or a static disjunction that `rel.types` spells out, no
/// relationship property map (the searches check types only), and endpoint
/// labels absent or a static conjunction.
pub(crate) fn followable(start: &NodePattern, rel: &RelPattern, end: &NodePattern) -> bool {
    rel.types_are_complete_filter()
        && rel.properties.is_none()
        && start.has_static_labels()
        && end.has_static_labels()
}

/// Whether no relationship occurs twice in `trail`.
fn unique_rels(trail: &[RelId]) -> bool {
    let mut seen = HashSet::new();
    trail.iter().all(|r| seen.insert(*r))
}

/// The path that starts at `start` and follows `trail`.
///
/// Precondition: `trail` comes from a search over this same graph view, so
/// every relationship it names and every node it touches is present and
/// the trail is contiguous. A miss breaks that same-view invariant and
/// panics rather than silently dropping a trail element.
pub(crate) fn path_from_trail(graph: &ExecGraph<'_>, start: NodeId, trail: &[RelId]) -> Path {
    let mut nodes = Vec::new();
    let mut rels = Vec::new();
    nodes.push(
        graph
            .node(start)
            .expect("path_from_trail: start node of the search's own view"),
    );
    let mut cur = start;
    for rid in trail {
        let rel = graph
            .rel(*rid)
            .expect("path_from_trail: trail relationship of the search's own view");
        debug_assert!(
            rel.start == cur || rel.end == cur,
            "path_from_trail: trail is contiguous"
        );
        let next = if rel.start == cur { rel.end } else { rel.start };
        rels.push(rel);
        nodes.push(
            graph
                .node(next)
                .expect("path_from_trail: trail node of the search's own view"),
        );
        cur = next;
    }
    Path { nodes, rels }
}
