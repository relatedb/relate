//! The chain kernel's count fold (LABS-1293): `MATCH (a …)-[:R]->()-[:R]->()-[:R]->(b …)
//! RETURN count(*)` (or `count(<pattern var>)`) answered by the unrolled
//! chain with the last hop tallied, never landed. Default match mode only;
//! every decline keeps the unchanged plan.

use relate_core::Result;
use relate_ir::{Clause, Direction, MatchMode};

use crate::ctx::{ExecCtx, WalkStart};

use super::super::agg_fold::FoldedReturn;
use super::super::graph_stats::GraphStats;
use super::super::match_folds::FoldCtx;
use super::super::pushdown::{flatten_and_owned, match_scope, Residual};
use super::counted_return;
use super::matcher::{self, Contract};
use super::{chain, chain_stages_fit, kernels_stood_down, record_walk_start, scan_segment};

/// The fold entry: `clauses` starts at the `MATCH`, its `RETURN` follows.
pub(crate) fn try_fold_chain_count(ctx: &FoldCtx<'_>) -> Result<Option<FoldedReturn>> {
    let (graph, rows, clauses, params) = (&ctx.graph, ctx.rows, ctx.clauses, ctx.params);
    let Some(Clause::Match {
        optional: false,
        match_mode: MatchMode::DifferentRelationships,
        patterns,
        where_clause,
        search: None,
        using_index: _,
        using_join: _,
    }) = clauses.first()
    else {
        return Ok(None);
    };
    let [entering] = rows else {
        return Ok(None);
    };
    let [pattern] = patterns.as_slice() else {
        return Ok(None);
    };
    if kernels_stood_down(graph.base()) {
        return Ok(None);
    }
    let Some(item) = counted_return(clauses.get(1), pattern) else {
        return Ok(None);
    };
    let conjuncts = where_clause
        .as_ref()
        .map(flatten_and_owned)
        .unwrap_or_default();
    // `*k` spellings unroll virtually onto the fixed-hop chain; the
    // unrolled pattern lives for the compile. The spelling gate lives in
    // the matcher, beside the plain-pattern proof it shares facts with.
    let unrolled = matcher::unroll_fixed(pattern);
    let compiled = unrolled
        .as_ref()
        .map(std::slice::from_ref)
        .unwrap_or(patterns.as_slice());
    let Some(Contract::Chain(contract)) = matcher::compile(
        compiled,
        MatchMode::DifferentRelationships,
        entering,
        &conjuncts,
    ) else {
        return Ok(None);
    };
    // Undirected hops keep the unchanged plan for now: their trail
    // corrections (a rel met from both ends) are the walker's.
    if contract
        .hops
        .iter()
        .any(|hop| hop.rel.direction == Direction::Both)
    {
        return Ok(None);
    }
    let stats = GraphStats::new(graph.base(), params);
    // The cost gate prices the hops the walk lands; the tally hop is the
    // one it never lands — `Chain::split_last_hop`'s one spelling of the
    // compile's hop invariant, shared with `chain::run`.
    let (tallied, landed) = contract.split_last_hop();
    if !chain_stages_fit(&stats, &conjuncts, &contract, landed) {
        return Ok(None);
    }
    #[cfg(test)]
    super::kernel_engaged();
    let ctx = ExecCtx::new(graph, params);
    let residual = Residual {
        conjuncts: &conjuncts,
        scope: &match_scope(entering, compiled),
    };
    record_walk_start(WalkStart::seed_side_forward());
    let starts = scan_segment(ctx, &contract.start, &residual)?;
    let count = chain::count(ctx, tallied, landed, starts)?;
    Ok(super::count_fold_tail(graph, u128::from(count), item))
}
