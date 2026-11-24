//! The transaction cluster: two residents on the same [`OpenTxn`] state — the
//! user's explicit transaction (ADR-0028: pinned snapshot and write overlay,
//! `BEGIN`/`COMMIT`/`ROLLBACK`, the failure epilogue `run_query` shares) and
//! the Bolt session's pipelined write batch (ADR-0642, [`TxnMode::Batch`]:
//! consecutive eligible CREATE-only auto-commit statements sharing one WAL
//! commit at the framed batch's end).

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use relate_core::{Error, Result, TransactionClock};
use relate_executor::{
    ExecRequest, QueryResult as ExecResult, RedoOp, SchemaSnapshot, StatementMark, WriteScratch,
};
use relate_planner::LogicalPlan;
use relate_storage::PayloadTally;

use crate::commit::try_wal_commit;
use crate::gate::MixClass;
use crate::{CancelToken, Connection, Value};

/// Why an open transaction exists (ADR-0642). The explicit `begin()`
/// transaction is the user's: it commits at COMMIT and fails for good on a
/// statement error. The pipelined write batch is the Bolt session's: it
/// absorbs consecutive eligible CREATE-only auto-commit statements of one
/// framed message batch, commits once at that batch's end, and gives up only
/// the failing statement's own redo — so `in_transaction` stays false and a
/// statement error rolls the statement back instead of failing the batch.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TxnMode {
    Explicit,
    Batch,
}

impl TxnMode {
    /// Whether this mode is the pipelined write batch (ADR-0642) — the one
    /// batch-vs-explicit predicate `write_batch_open`, `in_transaction` and
    /// the batch statement-failure policy read, so no site compares the mode
    /// to a variant directly.
    fn is_batch(self) -> bool {
        matches!(self, TxnMode::Batch)
    }

    /// Whether a transaction in this mode is the one `mode`'s commit
    /// consumes: `commit` never consumes the pipelined write batch, and the
    /// batch's commit never consumes the user's transaction (ADR-0642) —
    /// the one owner check, so no site compares the modes directly.
    fn commits_as(self, mode: TxnMode) -> bool {
        self == mode
    }
}

/// A connection-scoped explicit transaction (ADR-0028), or the pipelined
/// write batch ([`TxnMode::Batch`], ADR-0642) that runs on the same state.
pub(crate) struct OpenTxn {
    mode: TxnMode,
    txn_id: u64,
    /// The transaction's write overlay; `pull_local_alive` and `dispatch`
    /// in the parent module read it too.
    pub(super) scratch: RefCell<WriteScratch>,
    /// The transaction's terminal failure — the one carrier of "this
    /// transaction is uncommittable", the fact [`OpenTxn::refuse_if_failed`]
    /// gates every statement on. The explicit mode stores its authored
    /// refusal ("transaction failed; rollback required", via `fail`);
    /// a batch stores its unrecoverable rollback cause (ADR-0642 §3, via
    /// `abort_statement`'s batch arm) — a batch has no ROLLBACK, so its
    /// refusal names the cause, never the explicit mode's rollback demand.
    /// `None` while the transaction is still committable.
    failure: RefCell<Option<Error>>,
    /// The snapshot pin `Drop` releases after joining the worker.
    pub(super) pin_slot: usize,
    /// The store's ontology document captured at `BEGIN` (ADR-0121 §3):
    /// the document is store-level state, not graph-snapshot state, so the
    /// transaction's consistent read of it is pinned here — the committed
    /// fallback `db.ontology.get` / `db.ontology.apply` in this
    /// transaction read through the overlay's pending-op override
    /// (ADR-0667). A shared handle (pointer clone), never a
    /// document-sized copy.
    ontology_document: Option<Arc<str>>,
    /// The explicit transaction's statement-history mixing class
    /// (ADR-0667): the class of the successful statements it has run —
    /// a read or a schema command is [`MixClass::None`] and records
    /// nothing. One cell, not a flag per class: the mixing gate at
    /// admission (`gates_classify_under_guard`) refuses a statement of
    /// the other class, so the transaction's statements all carry one
    /// class and the recorded value never changes once set. The gate
    /// reads it; `settle_statement` records the successful statement's
    /// class. The history is the explicit mode's business alone
    /// (ADR-0667's rule is the explicit-transaction one): a pipelined
    /// write batch records nothing — [`Self::record_statement_class`]
    /// returns without touching the cell — so a batch's statements
    /// never consult, and never trip, a mixing history.
    mix_class: Cell<MixClass>,
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
    /// The in-flight statement's pre-state (ADR-0642), one value with three
    /// states: set before the statement runs and taken by the one
    /// statement-failure epilogue ([`OpenTxn::abort_statement`]). The
    /// batch-vs-explicit decision is taken once, when the statement begins —
    /// the epilogues match the value, they do not re-read the mode. A
    /// [`StatementMark`], not a [`ScratchCheckpoint`]: the batch admits only
    /// CREATE-only statements, so the mark's O(statement) image undoes exactly
    /// as much as the full batch clone would.
    in_flight: RefCell<InFlight>,
}

impl OpenTxn {
    /// The mixing class a successful statement of this transaction
    /// performed (ADR-0667) — the history half of the mixing gate.
    pub(crate) fn mix_class_performed(&self) -> MixClass {
        self.mix_class.get()
    }

    /// Record a successful statement's transaction-mixing class into the
    /// history (ADR-0667): the class comes from the admission's
    /// `ResultStamp`, and a failed statement never reaches here (the
    /// transaction is failed and admits nothing further). `None` records
    /// nothing — a read or schema command leaves the history alone — and
    /// the mixing gate has already refused the other class, so the
    /// recorded value never changes once set.
    /// ADR-0667's mixing history is the explicit transaction's rule: a
    /// pipelined write batch shares this `OpenTxn` but keeps no history —
    /// the cell stays [`MixClass::None`] for its whole life, so the
    /// admission gate's mixing arm never refuses a batch statement.
    pub(crate) fn record_statement_class(&self, class: MixClass) {
        if self.mode.is_batch() {
            return;
        }
        if !matches!(class, MixClass::None) {
            self.mix_class.set(class);
        }
    }
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
    /// The stored refusal names the mode's own failure — the explicit
    /// mode's rollback demand, or the batch's rollback cause.
    pub(super) fn refuse_if_failed(&self) -> Result<()> {
        if let Some(error) = self.failure.borrow().as_ref() {
            return Err(error.clone());
        }
        Ok(())
    }

    /// Mark the transaction failed: the one failure epilogue shared by
    /// [`Connection::fail_txn`] and the in-transaction statement path.
    /// Stores the explicit mode's authored refusal.
    pub(super) fn fail(&self) {
        *self.failure.borrow_mut() = Some(Error::semantic("transaction failed; rollback required"));
    }

    /// ADR-0642: the statement's pre-state is captured before it runs, so any
    /// failure — including a worker panic that never returns to
    /// `query_in_txn`'s own settle — can restore it. The one place the
    /// batch-vs-explicit decision is read: the epilogues match the value.
    fn begin_statement(&self) {
        *self.in_flight.borrow_mut() = if self.mode.is_batch() {
            InFlight::Batch(Box::new(BatchPreState {
                mark: self.scratch.borrow().mark_statement(),
                tally: self.cap_tally.borrow().clone(),
            }))
        } else {
            InFlight::Explicit
        };
    }

    /// The one statement-failure epilogue (ADR-0642, ADR-0028), matching the
    /// value [`Self::begin_statement`] left: a batched statement's partial
    /// redo must not stay in the batch (auto-commit would have discarded it),
    /// so its captured scratch is restored and the cap tally rewound to its
    /// captured value — the restored log is exactly as long as the tally's
    /// cursor again, so the next statement counts only its own ops, never the
    /// batch's whole redo — while the batch stays alive. An explicit
    /// transaction fails for good. Between statements it is inert.
    fn abort_statement(&self) {
        match std::mem::replace(&mut *self.in_flight.borrow_mut(), InFlight::Idle) {
            InFlight::Batch(pre) => {
                let BatchPreState { mark, tally } = *pre;
                // A rollback this mark cannot finish fails the batch instead:
                // its commit then refuses and the session closes, rather than
                // the failed statement's writes riding the batch to durability.
                match self.scratch.borrow_mut().rollback_statement(mark) {
                    Ok(()) => *self.cap_tally.borrow_mut() = tally,
                    Err(error) => *self.failure.borrow_mut() = Some(error),
                }
            }
            InFlight::Explicit => self.fail(),
            // A statement that failed before it began — a planning error, a
            // panicking worker — never reached `begin_statement`, so the
            // value says nothing about the mode: an explicit transaction
            // still fails for good, and a batch has nothing to roll back.
            InFlight::Idle => {
                if !self.mode.is_batch() {
                    self.fail();
                }
            }
        }
    }

    /// The statement epilogue: a failure hands over to
    /// [`Self::abort_statement`]; a success commits the statement's work
    /// into the batch (its captured pre-state is simply dropped) and —
    /// for an explicit transaction — joins the mixing history
    /// (ADR-0667). The one place the success outcome is discriminated.
    fn settle_statement(&self, result: Result<ExecResult>, class: MixClass) -> Result<ExecResult> {
        if result.is_err() {
            self.abort_statement();
        } else {
            *self.in_flight.borrow_mut() = InFlight::Idle;
            self.record_statement_class(class);
        }
        result
    }
}

/// One statement's in-flight pre-state (ADR-0642), the value
/// [`OpenTxn::begin_statement`] leaves and its epilogues take. The
/// batch-vs-explicit choice is made once, at construction; the epilogues
/// match the value instead of re-reading [`OpenTxn::mode`].
enum InFlight {
    /// Between statements: nothing to restore.
    Idle,
    /// An explicit transaction's statement: no rollback image, and a failure
    /// fails the transaction for good.
    Explicit,
    /// A batched statement's pre-state, restored on failure. Boxed: the mark
    /// is a cursor plus the statement state, and the two data-free variants
    /// should not pay for it in every `InFlight` value.
    Batch(Box<BatchPreState>),
}

/// What a batched statement's failure restores: the scratch mark and the
/// commit-cap tally as they stood before the statement ran. The tally covers
/// the whole log at that point (the cap check settles after every
/// statement), so restoring both leaves them in step again.
struct BatchPreState {
    mark: StatementMark,
    tally: PayloadTally,
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
    /// The statement's admission-derived mixing class (ADR-0667) —
    /// the fact `settle_statement` records into the explicit
    /// transaction's history after the statement succeeds.
    pub(super) mix_class: MixClass,

    /// The statement's PROFILE sink (ADR-0344 phase 2) — the same fact
    /// `DispatchCtx` holds for the dispatch epilogue, already coerced to
    /// the sink trait in `run_query` (the one coercion site).
    pub(super) profile_metrics: Option<&'a dyn relate_executor::PlanMetricsSink>,
}

impl Connection {
    /// Start an explicit transaction (ADR-0028). Nested `begin` is an error;
    /// an armed pipelined batch (ADR-0642) ends first, so its statements are
    /// committed before the user's transaction starts.
    pub fn begin(&mut self) -> Result<()> {
        self.commit_write_batch()?;
        self.begin_mode(TxnMode::Explicit)
    }

    /// Open the connection's one transaction state in `mode`: the
    /// explicit-transaction `begin` and the pipelined batch's lazy open
    /// (ADR-0642) share every step but the mode.
    fn begin_mode(&mut self, mode: TxnMode) -> Result<()> {
        let _read = self.inner.read_guard()?;
        if self.txn.is_some() {
            return Err(Error::semantic("already in a transaction"));
        }
        let pin = self.inner.graph.pin_now()?;
        let snapshot_ts = pin.ts();
        let pin_slot = pin.detach();
        let txn_id = self.inner.next_txn_id.fetch_add(1, Ordering::SeqCst);
        self.txn = Some(OpenTxn {
            mode,
            txn_id,
            scratch: RefCell::new(WriteScratch::new(
                snapshot_ts,
                Arc::clone(&self.inner.next_node_id),
                Arc::clone(&self.inner.next_rel_id),
            )),
            failure: RefCell::new(None),
            pin_slot,
            ontology_document: self.inner.ontology.get(),
            mix_class: Cell::new(MixClass::None),
            cap_tally: RefCell::new(PayloadTally::default()),
            clock: TransactionClock::begin(),
            in_flight: RefCell::new(InFlight::Idle),
        });
        Ok(())
    }

    /// Arm the pipelined write batch (ADR-0642) for one framed message
    /// batch: the eligible CREATE-only auto-commit statements the caller runs
    /// from here until [`Self::commit_write_batch`] share one WAL commit
    /// instead of committing one per statement. Infallible and inert — a
    /// batch opens only on the first eligible statement, and a surface that
    /// never calls this keeps per-statement commits.
    ///
    /// Arming also drops any unreported failure of the batch before it: an
    /// error belongs to the batch that produced it, so a caller that ignored
    /// it does not meet it again on an unrelated batch.
    pub fn begin_write_batch(&mut self) {
        self.write_batch_armed = true;
        self.batch_commit_error.borrow_mut().take();
    }

    /// End the armed pipelined write batch (ADR-0642): commit its statements'
    /// accumulated redo as one WAL commit and disarm. `Ok(None)` when nothing
    /// joined the batch — no bookmark is fabricated. An explicit transaction
    /// is untouched.
    ///
    /// A failure here is re-reported on the next call when it happened
    /// mid-batch: the statements it covered already answered SUCCESS on the
    /// wire, so the batch's end must not read as clean — the session answers
    /// the re-reported error by closing the connection.
    pub fn commit_write_batch(&mut self) -> Result<Option<String>> {
        self.write_batch_armed = false;
        if let Some(error) = self.batch_commit_error.borrow_mut().take() {
            return Err(error);
        }
        if !self.write_batch_open() {
            return Ok(None);
        }
        let result = self.commit_mode(TxnMode::Batch);
        if let Err(error) = &result {
            *self.batch_commit_error.borrow_mut() = Some(error.clone());
        }
        result
    }

    /// The open transaction's mode, if any — the one read of `OpenTxn::mode`
    /// that [`Self::write_batch_open`] and [`Self::in_transaction`] share.
    fn txn_mode(&self) -> Option<TxnMode> {
        self.txn.as_ref().map(|txn| txn.mode)
    }

    /// Apply the pipelined write batch policy (ADR-0642) to `logical`: an
    /// armed batch opens on the first eligible CREATE-only auto-commit
    /// statement, and any other statement ends it (committing what it holds)
    /// so the statement runs on the committed state it expects. One policy,
    /// one predicate — the same answer decides both edges. It lives here with
    /// the batch's other verbs, because it is transaction lifecycle: it opens
    /// and commits `self.txn`.
    pub(super) fn write_batch_policy(&mut self, logical: &LogicalPlan) -> Result<()> {
        // A statement outside any batch keeps the per-statement path
        // (ADR-0642 §4) without asking whether it would have been eligible.
        if !self.write_batch_armed && !self.write_batch_open() {
            return Ok(());
        }
        if relate_planner::plan_is_create_only(logical) {
            // The whole open precondition — armed, and no transaction of any
            // mode open — is `open_write_batch`'s alone.
            self.open_write_batch()?;
        } else {
            // A leading ineligible statement closes a batch that never
            // opened too: the rest of the framed batch commits per statement
            // (ADR-0642 §1).
            self.commit_write_batch()?;
        }
        Ok(())
    }

    /// Whether the connection's open transaction is the pipelined write batch
    /// (ADR-0642) — the one batch-open predicate the commit, the dispatch
    /// policy and the failure epilogue share.
    pub(super) fn write_batch_open(&self) -> bool {
        self.txn_mode().is_some_and(TxnMode::is_batch)
    }

    /// Open the pipelined batch's transaction (ADR-0642). The statement
    /// dispatch calls it for the first eligible statement of an armed batch;
    /// inside an open transaction of any mode it is a no-op, since that
    /// transaction already commits once — and a batch opens only when one
    /// was armed. The whole open precondition lives here; the caller does
    /// not re-derive either half.
    pub(super) fn open_write_batch(&mut self) -> Result<()> {
        if self.txn.is_some() || !self.write_batch_armed {
            return Ok(());
        }
        self.begin_mode(TxnMode::Batch)
    }

    /// Whether this statement runs as an auto-commit write — the routing
    /// fact's one home: a write route with no transaction open. The
    /// dispatch arm and the row-charge exemption both read this predicate;
    /// neither re-derives the conjunction as control flow.
    pub(super) fn auto_commit_write(&self, write_route: bool) -> bool {
        write_route && self.txn.is_none()
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
        self.commit_mode(TxnMode::Explicit)
    }

    /// The prepare-then-flush tail [`Self::commit`] and
    /// [`Self::commit_write_batch`] share: prepare `mode`'s transaction, then
    /// flush the redo it drained, fabricating no bookmark for a no-op drain.
    /// Each caller states its own precondition and delegates here.
    fn commit_mode(&mut self, mode: TxnMode) -> Result<Option<String>> {
        match self.prepare_commit(mode)? {
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
    pub(super) fn prepare_commit(&mut self, mode: TxnMode) -> Result<Option<PreparedRedo>> {
        self.inner.check_healthy()?;
        // The caller commits its own state only: `commit` never consumes the
        // pipelined write batch, and the batch's commit never consumes the
        // user's transaction (ADR-0642). Both refusals run before the take,
        // so the transaction stays in place for its owner.
        let txn = self
            .txn
            .as_ref()
            .ok_or_else(|| Error::semantic("no transaction"))?;
        if !txn.mode.commits_as(mode) {
            return Err(Error::semantic("no transaction"));
        }
        txn.refuse_if_failed()?;
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
    ///
    /// A pipelined write batch is not the user's transaction (ADR-0642): its
    /// statements already answered SUCCESS on the wire, so it commits first —
    /// the same order [`Self::begin`] uses — and the rollback finds no
    /// transaction to discard.
    pub fn rollback(&mut self) -> Result<()> {
        self.commit_write_batch()?;
        let Some(txn) = self.txn.take() else {
            return Err(Error::semantic("no transaction"));
        };
        self.inner.graph.unpin_slot(txn.pin_slot);
        Ok(())
    }

    /// Whether the user's explicit transaction is open. The pipelined write
    /// batch (ADR-0642) is not one: it is the session's own coalescing, so
    /// `in_transaction` stays false for it and no protocol state (the RUN
    /// `qid`, the transaction breach checks, the `db` selection) sees it.
    pub fn in_transaction(&self) -> bool {
        self.txn_mode().is_some_and(|mode| !mode.is_batch())
    }

    /// The failure epilogue for the statement paths a statement error never
    /// reaches — a panicking or unspawnable worker (ADR-0028): the same
    /// mode-aware statement settlement [`Self::query_in_txn`] applies, so a
    /// batched statement's partial redo is rolled back (its captured
    /// pre-state restored; the batch keeps its earlier statements,
    /// ADR-0642) and an explicit transaction fails for good.
    pub(super) fn fail_txn(&mut self) {
        if let Some(txn) = self.txn.as_mut() {
            txn.abort_statement();
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
        // ADR-0642: a batch captures the statement's pre-state so a failed
        // statement can restore it; `settle_statement` owns the epilogue.
        txn.begin_statement();
        let result = {
            // The transaction's write overlay applies to the writable
            // namespace (`neo4j`, the only one; ADR-0116 §5); a
            // read-only namespace (`system`) keeps a plain read view,
            // ADR-0229 §1. `Connection::statement_view` owns that choice
            // for this path and the notification derivation alike.
            // ADR-0121 §3: the begin-time ontology-document capture is
            // pinned on the view as the committed fallback — the
            // ontology readers override it with the overlay's pending
            // op (read-own-writes, ADR-0667), so a `set` or `drop` this
            // transaction pushed is what they serve.
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
        // check folded into the statement's own result, so the one
        // statement-failure epilogue ([`OpenTxn::abort_statement`]: batch
        // restores the statement and keeps the batch, explicit fails for
        // good) settles every outcome.
        let checked = result.and_then(|r| {
            txn.check_commit_cap(self.inner.resolved.wal_payload_limit)
                .map(|()| r)
        });
        txn.settle_statement(checked, run.mix_class)
    }
}

#[cfg(test)]
mod tests {
    /// ADR-0642, ADR-0650: a failed batched statement rewinds the commit-cap
    /// tally to its pre-statement value, so the tally stays in step with the
    /// rolled-back redo log and the next statement measures only its own ops.
    /// A tally reset to zero instead re-encodes the whole batch prefix on
    /// every later statement — the re-measurement the incremental tally
    /// exists to avoid. Asserted on the tally's own position, because the
    /// byte total it produces is the same either way: no behaviour-only test
    /// can see the difference.
    #[test]
    fn a_failed_batched_statement_leaves_the_cap_tally_in_step_with_the_log() {
        let db = crate::Database::open_with(
            ":memory:",
            crate::OpenOptions {
                wal_payload_limit: Some(1 << 20),
                ..crate::OpenOptions::default()
            },
        )
        .expect("the store opens");
        let mut conn = db.connect().expect("the connection opens");
        let big = "x".repeat(400 << 10);
        let create = |conn: &mut crate::Connection, id: i64| {
            conn.query(&format!("CREATE (:N {{id: {id}, p: '{big}'}})"))
        };
        conn.begin_write_batch();
        create(&mut conn, 1).expect("the first statement fits the cap");
        create(&mut conn, 2).expect("the second statement fits the cap");
        let error = create(&mut conn, 3).expect_err("the third crosses the 1 MiB cap");
        match error {
            crate::Error::Capacity {
                used,
                limit,
                kind: crate::CapacityKind::WalPayload,
            } => assert!(
                used > limit,
                "the refusal carries the bytes it counted against the cap: {used} vs {limit}"
            ),
            other => panic!("the third statement must be refused by the WAL payload cap: {other}"),
        }

        let txn = conn.txn.as_ref().expect("the batch stays open");
        let measured = txn.cap_tally.borrow().ops_count();
        let in_log = txn
            .scratch
            .borrow()
            .redo_since(0)
            .map_or(0, |ops| ops.len());
        assert!(in_log > 0, "the two accepted statements are the prefix");
        assert_eq!(
            measured, in_log,
            "the refused statement's ops are rolled back, and the tally must sit at the same position"
        );
    }
}
