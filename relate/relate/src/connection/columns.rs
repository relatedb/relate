//! The result-header naming policy: the authored-column restore for
//! auto-parameterized results — the one unit that re-spells columns the
//! compiled plan named as auto-extraction placeholders, substituting each
//! placeholder with its authored spelling (ADR-0718). The spelling pairs
//! ride the normalization's own channels, so the naming path runs no
//! parse; the trigger conjuncts and the substitution's own safety rules
//! live with the one substitution, `relate_parser::substitute_auto_parameters`.

use relate_planner::LogicalPlan;

/// Result columns of an auto-parameterized plan name the placeholder
/// (`$__AUTO_INT_0__`). The authored spelling is the substitution's
/// answer (ADR-0718): each placeholder in the column text becomes the
/// source text the normalization extracted it from (`RETURN 1` stays
/// `1`).
///
/// The trigger, aligned with the cache-hit inline route's containment
/// rule (ADR-0663) in the direction that matters: the plan's own
/// auto-parameter conjunct (the same predicate the rule's third
/// conjunct reads) plus a column that actually carries the placeholder.
/// A plan this restore touches is therefore always a plan the route's
/// conjunct declines (the column guard only narrows further); a plan
/// declined for expression depth or a NEXT chain is never touched, and
/// a user-authored column carrying the prefix in ordinary text is not
/// either — the plan conjunct refuses it. `spellings` is empty on
/// every original-text compile (such a plan never carries a
/// placeholder), and an empty pair list substitutes nothing.
pub(super) fn restore_authored_column_names(
    logical: &LogicalPlan,
    columns: &mut [String],
    spellings: &[relate_parser::AuthoredSpelling],
) {
    if !relate_planner::plan_references_auto_parameter(logical) {
        return;
    }
    for column in columns.iter_mut() {
        // The one decline: a column that does not carry the placeholder
        // keeps its spelling — the per-column guard is the policy's one
        // spelling (no whole-slice pre-scan: it tested exactly this
        // predicate again).
        if relate_parser::column_carries_auto_parameter(column) {
            *column = relate_parser::substitute_auto_parameters(column, spellings);
        }
    }
}

/// The authored spellings a streamed statement announces in place of the
/// plan's placeholder names (ADR-0663, ADR-0682): the restore above
/// applied to the planned output names, one entry per planned column
/// (`Some` where the spelling changed). `None` when no column is
/// renamed — the streamed `Columns` event and the result header then
/// spell the same names.
pub(super) fn authored_renames(
    logical: &LogicalPlan,
    spellings: &[relate_parser::AuthoredSpelling],
) -> Option<Vec<Option<String>>> {
    // The plan-wide conjunct runs ONCE (it is a full plan expression
    // walk); the per-column policy — carries a placeholder, substitute —
    // is then the only work per column. One pass, no clone-and-compare
    // wrapper, no per-column re-walk of the plan.
    if !relate_planner::plan_references_auto_parameter(logical) {
        return None;
    }
    let renames: Vec<Option<String>> = relate_planner::plan_output_names_expect(logical)
        .into_iter()
        .map(|original| {
            relate_parser::column_carries_auto_parameter(&original)
                .then(|| relate_parser::substitute_auto_parameters(&original, spellings))
                .filter(|renamed| *renamed != original)
        })
        .collect();
    renames.iter().any(Option::is_some).then_some(renames)
}
