//! The whole-graph adjacency observation the restore and rebuild test
//! modules compare graphs by, and the storage payload tests compare a
//! payload-installed open against a re-folded one with. Behind `test` or
//! the off-by-default `test-hooks` feature, never in a shipped build.

use relate_core::{NodeId, RelId};
use relate_ir::Direction;

use super::Graph;

/// One node's adjacency, raw and as each direction expands it.
#[derive(Debug, PartialEq)]
pub struct NodeAdjacency {
    pub(crate) id: NodeId,
    pub(crate) out: Vec<RelId>,
    pub(crate) r#in: Vec<RelId>,
    /// `Outgoing`, `Incoming`, then `Both` hops.
    pub(crate) expands: [Vec<RelId>; 3],
}

/// Every live node's [`NodeAdjacency`] at the graph's commit ts.
pub fn node_adjacencies(g: &Graph) -> Vec<NodeAdjacency> {
    let ts = g.commit_ts();
    g.all_node_ids_at(ts)
        .into_iter()
        .map(|id| {
            let expands = [Direction::Outgoing, Direction::Incoming, Direction::Both]
                .map(|dir| g.neighbors_at(id, dir, ts));
            NodeAdjacency {
                id,
                out: g.degree_out(id),
                r#in: g.degree_in(id),
                expands,
            }
        })
        .collect()
}
