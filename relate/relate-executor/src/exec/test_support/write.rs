//! Write machinery: scratches over a graph, write execution, and commit.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use relate_core::Result;
use relate_front::parse_and_lower;
use relate_planner::{plan, plan_with_tokens, NoPatternStats};

use crate::graph::Graph;
use crate::overlay::{ExecGraph, WriteScratch};
use crate::redo::RedoOp;

use super::super::{execute, execute_unvalidated, ExecRequest, QueryResult};

/// A write scratch reading at `ts`, its id allocators continuing the graph's.
pub(crate) fn write_scratch_at(g: &Graph, ts: u64) -> RefCell<WriteScratch> {
    RefCell::new(WriteScratch::new(
        ts,
        Arc::new(AtomicU64::new(g.next_node_id() as u64)),
        Arc::new(AtomicU64::new(g.next_rel_id() as u64)),
    ))
}

/// A write scratch reading at the graph's current commit.
pub(crate) fn write_scratch(g: &Graph) -> RefCell<WriteScratch> {
    write_scratch_at(g, g.commit_ts())
}

/// Run `cypher` on `scratch` through `execute`, constraint validation
/// included. Nothing reaches the graph until `commit_scratch`.
pub(crate) fn exec_write(
    g: &Graph,
    scratch: &RefCell<WriteScratch>,
    cypher: &str,
) -> Result<QueryResult> {
    run_write(g, scratch, cypher, execute)
}

/// Run `cypher` the way the facade's ADR-0210 auto-commit path runs a
/// `CALL { … } IN TRANSACTIONS` statement: planned with the store's
/// tokens resolved onto the IR (ADR-0541, the way `Database` plans) and
/// executed with the per-batch commit sink attached, so each batch
/// commits to the base and rebases the scratch mid-statement. The run
/// the mid-statement token staleness (a key interned by the statement's
/// own batches after the plan resolved it) is tested under.
pub(crate) fn exec_write_batched(
    g: &Graph,
    scratch: &RefCell<WriteScratch>,
    cypher: &str,
    sink: &dyn crate::ctx::BatchCommit,
) -> Result<QueryResult> {
    let q = parse_and_lower(cypher)?;
    let p = plan_with_tokens(q, &mut |_| false, &NoPatternStats, g)?;
    let params = BTreeMap::new();
    let mut request = ExecRequest::new(&p, &params, cypher);
    request.batch_commit = Some(sink);
    execute(&ExecGraph::write(g, scratch), &request)
}

/// [`exec_write`] through `execute_unvalidated`: the writes stay in the
/// slice the next constraint validation of `scratch` checks.
pub(crate) fn exec_write_unvalidated(
    g: &Graph,
    scratch: &RefCell<WriteScratch>,
    cypher: &str,
) -> Result<QueryResult> {
    run_write(g, scratch, cypher, execute_unvalidated)
}

/// One autocommit statement: `cypher` on a fresh scratch through `execute`,
/// committed when it succeeds and discarded when it fails. It composes this
/// module's write machinery, so it lives here rather than in the
/// forest-neutral `crate::test_support`.
pub(crate) fn exec_autocommit(g: &Graph, cypher: &str) -> Result<QueryResult> {
    let scratch = write_scratch(g);
    let result = exec_write(g, &scratch, cypher)?;
    commit_scratch(g, scratch);
    Ok(result)
}

/// [`exec_write`] on a fresh scratch, unwrapped; the scratch is dropped
/// without committing, so the graph is untouched. The uncommitted
/// counterpart of [`exec_autocommit`].
pub(crate) fn exec_write_uncommitted(g: &Graph, cypher: &str) -> QueryResult {
    exec_write(g, &write_scratch(g), cypher).unwrap()
}

fn run_write(
    g: &Graph,
    scratch: &RefCell<WriteScratch>,
    cypher: &str,
    run: fn(&ExecGraph<'_>, &ExecRequest<'_>) -> Result<QueryResult>,
) -> Result<QueryResult> {
    let p = plan(parse_and_lower(cypher)?, &mut |_| false)?;
    run(
        &ExecGraph::write(g, scratch),
        &ExecRequest::new(&p, &BTreeMap::new(), cypher),
    )
}

/// Apply `scratch`'s redo to the graph as the next commit. A scratch that
/// wrote nothing commits nothing, as a read-only statement does.
pub(crate) fn commit_scratch(g: &Graph, scratch: RefCell<WriteScratch>) {
    let ops = scratch.into_inner().take_redo();
    if !ops.is_empty() {
        g.apply_redo(ops, g.commit_ts() + 1).unwrap();
    }
}

/// The overlay seam's one `BatchCommit` test double (F-1-27): applies the
/// batch's redo ops to the base graph at the next commit ts and counts
/// its calls. The write-family module is its canonical home — both the
/// overlay batch-commit tests and the exec-subtree predicate-spec tests
/// construct it.
pub(crate) struct ApplySink<'a> {
    pub(crate) graph: &'a Graph,
    pub(crate) calls: AtomicUsize,
}

impl crate::ctx::BatchCommit for ApplySink<'_> {
    fn commit_batch(&self, ops: Vec<RedoOp>, _snapshot_ts: u64) -> Result<u64> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let commit_ts = self.graph.commit_ts() + 1;
        self.graph.apply_redo(ops, commit_ts)?;
        Ok(commit_ts)
    }
}
