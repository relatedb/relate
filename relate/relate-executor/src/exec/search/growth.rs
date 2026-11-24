//! The top-K growth policy both SEARCH scan shapes follow: when the
//! growth loop is complete, and how the next round's window widens.
//!
//! Whether a wider fetched window can reach a *new* candidate — the
//! growth precondition — is not policy but a fact of each scan shape,
//! stated at its call site: both paths always grow (a candidate — the
//! binding a row names, or a pattern row's own binding — may sit
//! arbitrarily deep in the ranking, so a deeper fetched entry is always
//! a new possible survivor); termination is the index's (`may_widen`),
//! never the shape's.
//!
//! The SKIP/LIMIT window pages over entities the whole MATCH statement
//! keeps (the pattern, the outer WHERE, a pre-bound binding), so both
//! loops widen while enough survivors have not been seen — for every
//! pattern shape alike (ADR-0678).

/// The top-K completion test, stated once for both paths: the loop ends
/// when the index cannot widen or enough candidates have a surviving
/// row. Both scans always grow (a candidate may sit arbitrarily deep in
/// the ranking), so no caller states a grows guard beside the test.
pub(super) fn top_k_complete(may_widen: bool, survivors: usize, needed: usize) -> bool {
    !may_widen || survivors >= needed
}

/// The next growth round's extra-candidate budget: geometric doubling
/// from `base`, clamped to at least 1 — the guarantee is this helper's
/// own, so no caller compensates with its own floor — so the window
/// grows toward the index's depth in logarithmic rounds.
pub(super) fn widen_extra(extra: usize, base: usize) -> usize {
    extra.saturating_add(extra).max(base.max(1))
}
