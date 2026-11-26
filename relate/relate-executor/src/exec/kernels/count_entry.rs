//! The count folds' shared entry (REL8-2214): the `MATCH … RETURN count(…)`
//! gates, the chain compile and the residual-plus-scan setup that the
//! chain-count fold and the two-hop tally fold both run before their own
//! tally. Each fold keeps only its contract-specific gates and its tally
//! step, so a change to the MATCH-shape gate or the residual wiring is made
//! once.

use std::borrow::Cow;
use std::collections::BTreeMap;

use relate_core::{NodeId, Result, Value};
use relate_ir::{Clause, Expr, MatchMode, PathPattern, Projection};

use crate::ctx::ExecCtx;
use crate::metrics::WalkStart;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::super::graph_stats::GraphStats;
use super::super::match_folds::FoldCtx;
use super::super::pushdown::{flatten_and_owned, match_scope, Residual};
use super::matcher::{self, Chain, Contract};
use super::{counted_return, kernels_stood_down, record_walk_start, scan_segment};

/// A clause the count folds may take: a plain default-mode `MATCH` of one
/// pattern entered with exactly one row, followed by a counted `RETURN`.
pub(super) struct CountEntry<'a> {
    graph: &'a ExecGraph<'a>,
    params: &'a BTreeMap<String, Value>,
    entering: &'a Row,
    /// The `RETURN`'s one count item.
    pub(super) item: &'a Projection,
    conjuncts: Vec<Expr>,
    /// The patterns with an exact `*k` spelling unrolled onto fixed hops;
    /// the unrolled pattern lives as long as the compile.
    compiled: Cow<'a, [PathPattern]>,
}

impl<'a> CountEntry<'a> {
    /// The entry gates: `clauses` starts at the `MATCH`, its `RETURN`
    /// follows. `None` is a decline — the caller keeps the unchanged plan.
    pub(super) fn open(ctx: &FoldCtx<'a>) -> Option<Self> {
        let Some(Clause::Match {
            optional: false,
            match_mode: MatchMode::DifferentRelationships,
            patterns,
            where_clause,
            search: None,
            using_index: _,
            using_join: _,
        }) = ctx.clauses.first()
        else {
            return None;
        };
        let [entering] = ctx.rows else {
            return None;
        };
        let [pattern] = patterns.as_slice() else {
            return None;
        };
        if kernels_stood_down(ctx.graph.base()) {
            return None;
        }
        let item = counted_return(ctx.clauses.get(1), pattern)?;
        let conjuncts = where_clause
            .as_ref()
            .map(flatten_and_owned)
            .unwrap_or_default();
        Some(Self {
            graph: ctx.graph,
            params: ctx.params,
            entering,
            item,
            conjuncts,
            // The spelling gate lives in the matcher, beside the
            // plain-pattern proof it shares facts with.
            compiled: matcher::chain_spelling(patterns),
        })
    }

    /// The hops of the (unrolled) pattern: the cheap structural fact a fold
    /// checks before it pays for the contract compile.
    pub(super) fn hop_count(&self) -> usize {
        // `open` proved exactly one pattern, and the chain spelling keeps it
        // exactly one.
        let [path] = &*self.compiled else {
            unreachable!("CountEntry::open admits exactly one pattern")
        };
        path.rels.len()
    }

    /// The compiled chain contract. `None` is a decline by design: any other
    /// contract family (the hub-star) or a pattern that does not compile has
    /// its own entry, and the count folds take chains only.
    pub(super) fn chain(&self) -> Option<Box<Chain<'_>>> {
        match matcher::compile(
            &self.compiled,
            MatchMode::DifferentRelationships,
            self.entering,
            &self.conjuncts,
        ) {
            Some(Contract::Chain(chain)) => Some(chain),
            _ => None,
        }
    }

    /// The flattened `WHERE` conjuncts, for the cost gate's estimate.
    pub(super) fn conjuncts(&self) -> &[Expr] {
        &self.conjuncts
    }

    /// The planner's statistics over this clause's graph and parameters.
    pub(super) fn stats(&self) -> GraphStats<'_> {
        GraphStats::new(self.graph.base(), self.params)
    }

    /// Engage: count the engagement (tests), record the walk start and scan
    /// the admitted start candidates. Called once every gate has passed.
    pub(super) fn scan(&self, chain: &Chain<'_>) -> Result<(ExecCtx<'a>, Vec<NodeId>)> {
        #[cfg(test)]
        super::kernel_engaged();
        let ctx = ExecCtx::new(self.graph, self.params);
        let residual = Residual {
            conjuncts: &self.conjuncts,
            scope: &match_scope(self.graph, self.entering, &self.compiled),
        };
        record_walk_start(WalkStart::seed_side_forward());
        let starts = scan_segment(ctx, &chain.start, &residual)?;
        Ok((ctx, starts))
    }
}
