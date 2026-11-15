//! The deferred SEARCH scan: the source reads a variable this MATCH binds,
//! so the pattern matches first and each row's binding is filtered against
//! its own candidate list.

use std::collections::{HashMap, HashSet};

use relate_core::{Result, Value};
use relate_ir::{Expr, MatchMode, PathPattern, SearchClause};
use relate_model::EntityKind;

use super::growth::{top_k_complete, top_k_grows, widen_extra};
use super::{
    attach_score, candidate_entity_id, check_binding_kind, search_spec_candidates, SearchEnv,
};
use crate::eval::where_clause_holds;
use crate::exec::candidates::ranked_better_cmp;
use crate::exec::match_clause::match_clause;
use crate::row::Row;

/// The deferred shape: matching the pattern first and filtering each row's
/// binding against its own candidate list. The same complex-pattern top-K
/// rule applies — the LIMIT counts distinct candidates that have a row — so
/// a window with too few surviving candidates is widened, and the LIMIT
/// keeps the best-ranked survivors (every row of a kept candidate rides
/// along).
pub(super) fn deferred_search_rows(
    env: &SearchEnv<'_>,
    input: &Row,
    binding_kind: Option<EntityKind>,
    match_mode: MatchMode,
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    search: &SearchClause,
) -> Result<Vec<Row>> {
    let ctx = env.ctx;
    // The pattern matches WITHOUT the MATCH's outer WHERE: each row's
    // binding and score are resolved from its own candidate list and the
    // score attached before the WHERE runs — the direct path's order of
    // operations, so a WHERE naming the SCORE AS variable behaves the
    // same in both paths.
    let matched = match_clause(
        ctx,
        vec![input.clone()],
        false,
        match_mode,
        patterns,
        None,
        None,
        None,
        None,
    )?;
    // The rows of a pre-bound binding already carry their only possible
    // candidate, so no wider window can add a survivor ([`top_k_grows`]).
    let binding_prebound = input.get_name(&search.binding_var).is_some();
    let grows = top_k_grows(search, !binding_prebound);
    let mut extra = 0usize;
    // The outer WHERE's verdict per matched row, cached at the row's
    // first in-window resolution: the binding's rank and score in its
    // own candidate list are window-invariant (a wider window only
    // appends lower-ranked candidates), so the verdict cannot change
    // between growth rounds.
    let mut where_ok: Vec<Option<bool>> = vec![None; matched.len()];
    // The terminal round's per-row binding and score, for the emitted
    // rows.
    let mut resolved: Vec<Option<(i64, f64)>> = vec![None; matched.len()];
    loop {
        let mut round = DeferredRound::default();
        for (index, row) in matched.iter().enumerate() {
            // One binding entry per matched row, by construction.
            let Some((id, rank, score)) =
                deferred_row_binding(env, row, extra, binding_kind, search, &mut round)?
            else {
                continue;
            };
            if where_ok[index].is_none() {
                let mut scored = row.clone();
                attach_score(&mut scored, search.score_var.as_ref(), Value::Float(score));
                // The canonical optional-WHERE gate (stellar round 2,
                // F-2-22): the one spelling every other optional-WHERE
                // site reads — a non-boolean predicate is the typed
                // rejection, never a silent drop.
                where_ok[index] = Some(where_clause_holds(
                    ctx.graph,
                    &scored,
                    where_clause,
                    ctx.params,
                )?);
            }
            resolved[index] = Some((id, score));
            if where_ok[index] == Some(true) {
                round.survivors.entry(id).and_modify(|slot| {
                    if rank < slot.0 {
                        *slot = (rank, score);
                    }
                });
            }
        }
        let needed = round.limit.unwrap_or(usize::MAX);
        if !top_k_complete(grows, round.may_widen, round.survivors.len(), needed) {
            extra = widen_extra(extra, needed);
            continue;
        }
        // The clause's LIMIT keeps the best-ranked survivors -- the one
        // canonical CIP-225 candidate order ([`ranked_better_cmp`] over
        // score and id), the same order the candidate lists carry.
        let mut kept: Vec<(i64, f64)> = round
            .survivors
            .iter()
            .map(|(id, (_rank, score))| (*id, *score))
            .collect();
        kept.sort_by(|a, b| ranked_better_cmp(b.1, b.0, a.1, a.0));
        kept.truncate(needed);
        let kept: HashSet<i64> = kept.into_iter().map(|(id, _)| id).collect();
        // The matched rows are consumed, not cloned: each emitted row is
        // moved out and carries the score its resolution recorded. A row
        // rides along when its WHERE verdict is true and its candidate is
        // among the kept survivors.
        let mut out = Vec::with_capacity(matched.len());
        for (index, mut row) in matched.into_iter().enumerate() {
            if where_ok[index] != Some(true) {
                continue;
            }
            let Some((id, score)) = resolved[index] else {
                continue;
            };
            if !kept.contains(&id) {
                continue;
            }
            attach_score(&mut row, search.score_var.as_ref(), Value::Float(score));
            out.push(row);
        }
        return Ok(out);
    }
}

/// One deferred growth round's accumulator: the clause LIMIT (evaluated
/// once — the LIMIT expression reads the input row, never a variable the
/// MATCH's pattern newly binds, enforced at validation, so every matched
/// row of one input row yields the same value; `None` until the first
/// row states it, and an absent combined LIMIT stays `usize::MAX`
/// without colliding with the sentinel), whether any row's scan could
/// widen, and each surviving candidate's best `(rank, score)` pair —
/// replaced whole, so the score always travels with the ranking that
/// kept it.
#[derive(Default)]
struct DeferredRound {
    limit: Option<usize>,
    may_widen: bool,
    survivors: HashMap<i64, (usize, f64)>,
}

/// One matched row's deferred binding: the row's own candidate list
/// decides whether its binding is a survivor, and at which rank and
/// score. Exactly one value per row, by construction.
fn deferred_row_binding(
    env: &SearchEnv<'_>,
    row: &Row,
    extra: usize,
    binding_kind: Option<EntityKind>,
    search: &SearchClause,
    round: &mut DeferredRound,
) -> Result<Option<(i64, usize, f64)>> {
    let Some(fetched) = search_spec_candidates(env, row, &search.spec, extra)? else {
        return Ok(None);
    };
    check_binding_kind(binding_kind, fetched.kind)?;
    if round.limit.is_none() {
        // The first fetched row states the clause LIMIT for the whole
        // round (every row's is the same; see DeferredRound).
        round.limit = Some(fetched.limit);
    }
    round.may_widen |= fetched.may_widen;
    let Some(bound) = row.get_name(&search.binding_var) else {
        return Ok(None);
    };
    let Some(rank) = fetched
        .candidates
        .iter()
        .position(|(candidate, _)| bound.eq_non_null(candidate))
    else {
        return Ok(None);
    };
    let id = candidate_entity_id(bound)?;
    let score = fetched.candidates[rank].1;
    round
        .survivors
        .entry(id)
        .and_modify(|slot| {
            if rank < slot.0 {
                *slot = (rank, score);
            }
        })
        .or_insert((rank, score));
    Ok(Some((id, rank, score)))
}
