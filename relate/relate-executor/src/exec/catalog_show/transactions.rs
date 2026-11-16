//! `SHOW TRANSACTIONS` and `TERMINATE TRANSACTIONS` row producers.

use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};

use crate::ctx::QueryCtx;
use crate::eval::eval;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::super::catalog::catalog_row;

/// The ids one id argument yields, under the whole argument contract both
/// transaction commands share: a list's elements, or the scalar itself as a
/// one-element slice, each of which must be a STRING or NULL. Anything else
/// is the argument's own type error — live raises it for `SHOW TRANSACTIONS
/// 123` and `TERMINATE TRANSACTIONS 123` alike, so neither command may
/// quietly filter such a value out or fold it onto the NULL id.
///
/// Borrowed, so neither a scalar nor a list argument is copied to be
/// checked.
fn transaction_ids(value: &Value) -> Result<&[Value]> {
    let items: &[Value] = match value {
        Value::List(items) => items,
        scalar => std::slice::from_ref(scalar),
    };
    for item in items {
        if !matches!(item, Value::String(_) | Value::Null) {
            return Err(Error::invalid_argument_type(
                "transaction id must be STRING",
            ));
        }
    }
    Ok(items)
}

pub(crate) fn show_transactions_rows(
    graph: &ExecGraph<'_>,
    c: &relate_parser::ShowTransactionsClause,
    rows: &[Row],
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
) -> Result<Vec<Row>> {
    let txid = "neo4j-transaction-1".to_string();
    let mut found = c.ids.is_empty();
    for expr in &c.ids {
        let expr = relate_front::lower_expr(expr)?;
        for row in rows {
            let value = eval(graph, row, &expr, params)?;
            found |= transaction_ids(&value)?
                .iter()
                .any(|v| matches!(v, Value::String(s) if **s == *txid));
        }
    }
    Ok(if found {
        vec![catalog_row([
            ("transactionId", Value::String(txid.into())),
            ("currentQuery", Value::String(qctx.query_text.into())),
            ("database", Value::String("neo4j".into())),
        ])]
    } else {
        Vec::new()
    })
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
            ids.extend(transaction_ids(&value)?.iter().cloned());
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
