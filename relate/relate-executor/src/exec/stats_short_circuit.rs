//! The statistics short-circuits, tried in order behind one eligibility
//! gate: the count-only operator (LABS-370), then the grouped label
//! aggregates over the label-set counts (ADR-0589). The dependency runs
//! one way: this module reads both answerers, neither reads it.

use relate_core::Result;
use relate_ir::Clause;

use crate::ctx::ExecCtx;
use crate::row::Row;

use super::count_stats::{count_only_stats_result, stats_view_serves};
use super::label_set_groups::label_set_groups_result;
use super::QueryResult;

/// `Ok(None)` = neither took the statement (or the view or seed is not
/// eligible, [`stats_view_serves`], checked once here), the clause loop's
/// scan answers.
pub(super) fn stats_short_circuit(
    ctx: ExecCtx<'_>,
    clauses: &[Clause],
    seed_rows: &[Row],
    scope: Option<&Row>,
) -> Result<Option<(Vec<Row>, QueryResult)>> {
    if !stats_view_serves(ctx.graph, seed_rows) {
        return Ok(None);
    }
    if let Some(answer) = count_only_stats_result(ctx.graph, clauses) {
        return Ok(Some(answer));
    }
    label_set_groups_result(ctx, clauses, scope)
}
