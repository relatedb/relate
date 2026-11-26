//! `CALL { … } IN TRANSACTIONS`: batch boundaries, per-batch commit and
//! rollback, and the `ON ERROR` / `REPORT STATUS` rows.
//!
//! The arm's disposition is `retain` (ADR-0660): its declared state is one
//! batch's output, and it answers as the arm's [`RowStream`] — one batch
//! runs, commits and is handed to the consumer before the next starts, so
//! the statement's output is never accumulated (see [`InTransactionsRows`]
//! and [`land_batch`]).
//!
//! The file also owns the arm's PROFILE batch facts (ADR-0681): the
//! counters and per-batch maxima the loop notes into the shared cell the
//! clause's record reads, and the per-batch byte accounting over the
//! opening marks. Both live in their own module beside this one
//! (`in_tx_profile.rs`), as does the body's **row-production strategy**
//! (ADR-0669, `in_tx_strategy.rs`) — the admission and the two producers
//! each batch dispatches to.

use std::collections::BTreeMap;

use relate_core::integer_args::{range_text, TX_BATCH_SIZE, TX_CONCURRENCY};
use relate_core::tokens::WorkspaceCharge;
use relate_core::{Error, ErrorKind, Result, Value};
use relate_ir::Query;

use super::in_tx_profile::BatchProfile;
use super::in_tx_strategy::{batch_pass_admission, pass_batch_rows, per_row_batch_rows};
use super::{null_pad_call_row, scope_seed, SubquerySpec};
use crate::ctx::QueryCtx;
use crate::eval::eval;
use crate::exec::feed::RowStream;
use crate::exec::scope::star_scope_names;
use crate::metrics::{InTxStats, ParkedRecord};
use crate::overlay::ExecGraph;
use crate::row::Row;

pub(crate) fn exec_call_in_transactions<'a>(
    graph: &'a ExecGraph<'a>,
    rows: RowStream<'a>,
    spec: SubquerySpec<'a>,
    tx: &'a relate_ir::InTransactions,
    params: &'a BTreeMap<String, Value>,
    qctx: QueryCtx<'a>,
    record: Option<ParkedRecord<'a>>,
) -> Result<RowStream<'a>> {
    // The `REPORT STATUS AS <name>` column, resolved once: every status
    // value below is written through `BatchOutcome::stamp`, so "the call
    // reports a status" has a single spelling.
    // Text render: the status column feeds the String-keyed stamp.
    let status_name = tx.report_status.as_ref().map(|name| name.to_string());
    let batch_pass = batch_pass_admission(spec.query);
    // The arm's PROFILE batch facts (REL8-2047): one cell shared between
    // the batch loop below (the writer) and the parked record (the
    // reader) — the pair derived together from the record's presence, the
    // one "this arm is profiled" decision, at the one attaching site —
    // and built only when a PROFILE record is attached, so an unprofiled
    // arm never notes a batch.
    let (record, profile) = match record {
        Some(record) => {
            let cell = std::rc::Rc::new(InTxStats::new());
            (
                Some(record.with_intx(std::rc::Rc::clone(&cell))),
                Some(cell),
            )
        }
        None => (None, None),
    };
    Ok(RowStream::emitting_to_completion(
        InTransactionsRows {
            graph,
            params,
            qctx,
            spec,
            tx,
            batch_pass,
            // The input is a pull stream: the arm holds one batch's rows,
            // never the statement's whole input (ADR-0660's retain
            // disposition). An empty input resolves nothing and runs no
            // batch — the batch-size/concurrency expressions have no row
            // to evaluate against (a fabricated empty row would silently
            // read null; ADR-0335).
            input: rows,
            batch_size: None,
            batch_index: 0,
            status_name,
            profile,
            pending: None,
            breaking: false,
            done: false,
        },
        record,
    ))
}

/// The arm's emitted rows (ADR-0660): each pull runs the next
/// `CALL { … } IN TRANSACTIONS` batch — its body, its commit, its
/// `ON ERROR` rows — and hands that one batch's rows to the consumer
/// before the next batch starts, so the statement's output is never
/// accumulated.
struct InTransactionsRows<'a> {
    graph: &'a ExecGraph<'a>,
    params: &'a BTreeMap<String, Value>,
    qctx: QueryCtx<'a>,
    /// The body and its importing scope, kept whole (the one bundle the
    /// planner hands the arm) — the strategy helpers take it by reference
    /// and the null-pad/report sites read its fields.
    spec: SubquerySpec<'a>,
    /// The batch-pass admission (ADR-0669), classified once per statement
    /// in the constructor: `true` when the body runs as one clause-engine
    /// pass over the whole batch instead of one nested invocation per row.
    batch_pass: bool,
    tx: &'a relate_ir::InTransactions,
    /// The input rows, pulled a batch at a time.
    input: RowStream<'a>,
    /// The resolved rows per transaction; resolved once, on the first
    /// batch, from that batch's first row (`None` while no row has been
    /// pulled, so an empty input never evaluates the option expressions).
    batch_size: Option<usize>,
    /// 0-based batch number, for the transaction id.
    batch_index: usize,
    /// The `REPORT STATUS AS <name>` column.
    status_name: Option<String>,
    /// The arm's PROFILE batch facts (REL8-2047), `Some` exactly when a
    /// PROFILE position was attached — the batch loop's counters and
    /// per-batch maxima, noted once per finished batch and read by the
    /// stream's parked record when the consumer is done. `None` keeps an
    /// unprofiled run at zero cost per batch.
    profile: Option<std::rc::Rc<InTxStats>>,
    /// The batch in hand, or `None` once it is spent.
    pending: Option<InstalledBatch>,
    /// `ON ERROR BREAK`: the remaining input rows never started a
    /// transaction, so each is emitted as a null-padded row, one per pull.
    breaking: bool,
    done: bool,
}

/// The batch in hand: its rows are emitted one per pull, and the workspace
/// reservation it carries is released when the batch is dropped — one
/// value, so the charge can never be replaced apart from the rows it
/// bounds.
struct InstalledBatch {
    rows: std::vec::IntoIter<Row>,
    /// Held, never read: its lifetime is the point — dropped with the
    /// batch, so the reservation is released the moment the rows are spent.
    _charge: WorkspaceCharge,
}

impl Iterator for InTransactionsRows<'_> {
    type Item = Result<Row>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // Emit the batch in hand, one row per pull; drop it (and with
            // it its reservation) the moment it is spent.
            if let Some(installed) = self.pending.as_mut() {
                match installed.rows.next() {
                    Some(row) => return Some(Ok(row)),
                    None => self.pending = None,
                }
            }
            if self.done {
                return None;
            }
            match self.advance() {
                Ok(true) => {}
                Ok(false) => return None,
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

impl InTransactionsRows<'_> {
    /// Run the next batch and put its output rows in hand; `false` once the
    /// input is spent. A failed batch under `ON ERROR BREAK` puts the
    /// remaining input rows in hand one at a time instead.
    fn advance(&mut self) -> Result<bool> {
        let Some(rows) = self.next_batch()? else {
            return Ok(false);
        };
        self.install(rows);
        Ok(true)
    }

    /// Produce the next batch — the `ON ERROR BREAK` drain row, or the next
    /// transaction's output — or `None` once the input is spent. Production
    /// and installation are separate so `advance` has one tail.
    fn next_batch(&mut self) -> Result<Option<ChargedRows>> {
        if self.breaking {
            return self.never_started_row();
        }
        match self.pull_batch()? {
            Some(batch) => Ok(Some(self.run_batch(batch)?)),
            None => Ok(None),
        }
    }

    /// Install a batch: its rows are emitted before the next batch runs, and
    /// its workspace reservation is held beside them for exactly that
    /// lifetime — installing replaces (and so releases) the previous
    /// batch's, so the consumer's held rows are billed across its pulls.
    fn install(&mut self, batch: ChargedRows) {
        self.pending = Some(InstalledBatch {
            rows: batch.rows.into_iter(),
            _charge: batch.charge,
        });
    }

    /// One `ON ERROR BREAK` pull: the next input row, which never started a
    /// transaction, as a null-padded row stamped `never started` — `None`
    /// once the input is spent.
    fn never_started_row(&mut self) -> Result<Option<ChargedRows>> {
        let Some(row) = self.input.next_row()? else {
            return Ok(None);
        };
        let row = null_pad_call_row(self.graph, row, self.spec.query, self.spec.scope);
        let mut batch = ChargedRows::new();
        batch.push(row)?;
        batch.stamp(&BatchOutcome::NeverStarted, self.status_name.as_deref())?;
        Ok(Some(batch))
    }

    /// Run one batch and answer the rows to install: the body's output on
    /// success, or the `ON ERROR` report rows of a handled failure (whose
    /// batch was rolled back). `ON ERROR FAIL` propagates the error; `BREAK`
    /// arms `self.breaking`, so the remaining input rows report as never
    /// started.
    fn run_batch(&mut self, batch: ChargedRows) -> Result<ChargedRows> {
        let snapshot = self.graph.scratch_snapshot();
        let tx_id = format!("relate-inner-{}", self.batch_index + 1);
        self.batch_index += 1;
        // The batch's PROFILE accounting (REL8-2047), opened with its
        // marks before the body runs — the one value the commit's peaks
        // and both notes go through.
        let profile = BatchProfile::open(self.profile.as_ref(), self.graph);
        match self.attempt_batch(&batch, &tx_id, &profile) {
            Ok(batch_out) => Ok(batch_out),
            Err(error) => {
                // The failed batch's facts note before the restore: the
                // scratch it created is still measurable, and every
                // failure policy — CONTINUE, BREAK and FAIL alike —
                // rolled this batch back.
                profile.rolled_back(&batch.rows);
                self.graph.restore_scratch(snapshot);
                self.report_failed_batch(&batch, &tx_id, error)
            }
        }
    }

    /// The success path of one batch, every stage that can fail folded
    /// into one `Result` so a body, boundary, capacity or commit error
    /// takes the same per-batch failure path: the body's rows, their
    /// status stamp, the write-boundary close, the output-size check, the
    /// commit, and the batch's PROFILE note once it succeeded. The output
    /// is the attempt's own — built here and answered only on success, so
    /// a failed attempt's partial rows are dropped with it.
    fn attempt_batch(
        &self,
        batch: &ChargedRows,
        tx_id: &str,
        profile: &BatchProfile<'_>,
    ) -> Result<ChargedRows> {
        let mut batch_out = ChargedRows::new();
        // The strategy is chosen once (ADR-0669's admission, classified per
        // statement): the two producers share one signature, so the choice
        // is the only thing that differs.
        let produce = if self.batch_pass {
            pass_batch_rows
        } else {
            per_row_batch_rows
        };
        produce(
            self.graph,
            &batch.rows,
            &self.spec,
            self.params,
            self.qctx,
            &mut batch_out,
        )?;
        // The success status is batch-invariant, so one stamping pass
        // after the row production covers every produced row.
        batch_out.stamp(
            &BatchOutcome::Committed {
                id: tx_id.to_string(),
            },
            self.status_name.as_deref(),
        )?;
        // Each `CALL { ... } IN TRANSACTIONS` batch is its
        // own transaction boundary, so the property default values
        // land here -- after the batch's writes, before the batch's
        // constraint validation.
        // The write boundary of this batch — the same stage the
        // statement boundary closes with, named once in `exec/`.
        crate::exec::close_write_boundary(self.graph)?;
        // The output-size check runs inside the attempt, before the
        // commit: a capacity refusal must take the same per-batch
        // failure path as a body or constraint error — the batch is
        // rolled back and reported — never fire on a batch already
        // durably committed.
        land_batch(self.graph, &batch_out.rows)?;
        // The commit peaks read here, before the commit: its rebase
        // drains the redo log the WAL figure reads (REL8-2047).
        let peaks = profile.read_peaks();
        // ADR-0210: with a facade-injected sink (auto-commit write
        // path), each batch commits durably before the next starts
        // and the scratch rebase keeps live state bounded by one
        // batch. The commit is folded into the attempt so a failed
        // batch commit (a conflict race, a capacity rejection, a WAL
        // error) takes the same per-batch failure path as body and
        // constraint errors: `ON ERROR CONTINUE`/`BREAK`/`RETRY` apply
        // and `REPORT STATUS` rows are produced. The redo is drained
        // only on commit success, so the failure path's
        // `restore_scratch` rolls a failed batch back cleanly.
        // Without a sink (explicit transactions, nested execution)
        // the scratch accumulates to statement end, as before.
        if let Some(sink) = self.qctx.batch_commit {
            self.graph.commit_batch(sink)?;
        }
        // The batch's PROFILE facts (REL8-2047) note only once the
        // commit succeeded — a failed commit takes the rolled-back
        // arm of `run_batch`.
        profile.committed(peaks, &batch.rows, &batch_out.rows);
        Ok(batch_out)
    }

    /// The `ON ERROR` half of a failed, already rolled-back batch: decode
    /// the failure policy once (`FAIL` propagates the error, `BREAK` arms
    /// `self.breaking`, `CONTINUE` goes on), then answer the report rows —
    /// the failed batch's own rows are gone with the rollback.
    fn report_failed_batch(
        &mut self,
        batch: &ChargedRows,
        tx_id: &str,
        error: Error,
    ) -> Result<ChargedRows> {
        // A cancellation — the caller's stop or the statement deadline
        // (ADR-0692) — is the statement's, not the batch's: `ON ERROR
        // CONTINUE`/`BREAK` must not absorb it and run on past the
        // deadline. The failed batch is already rolled back; earlier
        // batches stay committed.
        if matches!(error, Error::Cancelled) {
            return Err(error);
        }
        // CALL IN TRANSACTIONS retry: not implemented — see the
        // LABS-712 follow-up. The projection applies only a
        // RETRY's explicit THEN fallback (the duration is
        // deliberately unused), like a non-transient error. One
        // decode of the three-state outcome, all three named.
        match self.tx.on_error.outcome() {
            relate_ir::RetryFallback::Fail => return Err(error),
            relate_ir::RetryFallback::Break => self.breaking = true,
            relate_ir::RetryFallback::Continue => {}
        }
        // Report the failed batch (its own rows are gone with the
        // rollback), and on BREAK every remaining row, where no
        // transaction ever started and nothing was bound.
        let mut failed = ChargedRows::new();
        for row in &batch.rows {
            failed.push(failed_batch_row(
                self.graph,
                row,
                self.spec.query,
                self.spec.scope,
                self.params,
            )?)?;
        }
        failed.stamp(
            &BatchOutcome::Failed {
                id: tx_id.to_string(),
                error: transaction_error_message(&error),
            },
            self.status_name.as_deref(),
        )?;
        land_batch(self.graph, &failed.rows)?;
        Ok(failed)
    }

    /// Pull one transaction's input rows from the stream, charged as the
    /// batch grows so an `OF n ROWS` the statement chose cannot hold an
    /// unbilled container. The batch size is resolved once, on the first
    /// row (its expressions are evaluated against it); an empty input
    /// answers no batch and evaluates nothing.
    fn pull_batch(&mut self) -> Result<Option<ChargedRows>> {
        // No row at all is no batch — an absence, never a fabricated empty
        // one — and that holds at every batch boundary, not just the
        // first: the size is resolved after the first row any batch ever
        // pulled, so a later batch that opens on an exhausted input
        // answers `None` here, never an empty `Some`.
        let Some(first) = self.input.next_row()? else {
            return Ok(None);
        };
        // The size resolves once (from the first row any batch ever
        // pulled) and is bound here for both uses below — the `None`
        // state stays exactly "no row pulled yet", never a value the code
        // re-reads from the cell it just wrote.
        let size = match self.batch_size {
            Some(size) => size,
            None => {
                let size = resolve_batch_options(self.graph, &first, self.tx, self.params)?;
                self.batch_size = Some(size);
                size
            }
        };
        let mut batch = ChargedRows::new();
        batch.push(first)?;
        while batch.rows.len() < size {
            let Some(row) = self.input.next_row()? else {
                break;
            };
            batch.push(row)?;
        }
        Ok(Some(batch))
    }
}

/// A batch's rows with the workspace reservation that must outlive them —
/// the pair's invariant in its shape, so no `_`-prefixed binding can
/// silently drop the charge while the rows are still live. One type for
/// both sides of the arm: the input batch [`pull_batch`] pulls and a
/// batch's output [`run_batch`] produces, so the rows and the charge that
/// bounds them are never two independent bindings a caller can let drift.
pub(super) struct ChargedRows {
    rows: Vec<Row>,
    charge: WorkspaceCharge,
}

impl ChargedRows {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            charge: WorkspaceCharge::empty(),
        }
    }

    /// A container over rows already in hand, measured on construction —
    /// the seed-container twin of [`Self::new`], so a second copy of a
    /// batch is billed by the same type rather than a binding beside it.
    pub(super) fn from_rows(rows: Vec<Row>) -> Result<Self> {
        let mut batch = Self::new();
        batch.extend(rows)?;
        Ok(batch)
    }

    /// Append one row and re-measure — the pairing lives in the type, so
    /// a producer cannot grow the rows without billing them.
    fn push(&mut self, row: Row) -> Result<()> {
        self.rows.push(row);
        self.measure()
    }

    /// Append rows and re-measure, the batch-major twin of [`Self::push`].
    pub(super) fn extend(&mut self, rows: impl IntoIterator<Item = Row>) -> Result<()> {
        self.rows.extend(rows);
        self.measure()
    }

    /// The rows, moved out for a crossing that consumes them — the charge
    /// stays in `self`, which the caller keeps alive for exactly the rows'
    /// use, so the reservation never outlives what it bounds. No
    /// re-measure: the reservation is the rows' bound, and it stays.
    pub(super) fn take_rows(&mut self) -> Vec<Row> {
        std::mem::take(&mut self.rows)
    }

    /// Stamp an outcome onto these rows and re-measure: the `REPORT STATUS`
    /// entry widens every row, so the reservation must be re-taken after it
    /// — the one home for the stamp-then-measure pair, so no site can widen
    /// the rows without re-billing them.
    fn stamp(&mut self, outcome: &BatchOutcome, name: Option<&str>) -> Result<()> {
        outcome.stamp(&mut self.rows, name);
        self.measure()
    }

    /// Re-measure the rows held against the workspace ledger — the one
    /// spelling every producer shares, so a batch size the statement's own
    /// `OF n ROWS` chose cannot hold an unbilled container.
    fn measure(&mut self) -> Result<()> {
        self.charge
            .resize_with(|| crate::workspace::retained_rows_of(&self.rows))
    }
}

/// The stateless per-allocation size check on a batch about to be handed
/// to the consumer (the expression cap's meter) — not the workspace
/// reservation: that lives in the arm's `InstalledBatch`, resized when a
/// batch is installed and held for exactly the rows' lifetime. The retain
/// disposition's declared state is one batch (ADR-0660): a call of many
/// batches holds and charges one batch's rows, never a running total of
/// the statement's output.
fn land_batch(graph: &ExecGraph<'_>, rows: &[Row]) -> Result<()> {
    graph.check_expression_allocation(crate::workspace::retained_rows_of(rows))
}

/// Resolve the call's batch size (rows per transaction) and validate the
/// concurrency hint. The argument-class rules live in this one unit: a
/// wrong-typed value (a String, null) is the `TypeError` class, while a
/// FLOAT — whole or not — joins the integers outside the positive range
/// in the `ArgumentError` class: live's 22N03 for the batch size reads
/// "Expected 'value' to be of type INTEGER and in the range 1 to
/// 9223372036854775807 but found 1.0", the same rejection as -1 (ADR-0565
/// probes 0 under 22003). Unlike the ADR-0175 function sites
/// (`substring()` & co truncate a float), a float batch size never
/// converts. The concurrency hint is validated, never applied —
/// execution stays sequential — and its runtime check is type-only with
/// the same no-coercion rule: any FLOAT is the `TypeError` (live
/// matches), an integer of any value passes.
fn resolve_batch_options(
    graph: &ExecGraph<'_>,
    first: &Row,
    tx: &relate_ir::InTransactions,
    params: &BTreeMap<String, Value>,
) -> Result<usize> {
    let batch_size = match tx.batch_size.as_ref() {
        None => 1000,
        Some(expr) => {
            // A FLOAT maps to the one range rejection below (joined with
            // the integers outside the positive range), everything else
            // non-INTEGER is the wrong-typed rejection — both read the
            // site's row (integer_args, the single text home).
            let size = match eval(graph, first, expr, params)? {
                // `try_from` refuses an integer that does not fit
                // `usize` rather than truncating it, and zero is
                // refused because the site is positive-only — the
                // conversion bound and the positive range are one
                // rejection.
                Value::Int(size) => usize::try_from(size).ok().filter(|n| *n > 0),
                Value::Float(_) => None,
                _ => return Err(Error::ty(TX_BATCH_SIZE.type_error)),
            };
            size.ok_or_else(|| {
                Error::argument_value_kind(ErrorKind::NumberOutOfRange, range_text(&TX_BATCH_SIZE))
            })?
        }
    };
    // The concurrency hint is validated, not applied. Unlike the
    // batch size, a non-positive value is deliberately NOT rejected here:
    // live gives a parameter `0` the default concurrency and a negative
    // value a default-derived one, while only the *literal* forms are
    // rejected — at plan time, by `check_positive_int_arg`. The runtime
    // contract is type-only, and type-only means no float coercion: a
    // FLOAT parameter — whole or not — is the TypeError.
    if let relate_ir::Concurrency::Value(expr) = &tx.concurrency {
        if !matches!(eval(graph, first, expr, params)?, Value::Int(_)) {
            return Err(Error::ty(TX_CONCURRENCY.type_error));
        }
    }
    Ok(batch_size)
}

/// The outcome of one `CALL { … } IN TRANSACTIONS` batch, as the
/// `REPORT STATUS AS <name>` row reports it: `started`, `committed`, the
/// transaction id and the error message all derive from this one value, so
/// an invalid combination (committed without an id, a failed batch with no
/// error) is not representable. The RETRY states get their home here once
/// retry is implemented (LABS-712 follow-up).
enum BatchOutcome {
    Committed { id: String },
    Failed { id: String, error: String },
    NeverStarted,
}

impl BatchOutcome {
    /// Write this outcome's `REPORT STATUS AS <name>` column onto every row,
    /// when the call declares one. The one writer of that column: every
    /// reporting site hands its rows over instead of re-spelling the
    /// `Option<(&str, Value)>` policy.
    fn stamp(&self, rows: &mut [Row], status_name: Option<&str>) {
        let Some(name) = status_name else {
            return;
        };
        let status = self.status();
        for row in rows {
            row.insert(name.to_owned(), status.clone());
        }
    }

    /// The REPORT STATUS row's wire map, built from the one outcome value.
    fn status(&self) -> Value {
        let (started, committed, id, error) = match self {
            Self::Committed { id } => (true, true, Some(id.as_str()), None),
            Self::Failed { id, error } => (true, false, Some(id.as_str()), Some(error.as_str())),
            Self::NeverStarted => (false, false, None, None),
        };
        Value::Map(
            BTreeMap::from([
                ("started".into(), Value::Bool(started)),
                ("committed".into(), Value::Bool(committed)),
                (
                    "transactionId".into(),
                    id.map(|s| Value::String(s.into())).unwrap_or(Value::Null),
                ),
                (
                    "errorMessage".into(),
                    error
                        .map(|s| Value::String(s.into()))
                        .unwrap_or(Value::Null),
                ),
            ])
            .into(),
        )
    }
}

/// The row a failed batch reports under `ON ERROR CONTINUE`.
///
/// Cypher keeps the **imported** bindings on the failed row — a body
/// `RETURN i AS k` still answers `k = i` — while a value bound by the
/// iteration that failed reads null, because the batch's own rows are
/// dropped with the rollback. So the exported columns are evaluated over
/// the import seed with every other body-bound name read as null, rather
/// than all-nulled (which lost the imported values) or taken from the
/// half-finished batch (which reported rows the rollback removed).
///
/// A body that does not end in a row projection, or whose projection
/// aggregates or expands `*`, keeps the plain null-pad policy: there is no
/// per-row export to evaluate.
fn failed_batch_row(
    graph: &ExecGraph<'_>,
    outer: &Row,
    query: &Query,
    scope: &relate_ir::CallScope,
    params: &BTreeMap<String, Value>,
) -> Result<Row> {
    // The body's export shape through the one derivation every
    // export-shape consumer reads (`super::final_export_items`), gated to
    // the plain per-row projections a seed row can evaluate.
    let projected = super::final_export_items(query.final_stage())
        .filter(|(_, items)| super::plain_per_row_projection(items));
    let Some((clauses, items)) = projected else {
        return Ok(null_pad_call_row(graph, outer.clone(), query, scope));
    };
    // The imports plus, for every other name the body declares before its
    // RETURN, an explicit null: a row miss is an undefined variable, and
    // the failing iteration's bindings are gone with its rolled-back row.
    let mut seed = scope_seed(graph, query, scope, outer)?;
    for name in star_scope_names(clauses) {
        seed.entry(name).or_insert(Value::Null);
    }
    let mut row = outer.clone();
    for (index, item) in items.iter().enumerate() {
        let name = item.output_name(index);
        // A failed or unresolvable projection reads null (stellar round
        // 2, F-2-51) — the one policy, stated once: the projection can
        // fail for the same reason the batch did (the failing clause may
        // be this very expression), and a name the import seed does not
        // carry is the failing iteration's binding, gone with the
        // rollback — either way `ON ERROR CONTINUE` must not turn a
        // handled batch failure into a statement failure, so the column
        // reports null.
        let value = eval(graph, &seed, &item.expr, params).unwrap_or(Value::Null);
        row.insert(name, value);
    }
    Ok(row)
}

/// Render a failed inner-transaction batch's error for the `REPORT STATUS`
/// row. The typed `Error` classifies it: the uniqueness violation is its
/// own variant (matched first, never sniffed from rendered text) and keeps
/// the ADR-0172-pinned `A value already exists …` wrapper; the other typed
/// detail variants hand over the legacy bare inner text — token included,
/// single-sourced in relate-core — exactly as the stringly
/// `Type`/`Semantic` arms hand over their payload directly instead of
/// stripping a Display prefix.
pub(crate) fn transaction_error_message(error: &Error) -> String {
    match error {
        // First: the typed-detail hand-over below must not intercept this
        // variant — the wrapper is pinned byte-identical (ADR-0172).
        Error::UniquenessViolation { .. } => {
            format!("A value already exists and violates a uniqueness constraint: {error}")
        }
        Error::Type(message) | Error::Semantic(message) => message.clone(),
        // ADR-0307: division/modulo by zero hand over the payload their
        // legacy `Type` arm did (`/ by zero`); rounding keeps the bare
        // `ArithmeticError: …` text its legacy `Execution` Display had.
        Error::Arithmetic(failure) => failure.detail().to_string(),
        other => other
            .typed_detail_text()
            .unwrap_or_else(|| other.to_string()),
    }
}
