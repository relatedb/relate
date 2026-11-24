//! What a snapshot codec may ask of a query engine, and what recovery may
//! put back. Codecs never name a concrete graph.

use std::collections::HashSet;
use std::sync::Arc;

use relate_catalog::{ConstraintDef, IndexDef};
use relate_core::{Node, NodeId, PropertyMap, RelId, Relationship, Result};
use relate_model::Constraint;
use serde::{Deserialize, Serialize};

use crate::catalog_image::CheckpointTheater;
use crate::commit::TransactionRecord;
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
    /// Every retained transaction record, ascending `commit_ts`
    /// (ADR-0610) — the closed observation surfaces' whole-table read
    /// (the `db.transactions` procedure and the doc-hidden test API);
    /// the checkpoint's `transactions/` family publishes the same
    /// records through [`Self::retained_transactions_into`], the
    /// streaming form, never this materializing one. Empty on a store
    /// that never opted in.
    fn retained_transactions(&self) -> Vec<TransactionRecord>;
    /// Push every retained transaction record, ascending `commit_ts`
    /// (ADR-0610), to `sink` one at a time — the checkpoint family
    /// publish's bounded fold (ADR-0555): the publish serializes and
    /// appends each record as it arrives, so no whole-table Vec and no
    /// whole-interval encode ever exists. Empty on a store that never
    /// opted in.
    fn retained_transactions_into(
        &self,
        sink: &mut dyn FnMut(&TransactionRecord) -> Result<()>,
    ) -> Result<()>;
    /// Whether this graph's open enabled `value-provenance` retention
    /// (ADR-0610): the retained-chain family publishes while on and —
    /// the durable half of the collapse — drops while off.
    fn value_provenance_retention(&self) -> bool;
    /// Push the node chains the retained-chain family publishes this
    /// checkpoint (ADR-0610) to `sink`, one element's complete chain at
    /// a time — every node dirty since the last checkpoint, ids sorted
    /// so one checkpoint's delta is deterministic bytes, every version
    /// with its `xmin`/`xmax` beside the row it installed. The
    /// publish's bounded fold (ADR-0555): the family step serializes
    /// and appends each chain as it arrives, never a whole-delta Vec.
    /// Empty on a store that never opted in.
    fn retained_node_chains_into(
        &self,
        sink: &mut dyn FnMut(&RetainedNodeChain) -> Result<()>,
    ) -> Result<()>;
    /// [`Self::retained_node_chains_into`]'s twin over the relationship
    /// chains.
    fn retained_rel_chains_into(
        &self,
        sink: &mut dyn FnMut(&RetainedRelChain) -> Result<()>,
    ) -> Result<()>;
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
    fn finish_restore(&self);
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
    /// Retain one committed transaction's record (ADR-0610) — the one
    /// insert the live commit pipeline, the WAL replay and the
    /// `transactions/` family load funnel through; the implementation
    /// decides which records retain (a typed record of any family, or one
    /// carrying provenance fields), so no caller re-derives that policy.
    fn record_retained_transaction(&self, record: TransactionRecord);
    /// Drop every retained transaction record (ADR-0610) — the in-memory
    /// half of the collapse an open without the opt-in performs at its
    /// checkpoint.
    fn clear_retained_transactions(&self);
    /// Restore one node's complete retained version chain (ADR-0610) —
    /// the family load's install, replacing the checkpoint image's
    /// collapsed row for the id (or inserting a chain the image never
    /// held: a deleted element's tombstoned chain stays auditable).
    /// Only valid on a graph being restored, before any write.
    fn restore_retained_node_chain(&self, chain: RetainedNodeChain) -> Result<()>;
    /// [`Self::restore_retained_node_chain`]'s twin over the
    /// relationship chains.
    fn restore_retained_rel_chain(&self, chain: RetainedRelChain) -> Result<()>;
    /// The open path's retained-family window (ADR-0610): re-take the
    /// restore phase the codec tail's `finish_restore` closed — the
    /// `transactions/` and `chains/` families are the same open's later
    /// restore slices — run the load step, then close the phase again.
    /// Valid only in the open path's construction window (after the
    /// codec load, before the WAL replay or any write): a write in
    /// flight refuses, and adjacency is never rebuilt.
    fn retained_families_restore_step(&self, load: &mut dyn FnMut() -> Result<()>) -> Result<()>;
}

/// One retained version of a node's whole-row chain (ADR-0610): the
/// version's `xmin`/`xmax` beside the row it installed, in the WAL ops'
/// own field types and tagged value encoding — the retained-chain
/// family's part rows and the graph's restore share this one shape, so a
/// part decodes into the graph without a second value vocabulary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetainedNodeVersion {
    pub xmin: u64,
    pub xmax: u64,
    pub labels: Arc<Vec<String>>,
    pub properties: Arc<PropertyMap>,
}

/// One retained version of a relationship's chain (ADR-0610) —
/// [`RetainedNodeVersion`]'s twin: the bounds beside the row (endpoints
/// and type included, spelled per version so a part row decodes
/// standalone).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetainedRelVersion {
    pub xmin: u64,
    pub xmax: u64,
    pub start: NodeId,
    pub end: NodeId,
    pub rel_type: String,
    pub properties: Arc<PropertyMap>,
}

/// One element's complete retained version chain (ADR-0610): every
/// version, oldest to newest — the unit the retained-chain family
/// publishes per dirty element and restores per element.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetainedNodeChain {
    pub id: NodeId,
    pub versions: Vec<RetainedNodeVersion>,
}

/// [`RetainedNodeChain`]'s twin over the relationship chains.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetainedRelChain {
    pub id: RelId,
    pub versions: Vec<RetainedRelVersion>,
}

/// A graph a codec can both read and fill. `Default` builds the empty
/// engine recovery starts from.
pub trait SnapshotGraph: GraphImport + Default {}

impl<T: GraphImport + Default> SnapshotGraph for T {}
