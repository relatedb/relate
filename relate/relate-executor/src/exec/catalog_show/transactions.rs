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

pub(crate) fn terminate_transactions_rows(
    graph: &ExecGraph<'_>,
    c: &relate_parser::TerminateTransactionsClause,
    rows: &[Row],
    params: &BTreeMap<String, Value>,
) -> Result<Vec<Row>> {
    Ok({
        let mut out = Vec::new();
        for expr in &c.ids {
            let expr = relate_front::lower_expr(expr)?;
            for outer in rows {
                let values = match eval(graph, outer, &expr, params)? {
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
        }
        out
    })
}
