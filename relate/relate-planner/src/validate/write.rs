//! The 42I25 write-classification family: the per-clause answer
//! ([`clause_is_write`]), its unbatched refinement
//! ([`clause_is_unbatched_write`]), and the walk-owned facts the
//! composition rule threads — [`WriteHistory`] (the chain's accumulated
//! fact) and [`ClausePosition`] (one clause's position and the history
//! reaching it). Split from the clause-validation walk so the family has
//! one home beside the classification it refines.

use relate_ir::{Clause, Query};

/// Whether `clause` writes — the write-classification family's per-clause
/// answer. `is_write_procedure` classifies `CALL` targets: the canonical
/// registry-backed predicate, injected by the facade — the procedure
/// registry lives in the executor, which the planner cannot depend on.
/// Every write gate (the `IN TRANSACTIONS` composition rule, the
/// subquery-expression rejection, the §4 namespace gate in
/// `namespaces.rs`) classifies procedures through this one predicate, so
/// no literal procedure name is hardcoded here.
///
/// The clause half is the published per-clause answer
/// ([`crate::clause_mutates_store`]); this function only adds the answers
/// that live below the clause itself — a `CALL { … }` body, a `WHEN`/`ELSE`
/// body, a writes-flagged procedure — which is what the composition rules
/// need. (A nested body is walked here rather than classified outright: a
/// read-only `CALL { … }` body makes no write.) Module-private: its two
/// callers are this family's own folds; cross-module consumers
/// (the expression-body write refusal, the namespace gate) read
/// [`query_contains_write`].
fn clause_is_write(c: &Clause, is_write_procedure: &mut impl FnMut(&str) -> bool) -> bool {
    crate::clause_mutates_store(c)
        || matches!(c, Clause::CallSubquery { query, .. } if query_contains_write(query, is_write_procedure))
        || matches!(c, Clause::Call { name, .. }
            if name.with_str(|text| is_write_procedure(text)))
        || matches!(c, Clause::Conditional { branches, else_query }
            if branches.iter().any(|(_, query)| query_contains_write(query, is_write_procedure))
                || else_query.as_deref().is_some_and(|q| query_contains_write(q, is_write_procedure)))
}

pub(crate) fn query_contains_write(
    query: &Query,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
) -> bool {
    crate::query_any_clause(query, &mut |c| clause_is_write(c, is_write_procedure))
}

/// Whether `clause` writes outside `CALL { … } IN TRANSACTIONS`'s own
/// batches — the 42I25 refinement over [`clause_is_write`]: a batched
/// subquery's own writes run in its batches' own transactions, so they
/// never make their part (or stage) a writing one. The clause-validation
/// loop folds it into the 42I25 history each clause accumulates
/// ([`WriteHistory`]), so a batched subquery is refused after any earlier
/// unbatched write — in its own part's prefix or an earlier stage of the
/// statement's `NEXT` chain. The exclusion is the one canonical per-clause
/// reading ([`crate::clause_kind::clause_is_in_transactions`], as cheap as
/// the inline match it replaces), read before the classification so a
/// batched clause pays no recursive body walk.
fn clause_is_unbatched_write(
    clause: &Clause,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
) -> bool {
    !crate::clause_kind::clause_is_in_transactions(clause)
        && clause_is_write(clause, is_write_procedure)
}

/// The 42I25 write history of one `NEXT` chain: whether a write outside
/// `CALL { … } IN TRANSACTIONS`'s own batches precedes the point a
/// validation walk is at. A named fact at every boundary it crosses —
/// never a bare bool, and never `Scope` state: the clause loop owns the
/// running history ([`ClausePosition`]) and folds each clause's
/// unbatched writes into it, the stage driver returns its stage's answer
/// inside its stage validation, and the successor's stage seed carries
/// it — so a writing part precedes a later batched subquery exactly like
/// a write clause does: the chained statement's parts are one statement.
#[must_use = "fold the stage's write history into its successor's seed, or name the discard"]
#[derive(Clone, Copy, Debug)]
pub(crate) struct WriteHistory(bool);

impl WriteHistory {
    /// No unbatched write behind — a fresh statement's chain, and a
    /// nested body's own walk (a batched subquery inside any body is
    /// another rule's: the nested rejection, the expression-body write
    /// refusal, the FOREACH CALL refusal).
    pub(crate) fn empty() -> Self {
        Self(false)
    }

    /// The fold both chain drivers run: the incoming history, then the
    /// other side's answer.
    pub(crate) fn union(self, other: Self) -> Self {
        Self(self.0 || other.0)
    }

    /// The history after one more clause validates: this clause's
    /// unbatched write, if any, joins for every later clause's
    /// batched-subquery check.
    pub(crate) fn after(
        self,
        clause: &Clause,
        is_write_procedure: &mut impl FnMut(&str) -> bool,
    ) -> Self {
        Self(self.0 || clause_is_unbatched_write(clause, is_write_procedure))
    }

    /// Whether an unbatched write precedes — the one read the
    /// batched-subquery composition rule makes.
    pub(crate) fn unbatched_write_behind(self) -> bool {
        self.0
    }
}

/// One clause's position in the validation walk that checks it: the
/// part's clauses, the clause's index among them, and the 42I25 write
/// history accumulated to that point. The one value the position-reading
/// checks take — the `CALL { … }` subquery checker (its 42I25 rule and
/// `IN TRANSACTIONS` modifier battery) and the `USE` placement rule — so
/// position and history travel together and no checker re-derives the
/// other's half.
#[derive(Clone, Copy)]
pub(crate) struct ClausePosition<'a> {
    pub(crate) clauses: &'a [Clause],
    pub(crate) index: usize,
    pub(crate) write_history: WriteHistory,
}

impl ClausePosition<'_> {
    /// Whether this clause is its part's last *substantive* clause: no
    /// non-routing clause follows it (trailing routing `USE`s do not
    /// count) — the one terminality fact, consumed by the `RETURN`/
    /// `FINISH` terminal check and the `REPORT STATUS` terminality rule
    /// alike, so the two never disagree on a part's shape.
    pub(crate) fn is_terminal(&self) -> bool {
        self.clauses[self.index + 1..]
            .iter()
            .all(crate::clause_kind::clause_is_routing)
    }
}
