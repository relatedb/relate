//! The explicit-transaction cluster (ADR-0028): the open transaction's
//! pinned snapshot and write overlay, `BEGIN`/`COMMIT`/`ROLLBACK`, the
//! in-transaction dispatch, and the failure epilogue `run_query` shares.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use relate_core::{Error, Result, TransactionClock};
use relate_executor::{
    ExecRequest, QueryResult as ExecResult, RedoOp, SchemaSnapshot, WriteScratch,
};
use relate_planner::LogicalPlan;
use relate_storage::PayloadTally;

use crate::commit::try_wal_commit;
use crate::{CancelToken, Connection, Value};

/// A connection-scoped explicit transaction (ADR-0028).
pub(crate) struct OpenTxn {
    txn_id: u64,
    /// The transaction's write overlay; `pull_local_alive` and `dispatch`
    /// in the parent module read it too.
    pub(super) scratch: RefCell<WriteScratch>,
    /// The failed flag `run_query` checks before admitting a statement.
    pub(super) failed: Cell<bool>,
    /// The snapshot pin `Drop` releases after joining the worker.
    pub(super) pin_slot: usize,
    /// The store's ontology document captured at `BEGIN` (ADR-0121 §3):
    /// the document is store-level state, not graph-snapshot state, so the
    /// transaction's consistent read of it is pinned here — `db.ontology.get`
    /// and `db.ontology.apply` in this transaction see exactly this. A
    /// shared handle (pointer clone), never a document-sized copy.
    ontology_document: Option<Arc<str>>,
    /// The running tally of the transaction's prospective commit record
    /// (LABS-858). An explicit transaction commits as one WAL record, so
    /// its size is bounded by the store's resolved payload cap (ADR-0650,
    /// `OpenResolved::wal_payload_limit`) from its first statement on
    /// — the tally is what lets a statement that crosses the cap be
    /// refused there, instead of leaving every later statement to run and
    /// the commit to fail. It also owns the redo-log position it has
    /// counted to (its op count), so no second cursor field exists to
    /// drift out of step with it.
    cap_tally: RefCell<PayloadTally>,
    /// The instant the transaction began at, captured once at `BEGIN`: every
    /// statement's clock pin carries it, so `<kind>.transaction()` is one
    /// instant for the whole transaction.
    clock: TransactionClock,
}

impl OpenTxn {
    /// Refuse the statement that takes the transaction's prospective commit
    /// record over `payload_cap` (the store's resolved cap, ADR-0650), with
    /// the same refusal the commit would have produced (ADR-0494). Each op
    /// is encoded once, as
    /// the statement that appended it returns, so a statement that stays
    /// under the cap never re-encodes what it already counted. The tally
    /// owns the resume position (its own op count), so there is no second
    /// cursor beside it to keep in step.
    fn check_commit_cap(&self, payload_cap: u64) -> Result<()> {
        let scratch = self.scratch.borrow();
        let cursor = self.cap_tally.borrow().ops_count();
        // The log only grows while a transaction is open: the one path that
        // rolls a batch back, `CALL { … } IN TRANSACTIONS`, is refused
        // inside an explicit transaction. A position past the end is an
        // invariant breach — refused loudly, never read as an empty delta
        // (which would silently undercount the record the commit will
        // encode).
        let delta = scratch.redo_since(cursor).ok_or_else(|| {
            Error::storage(format!(
                "redo log shrank under the commit-cap cursor (position {cursor})"
            ))
        })?;
        let mut tally = self.cap_tally.borrow_mut();
        tally.add(&delta)?;
        let bytes = tally.bytes();
        if bytes > payload_cap {
            return Err(Error::capacity_wal_payload(bytes, payload_cap));
        }
        Ok(())
    }

    /// The failed-transaction gate: the one spelling of the refusal
    /// every statement path raises before touching a failed transaction.
    pub(super) fn refuse_if_failed(&self) -> Result<()> {
        if self.failed.get() {
            Err(Error::semantic("transaction failed; rollback required"))
        } else {
            Ok(())
        }
    }

    /// Mark the transaction failed: the one failure epilogue shared by
    /// [`Connection::fail_txn`] and the in-transaction statement path.
    pub(super) fn fail(&self) {
        self.failed.set(true);
    }
}

/// The drained redo of a prepared commit — the `Some` payload of
/// [`Connection::prepare_commit`]'s `Option`: `None` is the no-op commit
/// (nothing to flush, nothing that needs a wait shape); `Some` carries
/// the work whose flush may be large enough to need the worker wait
/// (ADR-0504). The routing keys on the option once — the standard
/// vocabulary, no bespoke variant to re-match at the flush boundary, so
/// flushing nothing is not expressible.
pub(super) struct PreparedRedo {
    snapshot_ts: u64,
    txn_id: u64,
    ops: Vec<RedoOp>,
}

/// The open transaction plus the dispatch decisions already made from it,
/// bundled so `Connection::query_in_txn` stays under
/// `clippy::too_many_arguments` (same trick as `ResultStamp` in `dispatch`):
/// the `begin`-pinned `txn` (the open-transaction invariant, threaded by
/// value instead of re-fetched), the target graph `dispatch_target` derived
/// from it, and the schema snapshot taken under the read guard.
pub(super) struct TxnRun<'a> {
    pub(super) txn: &'a OpenTxn,
    pub(super) target: &'a crate::engines::LiveQuery,
    pub(super) schema: &'a SchemaSnapshot,

    /// The statement's PROFILE sink (ADR-0344 phase 2) — the same fact
    /// `DispatchCtx` holds for the dispatch epilogue, already coerced to
    /// the sink trait in `run_query` (the one coercion site).
    pub(super) profile_metrics: Option<&'a dyn relate_executor::PlanMetricsSink>,
}

impl Connection {
    /// Start an explicit transaction (ADR-0028). Nested `begin` is an error.
    pub fn begin(&mut self) -> Result<()> {
        let _read = self.inner.read_guard()?;
        if self.txn.is_some() {
            return Err(Error::semantic("already in a transaction"));
        }
        let pin = self.inner.graph.pin_now()?;
        let snapshot_ts = pin.ts();
        let pin_slot = pin.detach();
        let txn_id = self.inner.next_txn_id.fetch_add(1, Ordering::SeqCst);
        self.txn = Some(OpenTxn {
            txn_id,
            scratch: RefCell::new(WriteScratch::new(
                snapshot_ts,
                Arc::clone(&self.inner.next_node_id),
                Arc::clone(&self.inner.next_rel_id),
            )),
            failed: Cell::new(false),
            pin_slot,
            ontology_document: self.inner.ontology.get(),
            cap_tally: RefCell::new(PayloadTally::default()),
            clock: TransactionClock::begin(),
        });
        Ok(())
    }

    /// Publish the transaction's redo as one WAL commit and return its
    /// commit bookmark (ADR-0502 §7): the ADR-0310 string form
    /// `relate:<n>` of the commit timestamp the WAL boundary assigned,
    /// `n` being the store's commit sequence — monotonically increasing
    /// across every connection and surface, so a later commit carries a
    /// greater `n` (compare the parsed `n`, not the strings — `relate:10`
    /// sorts before `relate:9` lexicographically). A transaction that
    /// committed nothing (empty redo — no write ran, or every write was a
    /// no-op) returns `Ok(None)`: no bookmark is fabricated for a commit
    /// that crossed no WAL boundary. The prepared half
    /// ([`Self::prepare_commit`]) then the flush ([`Self::flush_commit`]).
    ///
    /// ```
    /// use relate::Database;
    ///
    /// let db = Database::open(":memory:")?;
    /// let mut conn = db.connect()?;
    ///
    /// // A write transaction commits: its bookmark is the commit position.
    /// conn.begin()?;
    /// conn.query("CREATE (:Kept)")?;
    /// let bookmark = conn.commit()?.expect("the transaction wrote");
    /// assert!(bookmark.starts_with("relate:"));
    ///
    /// // An empty transaction commits nothing: no bookmark.
    /// conn.begin()?;
    /// assert!(conn.commit()?.is_none());
    /// # Ok::<(), relate::Error>(())
    /// ```
    pub fn commit(&mut self) -> Result<Option<String>> {
        match self.prepare_commit()? {
            Some(redo) => self.flush_commit(redo).map(Some),
            None => Ok(None),
        }
    }

    /// The commit's precondition-and-drain half: the health gate, the
    /// open and unfailed transaction (a failed one is restored for the
    /// rollback, ADR-0028), the redo drained and the snapshot pin
    /// released. Split from the flush so the heartbeat form
    /// ([`Self::commit_with_heartbeat`], ADR-0504) routes on the commit's
    /// own prepared work — `None` for the no-op drain, the drained redo
    /// otherwise — instead of a second derivation of the fact, with the
    /// preconditions running before any worker is spawned for a doomed
    /// flush.
    pub(super) fn prepare_commit(&mut self) -> Result<Option<PreparedRedo>> {
        self.inner.check_healthy()?;
        self.txn
            .as_ref()
            .ok_or_else(|| Error::semantic("no transaction"))?
            .refuse_if_failed()?;
        let txn = self.txn.take().expect("checked present above");
        let snapshot_ts = txn.scratch.borrow().snapshot_ts();
        let txn_id = txn.txn_id;
        let pin_slot = txn.pin_slot;
        let ops = txn.scratch.into_inner().take_redo();
        self.inner.graph.unpin_slot(pin_slot);
        Ok((!ops.is_empty()).then_some(PreparedRedo {
            snapshot_ts,
            txn_id,
            ops,
        }))
    }

    /// The commit's flush half: the single WAL commit of the drained
    /// redo — `Result<String>`, the bookmark, because a `None` prepare
    /// never reaches here: flushing nothing is not expressible in this
    /// signature, `None` exists only in the prepare/commit contract. `&mut self` for the same reason every
    /// worker-job closure takes it: `Connection` is `!Sync`, so a shared
    /// borrow cannot cross to the worker (`&mut` can — the type is
    /// `Send`).
    pub(super) fn flush_commit(&mut self, redo: PreparedRedo) -> Result<String> {
        // The assigned commit timestamp is only needed by the per-batch
        // sink; a whole-transaction commit rebases nothing. The two
        // zeros are the per-phase PROFILE timings (`parse_plan_ns`,
        // `execute_ns`) a whole-transaction commit never attributes —
        // named at the batch-commit call site in `commit.rs`. ADR-0502
        // §7: the timestamp is the commit's bookmark, threaded out
        // instead of dropped.
        let commit_ts = try_wal_commit(&self.inner, redo.snapshot_ts, redo.txn_id, redo.ops, 0, 0)?;
        Ok(crate::commit::bookmark_string(commit_ts))
    }

    /// Discard uncommitted writes. Connection drop is implicit rollback.
    pub fn rollback(&mut self) -> Result<()> {
        let Some(txn) = self.txn.take() else {
            return Err(Error::semantic("no transaction"));
        };
        self.inner.graph.unpin_slot(txn.pin_slot);
        Ok(())
    }

    pub fn in_transaction(&self) -> bool {
        self.txn.is_some()
    }

    pub(super) fn fail_txn(&mut self) {
        if let Some(txn) = self.txn.as_mut() {
            txn.fail();
        }
    }

    pub(super) fn query_in_txn(
        &self,
        run: TxnRun<'_>,
        logical: &LogicalPlan,
        params: &BTreeMap<String, Value>,
        cypher: &str,
        token: &CancelToken,
    ) -> Result<ExecResult> {
        let txn = run.txn;
        txn.refuse_if_failed()?;
        let result = {
            // The transaction's write overlay applies to the writable
            // namespace (`neo4j`, the only one; ADR-0116 §5); a
            // read-only namespace (`system`) keeps a plain read view,
            // ADR-0229 §1. `Connection::statement_view` owns that choice
            // for this path and the notification derivation alike.
            // ADR-0121 §3: the begin-time ontology-document capture is
            // pinned on the view — the transaction reads it consistently
            // with its pinned snapshots.
            let view = Self::statement_view(run.target, Some(&txn.scratch))
                .with_schema(run.schema)
                .with_cancel(Some(token))
                .with_ontology_document(txn.ontology_document.as_deref())
                .with_transaction_clock(txn.clock);
            run.target.execute(
                &view,
                &ExecRequest {
                    plan: logical,
                    params,
                    query_text: cypher,
                    principal: self.principal.as_deref(),
                    batch_commit: None,
                    // ADR-0344 phase 2: the PROFILE sink, threaded by
                    // the caller (the dispatch's ctx, already coerced).
                    plan_metrics: run.profile_metrics,
                },
            )
        };
        // The statement's redo is the transaction's, and the transaction
        // commits as one WAL record: a transaction already over the cap can
        // only fail at COMMIT, after every statement still to come has run.
        // Refuse it here, at the statement that crossed (LABS-858) — the
        // check folded into the statement's own result, so the failure
        // epilogue (fail the transaction, return the error) is spelled once.
        let checked = result.and_then(|r| {
            txn.check_commit_cap(self.inner.resolved.wal_payload_limit)
                .map(|()| r)
        });
        if checked.is_err() {
            txn.fail();
        }
        checked
    }
}
