//! Batch commit over the overlay: `commit_batch` hands the batch's redo
//! ops to the sink and rebases the scratch, and a failed batch restores
//! the pre-batch scratch.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::ExecGraph;
use crate::exec::test_support::{exec_write, write_scratch};
use crate::graph::Graph;
use crate::redo::RedoOp;
use relate_core::Result;

/// The overlay seam's one `BatchCommit` test double (F-1-27): applies the
/// batch's redo ops to the base graph at the next commit ts and counts
/// its calls.
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

/// ADR-0210: `ExecGraph::commit_batch` hands the batch's redo ops to the
/// sink, rebases the scratch onto the new commit timestamp, and preserves
/// the accumulated statement stats; a call with no new ops is a no-op
/// (the sink is not called again).
#[test]
fn commit_batch_applies_ops_and_rebases_scratch() {
    let g = Graph::default();
    let scratch = write_scratch(&g);
    exec_write(&g, &scratch, "CREATE (:Person {name: 'Ada'})").unwrap();
    assert_eq!(g.live_node_count(), 0, "uncommitted batch stays in scratch");

    let view = ExecGraph::write(&g, &scratch);
    let sink = ApplySink {
        graph: &g,
        calls: AtomicUsize::new(0),
    };
    view.commit_batch(&sink).unwrap();
    assert_eq!(sink.calls.load(Ordering::Relaxed), 1);
    assert_eq!(g.live_node_count(), 1, "batch 1 is committed to the base");
    assert_eq!(
        view.stats().nodes_created,
        1,
        "statement stats survive the rebase"
    );
    assert_eq!(
        scratch.borrow().snapshot_ts(),
        g.commit_ts(),
        "scratch rebased onto the batch commit"
    );

    // The fresh scratch works for the next batch, and an empty batch does
    // not reach the sink. (`exec_write` runs a full `execute`, which resets
    // statement stats — the cross-batch stats carry-over is asserted above,
    // inside one execute.)
    exec_write(&g, &scratch, "CREATE (:Person {name: 'Grace'})").unwrap();
    assert_eq!(g.live_node_count(), 1);
    view.commit_batch(&sink).unwrap();
    assert_eq!(g.live_node_count(), 2);
    view.commit_batch(&sink).unwrap();
    assert_eq!(
        sink.calls.load(Ordering::Relaxed),
        2,
        "an empty batch is not committed"
    );
}

#[test]
fn failed_batch_restores_the_pre_batch_scratch() {
    let g = Graph::default();
    let scratch = write_scratch(&g);
    exec_write(&g, &scratch, "CREATE (n:Person {name: 'Ada'})").unwrap();
    let (ada, props) = {
        let s = scratch.borrow();
        assert_eq!(s.redo_ops().len(), 1);
        let RedoOp::CreateNode { id, properties, .. } = &s.redo_ops()[0] else {
            panic!("first op is the create");
        };
        let node = s
            .batch
            .nodes
            .get(id)
            .expect("create installs the scratch node");
        assert!(
            Arc::ptr_eq(&node.properties, properties),
            "the scratch node and its redo op share the property map"
        );
        assert_eq!(
            s.applied_ops(),
            1,
            "the statement's own applied count is its create"
        );
        (*id, Arc::clone(properties))
    };
    exec_write(
        &g,
        &scratch,
        "UNWIND [1] AS i \
         CALL { WITH i CREATE (:Gone {name: 'no'}) RETURN i / 0 AS bad } \
         IN TRANSACTIONS OF 1 ROWS ON ERROR CONTINUE \
         RETURN bad",
    )
    .unwrap();
    let s = scratch.borrow();
    assert_eq!(s.redo_ops().len(), 1, "the failed batch's create is gone");
    assert_eq!(
        s.applied_ops(),
        0,
        "the second statement's entry reset zeroed the applied count and the \
         rollback image restored that pre-batch zero, so the failed batch's \
         create is not counted"
    );
    assert!(s.batch.nodes.contains_key(&ada));
    assert!(
        s.batch.nodes.keys().all(|id| *id == ada),
        "only the pre-batch node remains"
    );
    let RedoOp::CreateNode { properties, .. } = &s.redo_ops()[0] else {
        panic!("the original create is still the only redo op");
    };
    assert!(Arc::ptr_eq(properties, &props));
}
