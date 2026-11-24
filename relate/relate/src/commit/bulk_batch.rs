//! Batched write commits (ADR-0209, ADR-0210): the ADR-0210 per-batch
//! commit sink for `CALL { … } IN TRANSACTIONS`, and the ADR-0209
//! decide-then-execute bulk plan (`BulkBatchPlan::prepare` probes,
//! `run` commits batch-by-batch through the group-commit pipeline).

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use relate_executor::{
    analyze_bulk_write, BulkCursor, BulkTargets, BulkWrite, ExecGraph, QueryResult as ExecResult,
    RedoOp, StatementEffect,
};
use relate_planner::LogicalPlan;
use web_time::Instant;

use super::{acquire_guarded_pin, run_on_scratch, try_wal_commit, with_pinned_graph};
use crate::commit_timing::elapsed_nanos;
use crate::{CancelToken, Error, Result, Shared, Value};
use relate_engine_api::TxChannelState;

/// ADR-0210 per-batch commit sink for `CALL { … } IN TRANSACTIONS`: each
/// batch's redo ops go through the same group-commit pipeline as a whole
/// statement (`try_wal_commit`), so every batch is WAL-logged and applied
/// before the next one starts. `snapshot_ts` is the batch's base — the
/// previous batch's commit (or the statement's start for the first).
/// `tx_metadata` is the statement's per-transaction metadata cell
/// (ADR-0610): the first batch's commit drains it — the cell describes
/// the statement's transaction, and its first WAL record carries the
/// cell's map and record type.
pub(super) struct GroupBatchCommit<'a> {
    pub(super) shared: Arc<Shared>,
    /// The last committed batch's own commit timestamp (ADR-0502 §7), or
    /// `None` while no batch has committed — the statement's commit
    /// sequence when its final redo is empty, recorded here rather than
    /// re-derived from the scratch's rebase. The sink is single-threaded
    /// inside one statement.
    last_commit_ts: Cell<Option<u64>>,
    tx_metadata: &'a RefCell<TxChannelState>,
}

impl relate_executor::BatchCommit for GroupBatchCommit<'_> {
    fn commit_batch(&self, ops: Vec<RedoOp>, snapshot_ts: u64) -> Result<u64> {
        let txn_id = self.shared.next_txn_id.fetch_add(1, Ordering::SeqCst);
        // The batch's own assigned commit timestamp, handed back by the
        // commit pipeline — never `graph.commit_ts()` re-read after the
        // publication guard is released: a concurrent writer's commit
        // landing in between would rebase the scratch past a foreign
        // commit, letting later batches observe another transaction's
        // mid-statement writes.
        let commit_ts = try_wal_commit(
            &self.shared,
            snapshot_ts,
            txn_id,
            ops,
            0,
            0,
            std::mem::take(&mut self.tx_metadata.borrow_mut()),
        )?;
        self.last_commit_ts.set(Some(commit_ts));
        Ok(commit_ts)
    }
}

impl<'a> GroupBatchCommit<'a> {
    /// A sink over `shared` and the statement's metadata cell with no
    /// batch committed yet — the one construction, so the tracker always
    /// starts unset.
    pub(super) fn new(shared: Arc<Shared>, tx_metadata: &'a RefCell<TxChannelState>) -> Self {
        Self {
            shared,
            last_commit_ts: Cell::new(None),
            tx_metadata,
        }
    }

    /// The last batch's commit timestamp, `None` when no batch committed
    /// through this sink (ADR-0502 §7).
    pub(super) fn last_commit_ts(&self) -> Option<u64> {
        self.last_commit_ts.get()
    }
}

/// A decided batched bulk write (ADR-0209): the plan's bulk shape plus the
/// snapshotted target id space. `prepare` owns the whole probe —
/// eligibility, target collection, threshold — so `run` is only the
/// commit loop and `commit_write` reads as decide-then-execute.
pub(super) struct BulkBatchPlan {
    bulk: BulkWrite,
    targets: BulkTargets,
}

impl BulkBatchPlan {
    /// Decide whether `logical` runs batched: eligible bulk shape, a
    /// collectable target id space, and enough work to clear the
    /// threshold. `Ok(None)` declines to batch (the caller runs the
    /// single-commit path).
    pub(super) fn prepare(shared: &Arc<Shared>, logical: &LogicalPlan) -> Result<Option<Self>> {
        let Some(bulk) = analyze_bulk_write(logical) else {
            return Ok(None);
        };
        // Cheap gate first: an O(1) label-index estimate that can only
        // overestimate. Without it every small bulk-shaped write pays an
        // O(n) target collection before the threshold check — that is the
        // loadtest-visible regression path.
        let estimate = with_pinned_graph(shared, |graph, _| Ok(bulk.estimate_upper_bound(graph)))?;
        if estimate.is_some_and(|ub| ub < shared.resolved.bulk_batch_threshold) {
            return Ok(None);
        }
        // Phase 1: snapshot the matched entity id space under one pin.
        let targets = with_pinned_graph(shared, |graph, snapshot_ts| {
            bulk.collect_targets(graph, snapshot_ts)
        })?;
        let Some(targets) = targets else {
            return Ok(None);
        };
        if targets.total_work() < shared.resolved.bulk_batch_threshold {
            return Ok(None);
        }
        Ok(Some(Self { bulk, targets }))
    }

    /// Run the decided plan in bounded batches — each batch a fresh
    /// scratch committed through the normal group-commit path — and
    /// return the summed result beside the statement's commit sequence
    /// (ADR-0502 §7): the last committed batch's commit timestamp, or
    /// `None` when no batch's redo reached the WAL. `tx_metadata` is the
    /// statement's per-transaction metadata cell (ADR-0610): it seeds the
    /// batches' shared cell (an in-batch `CALL tx.setMetaData` merges
    /// into it), and the first committed batch's record carries it.
    pub(super) fn run(
        self,
        shared: &Arc<Shared>,
        params: &BTreeMap<String, Value>,
        parse_plan_ns: u64,
        cancel: &CancelToken,
        tx_metadata: TxChannelState,
    ) -> Result<(ExecResult, Option<u64>)> {
        let budget = shared.resolved.bulk_batch_budget;
        let timing = shared.timing_on.load(Ordering::Relaxed);
        // The admission-time ontology document (ADR-0121 §3), one Arc clone
        // shared by every batch (same capture rule as commit_optimistic).
        let ontology_document = shared.ontology.get();
        // The statement's metadata cell (ADR-0610): one cell across every
        // batch, drained by the first committed batch's `try_wal_commit`.
        let tx_metadata = RefCell::new(tx_metadata);
        let mut stats = relate_executor::QueryStats::default();
        // ADR-0402 Decision 2: a batched bulk write is a mutation by
        // construction (that is what made it eligible), and it applied a
        // change iff any batch committed redo (`sequence` below is `Some`).
        let mut cursor = BulkCursor::default();
        // One-shot: the first *committed* batch carries the statement's
        // parse/plan time; later batches take None and record 0.
        let mut parse_plan_ns = Some(parse_plan_ns);
        // The statement's commit sequence (ADR-0502 §7): the last
        // committed batch's timestamp — the position every batch's writes
        // are durable at.
        let mut sequence = None;
        loop {
            if cancel.is_cancelled() {
                return Err(Error::cancelled());
            }
            let txn_id = shared.next_txn_id.fetch_add(1, Ordering::SeqCst);
            let (snapshot_ts, (done, execute_ns), ops, workspace) = run_on_pinned_scratch(
                shared,
                cancel,
                ontology_document.as_deref(),
                &tx_metadata,
                |view| {
                    // Constraint validation is per-commit, which batching
                    // cannot honor: a constraint created after phase 1 fails
                    // the statement (earlier batches stay committed — the
                    // documented partial-commit semantics).
                    if !view.named_constraints().is_empty() {
                        return Err(Error::execution(
                            "a constraint was created during a batched bulk write; \
                             already-committed batches are not rolled back",
                        ));
                    }
                    let t_exec = timing.then(Instant::now);
                    let done = self.bulk.execute_chunk(
                        view,
                        &self.targets,
                        &mut cursor,
                        budget,
                        params,
                    )?;
                    let execute_ns = elapsed_nanos(t_exec);
                    stats.merge(&view.stats());
                    Ok((done, execute_ns))
                },
            )?;
            if !ops.is_empty() {
                sequence = Some(try_wal_commit(
                    shared,
                    snapshot_ts,
                    txn_id,
                    ops,
                    parse_plan_ns.take().unwrap_or(0),
                    execute_ns,
                    std::mem::take(&mut tx_metadata.borrow_mut()),
                )?);
            }
            // Keep the batch's measured write workspace admitted until its
            // redo has either been rejected or committed to the WAL.
            drop(workspace);
            if done {
                break;
            }
        }
        shared.check_healthy()?;
        // The ADR-0209 batched bulk write is its own statement boundary (no
        // clause loop runs): the batch loop pushed its own redo, so a
        // committed sequence is the applied fact and the attempt is
        // structural — both go through the effect type's own constructor,
        // never a literal.
        Ok((
            ExecResult::finished(
                Vec::new(),
                Vec::new(),
                stats,
                StatementEffect::from_applied(sequence.is_some()),
            ),
            sequence,
        ))
    }
}

/// The pinned-snapshot scaffold every optimistic write commit shares:
/// acquire the guard and pin, build a fresh `WriteScratch`, and run `f`
/// on its write view. Returns the scratch's final snapshot timestamp
/// (identical to the pin's for non-rebasing callers), `f`'s output, the
/// scratch's redo, and its workspace reservation. `tx_metadata` is the
/// statement's per-transaction metadata cell (ADR-0610), attached to the
/// view like `run_on_scratch` does.
fn run_on_pinned_scratch<T>(
    shared: &Arc<Shared>,
    cancel: &CancelToken,
    ontology_document: Option<&str>,
    tx_metadata: &RefCell<TxChannelState>,
    f: impl FnOnce(&ExecGraph<'_>) -> Result<T>,
) -> Result<(u64, T, Vec<RedoOp>, relate_core::tokens::WorkspaceCharge)> {
    let (_read, pin) = acquire_guarded_pin(shared)?;
    let out = run_on_scratch(shared, pin.ts(), cancel, ontology_document, tx_metadata, f)?;
    drop(pin);
    Ok(out)
}
