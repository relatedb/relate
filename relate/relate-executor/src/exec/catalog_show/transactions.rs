//! `SHOW TRANSACTIONS` and `TERMINATE TRANSACTIONS` row producers.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{Result, Value};

use crate::ctx::QueryCtx;
use crate::eval::eval;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::super::catalog::catalog_row;

pub(crate) fn show_transactions_rows(
    graph: &ExecGraph<'_>,
    c: &relate_parser::ShowTransactionsClause,
    rows: &[Row],
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
) -> Result<Vec<Row>> {
    Ok({
        let txid = "neo4j-transaction-1".to_string();
        let mut found = c.ids.is_empty();
        for expr in &c.ids {
            let expr = relate_front::lower_expr(expr)?;
            for row in rows {
                let value = eval(graph, row, &expr, params)?;
                found |= match value {
                    Value::String(ref s) => **s == *txid,
                    Value::List(ref xs) => xs
                        .iter()
                        .any(|v| matches!(v, Value::String(s) if **s == *txid)),
                    _ => false,
                };
            }
        }
        if found {
            vec![catalog_row([
                ("transactionId", Value::String(txid.into())),
                ("currentQuery", Value::String(qctx.query_text.into())),
                ("database", Value::String("neo4j".into())),
            ])]
        } else {
            Vec::new()
        }
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
        let mut out = Vec::new();
        for expr in &exprs {
            let values = match eval(graph, row, expr, params)? {
                Value::List(v) => v,
                v => Arc::new(vec![v]),
            };
            for value in values.iter() {
                let id = match value {
                    Value::String(_) => value.clone(),
                    _ => Value::Null,
                };
                if !out
                    .iter()
                    .any(|r: &Row| r.get("transactionId") == Some(&id))
                {
                    out.push(catalog_row([
                        ("transactionId", id),
                        ("username", Value::Null),
                        ("message", Value::String("Transaction not found.".into())),
                    ]));
                }
            }
        }
        Ok(out)
    })
}
