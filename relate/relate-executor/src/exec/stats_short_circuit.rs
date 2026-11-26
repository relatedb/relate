//! The statistics short-circuits, tried in order behind one eligibility
//! gate: the count-only operator (LABS-370), the grouped label aggregates
//! over the label-set counts (ADR-0589), then the grouped and extremum
//! aggregates over one labeled property's index postings (ADR-0611). The
//! dependency runs one way: this module reads the answerers, none of them
//! reads it — the dispatcher knows one seam, never the rungs.

use relate_core::Result;
use relate_ir::Clause;

use crate::ctx::ExecCtx;
use crate::row::Row;

use super::count_stats::count_only_stats_result;
use super::index_aggregates::index_aggregates_result;
use super::label_set_groups::label_set_groups_result;
use super::seeded_count::seeded_count_result;
use super::QueryResult;

/// The eligibility prologue every statistics short-circuit shares.
///
/// The exactness precondition: the base stats are only exact for a view
/// of the latest published state with no overlay — anything else
/// (in-tx scratch, pinned snapshot, in-flight apply) declines to the
/// scan, which reads its own writes (the overlay's predicate, ADR-0387
/// follow-up). A check that passes here can go stale (a writer opens
/// after it) — the windowed reads re-check under the stats lock and
/// close that gap; a check that fails is stable (a finished writer
/// advances `commit_ts`, so the pinned-equality never turns true again
/// for this view).
///
/// Round 3, F-1-3: the short-circuits answer a whole-population
/// statement — one unbound seed row, the statement's opening stage.
/// Anything else (zero rows, N > 1 rows, or a bound row) is a seeded
/// stage whose answer scales with the seeds — decline to the scan.
pub(super) fn stats_view_serves(graph: &crate::overlay::ExecGraph<'_>, seed_rows: &[Row]) -> bool {
    graph.sees_exact_base_stats() && matches!(SeedShape::of(seed_rows), SeedShape::Whole)
}

/// The shape of the rows a clause list starts from, classified once: the
/// whole-population statement (one unbound row), one bound row (a
/// `CALL (x) { … }` body invocation, ADR-0689), or anything else — a
/// seeded stage whose answer scales with its rows, which the statistics
/// never answer.
pub(super) enum SeedShape<'a> {
    Whole,
    Bound(&'a Row),
    Other,
}

impl<'a> SeedShape<'a> {
    pub(super) fn of(seed_rows: &'a [Row]) -> Self {
        match seed_rows {
            [seed] if seed.is_empty() => SeedShape::Whole,
            [seed] => SeedShape::Bound(seed),
            _ => SeedShape::Other,
        }
    }
}

/// `Ok(None)` = no rung took the statement (or the view or seed is not
/// eligible, [`stats_view_serves`], checked once here), the clause loop's
/// scan answers.
pub(super) fn stats_short_circuit(
    ctx: ExecCtx<'_>,
    clauses: &[Clause],
    seed_rows: &[Row],
    scope: Option<&Row>,
) -> Result<Option<(Vec<Row>, QueryResult)>> {
    // ADR-0689: one bound seed row (a `CALL (x) { … }` body invocation)
    // answers only the count-only rung (its dynamic names were resolved
    // before the call, `resolve_seed_names`).
    if let SeedShape::Bound(seed) = SeedShape::of(seed_rows) {
        return if ctx.graph.sees_exact_base_stats() {
            seeded_count_result(ctx, clauses, seed)
        } else {
            Ok(None)
        };
    }
    if !stats_view_serves(ctx.graph, seed_rows) {
        return Ok(None);
    }
    if let Some(answer) = count_only_stats_result(ctx.graph, clauses)? {
        return Ok(Some(answer));
    }
    if let Some(answer) = label_set_groups_result(ctx, clauses, scope)? {
        return Ok(Some(answer));
    }
    index_aggregates_result(ctx, clauses, scope)
}
