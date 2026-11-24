//! The deferred SEARCH scan: the source reads a variable this MATCH binds,
//! so the pattern matches first and each row's binding is filtered against
//! its own candidate list.

use std::collections::{HashMap, HashSet};

use relate_core::{Result, Value};
use relate_ir::{Expr, MatchMode, PathPattern, SearchClause};
use relate_model::EntityKind;

use super::growth::{top_k_complete, widen_extra};
use super::{
    attach_score, candidate_entity_id, check_binding_kind, search_spec_candidates, Growth,
    SearchEnv,
};
use crate::eval::where_clause_holds;
use crate::exec::candidates::TopWindow;
use crate::exec::match_clause::match_clause;
use crate::row::Row;

/// The deferred shape: matching the pattern first and filtering each row's
/// binding against its own candidate list. The window rule is the direct
/// path's (ADR-0678): SKIP and LIMIT page over the entities whose rows
/// survive, so a window with too few surviving candidates is widened, and
/// the LIMIT keeps the best-ranked survivors (every row of a kept entity
/// rides along).
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
    // The pattern matches WITHOUT the MATCH's outer WHERE: the entity-side
    // conjuncts' verdict is evaluated and cached per matched row here
    // (window-invariant, so one probe serves every growth round), while
    // the score-referencing conjuncts post-filter once in
    // `search_match_clause` — the ADR-0678 split, so this probe never
    // sees a score and reads the plain row.
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
    // The clause's window, stated once from the input row (the window
    // expressions never read a pattern binding — the front rejects that
    // — so one statement serves every row and every growth round; see
    // `spec_window`). The window stays in its named shape: `skip` and
    // `limit` read as fields at their use sites, never re-packed.
    let window = super::spec_window(env, input, &search.spec)?;
    // The deferred scan always grows: a matched row's binding may sit
    // arbitrarily deep in the ranking — pre-bound rows as much as
    // pattern-matched ones — so a wider window can always surface a row's
    // own candidate where a narrower one missed it. Termination is the
    // index's (`may_widen`), never the shape's.
    // The survivor count the growth loop needs before the window can
    // page — loop-invariant (the window never changes), computed once
    // beside the statement that owns it.
    let needed = window.skip.saturating_add(window.limit);
    let mut extra = 0usize;
    // Each matched row travels with its own cached state — the outer
    // WHERE's verdict (recorded at the row's first in-window resolution:
    // the binding's rank and score in its own candidate list are
    // window-invariant -- a wider window only appends lower-ranked
    // candidates -- so the verdict cannot change between growth rounds)
    // and the terminal round's binding and score for the emitted rows.
    // One value per row; no parallel collections kept in lockstep.
    let mut pairs: Vec<(Row, RowState)> = matched
        .into_iter()
        .map(|row| (row, RowState::default()))
        .collect();
    loop {
        let mut round = DeferredRound::default();
        for (row, state) in pairs.iter_mut() {
            // One binding entry per matched row, by construction.
            let Some((id, rank, score)) =
                deferred_row_binding(env, row, window, extra, binding_kind, search, &mut round)?
            else {
                continue;
            };
            let where_ok = match state.where_ok {
                Some(verdict) => verdict,
                None => {
                    // The canonical optional-WHERE gate (stellar round 2,
                    // F-2-22): the one spelling every other optional-WHERE
                    // site reads -- a non-boolean predicate is the typed
                    // rejection, never a silent drop. The entity-side WHERE
                    // never reads the score (ADR-0678's split moved every
                    // score-referencing conjunct to the post-filter), so
                    // the probe is the plain row.
                    let verdict = where_clause_holds(ctx.graph, row, where_clause, ctx.params)?;
                    state.where_ok = Some(verdict);
                    verdict
                }
            };
            // Only a WHERE-passing row makes its candidate a survivor: the
            // window pages over candidates with a surviving row, as the
            // direct path's per-candidate MATCH ... WHERE does. The
            // resolution is recorded beside the survival, so `resolved`
            // means "this row survived" by construction — matching the
            // fact the terminal pass below declares final.
            if where_ok {
                state.resolved = Some((id, score));
                round.survive(id, rank, score);
            }
        }
        // The survivors the SKIP and LIMIT windows page over: the loop
        // widens until both windows are full, or the index is exhausted.
        if !top_k_complete(round.may_widen, round.survivors.len(), needed) {
            extra = widen_extra(extra, needed);
            continue;
        }
        // The clause's window keeps the best-ranked survivors through the
        // canonical bounded collector — [`TopWindow`]'s own contract
        // (score descending, id ascending on ties, the SKIP page and the
        // LIMIT page), the same order the candidate lists carry: one
        // owner for the window instead of a hand-rolled sort + drain +
        // truncate.
        let mut top = TopWindow::new(window.skip, window.limit);
        for (&id, (_rank, score)) in &round.survivors {
            top.push(*score, id);
        }
        let kept: HashSet<i64> = top
            .into_sorted_rows()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        // The matched rows are consumed, not cloned: each emitted row is
        // moved out and carries the score its resolution recorded. A row
        // rides along when its resolution survived — `resolved` is
        // written only inside the WHERE-passing arm, so it is the one
        // terminal survivor fact — and its candidate is among the kept
        // survivors.
        let mut out = Vec::with_capacity(pairs.len());
        for (mut row, state) in pairs {
            let Some((id, score)) = state.resolved else {
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

/// One deferred growth round's accumulator: whether any row's scan could
/// widen, and each WHERE-surviving candidate's best `(rank, score)` pair
/// — replaced whole, so the score always travels with the ranking that
/// kept it. The clause's window is not round state: it is stated once
/// from the input row before the loop (`spec_window`) and read as the
/// plain `skip`/`limit` locals.
#[derive(Default)]
struct DeferredRound {
    may_widen: bool,
    survivors: HashMap<i64, (usize, f64)>,
}

impl DeferredRound {
    /// Record a WHERE-passing row's candidate, keeping its best (lowest)
    /// rank with the score that ranking carries.
    fn survive(&mut self, id: i64, rank: usize, score: f64) {
        self.survivors
            .entry(id)
            .and_modify(|slot| {
                if rank < slot.0 {
                    *slot = (rank, score);
                }
            })
            .or_insert((rank, score));
    }
}

/// One matched row's cached deferred state across growth rounds.
#[derive(Clone, Default)]
struct RowState {
    where_ok: Option<bool>,
    resolved: Option<(i64, f64)>,
}

/// One matched row's deferred binding: the row's own candidate list
/// decides whether its binding is a survivor, and at which rank and
/// score. Exactly one value per row, by construction; the caller
/// decides survival once the row's WHERE verdict is known.
fn deferred_row_binding(
    env: &SearchEnv<'_>,
    row: &Row,
    window: super::SearchWindow,
    extra: usize,
    binding_kind: Option<EntityKind>,
    search: &SearchClause,
    round: &mut DeferredRound,
) -> Result<Option<(i64, usize, f64)>> {
    let Some(fetched) =
        search_spec_candidates(env, row, &search.spec, window, Growth::Widen(extra))?
    else {
        return Ok(None);
    };
    check_binding_kind(binding_kind, fetched.kind)?;
    round.may_widen |= fetched.may_widen;
    // One binding entry per matched row, by construction: the front
    // validates the binding variable as this MATCH's own pattern
    // variable, and the pattern binds it in every row it emits (this
    // scan's pattern match is non-optional, so no null-padded row rides
    // here). A miss is that broken invariant, the enumerated-candidate
    // contract's — a panic, never a silently short result set.
    let bound = row
        .get_name(&search.binding_var)
        .expect("every matched row binds the SEARCH variable");
    let Some(rank) = fetched
        .candidates
        .iter()
        .position(|(candidate, _)| bound.eq_non_null(candidate))
    else {
        return Ok(None);
    };
    let id = candidate_entity_id(bound)?;
    let score = fetched.candidates[rank].1;
    Ok(Some((id, rank, score)))
}
