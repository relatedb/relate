//! The MATCH-plus-aggregate fold family's one entry: the dispatcher asks
//! it, per plain `MATCH`, whether a fold answers the clause together with
//! the clauses after it (the boolean-count probe, the walk count, the
//! fixed-length path fold, the one-hop fold). Moved out of the dispatcher
//! so the chain gate (`match_chain`) asks the very same recognizers.

use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::{Clause, MatchMode};

use crate::overlay::ExecGraph;
use crate::row::Row;

use super::exists_probe::try_fold_boolean_count;
use super::pushdown::{try_fold_one_hop_aggs, try_fold_path_aggs, FoldedReturn};

/// The question the ladder is asked: answer the clause over real entering
/// rows, or only whether any rung would take it. A rung over zero rows is
/// total (it recognizes the shape and yields its empty answer), so
/// `WouldTake` runs the same recognizers over none and discards the answer.
enum FoldAsk<'a> {
    Answer(&'a [Row]),
    WouldTake,
}

/// The fold family's answer for the `MATCH` at `clauses[0]` together with
/// the clauses after it, over the entering `rows`, if a fold takes it.
pub(crate) fn try_match_folds(
    graph: &ExecGraph<'_>,
    rows: &[Row],
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
) -> Result<Option<FoldedReturn>> {
    fold_ladder(graph, FoldAsk::Answer(rows), clauses, params)
}

/// Whether any fold would take the `MATCH` at `clauses[0]` — the streaming
/// chain's admission question (`match_chain`), asked of the very same
/// recognizers, with no rows to fold.
pub(crate) fn fold_would_take(
    graph: &ExecGraph<'_>,
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
) -> Result<bool> {
    Ok(fold_ladder(graph, FoldAsk::WouldTake, clauses, params)?.is_some())
}

fn fold_ladder(
    graph: &ExecGraph<'_>,
    ask: FoldAsk<'_>,
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
) -> Result<Option<FoldedReturn>> {
    let rows: &[Row] = match ask {
        FoldAsk::Answer(rows) => rows,
        FoldAsk::WouldTake => &[],
    };
    let Some((
        Clause::Match {
            patterns,
            where_clause,
            match_mode,
            ..
        },
        tail,
    )) = clauses.split_first()
    else {
        return Ok(None);
    };
    let where_clause = where_clause.as_ref();
    if let Some(folded) = try_fold_boolean_count(graph, rows, clauses, params)? {
        return Ok(Some(folded));
    }
    // The walk-count arm (`kernels::frontier_count`): a bounded
    // REPEATABLE ELEMENTS quantified pattern under a plain `count`, ahead
    // of the multi-hop REPEATABLE decline below.
    if let Some(folded) =
        super::kernels::frontier_count::try_fold_walk_count(graph, rows, clauses, params)?
    {
        return Ok(Some(folded));
    }
    // `skip_rel` keeps one relationship out of the next hop, which is the
    // DIFFERENT RELATIONSHIPS rule. A single hop has nothing to skip, so
    // REPEATABLE ELEMENTS still uses the folds. Wider patterns decline and the
    // ordinary matcher applies the mode.
    let single_hop = patterns.len() == 1 && patterns[0].rels.len() == 1;
    if !single_hop && matches!(match_mode, MatchMode::RepeatableElements) {
        return Ok(None);
    }
    // The path fold first: it compiles the full hop plan once and serves the
    // widest coverage. The chain count and one-hop folds follow, each
    // serving exactly the queries the path fold declined — no coverage
    // gate between the rungs, so no query compiles the plan twice.
    if let Some(folded) = try_fold_path_aggs(graph, rows, patterns, where_clause, tail, params)? {
        return Ok(Some(folded));
    }
    // The plain fixed-hop and exact `*k` counts the path fold declines —
    // the chain kernel's own gates own what it accepts.
    if let Some(folded) = super::kernels::try_fold_chain_count(graph, rows, clauses, params)? {
        return Ok(Some(folded));
    }
    try_fold_one_hop_aggs(graph, rows, patterns, where_clause, tail, params)
}
