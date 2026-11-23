//! `SHOW TRANSACTIONS` and `TERMINATE TRANSACTIONS` row producers.

use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};

use crate::ctx::QueryCtx;
use crate::eval::eval;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::super::catalog::{catalog_row, principal_cell};

/// The ids one id argument yields, under the whole argument contract both
/// transaction commands share: a list's elements, or the scalar itself as a
/// one-element slice. `None` is a NULL element (the absent id); every other
/// element must be a STRING and surfaces as its text. Anything else is the
/// argument's own type error — live raises it for `SHOW TRANSACTIONS 123`
/// and `TERMINATE TRANSACTIONS 123` alike, so neither command may quietly
/// filter such a value out or fold it onto the NULL id.
fn transaction_ids(value: &Value) -> Result<Vec<Option<&str>>> {
    let items: &[Value] = match value {
        Value::List(items) => items,
        scalar => std::slice::from_ref(scalar),
    };
    items
        .iter()
        .map(|item| match item {
            Value::String(s) => Ok(Some(&**s)),
            Value::Null => Ok(None),
            _ => Err(Error::invalid_argument_type(
                "transaction id must be STRING",
            )),
        })
        .collect()
}

/// The id of the one running transaction an embedded engine holds.
const SELF_TRANSACTION_ID: &str = "neo4j-transaction-1";

pub(crate) fn show_transactions_rows(
    graph: &ExecGraph<'_>,
    c: &relate_parser::ShowTransactionsClause,
    rows: &[Row],
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
) -> Result<Vec<Row>> {
    // Every id argument is evaluated even after one matches: a malformed
    // later argument is the argument's own type error, never skipped.
    let mut found = c.ids.is_empty();
    for expr in &c.ids {
        let expr = relate_front::lower_expr(expr)?;
        for row in rows {
            let value = eval(graph, row, &expr, params)?;
            found |= transaction_ids(&value)?
                .into_iter()
                .flatten()
                .any(|id| id == SELF_TRANSACTION_ID);
        }
    }
    Ok(if found {
        vec![self_row(graph, params, qctx)]
    } else {
        Vec::new()
    })
}

/// The self row: the one running transaction an embedded engine holds,
/// carrying every `YIELD *` column in the front table's (alphabetical)
/// order. `database` follows the session's namespace; `username` is the
/// connection's authenticated principal (ADR-0273), empty for an embedded
/// connection. There is one embedded connection (`connectionId` empty) and
/// no client socket (`clientAddress` null under Cypher 25). `parameters` is
/// the statement's own parameter map, `metaData` is empty (no transaction
/// metadata API) and `currentQueryStatus` is `running`. Every other
/// server-observability column (timings, lock/page/heap counters, query id,
/// planner/runtime, ...) has no engine state behind it and is a typed Null,
/// never a fabricated value: that is the one place a column absent from the
/// filled cells falls back to Null.
fn self_row(graph: &ExecGraph<'_>, params: &BTreeMap<String, Value>, qctx: QueryCtx<'_>) -> Row {
    let mut filled: BTreeMap<&str, Value> = BTreeMap::from([
        ("transactionId", Value::String(SELF_TRANSACTION_ID.into())),
        ("currentQuery", Value::String(qctx.query_text.into())),
        ("database", Value::String(graph.namespace().as_str().into())),
        ("username", principal_cell(qctx.principal, "")),
        ("status", Value::String("Running".into())),
        ("connectionId", Value::String("".into())),
        ("currentQueryStatus", Value::String("running".into())),
        ("metaData", Value::Map(Default::default())),
        ("parameters", Value::Map(params.clone().into())),
    ]);
    let row: Row = relate_front::show_columns::full_transaction_columns()
        .into_iter()
        .map(|column| {
            let cell = filled.remove(column.as_str()).unwrap_or(Value::Null);
            (column, cell)
        })
        .collect();
    assert!(
        filled.is_empty(),
        "self-row cells {:?} name no SHOW TRANSACTIONS column",
        filled.keys().collect::<Vec<_>>()
    );
    row
}

/// The id live reports for one STRING id: the database name of a
/// `<database>-transaction-<id>` id is lowercased (`'noDB-transaction-123'`
/// reports `'nodb-transaction-123'`), the rest of the id is kept as written.
/// Applied before the per-row dedup, so two spellings of one id collapse
/// onto the canonical value live reports.
fn reported_id(id: &str) -> Value {
    let reported = match id.split_once("-transaction-") {
        Some((database, rest)) => format!("{}-transaction-{rest}", database.to_lowercase()),
        None => id.to_owned(),
    };
    Value::String(reported.into())
}

/// `TERMINATE TRANSACTIONS` projects over each input row independently: the
/// id expressions are evaluated against *that* row, the row's value is
/// deduplicated within itself, and the rows are concatenated — the clause's
/// `YIELD`/`RETURN`/paging then run once over the flattened result. The
/// projection is per row, never over the whole input set: live reports one
/// row per input value (`UNWIND ['a','b'] AS x TERMINATE TRANSACTIONS x`
/// gives two rows), while a *list* argument deduplicates within itself
/// (`TERMINATE TRANSACTIONS ['a','a','b']` gives two). A cross-row dedup
/// would collapse the former to one row and a whole-set scan would repeat
/// the latter per input row; both were observed before this fix.
pub(crate) fn terminate_transactions_rows(
    graph: &ExecGraph<'_>,
    c: &relate_parser::TerminateTransactionsClause,
    rows: Vec<Row>,
    clause: &relate_parser::Clause,
    params: &BTreeMap<String, Value>,
) -> Result<Vec<Row>> {
    let exprs = c
        .ids
        .iter()
        .map(relate_front::lower_expr)
        .collect::<Result<Vec<_>>>()?;
    super::super::catalog::project_catalog_by_row(graph, rows, clause, params, |row| {
        let mut ids = Vec::new();
        for expr in &exprs {
            let value = eval(graph, row, expr, params)?;
            ids.extend(transaction_ids(&value)?.into_iter().map(|id| match id {
                Some(text) => reported_id(text),
                None => Value::Null,
            }));
        }
        // An argument that names no transaction at all is live's missing-id
        // rejection, not an empty successful result; a NULL id is one absent
        // id and stays a row.
        if ids.is_empty() {
            return Err(relate_core::ce_messages::missing_transaction_id());
        }
        Ok(crate::distinct_set::dedup_first_seen(ids)?
            .into_iter()
            .map(|id| {
                catalog_row([
                    ("transactionId", id),
                    ("username", Value::Null),
                    ("message", Value::String("Transaction not found.".into())),
                ])
            })
            .collect())
    })
}

#[cfg(test)]
mod tests {
    /// Pins the `YIELD *` column set and order: the self row carries keys in
    /// exactly the front table's order.
    #[test]
    fn show_transactions_row_key_order_matches_front_column_table() {
        crate::exec::test_support::assert_row_keys_match(
            None,
            "SHOW TRANSACTIONS YIELD *",
            &relate_front::show_columns::full_transaction_columns(),
        );
    }
}
