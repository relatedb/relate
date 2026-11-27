//! The `CALL { … } IN TRANSACTIONS` body's row-production strategy
//! (ADR-0669): the statement-level admission [`batch_pass_admission`] and
//! the two producers it chooses between — the batch pass
//! ([`pass_batch_rows`], one clause-engine crossing over the whole batch)
//! and the per-row path ([`per_row_batch_rows`]). Split out of
//! `in_transactions.rs`, which keeps the arm's streaming loop, its
//! charging invariant, the option resolution and the error/status
//! rendering.

use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::{Clause, Query};

use super::{execute_query_seeded, row_product, scope_seed, SubquerySpec};
use crate::ctx::QueryCtx;
use crate::exec::call::in_transactions::ChargedRows;
use crate::exec::row_production::body_projects_rows;
use crate::overlay::ExecGraph;
use crate::row::Row;

/// Whether an `IN TRANSACTIONS` body takes the batch pass (ADR-0669): a
/// single flat part, no `NEXT` stage, non-projecting, with no `WITH` and
/// no schema/admin DDL among its own clauses. Everything else about the
/// body runs in the pass exactly as its clause list would in a plain
/// statement over the batch's seed rows — `MATCH`, procedure `CALL`,
/// nested `CALL { … }`, conditionals, graph-reading expressions
/// included: pipeline order, one clause-engine invocation and one
/// nested-budget tick per batch, the per-row statement-engine loop
/// never entered. The batch commits per batch either way (ADR-0210);
/// what the pass removes is the per-row invocation, which is pure
/// overhead — the caller already named the batch.
///
/// The exclusions are structural, not semantic preferences:
/// - a `WITH`-carrying body needs the importing-scope re-attach
///   (`run_nested_clauses_scoped`), which re-attaches one scope row per
///   nested invocation — per-row by construction. The legacy
///   importing-`WITH` form keeps the per-row path with it;
/// - a projecting body (a trailing clause the canonical table says
///   spills rows — [`body_projects_rows`], the same classification the
///   per-row merge consults, never inferred from the clause set) merges
///   each body row back onto its originating outer row, a lineage the
///   pass does not track. A terminal `LOAD CSV` projects and stays
///   per-row with it.
/// - schema/admin DDL has no per-row work to batch and stays per-row.
///
/// The pass gives up Neo4j's per-row read-after-write interleaving — the
/// body evaluates the batch as a unit. That decision, its within-clause
/// coincidence and the smaller-batch remedy are recorded once in ADR-0669.
pub fn batch_pass_admission(query: &Query) -> bool {
    query.is_single_flat_part()
        && !body_projects_rows(query)
        && !relate_planner::query_any_clause(query, &mut |clause| {
            matches!(clause, Clause::With { .. }) || relate_planner::clause_is_ddl_command(clause)
        })
}

/// The batch pass (ADR-0669): the whole batch's scope seeds — the same
/// per-row [`scope_seed`] policy — in one [`execute_query_seeded`]
/// crossing, the batch's one nested-budget tick. A non-projecting
/// body's pass-through is the outer rows (the batch twin of the
/// per-row path's `outer.clone()`), so the crossing's own output is
/// discarded. Rollback stays with the caller's per-batch snapshot.
pub(super) fn pass_batch_rows(
    graph: &ExecGraph<'_>,
    batch: &[Row],
    spec: &SubquerySpec<'_>,
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
    out: &mut ChargedRows,
) -> Result<()> {
    let mut seeds = ChargedRows::from_rows(
        batch
            .iter()
            .map(|row| scope_seed(graph, spec.query, spec.scope, row))
            .collect::<Result<Vec<_>>>()?,
    )?;
    // The seeds are a second copy of the batch, live for the crossing's
    // lifetime: `seeds`'s own charge keeps them billed (it drops with the
    // struct) while the body runs, so the ledger sees both copies. The
    // crossing's own output is discarded — a non-projecting body's
    // pass-through is the outer rows below.
    execute_query_seeded(graph, spec.query, params, seeds.take_rows(), qctx)?;
    out.extend(batch.iter().cloned())
}

/// The per-row path, for the bodies the pass cannot take (a
/// `WITH`-carrying body, a projecting body, schema/admin DDL): one
/// nested statement-engine invocation per row through
/// [`row_product`], the `OPTIONAL CALL` empty-body pad included.
pub(super) fn per_row_batch_rows(
    graph: &ExecGraph<'_>,
    batch: &[Row],
    spec: &SubquerySpec<'_>,
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
    out: &mut ChargedRows,
) -> Result<()> {
    for row in batch {
        out.extend(row_product(
            graph,
            row,
            spec.optional,
            spec.query,
            spec.scope,
            params,
            qctx,
        )?)?;
    }
    Ok(())
}
