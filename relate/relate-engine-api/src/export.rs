//! What a snapshot codec may ask of a query engine, and what recovery may
//! put back. Codecs never name a concrete graph.

use std::collections::HashSet;

use relate_catalog::{ConstraintDef, IndexDef};
use relate_core::{Node, NodeId, RelId, Relationship, Result};
use relate_model::Constraint;

use crate::catalog_image::CheckpointTheater;
use crate::redo::RedoOp;

pub trait GraphExport {
    fn commit_ts(&self) -> u64;
    fn next_node_id(&self) -> i64;
    fn next_rel_id(&self) -> i64;
    fn live_node_count(&self) -> usize;
    fn dirty_node_ids(&self) -> HashSet<NodeId>;
    fn dirty_rel_ids(&self) -> HashSet<RelId>;
    fn all_node_ids_at(&self, ts: u64) -> Vec<NodeId>;
    fn all_rel_ids_at(&self, ts: u64) -> Vec<RelId>;
    /// The live relationship count at `ts` — the checkpoint's payload
    /// threshold reads this, never the id list.
    fn live_rel_count(&self, ts: u64) -> usize;
    fn node_at(&self, id: NodeId, ts: u64) -> Option<Node>;
    fn rel_at(&self, id: RelId, ts: u64) -> Option<Relationship>;
    fn node_checkpoint_bytes(&self, id: NodeId, ts: u64) -> Result<Option<u64>>;
    fn rel_checkpoint_bytes(&self, id: RelId, ts: u64) -> Result<Option<u64>>;
    fn named_indexes(&self) -> Vec<IndexDef>;
    fn named_constraints(&self) -> Vec<ConstraintDef>;
    fn checkpoint_theater_for_write(&self) -> Result<CheckpointTheater>;
    fn graph_type_spec(&self) -> Result<Option<String>>;
    fn clear_checkpoint_dirty(&self);
    /// SHOW-style catalog DDL the schema mirror writes beside a checkpoint.
    fn catalog_ddl(&self) -> Vec<String>;
    /// Visit every adjacency chunk of the graph in ascending node-id order,
    /// out side before in side per node — the order the opt-in checkpoint
    /// payload writes its rows in. `ts` is the checkpoint's snapshot time:
    /// a chunk carries the tombstone ids that hide a hop at it. Streamed,
    /// one node's chunks at a time: no whole-graph chunk image is retained.
    /// The walk stops at the visitor's first error and returns it.
    fn for_each_adjacency_chunk(
        &self,
        ts: u64,
        f: &mut dyn FnMut(AdjacencyChunk) -> Result<()>,
    ) -> Result<()>;
}

/// One adjacency chunk as the opt-in checkpoint payload serializes it
/// (ADR-0551): one `(node, rel type, direction)` entry of the adjacency
/// store. Its hops live in two tiers — the unsealed delta hop ids and,
/// once folded, a sealed codec stream over `sealed_hops` hops; a chunk can
/// hold both, and the sealed stream is carried verbatim, never re-encoded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdjacencyChunk {
    pub node_id: NodeId,
    pub type_id: u32,
    /// `true` for the node's incoming (end) side, `false` for outgoing.
    pub incoming: bool,
    /// Hop count `sealed` encodes; 0 when the chunk holds no sealed stream.
    pub sealed_hops: u64,
    /// The sealed ADR-0551 codec stream, verbatim — empty when unsealed.
    pub sealed: Vec<u8>,
    /// The unsealed delta hop ids, in stored ts-DESC order.
    pub delta: Vec<RelId>,
    /// The hop ids a tombstone hides at the walk's `ts`. A deleted or
    /// repositioned hop still sits in `sealed` (the fold never rewrites the
    /// stream), so its id travels here and the install hides it.
    pub tombstones: Vec<RelId>,
}

/// One adjacency chunk as an open-time install reads it out of the opt-in
/// checkpoint payload (ADR-0551): borrowed from the decoded payload row, so
/// the install copies the sealed stream straight into the arena and the hop
/// ids straight into their chunk — no intermediate `Vec` per chunk.
#[derive(Clone, Copy, Debug)]
pub struct AdjacencyChunkRef<'a> {
    pub node_id: NodeId,
    pub type_id: u32,
    /// `true` for the node's incoming (end) side, `false` for outgoing.
    pub incoming: bool,
    /// Hop count `sealed` encodes; 0 when the chunk holds no sealed stream.
    pub sealed_hops: u64,
    /// The sealed ADR-0551 codec stream, verbatim — empty when unsealed.
    pub sealed: &'a [u8],
    /// The unsealed delta hop ids, in stored ts-DESC order.
    pub delta: &'a [RelId],
    /// The hop ids a tombstone hides at the payload's watermark — the ids
    /// the verbatim sealed stream still carries but a read hides.
    pub tombstones: &'a [RelId],
}

/// Where a checkpoint load's adjacency comes from: derived from the restored
/// relationship rows, or installed from the opt-in checkpoint payload
/// (ADR-0551).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdjacencySource {
    /// Build adjacency from the restored relationship rows.
    RelRows,
    /// The load installed every chunk of the checkpoint's adjacency payload;
    /// keep it instead of rebuilding.
    Payload,
}

/// The fill side of a graph: what a codec installs checkpoint rows through.
/// `Sync` because the restore installs one worker's rows per core — a
/// slice's install is the same interior-mutable call the single-threaded
/// path made.
pub trait GraphImport: GraphExport + Sync {
    fn set_next_node_id(&self, id: i64);
    fn set_next_rel_id(&self, id: i64);
    fn set_commit_ts(&self, ts: u64);
    fn install_named_index(&self, index: IndexDef) -> Result<()>;
    fn install_named_constraint(&self, constraint: ConstraintDef) -> Result<()>;
    fn restore_checkpoint_theater(&self, theater: CheckpointTheater) -> Result<()>;
    fn mark_legacy_unenforced(&self);
    /// End the checkpoint load. `adjacency` says whether that tail derives
    /// adjacency from the restored rel rows or keeps the payload install;
    /// the load-tail index rebuild is the open flow's own post-replay step.
    fn finish_restore(&self, adjacency: AdjacencySource);
    /// Install one decoded window of the checkpoint's adjacency payload —
    /// [`GraphExport::for_each_adjacency_chunk`]'s inverse: a sealed stream
    /// lands in the sealed tier as it stands, the delta hop ids in their
    /// chunk, and a chunk's tombstone ids hide the hops they name. Both are
    /// borrowed from the decoded row, so the install copies them straight
    /// into their storage. A graph that cannot take a chunk returns an
    /// error; the caller falls back to the row re-fold.
    fn install_adjacency_chunks(&self, chunks: &[AdjacencyChunkRef<'_>]) -> Result<()>;
    /// Install a slice of checkpoint nodes — one decoded batch, a
    /// window-disjoint run of ids. The installer owns the row-id check:
    /// every id in the slice is validated here, so a caller does not
    /// repeat it, and an earlier row for an id is replaced by the later one.
    fn load_checkpoint_nodes(&self, nodes: &[Node]) -> Result<()>;
    /// Relationship sibling of [`Self::load_checkpoint_nodes`], with the
    /// same id obligation over a row's id and both endpoints.
    fn load_checkpoint_rels(&self, rels: &[Relationship]) -> Result<()>;
    /// Restore one node: the one-element slice of
    /// [`Self::load_checkpoint_nodes`].
    fn load_checkpoint_node(&self, node: Node) -> Result<()> {
        self.load_checkpoint_nodes(std::slice::from_ref(&node))
    }
    /// Restore one relationship: the one-element slice of
    /// [`Self::load_checkpoint_rels`].
    fn load_checkpoint_rel(&self, rel: Relationship) -> Result<()> {
        self.load_checkpoint_rels(std::slice::from_ref(&rel))
    }
    fn apply_replayed_redo(&self, ops: Vec<RedoOp>, commit_ts: u64) -> Result<()>;
    fn rebuild_indexes(&self);
    fn restore_graph_type_spec(&self, spec: Option<&str>) -> Result<()>;
    fn restore_legacy_enforced_constraints(&self, constraints: Vec<Constraint>) -> Result<()>;
}

/// A graph a codec can both read and fill. `Default` builds the empty
/// engine recovery starts from.
pub trait SnapshotGraph: GraphImport + Default {}

impl<T: GraphImport + Default> SnapshotGraph for T {}
