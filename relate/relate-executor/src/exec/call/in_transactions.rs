//! `CALL { … } IN TRANSACTIONS`: batch boundaries, per-batch commit and
//! rollback, and the `ON ERROR` / `REPORT STATUS` rows.
//!
//! The arm's disposition is `retain` (ADR-0660): its declared state is one
//! batch's output, and it answers as the arm's [`RowStream`] — one batch
//! runs, commits and is handed to the consumer before the next starts, so
//! the statement's output is never accumulated (see [`InTransactionsRows`]
//! and [`land_batch`]).

use std::collections::BTreeMap;

use relate_core::tokens::WorkspaceCharge;
use relate_core::{Error, ErrorKind, Result, Value};
use relate_ir::Query;

use super::{call_subquery_row, empty_body_row, null_pad_call_row, scope_seed, SubquerySpec};
use crate::ctx::QueryCtx;
use crate::eval::eval;
use crate::exec::feed::RowStream;
use crate::exec::scope::star_scope_names;
use crate::metrics::PlanMetricsSite;
use crate::overlay::ExecGraph;
use crate::row::Row;

pub(crate) fn exec_call_in_transactions<'a>(
    graph: &'a ExecGraph<'a>,
    rows: RowStream<'a>,
    spec: SubquerySpec<'a>,
    tx: &'a relate_ir::InTransactions,
    params: &'a BTreeMap<String, Value>,
    qctx: QueryCtx<'a>,
    record: Option<(PlanMetricsSite<'a>, usize)>,
) -> Result<RowStream<'a>> {
    let SubquerySpec {
        query,
        optional,
        scope,
    } = spec;
    // The `REPORT STATUS AS <name>` column, resolved once: every status
    // value below is written through `BatchOutcome::stamp`, so "the call
    // reports a status" has a single spelling.
    // Text render: the status column feeds the String-keyed stamp.
    let status_name = tx.report_status.as_ref().map(|name| name.to_string());
    Ok(RowStream::emitting_to_completion(
        InTransactionsRows {
            graph,
            params,
            qctx,
            query,
            scope,
            optional,
            tx,
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
            pending: Vec::new().into_iter(),
            // The installed batch's workspace reservation: the state and
            // the charge that bounds it have one owner and one lifetime —
            // resized when a batch is installed, released when the next
            // replaces it — so the declared one-batch disposition is
            // billed for as long as the consumer still holds its rows.
            pending_charge: WorkspaceCharge::empty(),
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
    query: &'a Query,
    scope: &'a relate_ir::CallScope,
    optional: bool,
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
    /// The batch in hand: its rows are emitted before the next batch runs.
    pending: std::vec::IntoIter<Row>,
    /// The installed batch's workspace reservation, held beside `pending`
    /// for exactly its lifetime.
    pending_charge: WorkspaceCharge,
    /// `ON ERROR BREAK`: the remaining input rows never started a
    /// transaction, so each is emitted as a null-padded row, one per pull.
    breaking: bool,
    done: bool,
}

impl Iterator for InTransactionsRows<'_> {
    type Item = Result<Row>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(row) = self.pending.next() {
                return Some(Ok(row));
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
        if self.breaking {
            let Some(row) = self.input.next_row()? else {
                return Ok(false);
            };
            let mut row = null_pad_call_row(self.graph, row, self.query, self.scope);
            BatchOutcome::NeverStarted
                .stamp(std::slice::from_mut(&mut row), self.status_name.as_deref());
            charge_batch(&mut self.pending_charge, std::slice::from_ref(&row))?;
            self.pending = vec![row].into_iter();
            return Ok(true);
        }
        let Some(batch) = self.pull_batch()? else {
            return Ok(false);
        };
        let batch_index = self.batch_index;
        self.batch_index += 1;
        let snapshot = self.graph.scratch_snapshot();
        let tx_id = format!("relate-inner-{}", batch_index + 1);
        let mut batch_out = Vec::new();
        let mut out_charge = WorkspaceCharge::empty();
        let attempt = (|| -> Result<()> {
            for row in &batch.rows {
                let mut produced = call_subquery_row(
                    self.graph,
                    row,
                    self.query,
                    self.scope,
                    self.params,
                    self.qctx,
                )?;
                if produced.is_empty() {
                    produced.extend(empty_body_row(
                        self.graph,
                        row.clone(),
                        self.optional,
                        self.query,
                        self.scope,
                    ));
                }
                batch_out.extend(produced);
                // The batch's output is charged as it grows — the one
                // spelling the input pull shares — so a batch size the
                // statement's own `OF n ROWS` chose cannot hold an
                // unbilled container on the workspace ledger.
                charge_batch(&mut out_charge, &batch_out)?;
            }
            // The success status is batch-invariant, so one stamping pass
            // after the row loop covers every produced row.
            BatchOutcome::Committed { id: tx_id.clone() }
                .stamp(&mut batch_out, self.status_name.as_deref());
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
            land_batch(self.graph, &batch_out)?;
            // ADR-0210: with a facade-injected sink (auto-commit write
            // path), each batch commits durably before the next starts
            // and the scratch rebase keeps live state bounded by one
            // batch. The commit is folded into the attempt so a failed
            // batch commit (a conflict race, a capacity rejection, a WAL
            // error) takes the same per-batch failure path as body and
            // constraint errors: `ON ERROR CONTINUE`/`BREAK`/`RETRY` apply
            // and `REPORT STATUS` rows are produced. The redo is drained
            // only on commit success, so the `Err` arm's
            // `restore_scratch` rolls a failed batch back cleanly.
            // Without a sink (explicit transactions, nested execution)
            // the scratch accumulates to statement end, as before.
            if let Some(sink) = self.qctx.batch_commit {
                self.graph.commit_batch(sink)?;
            }
            Ok(())
        })();
        // The outcome decides which rows and charge are in hand; the
        // installation itself has one tail below.
        let (rows, charge) = match attempt {
            Ok(()) => (batch_out, out_charge),
            Err(error) => {
                self.graph.restore_scratch(snapshot);
                // CALL IN TRANSACTIONS retry: not implemented — see the
                // LABS-712 follow-up. The projection applies only a
                // RETRY's explicit THEN fallback (the duration is
                // deliberately unused), like a non-transient error. One
                // decode of the three-state outcome, all three named.
                let mode = self.tx.on_error.outcome();
                self.breaking = match mode {
                    relate_ir::RetryFallback::Fail => return Err(error),
                    relate_ir::RetryFallback::Continue => false,
                    relate_ir::RetryFallback::Break => true,
                };
                // Report the failed batch (its own rows are gone with the
                // rollback), and on BREAK every remaining row, where no
                // transaction ever started and nothing was bound.
                let mut failed_rows = Vec::new();
                let mut failed_charge = WorkspaceCharge::empty();
                for row in &batch.rows {
                    failed_rows.push(failed_batch_row(
                        self.graph,
                        row,
                        self.query,
                        self.scope,
                        self.params,
                    )?);
                    charge_batch(&mut failed_charge, &failed_rows)?;
                }
                BatchOutcome::Failed {
                    id: tx_id.clone(),
                    error: transaction_error_message(&error),
                }
                .stamp(&mut failed_rows, self.status_name.as_deref());
                land_batch(self.graph, &failed_rows)?;
                (failed_rows, failed_charge)
            }
        };
        // The reservation moves with the rows: installing a batch replaces
        // the previous charge (releasing it), so the consumer's held rows
        // are billed across its pulls.
        self.pending_charge = charge;
        self.pending = rows.into_iter();
        Ok(true)
    }

    /// Pull one transaction's input rows from the stream, charged as the
    /// batch grows so an `OF n ROWS` the statement chose cannot hold an
    /// unbilled container. The batch size is resolved once, on the first
    /// row (its expressions are evaluated against it); an empty input
    /// answers no batch and evaluates nothing.
    fn pull_batch(&mut self) -> Result<Option<HeldBatch>> {
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
        let mut batch = HeldBatch {
            rows: Vec::new(),
            charge: WorkspaceCharge::empty(),
        };
        batch.rows.push(first);
        charge_batch(&mut batch.charge, &batch.rows)?;
        while batch.rows.len() < size {
            let Some(row) = self.input.next_row()? else {
                break;
            };
            batch.rows.push(row);
            charge_batch(&mut batch.charge, &batch.rows)?;
        }
        Ok(Some(batch))
    }
}

/// One transaction's input batch with the reservation that must outlive it
/// — the pair's invariant in its shape, so no `_`-prefixed binding can
/// silently drop the charge while the rows are still live.
struct HeldBatch {
    rows: Vec<Row>,
    charge: WorkspaceCharge,
}

/// Charge a growing batch against the workspace ledger — the one spelling
/// the input pull and the batch's output share.
fn charge_batch(charge: &mut WorkspaceCharge, rows: &[Row]) -> Result<()> {
    charge.resize_with(|| crate::workspace::retained_rows_of(rows))
}

/// The stateless per-allocation size check on a batch about to be handed
/// to the consumer (the expression cap's meter) — not the workspace
/// reservation: that lives in the arm's `pending_charge`, resized when a
/// batch is installed and held for exactly the rows' lifetime. The retain
/// disposition's declared state is one batch (ADR-0660): a call of many
/// batches holds and charges one batch's rows, never a running total of
/// the statement's output.
fn land_batch(graph: &ExecGraph<'_>, rows: &[Row]) -> Result<()> {
    graph.check_expression_allocation(crate::workspace::retained_rows_of(rows))
}

/// Resolve the call's batch size (rows per transaction) and validate the
/// concurrency hint. The argument-class rules live in this one unit: a
/// non-integer is the `TypeError` class, an integer outside the positive
/// range is the `ArgumentError` class (live Neo4j 2026.10.1 reports 0
/// under 22003). The concurrency hint is validated, never applied —
/// execution stays sequential.
fn resolve_batch_options(
    graph: &ExecGraph<'_>,
    first: &Row,
    tx: &relate_ir::InTransactions,
    params: &BTreeMap<String, Value>,
) -> Result<usize> {
    const MSG: &str = "batch size must be a positive integer";
    let batch_size = match tx.batch_size.as_ref() {
        None => 1000,
        Some(expr) => {
            let size = eval(graph, first, expr, params)?
                .as_int()
                .ok_or_else(|| Error::ty(MSG))?;
            // A narrowing cast is not a range guard: on a 32-bit target a
            // value above u32::MAX would truncate (to 0, panicking
            // `chunks(0)`), so the conversion and the positive range are one
            // rejection.
            usize::try_from(size)
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| Error::argument_value_kind(ErrorKind::NumberOutOfRange, MSG))?
        }
    };
    // The CIP-260 concurrency hint is validated, not applied. Unlike the
    // batch size, a non-positive value is deliberately NOT rejected here:
    // CIP-34 (neo4j-verified scenarios 17-19) gives a parameter `0` the
    // default concurrency and a negative value a default-derived one, while
    // only the *literal* forms are rejected — at plan time, by
    // `check_positive_int_arg`. The runtime contract is type-only.
    if let relate_ir::Concurrency::Value(expr) = &tx.concurrency {
        eval(graph, first, expr, params)?
            .as_int()
            .ok_or_else(|| Error::ty("concurrency must be an integer"))?;
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
    let mut seed = scope_seed(graph, scope, outer)?;
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
