//! The MATCH-plus-aggregate fold family: the rungs in [`ANY_MODE_RUNGS`]
//! and [`DEFAULT_SINGLE_HOP_RUNGS`], in order, answering whether a fold
//! answers a plain `MATCH` together with the clauses after it. The
//! dispatcher asks over the real entering rows before it opens the
//! streaming chain; the chain gate (`match_chain`) asks the zero-rows
//! form ([`fold_would_take`]) before a mid-pipeline `MATCH` becomes a
//! streaming stage.

use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::{Clause, Expr, MatchMode, PathPattern};

use crate::overlay::ExecGraph;
use crate::row::Row;

use super::agg_fold::{try_fold_one_hop_aggs, try_fold_path_aggs, FoldedReturn};
use super::exists_probe::try_fold_boolean_count;
use super::kernels::frontier_count::try_fold_walk_count;

/// One fold rung over the shared context — the signature every rung
/// carries, so the ladder's tables name the rungs themselves, never an
/// adapter.
type Rung = fn(&FoldCtx<'_>) -> Result<Option<FoldedReturn>>;

/// The rungs that answer every match mode, in order: the boolean-count
/// probe, then the walk-count kernel (a bounded REPEATABLE ELEMENTS
/// quantified pattern under a plain `count`).
const ANY_MODE_RUNGS: [Rung; 2] = [try_fold_boolean_count, try_fold_walk_count];

/// The default-mode rungs, in order — the path fold first (it compiles
/// the full hop plan once and serves the widest coverage), the chain
/// count and one-hop folds after, each serving exactly the queries the
/// path fold declined. No coverage gate between the rungs, so no query
/// compiles the plan twice.
const DEFAULT_SINGLE_HOP_RUNGS: [Rung; 3] = [
    try_fold_path_aggs,
    super::kernels::try_fold_chain_count,
    try_fold_one_hop_aggs,
];

/// Everything every rung reads, split once: the clause at the head with
/// its patterns and `WHERE`, and the clauses after it. The head is a
/// plain `MATCH` — not `OPTIONAL`, no `SEARCH` — the facts the ladder's
/// entry checks and the rungs rely on.
pub(crate) struct FoldCtx<'a> {
    pub(super) graph: &'a ExecGraph<'a>,
    pub(super) rows: &'a [Row],
    pub(super) clauses: &'a [Clause],
    pub(super) params: &'a BTreeMap<String, Value>,
    pub(super) patterns: &'a [PathPattern],
    pub(super) where_clause: Option<&'a Expr>,
    pub(super) tail: &'a [Clause],
}

/// Whether any fold would take the `MATCH` at `clauses[0]` — the streaming
/// chain's question (`match_chain`), asked before a mid-pipeline `MATCH`
/// becomes a streaming stage. It runs the same ladder over **no** rows and
/// discards the answer, so it sees exactly the rungs that are total over
/// an empty row set — the aggregate folds, which recognize the shape and
/// yield their empty answer. A rung that gates on the entering rows (the
/// chain count asks for exactly one) is invisible to the probe: such a
/// clause streams, and the streaming chain answers what the fold would
/// have — a routing choice, never a different answer.
pub(crate) fn fold_would_take(
    graph: &ExecGraph<'_>,
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
) -> Result<bool> {
    Ok(try_match_folds(graph, &[], clauses, params)?.is_some())
}

/// The fold family's answer for the `MATCH` at `clauses[0]` together with
/// the clauses after it, over the entering `rows`, if a fold takes it.
pub(crate) fn try_match_folds(
    graph: &ExecGraph<'_>,
    rows: &[Row],
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
) -> Result<Option<FoldedReturn>> {
    let Some((
        Clause::Match {
            optional: false,
            patterns,
            where_clause,
            match_mode,
            search: None,
            ..
        },
        tail,
    )) = clauses.split_first()
    else {
        return Ok(None);
    };
    let ctx = FoldCtx {
        graph,
        rows,
        clauses,
        params,
        patterns,
        where_clause: where_clause.as_ref(),
        tail,
    };
    for rung in ANY_MODE_RUNGS {
        if let Some(folded) = rung(&ctx)? {
            return Ok(Some(folded));
        }
    }
    // `skip_rel` keeps one relationship out of the next hop, which is the
    // DIFFERENT RELATIONSHIPS rule. A single hop has nothing to skip, so
    // REPEATABLE ELEMENTS still uses the folds; a wider pattern under the
    // mode declines here and the ordinary matcher applies the mode.
    let single_hop = patterns.len() == 1 && patterns[0].rels.len() == 1;
    if !single_hop && matches!(match_mode, MatchMode::RepeatableElements) {
        return Ok(None);
    }
    for rung in DEFAULT_SINGLE_HOP_RUNGS {
        if let Some(folded) = rung(&ctx)? {
            return Ok(Some(folded));
        }
    }
    Ok(None)
}
