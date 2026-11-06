//! The in-memory query engine: today's graph and executor behind
//! [`relate_engine_api::QueryEngine`]. The facade holds this type. A later
//! engine implements the same trait in its own crate.

use std::cell::RefCell;
use std::ops::{Deref, DerefMut};

use relate_core::Result;
use relate_engine_api::{GraphExport, GraphImport, QueryEngine, ReadRequest, WriteRequest};
use relate_planner::PatternStats;

use crate::exec::{execute, execute_read_at, ExecRequest, QueryResult, ReadAdmission};
use crate::graph::Graph;
use crate::overlay::{ExecGraph, WriteScratch};
use crate::redo::RedoOp;

/// Execution plus the in-memory graph and index layout.
pub struct InMemoryQueryEngine {
    graph: Graph,
}

impl InMemoryQueryEngine {
    pub fn new(graph: Graph) -> Self {
        Self { graph }
    }

    /// The graph a snapshot codec checkpoints. Reload still opens a [`Graph`]
    /// and wraps it here.
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// Run one planned statement on a view the caller already built.
    /// Profile metrics and batch commits ride on `request`.
    pub fn execute(&self, view: &ExecGraph<'_>, request: &ExecRequest<'_>) -> Result<QueryResult> {
        execute(view, request)
    }
}

impl Default for InMemoryQueryEngine {
    fn default() -> Self {
        Self::new(Graph::default())
    }
}

impl Deref for InMemoryQueryEngine {
    type Target = Graph;

    fn deref(&self) -> &Graph {
        &self.graph
    }
}

impl DerefMut for InMemoryQueryEngine {
    fn deref_mut(&mut self) -> &mut Graph {
        &mut self.graph
    }
}

impl QueryEngine for InMemoryQueryEngine {
    type Scratch = RefCell<WriteScratch>;
    type Output = QueryResult;

    fn gc_versions(&self) {
        self.graph.gc_versions();
    }

    fn snapshot_pinned(&self) -> bool {
        self.graph.snapshot_pinned()
    }

    fn apply_redo(&self, ops: Vec<RedoOp>, commit_ts: u64) -> Result<()> {
        self.graph.apply_redo(ops, commit_ts)
    }

    fn validate_redo_group(&self, records: &[(&[RedoOp], u64)]) -> Result<()> {
        self.graph
            .validate_redo_group_applicability(records.iter().copied())
    }

    fn pattern_stats<'a>(
        &'a self,
        params: &'a std::collections::BTreeMap<String, relate_core::Value>,
    ) -> Box<dyn PatternStats + 'a> {
        Box::new(crate::exec::graph_stats::GraphStats::new(&self.graph, params))
    }

    fn execute_read(&self, request: ReadRequest<'_>) -> Result<QueryResult> {
        execute_read_at(&ReadAdmission {
            graph: &self.graph,
            snapshot_ts: request.snapshot_ts,
            schema: request.schema,
            request: ExecRequest {
                plan: request.plan,
                params: request.params,
                query_text: request.query_text,
                principal: request.principal,
                batch_commit: None,
                plan_metrics: None,
            },
            cancel: request.cancel,
            ontology_document: request.ontology_document,
        })
    }

    fn execute_write(&self, request: WriteRequest<'_, Self::Scratch>) -> Result<QueryResult> {
        let view = ExecGraph::write(&self.graph, request.scratch);
        self.execute(
            &view,
            &ExecRequest {
                plan: request.plan,
                params: request.params,
                query_text: request.query_text,
                principal: request.principal,
                batch_commit: None,
                plan_metrics: None,
            },
        )
    }

    fn take_redo(&self, scratch: &mut Self::Scratch) -> Vec<RedoOp> {
        scratch.get_mut().take_redo()
    }
}

impl GraphExport for InMemoryQueryEngine {
    fn commit_ts(&self) -> u64 {
        Graph::commit_ts(&self.graph)
    }
    fn next_node_id(&self) -> i64 {
        Graph::next_node_id(&self.graph)
    }
    fn next_rel_id(&self) -> i64 {
        Graph::next_rel_id(&self.graph)
    }
    fn live_node_count(&self) -> usize {
        Graph::live_node_count(&self.graph)
    }
    fn dirty_node_ids(&self) -> std::collections::HashSet<relate_core::NodeId> {
        Graph::dirty_node_ids(&self.graph)
    }
    fn dirty_rel_ids(&self) -> std::collections::HashSet<relate_core::RelId> {
        Graph::dirty_rel_ids(&self.graph)
    }
    fn all_node_ids_at(&self, ts: u64) -> Vec<relate_core::NodeId> {
        Graph::all_node_ids_at(&self.graph, ts)
    }
    fn all_rel_ids_at(&self, ts: u64) -> Vec<relate_core::RelId> {
        Graph::all_rel_ids_at(&self.graph, ts)
    }
    fn node_at(&self, id: relate_core::NodeId, ts: u64) -> Option<relate_core::Node> {
        Graph::node_at(&self.graph, id, ts)
    }
    fn rel_at(&self, id: relate_core::RelId, ts: u64) -> Option<relate_core::Relationship> {
        Graph::rel_at(&self.graph, id, ts)
    }
    fn node_checkpoint_bytes(&self, id: relate_core::NodeId, ts: u64) -> Result<Option<u64>> {
        Graph::node_checkpoint_bytes(&self.graph, id, ts)
    }
    fn rel_checkpoint_bytes(&self, id: relate_core::RelId, ts: u64) -> Result<Option<u64>> {
        Graph::rel_checkpoint_bytes(&self.graph, id, ts)
    }
    fn named_indexes(&self) -> Vec<relate_catalog::IndexDef> {
        Graph::named_indexes(&self.graph)
    }
    fn named_constraints(&self) -> Vec<relate_catalog::ConstraintDef> {
        Graph::named_constraints(&self.graph)
    }
    fn checkpoint_theater_for_write(&self) -> Result<relate_engine_api::CheckpointTheater> {
        Graph::checkpoint_theater_for_write(&self.graph)
    }
    fn graph_type_spec(&self) -> Result<Option<String>> {
        Graph::graph_type_spec(&self.graph)
    }
    fn clear_checkpoint_dirty(&self) {
        Graph::clear_checkpoint_dirty(&self.graph)
    }
    fn catalog_ddl(&self) -> Vec<String> {
        crate::catalog_create_statements(&self.graph)
    }
}

impl GraphImport for InMemoryQueryEngine {
    fn set_next_node_id(&self, id: i64) {
        Graph::set_next_node_id(&self.graph, id)
    }
    fn set_next_rel_id(&self, id: i64) {
        Graph::set_next_rel_id(&self.graph, id)
    }
    fn set_commit_ts(&self, ts: u64) {
        Graph::set_commit_ts(&self.graph, ts)
    }
    fn install_named_index(&self, index: relate_catalog::IndexDef) -> Result<()> {
        Graph::install_named_index(&self.graph, index)
    }
    fn install_named_constraint(&self, constraint: relate_catalog::ConstraintDef) -> Result<()> {
        Graph::install_named_constraint(&self.graph, constraint)
    }
    fn restore_checkpoint_theater(&self, theater: relate_engine_api::CheckpointTheater) -> Result<()> {
        Graph::restore_checkpoint_theater(&self.graph, theater)
    }
    fn mark_legacy_unenforced(&self) {
        Graph::mark_legacy_unenforced(&self.graph)
    }
    fn finish_restore(&self) {
        Graph::finish_restore(&self.graph)
    }
    fn load_checkpoint_node(&self, node: relate_core::Node) -> Result<()> {
        Graph::load_checkpoint_node(&self.graph, node)
    }
    fn load_checkpoint_rel(&self, rel: relate_core::Relationship) -> Result<()> {
        Graph::load_checkpoint_rel(&self.graph, rel)
    }
    fn apply_replayed_redo(&self, ops: Vec<RedoOp>, commit_ts: u64) -> Result<()> {
        Graph::apply_replayed_redo(&self.graph, ops, commit_ts)
    }
    fn rebuild_indexes(&self) {
        Graph::rebuild_indexes(&self.graph)
    }
    fn restore_graph_type_spec(&self, spec: Option<&str>) -> Result<()> {
        Graph::restore_graph_type_spec(&self.graph, spec)
    }
    fn restore_legacy_enforced_constraints(
        &self,
        constraints: Vec<relate_model::Constraint>,
    ) -> Result<()> {
        Graph::restore_legacy_enforced_constraints(&self.graph, constraints)
    }
}
