//! The un-deferred SEARCH scan: the source reads none of the MATCH's pattern
//! variables, so each candidate seeds the binding and the pattern matches
//! per candidate.

use relate_core::{Result, Value};
use relate_ir::{Expr, MatchMode, PathPattern, SearchClause};
use relate_model::EntityKind;

use super::growth::{top_k_complete, widen_extra};
use super::{
    attach_score, check_binding_kind, search_spec_candidates, spec_window, Growth, SearchEnv,
};
use crate::exec::match_clause::match_clause;
use crate::row::Row;

/// The un-deferred shape: the source specification reads none of this
/// MATCH's pattern variables, so a candidate seeds the binding variable
/// and the pattern matches per candidate.
///
/// The window rule (ADR-0678, one rule for every pattern shape): SKIP and
/// LIMIT page over the *entities* that satisfy the whole MATCH statement —
/// the pattern, the outer WHERE, and a pre-bound binding value. The loop
/// widens the fetched window (geometrically, until the index is
/// exhausted) while too few entities have survived, so a WHERE-passing
/// candidate ranked below the raw top-K is returned, and the K counts
/// survivors, never raw hits. A constraint the index definition already
/// implies filters no index member, so for such patterns the first window
/// already carries LIMIT survivors and nothing widens.
///
/// Every pattern row of a kept entity rides along: the LIMIT counts
/// entities, not rows. A pre-bound binding keeps the same rule with a
/// window of one — the pre-bound value is the only entity that can
/// survive, however deep it sits (live 2026.10.1: one row per incoming
/// row), and a pre-bound null (an OPTIONAL MATCH that matched nothing)
/// joins with no candidate at all.
pub(super) fn direct_search_rows(
    env: &SearchEnv<'_>,
    input: &Row,
    binding_kind: Option<EntityKind>,
    match_mode: MatchMode,
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    search: &SearchClause,
) -> Result<Vec<Row>> {
    let ctx = env.ctx;
    let prebound = input.get_name(&search.binding_var);
    // A null pre-bound binding matches no pattern row, so no candidate can
    // join it -- no window is worth fetching.
    if prebound.is_some_and(Value::is_null) {
        return Ok(Vec::new());
    }
    // The clause's window, stated once for this input row (the window
    // expressions never read a pattern binding — the front rejects that
    // — so one statement serves every growth round; see `spec_window`).
    let window = spec_window(env, input, &search.spec)?;
    // The needed survivor count, stated once above the loop: the clause
    // LIMIT, or the single join partner a pre-bound binding can ever
    // reach (capped by the LIMIT — a LIMIT-0 clause keeps nothing, a
    // pre-bound binding included). Tested at the candidate loop's head,
    // so a satisfied window — LIMIT 0 the loudest case — completes
    // before any candidate is processed.
    let needed = if prebound.is_some() {
        1.min(window.limit)
    } else {
        window.limit
    };
    // The binding is seeded per candidate, so a wider window always
    // reaches candidates the current one cannot — a pre-bound binding's
    // partner may sit arbitrarily deep in the ranking: the direct scan
    // always grows, so its completion test needs no grows guard.
    let mut out = Vec::new();
    // For a pre-bound row: its binding's own candidate was processed
    // without surviving — consumed by the SKIP window or rejected by the
    // MATCH. An entity appears once in its own ranking (a wider window
    // only appends lower-ranked entities), so that verdict is the row's
    // final one: set, the growth loop is done for this row instead of
    // widening to exhaustion.
    let mut binding_spent = false;
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
        // (LIMIT 0 is the loudest case: needed is 0 on the first test).
        if survivors >= needed {
            break;
        }
        for (candidate, score) in fetched.candidates.into_iter().skip(processed) {
            processed += 1;
            // The binding's own candidate — the only one a pre-bound row
            // can ever keep; every other candidate is dropped below.
            let binding_candidate =
                prebound.is_some_and(|existing| existing.eq_non_null(&candidate));
            if prebound.is_some() && !binding_candidate {
                // A different non-null binding is dropped rather than
                // overwritten (live Neo4j 2026.09.0: `MATCH (p) SEARCH p IN
                // (VECTOR ...) WITH p MATCH (p) SEARCH p IN (FULLTEXT ...)`
                // intersects the two result sets).
                continue;
            }
            let mut seeded = input.clone();
            seeded.insert(search.binding_var.to_string(), candidate);
            attach_score(&mut seeded, search.score_var.as_ref(), Value::Float(score));
            let matched = match_clause(
                ctx,
                vec![seeded],
                false,
                match_mode,
                patterns,
                where_clause,
                None,
                None,
                None,
            )?;
            if matched.is_empty() {
                binding_spent |= binding_candidate;
                continue;
            }
            // The candidate survives the whole MATCH: the SKIP window
            // consumes its leading survivors, the LIMIT keeps the rest.
            if skipped < window.skip {
                skipped += 1;
                binding_spent |= binding_candidate;
                continue;
            }
            out.extend(matched);
            survivors += 1;
            if survivors >= needed {
                break;
            }
        }
        if binding_spent || top_k_complete(fetched.may_widen, survivors, needed) {
            break;
        }
        extra = widen_extra(extra, window.limit);
    }
    Ok(out)
}
