//! The FOREACH side-effect boundary — the one home of its policy, split
//! out of the dispatcher (the dispatch arm is a one-line delegation, like
//! the DDL arms).

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{Result, Value};
use relate_ir::{Clause, Expr};
use relate_names::SmallName;

use super::dispatch::run_nested_clauses;
use crate::ctx::QueryCtx;
use crate::eval::{eval, into_items};
use crate::overlay::ExecGraph;
use crate::row::Row;

/// Run a FOREACH clause over `rows`: the body is side effects, and its
/// produced rows are discarded (`run_nested_clauses`' result is dropped) —
/// a void CALL's rows and a conditional's merge are consumed by the
/// boundary by design.
///
/// The RETURN-inside-FOREACH composition is the *front end's* rule
/// (`validate_foreach_body`, the statically decidable home): the parser
/// admits the clause list, validation rejects the composition before
/// lowering, and this boundary does not re-scan the body. A caller that
/// bypasses validation therefore gets the body's RETURN executed and its
/// rows silently discarded — ownership, not a fail-closed backstop.
///
/// ADR-0395: each item's clause list is one nested clause-engine pass per
/// row, so a wide list compounds on the statement's budget like any other
/// nesting. FOREACH clauses are not the statement's operators: the nested
/// boundary is the explicit `None` site — no PROFILE facts cross it, and
/// no produced rows or statement result crosses it either.
pub(crate) fn exec_foreach(
    graph: &ExecGraph<'_>,
    rows: &[Row],
    variable: &SmallName,
    list: &Expr,
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
) -> Result<()> {
    for row in rows {
        // Neo4j treats a non-list expression as a one-element list (the
        // harness pins `FOREACH(v IN 1 | …)` and `FOREACH(v IN <boolean
        // expression> | …)`); only null iterates zero times.
        let values = match eval(graph, row, list, params)? {
            Value::List(values) => values,
            Value::Null => continue,
            single => Arc::new(vec![single]),
        };
        for value in into_items(values) {
            let mut inner = row.clone();
            inner.insert(variable.to_string(), value);
            let _ = run_nested_clauses(graph, clauses, params, vec![inner], qctx)?;
        }
    }
    Ok(())
}
