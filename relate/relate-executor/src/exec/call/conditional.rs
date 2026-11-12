//! CIP-135 conditional (`WHEN … THEN …`) branches: one bucket per branch,
//! the taken branch run on its driving row, and the row-shape fact the
//! NEXT-stage seeding reads.

use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::{Expr, Query};

use super::{execute_query_scoped, project_row};
use crate::ctx::QueryCtx;
use crate::eval::{bool_or_null, eval};
use crate::exec::scope::refresh_rows;
use crate::exec::{materialize_rows_strict, QueryResult};
use crate::overlay::ExecGraph;
use crate::row::Row;

/// Run the conditional over `rows` and return both the surviving rows — the
/// NEXT stage's seed — and the clause's finished statement result.
///
/// The result's column list is the clause's plan fact: every branch and
/// `ELSE` returns the same ordered names, so it holds for an empty result
/// exactly as for a non-empty one. The derivation is the planner's
/// [`relate_planner::conditional_declared_columns`], whose empty seed is
/// the same one the conditional's validation uses, so the executor's
/// materialization and the plan agree by construction.
pub(crate) fn exec_conditional(
    graph: &ExecGraph<'_>,
    rows: Vec<Row>,
    branches: &[(Expr, Query)],
    else_query: Option<&Query>,
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
) -> Result<(Vec<Row>, QueryResult)> {
    // One bucket per branch plus `ELSE`: the conditional's output is grouped
    // by branch in the branches' written order (Neo4j's observed order — see
    // the CIP-213 PE3.2 note), while each row is still driven on its own.
    let mut buckets: Vec<Vec<Row>> = vec![Vec::new(); branches.len() + 1];
    // CIP-135's conditional is a per-row operator: a driving row's branch
    // predicate is evaluated against the state its predecessors left (the
    // taken branch body ran already), so a branch that writes is visible to
    // the next row's `WHEN` and a branch aggregates one incoming row at a
    // time — the NEXT-stage scenarios' "sees updates made by the branch of
    // a previous row" shape.
    for mut row in rows {
        // The row's element bindings were captured by the previous stage;
        // a predecessor's branch may have relabelled or changed them since,
        // so the predicate and the body read them again from the graph.
        refresh_rows(graph, std::slice::from_mut(&mut row));
        let Some((index, query)) = taken_branch(graph, &row, branches, else_query, params)? else {
            continue;
        };
        // The driving row is the branch body's importing scope: its bindings
        // stay visible through the body's whole NEXT chain (CIP-213), exactly
        // as a `CALL (…) { … }` body's imports do
        // (`NextStatementAcceptance` "By-table semantics: CALL and WHEN no
        // special behavior"). No Rule 10 exclusion applies, so this is the
        // public scoped entry with no import set.
        let result = execute_query_scoped(graph, query, params, row, None, qctx)?;
        // A body that projects no statement result (a write-only / `FINISH`
        // / DDL body) contributes no rows: its surviving seed row is a
        // pass-through, never a result. The branch still ran, so its writes
        // stand.
        if body_projects_rows(query) {
            append_query_rows(&mut buckets[index], result);
        }
    }
    let rows: Vec<Row> = buckets.into_iter().flatten().collect();
    let columns = relate_planner::conditional_declared_columns(branches, else_query)?;
    // The shared clause-exit tail records this clause's PROFILE tick from
    // the returned rows, so the projection runs on a copy: moving the cells
    // out here would make the operator report the nulled slots'
    // `memory_bytes` instead of its output rows'.
    let mut projected = rows.clone();
    let data = materialize_rows_strict(&mut projected, &columns)?;
    Ok((
        rows,
        QueryResult::intermediate(columns, data, graph.stats()),
    ))
}

/// The row's one taken branch, in source order — its bucket index and
/// query. `None` when no predicate holds and there is no `ELSE`: nothing
/// runs for the row. The `ELSE` body's index is `branches.len()`, one past
/// the last `WHEN` bucket.
fn taken_branch<'q>(
    graph: &ExecGraph<'_>,
    row: &Row,
    branches: &'q [(Expr, Query)],
    else_query: Option<&'q Query>,
    params: &BTreeMap<String, Value>,
) -> Result<Option<(usize, &'q Query)>> {
    for (index, (predicate, query)) in branches.iter().enumerate() {
        let value = eval(graph, row, predicate, params)?;
        // A `WHEN` predicate is a boolean: the one requiring-site policy
        // (`eval::bool_or_null`) leaves the branch untaken on null/false and
        // rejects any other non-boolean value (Neo4j 22G03), never
        // truthy-coercing it.
        if let Some(true) = bool_or_null(&value, "WHEN predicate")? {
            return Ok(Some((index, query)));
        }
    }
    Ok(else_query.map(|query| (branches.len(), query)))
}

/// Whether a conditional branch body projects a statement result of its own
/// — the rows a taken branch contributes. A body concluding in a write-only
/// clause, a `FINISH`, schema DDL or a row-less catalog command has none
/// (`super::super::part_projects_rows`). A body that is itself a NEXT chain
/// is judged by its final stage.
fn body_projects_rows(query: &Query) -> bool {
    super::stage_projects_rows(&query.final_stage().parts)
}

/// Whether a conditional clause projects rows in any branch — the fact the
/// NEXT-stage seeding policy reads: a conditional whose branches are all
/// unit bodies is itself unit output, so an empty result still seeds the
/// next stage with one row (ADR-0287) instead of ending the chain.
pub(crate) fn conditional_produces_rows(
    branches: &[(Expr, Query)],
    else_query: Option<&Query>,
) -> bool {
    branches.iter().any(|(_, body)| body_projects_rows(body))
        || else_query.is_some_and(body_projects_rows)
}

pub(crate) fn append_query_rows(out: &mut Vec<Row>, result: QueryResult) {
    for values in result.rows {
        out.push(project_row(&result.columns, &values).collect());
    }
}
