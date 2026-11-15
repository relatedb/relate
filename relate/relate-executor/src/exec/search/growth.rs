//! The top-K growth policy both SEARCH scan shapes follow: when a window
//! widens, and when the growth loop is complete.

use relate_ir::SearchClause;

/// The top-K growth policy, the one spelling both search paths follow:
/// only a complex pattern widens its window (a plain pattern's LIMIT is a
/// hard candidate window), and only while a wider window can reach a
/// candidate the current one cannot — always true in the direct path
/// (the binding is seeded per candidate, so a pre-bound binding's
/// partner may sit arbitrarily deep), never true in the deferred path
/// once the binding is pre-bound (its rows already carry their only
/// possible candidate, so no wider window can add a survivor).
pub(super) fn top_k_grows(search: &SearchClause, wider_reaches_new_candidate: bool) -> bool {
    search.complex_pattern && wider_reaches_new_candidate
}

/// The top-K completion test, stated once for both paths: the growth
/// loop ends when the clause does not grow, the index cannot widen, or
/// enough candidates have a surviving row.
pub(super) fn top_k_complete(
    grows: bool,
    may_widen: bool,
    survivors: usize,
    needed: usize,
) -> bool {
    !grows || !may_widen || survivors >= needed
}

/// The next growth round's extra-candidate budget: geometric doubling
/// from `base` — which is at least 1, guaranteed by the caller's
/// zero-LIMIT break — so the window grows toward the index's depth in
/// logarithmic rounds.
pub(super) fn widen_extra(extra: usize, base: usize) -> usize {
    extra.saturating_add(extra).max(base)
}
