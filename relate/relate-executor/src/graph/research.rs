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
use crate::adjacency::AdjHop;
use crate::store::{hop_other, LiveNode};

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
    // The live read's merged delta+sealed hop stream (ADR-0551), at the
    // live read ts so a tombstoned sealed position is dropped.
    let ts = graph.read_ts();
    // the pairing bound once: each adjacency map with its scan
    // direction in the same row; Both walks both rows
    for (map, sealed, scan_dir) in [
        (&graph.out_adj, &graph.sealed_out, Direction::Outgoing),
        (&graph.in_adj, &graph.sealed_in, Direction::Incoming),
    ] {
        if dir != Direction::Both && dir != scan_dir {
            continue;
        }
        let Some(adj) = map.get(&node) else {
            continue;
        };
        let region = sealed.get(&node);
        adj.for_each_hop_of(region.as_deref(), tid, ts, |hop| {
            if let Some(other) = live_other(graph, &hop, node, scan_dir) {
                buf.push(other);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0551: a checkpoint fold moves a chunk's hops into the sealed
    /// tier — the live-neighbor read walks the merged delta+sealed
    /// stream, so the fold hides none of them.
    #[test]
    fn live_neighbors_sees_sealed_hops() {
        let g = Graph::default();
        let (a, b, _, type_id) = crate::graph::test_support::folded_floor_chunk(&g);
        let floor = crate::adjacency::COMPACTION_DELTA_FLOOR;
        assert_eq!(g.sealed_out.get(&a).unwrap().chunk_len(type_id), floor);

        let mut buf = Vec::new();
        live_neighbors(&g, a, type_id, Direction::Outgoing, &mut buf);
        assert_eq!(buf, vec![b; floor]);
    }
}
