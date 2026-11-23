//! What a checkpoint may ask of a query engine, and what recovery may
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
    /// The ids changed since the previous take — one carrier
    /// ([`CheckpointDirty`]) for the pair, so an implementor spells the
    /// node/rel split once.
    fn checkpoint_dirty(&self) -> CheckpointDirty;
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
    /// SHOW-style catalog DDL the schema mirror writes beside a checkpoint.
    fn catalog_ddl(&self) -> Vec<String>;
}

/// The checkpoint-dirty marks a graph with live write state carries: a
/// checkpoint's cut takes them, a failed dump hands them back. Separate
/// from [`GraphExport`], so a read-only view over a frozen cut — the
/// storage crate's checkpoint cut — can neither clear nor restore the
/// live marks.
pub trait CheckpointDirtyMarks: GraphExport {
    /// Hand over the ids changed since the previous take and clear them:
    /// the checkpoint's cut takes the marks it is about to dump.
    fn take_checkpoint_dirty(&self) -> CheckpointDirty;
    /// Hand back a failed dump's marks, so the next checkpoint covers them.
    fn merge_checkpoint_dirty(&self, dirty: &CheckpointDirty);
}

/// The ids a graph's checkpoint-dirty tracking has marked since the
/// previous take (ADR-0022).
#[derive(Clone, Default)]
pub struct CheckpointDirty {
    pub nodes: HashSet<NodeId>,
    pub rels: HashSet<RelId>,
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
}

/// A graph a codec can both read and fill. `Default` builds the empty
/// engine recovery starts from.
pub trait SnapshotGraph: GraphImport + Default {}

impl<T: GraphImport + Default> SnapshotGraph for T {}
