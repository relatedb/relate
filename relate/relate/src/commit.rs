//! Optimistic write commits: private-overlay execution and the group-commit
//! WAL append (independent waiters share one `sync_all`, ADR-0019). The
//! group machinery — the admission queue, its slots and waiters, the
//! leader's per-slot admission and flush — lives in [`group`]; timing
//! histograms live in [`commit_timing`].

use std::cell::RefCell;
use std::sync::atomic::Ordering;
use std::sync::{Arc, MutexGuard};
use web_time::Instant;

use relate_core::tokens::WorkspaceCharge;
use relate_core::PreviewFeature;
use relate_engine_api::{TransactionRecord, TxChannelState};
use relate_executor::{
    result_rows_bytes, ExecGraph, ExecRequest, Graph, QueryResult as ExecResult, RedoOp,
    SnapshotPin, WriteScratch,
};
use relate_storage::{EncodedOps, Wal, WalRecord};

use crate::checkpoint::{begin_publication, lock_wal_checked, Publication};
use crate::{CancelToken, DurabilityMode, Result, Shared};
use record_provenance::ProvenanceFields;
// The injected-failure hooks (and through them the tests submodule's
// glob of this module) spell `Error` bare; no production path here does
// since the group machinery moved to `group`.
#[cfg(test)]
use crate::Error;

mod admission;
mod bulk_batch;
mod group;
mod record_provenance;

use bulk_batch::{BulkBatchPlan, GroupBatchCommit};
pub(crate) use group::GroupCommit;
use group::{flush_group, CommitSlot, CommitWaiter, Waiters};

use crate::commit_timing::TimingSample;
pub(crate) use crate::commit_timing::{
    commit_timing_env, elapsed_nanos, format_commit_timing, CommitTimingBuf,
};

/// A record accepted into a commit batch: the redo to append/apply, the
/// commit timestamp assigned at admission, and the timing fields recorded by
/// the group path. `apply_redo` consumes `ops`; the rest stay readable on the
/// error path. The ops' encoding travels beside it in [`Admitted`], so the
/// append takes it by move and this record never holds a moved-out slot.
pub(crate) struct Accepted {
    ops: Vec<RedoOp>,
    commit_ts: u64,
    /// The LSN the append assigned, stamped between the append and the
    /// apply loop (0 until then) — the retained record's `lsn` (ADR-0610).
    lsn: u64,
    /// Recorded by the fused preflight/append loop in [`commit_preflighted`].
    encode_write_ns: u64,
    wal_lock_ns: u64,
    parse_plan_ns: u64,
    execute_ns: u64,
    /// The transaction's provenance half (ADR-0610) — the one declared
    /// shape ([`ProvenanceFields`]) threaded from admission to the
    /// retained-table insert after the apply: the metadata map, the
    /// derived changed keys, and the record's family.
    provenance: ProvenanceFields,
}

/// An [`Accepted`] record with the one encoding its append consumes.
pub(crate) struct Admitted {
    record: Accepted,
    encoded: EncodedOps,
}

impl Admitted {
    /// A single replicated record: no group-commit timing fields. The
    /// provenance fields (ADR-0610) are the caller's — a replica keeps
    /// its source's provenance while it has the opt-in, and serializes
    /// none without it.
    pub(crate) fn replicated(
        ops: Vec<RedoOp>,
        encoded: EncodedOps,
        commit_ts: u64,
        provenance: ProvenanceFields,
    ) -> Self {
        Self {
            record: Accepted {
                ops,
                commit_ts,
                lsn: 0,
                encode_write_ns: 0,
                wal_lock_ns: 0,
                parse_plan_ns: 0,
                execute_ns: 0,
                provenance,
            },
            encoded,
        }
    }
}

/// Timing measured by one [`commit_preflighted`] group when `timing_on` is
/// set: one sample per accepted record plus whether the group ran the Full
/// durability barrier (its single `sync_all`, ADR-0019). [`flush_group`]
/// records both under one timing lock; the replicated path drops it.
pub(crate) struct GroupTiming {
    samples: Vec<TimingSample>,
    synced: bool,
}

/// The ADR-0310 bookmark string form of a commit timestamp — the one
/// spelling of the engine's bookmark (`relate:<n>`, ADR-0502 §7),
/// consulted by both stamping sites: the auto-commit result's
/// ([`crate::QueryResult`]'s `bookmark`) and `Connection::commit`'s return.
pub(crate) fn bookmark_string(commit_ts: u64) -> String {
    format!("{BOOKMARK_PREFIX}{commit_ts}")
}

/// The bookmark string's one prefix, shared by the encoder and decoder.
const BOOKMARK_PREFIX: &str = "relate:";

/// The commit sequence `n` of an ADR-0310 bookmark string — the decoder
/// beside [`bookmark_string`]'s one encoder; `None` for anything that is not
/// exactly `relate:<n>`. Tests compare the parsed sequences, never the
/// strings (`relate:10` sorts before `relate:9`), and the crate's
/// provenance examples reach it through the crate root's doc-hidden
/// re-export — the one decoder, never re-inlined beside a bookmark read.
pub fn bookmark_sequence(bookmark: &str) -> Option<u64> {
    bookmark.strip_prefix(BOOKMARK_PREFIX)?.parse().ok()
}

/// Auto-commit write entry point. ADR-0209: eligible global bulk writes
/// whose estimated touched-entity count reaches the threshold run in
/// bounded batches, one WAL commit per batch; everything else takes the
/// single commit path. The statement's execution facts ride the
/// `ExecRequest` (plan, params, query text, principal, PROFILE sink) —
/// the same bundle `commit_optimistic` executes. `tx_metadata` is the
/// connection's per-transaction metadata cell (ADR-0610), taken by the
/// dispatch arm: this implicit transaction's whole life is the statement,
/// so its record carries the cell's map and record type (and a failed or
/// write-less statement drops both with the transaction).
///
/// Returns the executor result beside the statement's commit sequence
/// (ADR-0502 §7): the commit timestamp the WAL boundary assigned the
/// statement's own commit — the last batch's, for the batched paths — or
/// `None` when the statement committed nothing.
pub(crate) fn commit_write(
    shared: &Arc<Shared>,
    request: &ExecRequest<'_>,
    parse_plan_ns: u64,
    cancel: &CancelToken,
    tx_metadata: TxChannelState,
) -> Result<(ExecResult, Option<u64>)> {
    match BulkBatchPlan::prepare(shared, request.plan)? {
        // ADR-0209 bulk batches run their own clause loop (the batched
        // commit pipeline): neither of the request's per-statement facts
        // applies — the bulk loop records no PROFILE metrics, and its
        // redo ops carry no principal (they are already validated
        // statement output).
        Some(plan) => plan.run(shared, request.params, parse_plan_ns, cancel, tx_metadata),
        None => commit_optimistic(shared, request, parse_plan_ns, cancel, tx_metadata),
    }
}

/// The guard-and-pin acquisition every optimistic write commit starts
/// with: under the publication read guard, check health and pin the
/// graph. The GraphCell swap protocol requires the pin to be taken while
/// the publication read guard is held, in ALL paths: the reload-swap path
/// (follow.rs) checks `snapshot_pinned()` and replaces the graph under
/// the WRITE lock, so a guardless `pin_now` could interleave between that
/// check and the replace and leave the statement's pin/reference on a
/// graph being overwritten in place through the UnsafeCell. The caller
/// chooses the guard's lifetime — drop it before execution only when
/// batch commits take the publication WRITE lock from inside `execute`
/// (ADR-0210); the pin outlives the guard either way (the read-path
/// pattern, ADR-0137).
fn acquire_guarded_pin(
    shared: &Arc<Shared>,
) -> Result<(crate::shared::ReadGuard<'_>, SnapshotPin<'_>)> {
    let read_guard = shared.read_guard()?;
    let pin = shared.graph.pin_now()?;
    Ok((read_guard, pin))
}

/// The guard-agnostic execute-on-fresh-scratch step: build a fresh
/// `WriteScratch` at `snapshot_ts` over the pinned graph and run `f` on
/// its write view. Returns the scratch's final snapshot timestamp — the
/// pin's own unless `f` rebased the scratch (ADR-0210 batch commits) —
/// then `f`'s output, the scratch's redo, and the reservation that keeps
/// that write workspace admitted through its WAL boundary. The caller holds
/// the guard/pin its lock protocol requires; this step touches neither.
/// `tx_metadata` is the statement's per-transaction metadata cell
/// (ADR-0610) — attached to the view so an in-statement
/// `CALL tx.setMetaData` merges into it, and drained by the commit that
/// follows on this thread.
fn run_on_scratch<T>(
    shared: &Arc<Shared>,
    snapshot_ts: u64,
    cancel: &CancelToken,
    ontology_document: Option<&str>,
    tx_metadata: &RefCell<TxChannelState>,
    f: impl FnOnce(&ExecGraph<'_>) -> Result<T>,
) -> Result<(u64, T, Vec<RedoOp>, WorkspaceCharge)> {
    let scratch = RefCell::new(WriteScratch::new(
        snapshot_ts,
        Arc::clone(&shared.next_node_id),
        Arc::clone(&shared.next_rel_id),
    ));
    let out = {
        let view = ExecGraph::write(&shared.graph, &scratch)
            .with_cancel(Some(cancel))
            .with_ontology_document(ontology_document)
            .with_tx_metadata(Some(tx_metadata));
        f(&view)?
    };
    let mut scratch = scratch.into_inner();
    let final_ts = scratch.snapshot_ts();
    let (ops, workspace) = scratch.take_redo_with_workspace()?;
    Ok((final_ts, out, ops, workspace))
}

/// The guard-and-pin ceremony every optimistic write commit starts with:
/// acquire the guard and pin, then run `f` on the pinned graph at the
/// pin's snapshot timestamp.
fn with_pinned_graph<T>(
    shared: &Arc<Shared>,
    f: impl FnOnce(&Graph, u64) -> Result<T>,
) -> Result<T> {
    let (_read, pin) = acquire_guarded_pin(shared)?;
    let out = f(&shared.graph, pin.ts());
    drop(pin);
    out
}

/// Returns the raw executor result beside the statement's commit sequence
/// (ADR-0502 §7); the caller (`Connection::dispatch`) stamps the resolved
/// namespace, like it does for every sibling path. The sequence is the
/// commit timestamp the statement's own WAL commit was assigned — or, when
/// the statement's batches committed mid-execution and no final redo
/// followed (`CALL { … } IN TRANSACTIONS`, ADR-0210), the last batch's —
/// `None` when the statement committed nothing.
fn commit_optimistic(
    shared: &Arc<Shared>,
    request: &ExecRequest<'_>,
    parse_plan_ns: u64,
    cancel: &CancelToken,
    tx_metadata: TxChannelState,
) -> Result<(ExecResult, Option<u64>)> {
    let txn_id = shared.next_txn_id.fetch_add(1, Ordering::SeqCst);
    let timing = shared.timing_on.load(Ordering::Relaxed);
    // The statement's per-transaction metadata cell (ADR-0610): the state
    // the dispatch arm took from the connection, live through execution
    // (an in-statement `CALL tx.setMetaData` merges into it) and drained
    // by this statement's own commit below.
    let tx_metadata = RefCell::new(tx_metadata);
    // ADR-0210: a statement carrying `CALL { … } IN TRANSACTIONS` commits
    // each batch through the group-commit pipeline from inside execution,
    // which needs the publication WRITE lock — so it must not hold the
    // publication read guard across `execute` (same-thread read→write
    // would deadlock). The snapshot pin alone keeps the graph version
    // alive (a pin already outlives the read guard on the read path,
    // ADR-0137), and timestamped version retention keeps mid-execution
    // reads consistent while own batches apply at newer timestamps.
    let batched = relate_planner::plan_contains_in_transactions(request.plan);
    // The base timestamp the statement's own final commit conflict-checks
    // against: the statement's snapshot normally, the last batch's commit
    // after ADR-0210 rebases.
    // Created outside the guard block so its recorded sequence (ADR-0502
    // §7) stays readable after execution: a batched statement whose final
    // redo is empty committed through the sink alone, and its bookmark is
    // the last batch's commit timestamp.
    let sink = batched.then(|| GroupBatchCommit::new(Arc::clone(shared), &tx_metadata));
    let (result, commit_base_ts, ops, execute_ns, write_workspace) = {
        let (read_guard, pin) = acquire_guarded_pin(shared)?;
        // The ontology document the statement reads (ADR-0121 §3), captured
        // with the pin under the publication guard: `db.ontology.get` /
        // `db.ontology.apply` see this admission-time document even if a
        // concurrent store-level ontology commit lands mid-statement. A
        // shared handle — the capture is an `Arc` pointer clone.
        let ontology_document = shared.ontology.get();
        // The two lock protocols share one linear flow; `batched` decides
        // only the guard's lifetime and the batch-commit sink:
        // - batched: drop the guard BEFORE `execute` — batch commits take
        //   the publication WRITE lock from inside `execute`, where a held
        //   read guard would deadlock (the pin outlives the guard, the
        //   read-path pattern, ADR-0137);
        // - unbatched: hold the guard across `execute` (it stays in scope
        //   to the end of the block) — `validate_constraints`'s
        //   live-theater fallback relies on it (overlay/catalog.rs).
        if batched {
            drop(read_guard);
        }
        let (commit_base_ts, (result, execute_ns), ops, workspace) = run_on_scratch(
            shared,
            pin.ts(),
            cancel,
            ontology_document.as_deref(),
            &tx_metadata,
            |view| {
                let t_exec = timing.then(Instant::now);
                // The request's own facts ride through; the batch-commit
                // sink is this path's one addition.
                let mut request = *request;
                request.batch_commit = sink
                    .as_ref()
                    .map(|s| s as &dyn relate_executor::BatchCommit);
                let result = shared.graph.execute(view, &request)?;
                Ok((result, elapsed_nanos(t_exec)))
            },
        )?;
        drop(pin);
        (result, commit_base_ts, ops, execute_ns, workspace)
    };
    // The result remains resident after the write commits, until the
    // surface has consumed it. Reserve it before the WAL boundary: once a
    // WAL record is accepted, this request must report its committed write,
    // never turn into a retryable token-limit error.
    let result_reservation = WorkspaceCharge::retain_with(|| result_rows_bytes(&result.rows))?;
    // The statement's own final commit supersedes any batch's: its
    // timestamp is greater (the sequence is monotonic) and covers the
    // remaining redo. With no final redo (ADR-0502 §7: a batched statement
    // whose redo drained through the sink, or a write-shaped statement that
    // produced none) the bookmark is the last batch's commit timestamp
    // the sink recorded, or `None` when no batch committed.
    let sequence = if ops.is_empty() {
        sink.as_ref().and_then(GroupBatchCommit::last_commit_ts)
    } else {
        Some(try_wal_commit(
            shared,
            commit_base_ts,
            txn_id,
            ops,
            parse_plan_ns,
            execute_ns,
            tx_metadata.into_inner(),
        )?)
    };
    // The one tail: release the write workspace, hand the result
    // reservation to the ledger, then the health check.
    drop(write_workspace);
    result_reservation.transfer_to_ledger();
    shared.check_healthy()?;
    Ok((result, sequence))
}

/// WAL append is the serialize point. Independent waiters share one fsync
/// (ADR-0019). Conflict is returned to the client (ADR-0017). On success
/// returns the commit timestamp this record was assigned at admission —
/// the value the per-batch sink (`GroupBatchCommit`) must rebase onto,
/// not a re-read of the live commit clock. `tx_metadata` is the
/// transaction's metadata cell (ADR-0610), already taken from its cell by
/// the caller: this pipeline owns the record's provenance half — while
/// the `value-provenance` feature is off (or the cell is empty) every
/// tail field is omitted and the record's bytes are exactly the
/// pre-feature form, and while it is on the commit's changed keys are
/// derived here, once, from the redo and the pre-state at the statement's
/// `snapshot_ts`, and the record carries its family type.
pub(super) fn try_wal_commit(
    shared: &Arc<Shared>,
    snapshot_ts: u64,
    txn_id: u64,
    ops: Vec<RedoOp>,
    parse_plan_ns: u64,
    execute_ns: u64,
    tx_metadata: TxChannelState,
) -> Result<u64> {
    shared.check_healthy()?;
    crate::ensure_writable(shared)?;
    // ADR-0610: the record's provenance half, derived once here at
    // admission (the one construction home, with the replicated path's,
    // in `record_provenance`). Every tail field is omitted while the
    // feature is off or when the commit carries nothing: the record's
    // bytes are exactly the pre-feature form.
    let provenance = record_provenance::derive(shared, &ops, snapshot_ts, tx_metadata)?;
    // The ops are JSON-encoded exactly once, here, on the caller's thread
    // and off the serial group path (ADR-0094 as amended): the leader only
    // stamps the record header into this buffer. The WAL payload cap refuses
    // an oversized record at this encode. The workspace is charged as the
    // buffer grows (ADR-0558, refined): each growth step is reserved before
    // it is allocated, so concurrent large writers are refused by the budget
    // instead of allocating past it. The charge ends equal to the buffer the
    // slot retains for the leader and is held until its waiter completes,
    // and all callers retain their own charge while those buffers coexist.
    // The final `Vec<RedoOp>` was reserved before its allocation when the
    // scratch was converted. Consequently a later write is refused at
    // admission instead of evicting a write that has already crossed the
    // WAL boundary.
    let mut wal_workspace = WorkspaceCharge::empty();
    let encoded = EncodedOps::encode_tail(
        &ops,
        provenance.tail(),
        &mut wal_workspace,
        shared.resolved.wal_payload_limit,
    )?;
    let waiter = CommitWaiter::new();
    let slot = CommitSlot {
        snapshot_ts,
        txn_id,
        ops,
        encoded,
        parse_plan_ns,
        execute_ns,
        enqueued: Instant::now(),
        waiter: Arc::clone(&waiter),
        provenance,
    };
    let is_leader = {
        let mut gc = shared.group.lock()?;
        shared.check_healthy()?;
        // Authoritative admission. `ensure_writable` above can pass, then
        // `demote` can set `draining` and observe an empty queue, before this
        // slot is pushed. Re-checking under the group lock means the write
        // either joins the queue the drain waits on or is refused retryably.
        crate::ensure_writable(shared)?;
        gc.queue.push(slot);
        if gc.has_leader {
            false
        } else {
            gc.has_leader = true;
            true
        }
    };
    if is_leader {
        loop {
            let batch = {
                let mut gc = shared.group.lock()?;
                if gc.queue.is_empty() {
                    gc.has_leader = false;
                    break;
                }
                std::mem::take(&mut gc.queue)
            };
            if let Err(e) = flush_group(shared, batch) {
                let rest = {
                    let mut gc = shared.group.lock()?;
                    gc.has_leader = false;
                    std::mem::take(&mut gc.queue)
                };
                for slot in rest {
                    slot.waiter.complete(Err(e.clone()));
                }
                break;
            }
        }
    }
    let ts = waiter.wait()?;
    shared.check_healthy()?;
    Ok(ts)
}

/// Apply one replicated record through the shared commit core — the replica
/// write path's whole choreography, every step delegated to its named
/// canonical helper (`ensure_writable`, `begin_publication`,
/// `admit_redo_capacity`, `lock_wal_checked`) beside the group-commit
/// leader, so the entry is self-fencing like `try_wal_commit`.
pub(crate) fn commit_replicated(shared: &Arc<Shared>, record: &WalRecord) -> Result<u64> {
    crate::ensure_writable(shared)?;
    let publication = begin_publication(shared)?;
    // Authoritative admission. This path never joins the group-commit
    // queue, so `demote` cannot see it in `wait_for_drain`. The publication
    // write lock is the same one `demote` holds across releasing the writer
    // lease, so a commit that passed the check above either finishes before
    // that release or is refused here.
    crate::ensure_writable(shared)?;
    // The canonical capacity admission and durability-fenced WAL acquire:
    // this path spells neither policy itself.
    shared.admit_redo_capacity(&record.ops)?;
    // ADR-0610: a replica keeps its source's provenance while the local
    // opt-in is set — the record's own fields pass through, derived at
    // the source against the source's pre-state — and serialize none
    // without it (a replica that never opted in is indistinguishable).
    // The gate read derefs the GraphCell under the publication write
    // guard `begin_publication` above already holds — the same guard the
    // rest of this path's graph derefs run under.
    let enabled = shared
        .graph
        .preview()
        .enabled(PreviewFeature::ValueProvenance);
    let provenance = record_provenance::passthrough(enabled, record);
    // The record's one encoding, before the WAL lock is taken.
    // Charged to this thread's workspace like an admitted commit's, and held
    // until the record is appended.
    let mut wal_workspace = WorkspaceCharge::empty();
    let encoded = EncodedOps::encode_tail(
        &record.ops,
        provenance.tail(),
        &mut wal_workspace,
        shared.resolved.wal_payload_limit,
    )?;
    let wal = lock_wal_checked(shared)?;
    // The replicated path records no group-commit timing: it does not ask
    // the core to measure (`measure: false`), so it pays for no samples it
    // would drop.
    let committed = commit_preflighted(
        shared,
        wal,
        vec![Admitted::replicated(
            record.ops.clone(),
            encoded,
            record.commit_ts,
            provenance,
        )],
        publication,
        false,
    )?;
    shared.check_healthy()?;
    Ok(committed.last_lsn)
}

/// What one preflighted commit group produced: the timing samples (only
/// when `RELATE_COMMIT_TIMING` is on) and the last LSN the group
/// assigned — the pipeline is the one place that numbers the records, so a
/// caller that reports the LSN (the replicated path) reads it here instead
/// of pre-computing it.
pub(crate) struct Committed {
    pub(crate) timing: Option<GroupTiming>,
    pub(crate) last_lsn: u64,
}

/// Shared commit core for the group-commit leader and replicated apply:
/// preflight, append, the fault-injection hooks, the durability barrier, the
/// ordered `apply_redo`, and `finish_applied_commit`. Runs under the caller's
/// WAL mutex and publication guard; any uncertain physical step poisons the
/// shared state. Expects a non-empty group: `Committed::last_lsn` is the
/// last LSN the group assigned, so an empty group would report the previous
/// record's. Measures per-record timing whenever the caller passes
/// `measure: true` (the group-commit leader forwards `shared.timing_on`; the
/// replicated path passes `false` and pays for no samples it would drop) and
/// returns the samples (each carrying the group's single `sync_all` cost)
/// plus whether the group ran the Full durability barrier, for the caller to
/// record.
pub(crate) fn commit_preflighted(
    shared: &Arc<Shared>,
    mut wal: MutexGuard<'_, Wal>,
    admitted: Vec<Admitted>,
    publication: Publication<'_>,
    measure: bool,
) -> Result<Committed> {
    // Split each admitted record into what the append consumes (its
    // timestamp and encoding) and what apply and timing keep, in one pass
    // over one source.
    let mut records = Vec::with_capacity(admitted.len());
    let mut accepted = Vec::with_capacity(admitted.len());
    for Admitted { record, encoded } in admitted {
        records.push(relate_write_local::AssignedRecord {
            commit_ts: record.commit_ts,
            encoded,
        });
        accepted.push(record);
    }
    shared.graph.validate_redo_group_applicability(
        accepted.iter().map(|a| (a.ops.as_slice(), a.commit_ts)),
    )?;

    // ADR-0120 §2/§3: the group's store-level ontology outcome (the last
    // ontology op wins), scanned BEFORE the first append. Nothing is
    // derived from the document (ADR-0229 §3) and it was validated at set
    // time, before the redo was pushed, so this cannot fail; the 16 MiB
    // envelope below is the WAL preflight's own `RedoOp` validation.
    let ontology = crate::ontology::prepare_committed(accepted.iter().map(|a| a.ops.as_slice()));
    // ADR-0649's one set/drop gate: a store whose ontology document
    // comes from the committed schema file accepts no procedure- or
    // API-sourced swap — the file is the document's source. Checked
    // where every ontology op funnels, before any append.
    if ontology.is_some() {
        crate::ontology::refuse_ontology_swap(shared)?;
    }

    // Checkpoint admission was closed and any old worker fully drained before
    // the first frame. Keep the publication lock until every redo is applied.
    // Each record's encoding was made at admission; the append stamps the
    // header into it, and the write cost is recorded directly in this
    // single append loop, avoiding a parallel per-record vector.
    let appended = match relate_write_local::append_assigned(&mut wal, records) {
        Ok(appended) => appended,
        Err(cause) => return Err(shared.poison(&cause)),
    };
    for (a, written) in accepted.iter_mut().zip(appended) {
        debug_assert_eq!(
            a.commit_ts, written.commit_ts,
            "the append reports records in group order"
        );
        a.encode_write_ns = written.encode_write_ns;
        // ADR-0610: the LSN the append assigned — the retained record's
        // own `lsn`, read by the apply loop's table insert below.
        a.lsn = written.lsn;
    }
    #[cfg(test)]
    if shared.fail_after_append.swap(false, Ordering::SeqCst) {
        return Err(shared.poison(&Error::io("injected post-append failure")));
    }

    // Full durability barriers once for the accepted group. Relaxed modes
    // acknowledge after apply: EverySecond's worker barriers the dirty WAL,
    // while Snapshot relies on a completed checkpoint. Rotation below adds no
    // second data-segment barrier: `sync` above already made the group's
    // records durable under the same WAL mutex.
    let synced = shared.resolved.durability_mode == DurabilityMode::Full;
    let sync_ns = if synced {
        match wal.sync() {
            Ok(ns) => ns,
            Err(e) => return Err(shared.poison(&e)),
        }
    } else {
        0
    };
    #[cfg(test)]
    if shared.fail_after_sync.swap(false, Ordering::SeqCst) {
        // Simulate an unexpected graph change after validation: the actual
        // apply_redo call below must fail and take the real poison branch.
        shared.graph.apply_redo(vec![], accepted[0].commit_ts)?;
    }
    let mut samples = measure.then(|| Vec::with_capacity(accepted.len()));
    for a in accepted.iter_mut() {
        let t_apply = measure.then(Instant::now);
        // `apply_redo` consumes the ops: the WAL already encoded them
        // (admission encoded them, above), so apply moves them out instead
        // of cloning the property maps. `mem::take` leaves the entry in place
        // so timing can still read its other fields.
        let ops = std::mem::take(&mut a.ops);
        let commit_ts = a.commit_ts;
        if let Err(e) = shared.graph.apply_redo(ops, commit_ts) {
            return Err(shared.poison(&e));
        }
        // ADR-0610: the durable, applied record joins the retained table —
        // the live pipeline's one insert. The table itself decides which
        // records retain (a record with no type and neither provenance
        // field — the only form a store that never opted in produces —
        // joins nothing), and the fields were consumed by the WAL encode
        // above, so this is their second and last home. The tail-less
        // form's family is the provenance default
        // (`RecordType::default`).
        let provenance = std::mem::take(&mut a.provenance);
        shared.graph.record_transaction(TransactionRecord {
            lsn: a.lsn,
            commit_ts,
            metadata: provenance.metadata,
            changed_keys: provenance.changed_keys,
            record_type: provenance.record_type.unwrap_or_default(),
        });
        let apply_ns = elapsed_nanos(t_apply);
        if let Some(samples) = samples.as_mut() {
            samples.push(TimingSample {
                parse_plan_ns: a.parse_plan_ns,
                execute_ns: a.execute_ns,
                wal_lock_ns: a.wal_lock_ns,
                encode_write_ns: a.encode_write_ns,
                sync_ns,
                apply_ns,
            });
        }
    }
    // The ontology commit is durable and applied: update the store's
    // document state and the segment-magic flag now, under the still-held
    // WAL mutex and publication boundary, so no checkpoint can publish or
    // recycle past this commit without observing it (ADR-0120 §2/§4).
    if let Some(committed) = ontology {
        crate::ontology::install_committed(shared, &mut wal, committed);
    }
    // ADR-0503: the applied group is a new data state — every cached plan
    // is stale now. `finish_applied_commit`'s data-state publish (still
    // under this publication write boundary, so no admission can hold a
    // read guard across it) advances the stats generation with the
    // horizon; a read that planned before this group pinned its own
    // snapshot, exactly as before.
    // Rotation does not add another data-segment commit barrier: `sync` above
    // already made the group's records durable under the same WAL mutex.
    // `finish_applied_commit` releases the checkpoint publication boundary and
    // schedules a mode-appropriate rotation/checkpoint. Waiters are completed
    // by the caller's RAII `Waiters`.
    let last_lsn = wal.lsn;
    shared.finish_applied_commit(wal, publication)?;
    Ok(Committed {
        timing: samples.map(|samples| GroupTiming { samples, synced }),
        last_lsn,
    })
}

#[cfg(test)]
mod tests;
