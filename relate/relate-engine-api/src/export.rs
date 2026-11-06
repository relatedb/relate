//! What a snapshot codec may ask of a query engine, and what recovery may
//! put back. Codecs never name a concrete graph.

use std::collections::HashSet;

use relate_catalog::{ConstraintDef, IndexDef};
use relate_core::{Node, NodeId, Relationship, RelId, Result};
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
}

pub trait GraphImport: GraphExport {
    fn set_next_node_id(&self, id: i64);
    fn set_next_rel_id(&self, id: i64);
    fn set_commit_ts(&self, ts: u64);
    fn install_named_index(&self, index: IndexDef) -> Result<()>;
    fn install_named_constraint(&self, constraint: ConstraintDef) -> Result<()>;
    fn restore_checkpoint_theater(&self, theater: CheckpointTheater) -> Result<()>;
    fn mark_legacy_unenforced(&self);
    fn finish_restore(&self);
    fn load_checkpoint_node(&self, node: Node) -> Result<()>;
    fn load_checkpoint_rel(&self, rel: Relationship) -> Result<()>;
    fn apply_replayed_redo(&self, ops: Vec<RedoOp>, commit_ts: u64) -> Result<()>;
    fn rebuild_indexes(&self);
    fn restore_graph_type_spec(&self, spec: Option<&str>) -> Result<()>;
    fn restore_legacy_enforced_constraints(&self, constraints: Vec<Constraint>) -> Result<()>;
}

/// A graph a codec can both read and fill. `Default` builds the empty
/// engine recovery starts from.
pub trait SnapshotGraph: GraphImport + Default {}

impl<T: GraphImport + Default> SnapshotGraph for T {}
