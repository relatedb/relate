//! The in-memory graph as the snapshot contract other crates call.

use std::collections::HashSet;

use relate_catalog::{ConstraintDef, IndexDef};
use relate_core::{Node, NodeId, RelId, Relationship, Result};
use relate_engine_api::{CheckpointTheater, GraphExport, GraphImport, RedoOp};
use relate_model::Constraint;

use super::Graph;

impl GraphExport for Graph {
    fn commit_ts(&self) -> u64 {
        Graph::commit_ts(self)
    }
    fn next_node_id(&self) -> i64 {
        Graph::next_node_id(self)
    }
    fn next_rel_id(&self) -> i64 {
        Graph::next_rel_id(self)
    }
    fn live_node_count(&self) -> usize {
        Graph::live_node_count(self)
    }
    fn dirty_node_ids(&self) -> HashSet<NodeId> {
        Graph::dirty_node_ids(self)
    }
    fn dirty_rel_ids(&self) -> HashSet<RelId> {
        Graph::dirty_rel_ids(self)
    }
    fn all_node_ids_at(&self, ts: u64) -> Vec<NodeId> {
        Graph::all_node_ids_at(self, ts)
    }
    fn all_rel_ids_at(&self, ts: u64) -> Vec<RelId> {
        Graph::all_rel_ids_at(self, ts)
    }
    fn node_at(&self, id: NodeId, ts: u64) -> Option<Node> {
        Graph::node_at(self, id, ts)
    }
    fn rel_at(&self, id: RelId, ts: u64) -> Option<Relationship> {
        Graph::rel_at(self, id, ts)
    }
    fn node_checkpoint_bytes(&self, id: NodeId, ts: u64) -> Result<Option<u64>> {
        Graph::node_checkpoint_bytes(self, id, ts)
    }
    fn rel_checkpoint_bytes(&self, id: RelId, ts: u64) -> Result<Option<u64>> {
        Graph::rel_checkpoint_bytes(self, id, ts)
    }
    fn named_indexes(&self) -> Vec<IndexDef> {
        Graph::named_indexes(self)
    }
    fn named_constraints(&self) -> Vec<ConstraintDef> {
        Graph::named_constraints(self)
    }
    fn checkpoint_theater_for_write(&self) -> Result<CheckpointTheater> {
        Graph::checkpoint_theater_for_write(self)
    }
    fn graph_type_spec(&self) -> Result<Option<String>> {
        Graph::graph_type_spec(self)
    }
    fn clear_checkpoint_dirty(&self) {
        Graph::clear_checkpoint_dirty(self)
    }
    fn catalog_ddl(&self) -> Vec<String> {
        crate::catalog_create_statements(self)
    }
}

impl GraphImport for Graph {
    fn set_next_node_id(&self, id: i64) {
        Graph::set_next_node_id(self, id)
    }
    fn set_next_rel_id(&self, id: i64) {
        Graph::set_next_rel_id(self, id)
    }
    fn set_commit_ts(&self, ts: u64) {
        Graph::set_commit_ts(self, ts)
    }
    fn install_named_index(&self, index: IndexDef) -> Result<()> {
        Graph::install_named_index(self, index)
    }
    fn install_named_constraint(&self, constraint: ConstraintDef) -> Result<()> {
        Graph::install_named_constraint(self, constraint)
    }
    fn restore_checkpoint_theater(&self, theater: CheckpointTheater) -> Result<()> {
        Graph::restore_checkpoint_theater(self, theater)
    }
    fn mark_legacy_unenforced(&self) {
        Graph::mark_legacy_unenforced(self)
    }
    fn finish_restore(&self) {
        Graph::finish_restore(self)
    }
    fn load_checkpoint_node(&self, node: Node) -> Result<()> {
        Graph::load_checkpoint_node(self, node)
    }
    fn load_checkpoint_rel(&self, rel: Relationship) -> Result<()> {
        Graph::load_checkpoint_rel(self, rel)
    }
    fn apply_replayed_redo(&self, ops: Vec<RedoOp>, commit_ts: u64) -> Result<()> {
        Graph::apply_replayed_redo(self, ops, commit_ts)
    }
    fn rebuild_indexes(&self) {
        Graph::rebuild_indexes(self)
    }
    fn restore_graph_type_spec(&self, spec: Option<&str>) -> Result<()> {
        Graph::restore_graph_type_spec(self, spec)
    }
    fn restore_legacy_enforced_constraints(&self, constraints: Vec<Constraint>) -> Result<()> {
        Graph::restore_legacy_enforced_constraints(self, constraints)
    }
}
