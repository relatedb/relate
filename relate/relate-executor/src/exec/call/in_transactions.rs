//! `CALL { … } IN TRANSACTIONS`: batch boundaries, per-batch commit and
//! rollback, and the `ON ERROR` / `REPORT STATUS` rows.

use std::collections::{BTreeMap, HashSet};

use relate_core::{Error, ErrorKind, Result, Value};
use relate_ir::Query;

use super::{call_subquery_row, empty_body_row, null_pad_call_row, scope_seed, SubquerySpec};
use crate::ctx::QueryCtx;
use crate::eval::{eval, is_agg_expr};
use crate::exec::constraint_validation::validate_constraints;
use crate::exec::scope::{has_star_item, star_scope_names, terminal_return_items};
use crate::overlay::ExecGraph;
use crate::row::Row;

pub(crate) fn exec_call_in_transactions(
    graph: &ExecGraph<'_>,
    rows: Vec<Row>,
    spec: SubquerySpec<'_>,
    tx: &relate_ir::InTransactions,
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
) -> Result<Vec<Row>> {
    let SubquerySpec {
        query,
        optional,
        scope,
        in_transactions: _,
    } = spec;
    // No input rows: no batch runs, and the batch-size/concurrency
    // expressions have no row to evaluate against — the call is a no-op
    // (a fabricated empty row would silently read null; ADR-0335).
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let batch_size = resolve_batch_options(graph, &rows[0], tx, params, qctx)?;
    // The `REPORT STATUS AS <name>` column, resolved once: every status
    // value below is written through `BatchOutcome::stamp`, so "the call
    // reports a status" has a single spelling.
    // Text render: the status column feeds the String-keyed stamp.
    let status_name = tx.report_status.as_ref().map(|name| name.to_string());
    let mut out = Vec::new();
    let mut batches = rows.chunks(batch_size).enumerate();
    for (batch_index, batch) in batches.by_ref() {
        let snapshot = graph.scratch_snapshot();
        let tx_id = format!("relate-inner-{}", batch_index + 1);
        let mut batch_out = Vec::new();
        let attempt = (|| -> Result<()> {
            for row in batch {
                let mut produced = call_subquery_row(graph, row, query, scope, params, qctx)?;
                if produced.is_empty() {
                    produced.extend(empty_body_row(row.clone(), optional, query, scope));
                }
                batch_out.extend(produced);
            }
            // The success status is batch-invariant, so one stamping pass
            // after the row loop covers every produced row.
            BatchOutcome::Committed { id: tx_id.clone() }
                .stamp(&mut batch_out, status_name.as_deref());
            // CIP-262: each `CALL { ... } IN TRANSACTIONS` batch is its
            // own transaction boundary, so the property default values
            // land here -- after the batch's writes, before the batch's
            // constraint validation.
            super::super::property_defaults::apply_property_defaults(graph)?;
            validate_constraints(graph)?;
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
            if let Some(sink) = qctx.batch_commit {
                graph.commit_batch(sink)?;
            }
            Ok(())
        })();
        match attempt {
            Ok(()) => out.extend(batch_out),
            Err(error) => {
                graph.restore_scratch(snapshot);
                // CALL IN TRANSACTIONS retry: not implemented — see the
                // LABS-712 follow-up. The projection applies only a
                // RETRY's explicit THEN fallback (the duration is
                // deliberately unused), like a non-transient error.
                let mode = tx.on_error.outcome();
                if mode == relate_ir::RetryFallback::Fail {
                    return Err(error);
                }
                // Report the failed batch (its own rows are gone with the
                // rollback), and on BREAK every remaining row, where no
                // transaction ever started and nothing was bound.
                let mut failed_rows = Vec::new();
                for row in batch {
                    failed_rows.push(failed_batch_row(graph, row, query, scope, params, qctx)?);
                }
                BatchOutcome::Failed {
                    id: tx_id.clone(),
                    error: transaction_error_message(&error),
                }
                .stamp(&mut failed_rows, status_name.as_deref());
                out.extend(failed_rows);
                if mode == relate_ir::RetryFallback::Break {
                    let mut never_started_rows = Vec::new();
                    for (_, batch) in batches.by_ref() {
                        for row in batch {
                            never_started_rows.push(null_pad_call_row(row.clone(), query, scope));
                        }
                    }
                    BatchOutcome::NeverStarted
                        .stamp(&mut never_started_rows, status_name.as_deref());
                    out.extend(never_started_rows);
                    break;
                }
            }
        }
    }
    Ok(out)
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
    qctx: QueryCtx<'_>,
) -> Result<usize> {
    const MSG: &str = "batch size must be a positive integer";
    let batch_size = match tx.batch_size.as_ref() {
        None => 1000,
        Some(expr) => {
            let size = eval(graph, first, expr, params, qctx.cypher_version)?
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
        let value = eval(graph, first, expr, params, qctx.cypher_version)?;
        if value.as_int().is_none() {
            return Err(Error::ty("concurrency must be an integer"));
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
    qctx: QueryCtx<'_>,
) -> Result<Row> {
    // The body's export shape is its FINAL stage's projection — the same
    // trailing-clause classification `call_subquery_row` and the conditional
    // branches read (`final_stage`).
    let projected = query.final_stage().parts.first().and_then(|part| {
        let (clauses, items) = terminal_return_items(part)?;
        (!has_star_item(items) && !items.iter().any(|item| is_agg_expr(&item.expr)))
            .then_some((clauses, items))
    });
    let Some((clauses, items)) = projected else {
        return Ok(null_pad_call_row(outer.clone(), query, scope));
    };
    // The imports plus, for every other name the body declares before its
    // RETURN, an explicit null: a row miss is an undefined variable, and
    // the failing iteration's bindings are gone with its rolled-back row.
    let mut seed = scope_seed(scope, outer)?;
    for name in star_scope_names(clauses) {
        seed.entry(name).or_insert(Value::Null);
    }
    let mut row = outer.clone();
    for (index, item) in items.iter().enumerate() {
        let name = item.output_name(index);
        let mut vars = HashSet::new();
        relate_ir::expr_vars(&item.expr, &mut vars);
        let value = if vars.iter().all(|v| seed.contains_key(v)) {
            // The projection can fail for the same reason the batch did
            // (the failing clause may be this very expression); the failed
            // row reports that column as null — `ON ERROR CONTINUE` must
            // not turn a handled batch failure into a statement failure.
            match eval(graph, &seed, &item.expr, params, qctx.cypher_version) {
                Ok(value) => value,
                Err(_) => Value::Null,
            }
        } else {
            Value::Null
        };
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
