//! The branch-set model of a `WHEN … THEN … / ELSE` clause: the branches
//! plus the `ELSE` half riding as one value, with the bucket/index space
//! its consumers read. A pure IR-shape model — the row-production policy
//! over it lives in the sibling `row_production` ladder
//! (`body_projects_rows` and its folds), which imports the model downward
//! (REL8-2370's review round).

use relate_ir::{Expr, Query};

/// The one branch-set model of a `WHEN … THEN … / ELSE` clause, so no
/// consumer re-derives "branches plus ELSE" at its own call site — the
/// model owns one enumeration ([`bodies`](Self::bodies)) and the
/// index-space accessors ([`else_index`](Self::else_index) /
/// [`bucket_count`](Self::bucket_count)); the planner bridge and the
/// predicate walk take [`raw`](Self::raw). The row-production policy
/// over the branch set lives in the ladder (`row_production`), never as
/// model methods.
#[derive(Clone, Copy)]
pub(in crate::exec) struct ConditionalBranches<'q> {
    branches: &'q [(Expr, Query)],
    else_query: Option<&'q Query>,
}

impl<'q> ConditionalBranches<'q> {
    pub(in crate::exec) fn new(
        branches: &'q [(Expr, Query)],
        else_query: Option<&'q Query>,
    ) -> Self {
        Self {
            branches,
            else_query,
        }
    }

    /// The one traversal of the branch set: every `WHEN` body in source
    /// order as `Some(body)`, the `ELSE` body last — `None` when absent,
    /// the one explicit `ELSE` marker. The bucket/index space is not
    /// carried here: [`else_index`](Self::else_index) and
    /// [`bucket_count`](Self::bucket_count) are the only places it is
    /// read.
    pub(in crate::exec) fn bodies(&self) -> impl Iterator<Item = Option<&'q Query>> {
        let Self {
            branches,
            else_query,
        } = *self;
        branches
            .iter()
            .map(|(_, body)| Some(body))
            .chain(std::iter::once(else_query))
    }

    /// The bucket/index space over the branch set: one slot per `WHEN`
    /// branch plus the `ELSE` slot, whether or not an `ELSE` is present.
    /// The one home of that arithmetic, so no consumer copies it from the
    /// raw halves.
    pub(in crate::exec) fn bucket_count(&self) -> usize {
        self.else_index() + 1
    }

    /// The `ELSE` body's bucket index: one past the last `WHEN` slot —
    /// the single spelling of the index space every bucket consumer
    /// (sizing, the taken-branch walk) reads.
    pub(in crate::exec) fn else_index(&self) -> usize {
        self.branches.len()
    }

    /// The raw halves, for the planner's own derivations
    /// ([`relate_planner::conditional_declared_columns`]) and the
    /// predicate walk, which still take them as a pair.
    pub(in crate::exec) fn raw(&self) -> (&'q [(Expr, Query)], Option<&'q Query>) {
        (self.branches, self.else_query)
    }
}
