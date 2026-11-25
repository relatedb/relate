//! The un-deferred SEARCH scans: the source reads none of the MATCH's pattern
//! variables, so each candidate seeds the binding and the pattern matches
//! per candidate. Two row sources share the module: the ranked-window scan
//! ([`direct_search_rows`]) and the pre-bound row's single-entity
//! membership question ([`pre_bound_search_rows`]).

use relate_core::{Result, Value};
use relate_ir::{Expr, MatchMode, PathPattern, SearchClause};
use relate_model::EntityKind;

use super::growth::{top_k_complete, widen_extra};
use super::{
    attach_score, check_binding_kind, search_spec_candidates, spec_window, Growth, SearchEnv,
};
use crate::exec::match_clause::match_clause;
use crate::row::Row;

/// Seed the binding with one candidate and match the pattern against it:
/// the one per-candidate step both row sources share.
fn match_candidate(
    env: &SearchEnv<'_>,
    input: &Row,
    search: &SearchClause,
    (candidate, score): (Value, f64),
    match_mode: MatchMode,
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
) -> Result<Vec<Row>> {
    let mut seeded = input.clone();
    seeded.insert(search.binding_var.to_string(), candidate);
    attach_score(&mut seeded, search.score_var.as_ref(), Value::Float(score));
    match_clause(
        env.ctx,
        vec![seeded],
        false,
        match_mode,
        patterns,
        where_clause,
        None,
        None,
        None,
    )
}

/// The un-deferred shape: the source specification reads none of this
/// MATCH's pattern variables, so a candidate seeds the binding variable
/// and the pattern matches per candidate. The input row's binding is
/// unbound; a pre-bound one is [`pre_bound_search_rows`]'s.
///
/// The window rule (ADR-0678, one rule for every pattern shape): SKIP and
/// LIMIT page over the *entities* that satisfy the whole MATCH statement —
/// the pattern and the outer WHERE. The loop widens the fetched window
/// (geometrically, until the index is exhausted) while too few entities
/// have survived, so a WHERE-passing candidate ranked below the raw top-K
/// is returned, and the K counts survivors, never raw hits. A constraint
/// the index definition already implies filters no index member, so for
/// such patterns the first window already carries LIMIT survivors and
/// nothing widens.
///
/// Every pattern row of a kept entity rides along: the LIMIT counts
/// entities, not rows.
pub(super) fn direct_search_rows(
    env: &SearchEnv<'_>,
    input: &Row,
    binding_kind: Option<EntityKind>,
    match_mode: MatchMode,
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    search: &SearchClause,
) -> Result<Vec<Row>> {
    // The clause's window, stated once for this input row (the window
    // expressions never read a pattern binding — the front rejects that
    // — so one statement serves every growth round; see `spec_window`).
    let window = spec_window(env, input, &search.spec)?;
    // The binding is seeded per candidate, so a wider window always
    // reaches candidates the current one cannot: the direct scan always
    // grows, so its completion test needs no grows guard.
    let mut out = Vec::new();
    let mut extra = 0usize;
    // Candidates already matched in an earlier (narrower, hence identical
    // prefix of the same ranking) round.
    let mut processed = 0usize;
    // The SKIP window's consumed survivors, and the kept ones — both
    // cumulative across growth rounds.
    let mut skipped = 0usize;
    let mut survivors = 0usize;
    while let Some(fetched) =
        search_spec_candidates(env, input, &search.spec, window, Growth::Widen(extra))?
    {
        check_binding_kind(binding_kind, fetched.kind)?;
        // A satisfied window completes before any candidate is processed
        // (LIMIT 0 is the loudest case: the limit is 0 on the first test).
        if survivors >= window.limit {
            break;
        }
        for candidate in fetched.candidates.into_iter().skip(processed) {
            processed += 1;
            let matched = match_candidate(
                env,
                input,
                search,
                candidate,
                match_mode,
                patterns,
                where_clause,
            )?;
            if matched.is_empty() {
                continue;
            }
            // The candidate survives the whole MATCH: the SKIP window
            // consumes its leading survivors, the LIMIT keeps the rest.
            if skipped < window.skip {
                skipped += 1;
                continue;
            }
            out.extend(matched);
            survivors += 1;
            if survivors >= window.limit {
                break;
            }
        }
        if top_k_complete(fetched.may_widen, survivors, window.limit) {
            break;
        }
        extra = widen_extra(extra, window.limit);
    }
    Ok(out)
}

/// The pre-bound shape: the input row already binds the SEARCH variable
/// (`MATCH (p) SEARCH p IN (VECTOR ...) WITH p MATCH (p) SEARCH p IN
/// (FULLTEXT ...)`), so the clause asks one single-entity membership
/// question — is the pre-bound entity among the index's ranked
/// candidates, and does it survive the MATCH — instead of paging a
/// ranking. A different candidate is never seeded over the binding (live
/// Neo4j 2026.09.0 intersects the two result sets). The window rule
/// (ADR-0678) reduces to a window of one: the pre-bound value is the
/// only entity that can survive, however deep it sits (live 2026.10.1:
/// one row per incoming row), so a SKIP window consumes it and a LIMIT-0
/// window keeps nothing — both answer empty without fetching — and a
/// pre-bound null (an OPTIONAL MATCH that matched nothing) joins with no
/// candidate at all.
pub(super) fn pre_bound_search_rows(
    env: &SearchEnv<'_>,
    input: &Row,
    binding_kind: Option<EntityKind>,
    match_mode: MatchMode,
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    search: &SearchClause,
) -> Result<Vec<Row>> {
    let bound = input
        .get_name(&search.binding_var)
        .expect("the pre-bound source is picked only for a row that binds the SEARCH variable");
    if bound.is_null() {
        return Ok(Vec::new());
    }
    let window = spec_window(env, input, &search.spec)?;
    if window.skip > 0 || window.limit == 0 {
        return Ok(Vec::new());
    }
    let mut extra = 0usize;
    // Candidates already compared in an earlier (narrower, hence identical
    // prefix of the same ranking) round.
    let mut processed = 0usize;
    while let Some(fetched) =
        search_spec_candidates(env, input, &search.spec, window, Growth::Widen(extra))?
    {
        check_binding_kind(binding_kind, fetched.kind)?;
        // An entity appears once in its own ranking (a wider window only
        // appends lower-ranked entities), so its candidate is the row's
        // one verdict: kept rows or none, never another round.
        let found = fetched
            .candidates
            .into_iter()
            .skip(processed)
            .find(|(candidate, _)| {
                processed += 1;
                bound.eq_non_null(candidate)
            });
        if let Some(candidate) = found {
            return match_candidate(
                env,
                input,
                search,
                candidate,
                match_mode,
                patterns,
                where_clause,
            );
        }
        if !fetched.may_widen {
            break;
        }
        extra = widen_extra(extra, window.limit);
    }
    Ok(Vec::new())
}
