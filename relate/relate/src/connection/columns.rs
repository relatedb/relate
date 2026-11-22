//! The result-header naming policy: the authored-column restore for
//! auto-parameterized results (ADR-0663) — the one unit that re-spells
//! columns the compiled plan named as auto-extraction placeholders,
//! reading the authored spelling through the front's parse under the
//! admission's own preview options. The structural remainder — carrying
//! the authored spelling through the normalization so the re-parse
//! disappears entirely — is REL8-2009.

use relate_planner::LogicalPlan;

/// Result columns of an auto-parameterized plan name the placeholder
/// (`$__AUTO_INT_0__`). The statement's own return items already carry
/// the column text (`1 + 2`, `(1 + 2)`), so use those instead of the
/// rewritten expression.
///
/// The re-parse is a two-part trigger, aligned with the cache-hit inline
/// route's containment rule (ADR-0663) in the direction that matters:
/// the plan's own auto-parameter conjunct (the same predicate the rule's
/// third conjunct reads) plus a column that actually carries the
/// placeholder. A plan this restore re-parses is therefore always a plan
/// the route's conjunct declines (the column guard only narrows
/// further); a plan declined for expression depth or a NEXT chain is
/// never re-parsed, and a user-authored column carrying the prefix in
/// ordinary text is not either — the plan conjunct refuses it.
///
/// The parse runs under the store's preview-derived options — the same
/// derivation, from the admission's own preview copy, that the
/// statement's admission parsed with — so an admitted statement always
/// re-parses: the same parser, the same grammar, the same text. The
/// projection shape is the parser's one model (`Statement::projection_shape`):
/// plain items — a trailing `RETURN`, a UNION head's own arm, a sole
/// conditional's representative body — restore their authored names;
/// every other shape (`RETURN *`, a trailing column-exporting `CALL`,
/// no result) keeps the plan's spelling — the named degradation.
pub(super) fn restore_authored_column_names(
    logical: &LogicalPlan,
    cypher: &str,
    columns: &mut [String],
    preview: relate_core::PreviewFeatures,
) {
    if !columns
        .iter()
        .any(|column| relate_parser::column_carries_auto_parameter(column))
    {
        return;
    }
    if !relate_planner::plan_references_auto_parameter(logical) {
        return;
    }
    // The same preview-derived options the admission's parse used, via
    // the front's one derivation — never the parser's floor — so this
    // cannot fail for a statement that admitted: the invariant stated,
    // not a handled failure (a silent return would leave a placeholder
    // in the user-visible columns with nothing recorded).
    let names = relate_front::authored_column_names(cypher, preview)
        .expect("an admitted statement re-parses under its own preview options");
    let Some(names) = names else {
        return;
    };
    if names.len() != columns.len() {
        return;
    }
    for (column, authored) in columns.iter_mut().zip(names) {
        // The one decline: a column that does not carry the placeholder
        // keeps its spelling — the names themselves are total (a parsed
        // item always has one).
        if relate_parser::column_carries_auto_parameter(column) {
            *column = authored;
        }
    }
}
