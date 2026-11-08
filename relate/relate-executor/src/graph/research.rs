//! The research kernel's live-read family — research-only
//! (speed-of-light kernel): compiled only under the `research`
//! feature, not engine behavior (ADR-0476). One home for the reads
//! `sol` reads, beside the graph module's other live readers, out of
//! the engine's general-purpose module root — extracted when the qpp
//! kernel shared them, and kept as the sol kernel's own after the qpp
//! half retired with its prototype (ADR-0478).

use relate_core::NodeId;
use relate_ir::Direction;

use super::{live_version, Graph};
use crate::store::{hop_other, AdjHop, LiveNode};

/// Run `f` on `node`'s live record under one short `nodes` guard — the
/// research kernel's one live-node read.
pub(super) fn with_node<R>(
    graph: &Graph,
    node: NodeId,
    f: impl FnOnce(&LiveNode) -> R,
) -> Option<R> {
    let vers = graph.nodes.get(&node)?;
    let live = live_version(&vers)?;
    Some(f(live.live))
}

/// The other endpoint of a live `hop` from `node` in direction `dir`,
/// or `None` when the rel has no live version or the endpoint does not
/// match `node` — the liveness and endpoint checks the executor's
/// `hop` applies to a superset-list hop, the endpoint half delegated
/// to the canonical `store::hop_other`.
fn live_other(graph: &Graph, hop: &AdjHop, node: NodeId, dir: Direction) -> Option<NodeId> {
    let vers = graph.rels.get(&hop.id)?;
    let v = live_version(&vers)?;
    hop_other(node, v.live.start, v.live.end, dir)
}

/// The other endpoints of `node`'s live rels of interned type `tid` in
/// direction `dir`, copied into the caller's reused `buf` (cleared
/// here, so no two guards on one map are ever held at once).
/// `Direction::Outgoing` walks `node`'s outgoing chunk,
/// `Direction::Incoming` its incoming one; `Both` walks both, outgoing
/// then incoming — each incident rel once, except a self-loop, which
/// sits in both chunks and enumerates once per side. Each hop's rel is
/// checked live and endpoint-checked, the checks the executor's `hop`
/// applies to a superset-list hop.
pub(super) fn live_neighbors(
    graph: &Graph,
    node: NodeId,
    tid: u32,
    dir: Direction,
    buf: &mut Vec<NodeId>,
) {
    buf.clear();
    // the pairing bound once: each adjacency map with its scan
    // direction in the same row; Both walks both rows
    for (map, scan_dir) in [
        (&graph.out_adj, Direction::Outgoing),
        (&graph.in_adj, Direction::Incoming),
    ] {
        if dir != Direction::Both && dir != scan_dir {
            continue;
        }
        let Some(adj) = map.get(&node) else {
            continue;
        };
        for hop in adj.chunk(tid) {
            if let Some(other) = live_other(graph, hop, node, scan_dir) {
                buf.push(other);
            }
        }
    }
}
