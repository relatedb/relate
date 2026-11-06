//! Facade-facing query engine. One call per statement or per commit.
//! Row-at-a-time execution stays inside the engine.

use std::collections::BTreeMap;

use relate_core::{CancelToken, Result, Value};
use relate_planner::{LogicalPlan, PatternStats};

use crate::catalog_image::SchemaSnapshot;
use crate::export::{GraphExport, GraphImport};
use crate::redo::RedoOp;

/// A read the facade has already planned and admitted.
pub struct ReadRequest<'a> {
    pub snapshot_ts: u64,
    pub schema: &'a SchemaSnapshot,
    pub plan: &'a LogicalPlan,
    pub params: &'a BTreeMap<String, Value>,
    pub query_text: &'a str,
    pub principal: Option<&'a str>,
    pub cancel: Option<&'a CancelToken>,
    pub ontology_document: Option<&'a str>,
}

/// A write the facade has already planned. `scratch` is the engine's
/// private transaction state.
pub struct WriteRequest<'a, S> {
    pub scratch: &'a mut S,
    pub plan: &'a LogicalPlan,
    pub params: &'a BTreeMap<String, Value>,
    pub query_text: &'a str,
    pub principal: Option<&'a str>,
    pub cancel: Option<&'a CancelToken>,
    pub ontology_document: Option<&'a str>,
}

pub trait QueryEngine: GraphExport + GraphImport {
    /// Private transaction scratch. Not shared across engines.
    type Scratch;

    /// Rows, counters, and effect. The concrete engine names the result
    /// type so the facade does not depend on a per-row trait.
    type Output;

    fn gc_versions(&self);
    fn snapshot_pinned(&self) -> bool;
    fn apply_redo(&self, ops: Vec<RedoOp>, commit_ts: u64) -> Result<()>;
    fn validate_redo_group(&self, records: &[(&[RedoOp], u64)]) -> Result<()>;
    /// Planner cost input for one statement. Estimates that read `params`
    /// must not outlive this borrow.
    fn pattern_stats<'a>(
        &'a self,
        params: &'a BTreeMap<String, Value>,
    ) -> Box<dyn PatternStats + 'a>;
    fn execute_read(&self, request: ReadRequest<'_>) -> Result<Self::Output>;
    fn execute_write(&self, request: WriteRequest<'_, Self::Scratch>) -> Result<Self::Output>;
    fn take_redo(&self, scratch: &mut Self::Scratch) -> Vec<RedoOp>;
}
