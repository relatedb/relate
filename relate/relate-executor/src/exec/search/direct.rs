//! The un-deferred SEARCH scan: the source reads none of the MATCH's pattern
//! variables, so each candidate seeds the binding and the pattern matches
//! per candidate.

use relate_core::{Result, Value};
use relate_ir::{Expr, MatchMode, PathPattern, SearchClause};
use relate_model::EntityKind;

use super::growth::{top_k_complete, top_k_grows, widen_extra};
use super::{attach_score, check_binding_kind, search_spec_candidates, SearchEnv};
use crate::exec::match_clause::match_clause;
use crate::row::Row;

/// The un-deferred shape: the source specification reads none of this
/// MATCH's pattern variables, so a candidate seeds the binding variable
/// and the pattern matches per candidate.
///
/// A *complex* pattern's LIMIT counts candidates that produce at least one
/// row, not fetched candidates (CIP-224's top-K guarantee): a window whose
/// candidates the pattern or the MATCH's outer WHERE rejected is widened
/// until the LIMIT is met or the index is exhausted, and the deeper
/// candidates are matched too. Verified on Neo4j 2026.10.1:
/// `MATCH (movie:Movie)<-[:ACTED_IN]-({name: 'Al Pacino'}) SEARCH movie IN
/// (... LIMIT 5)` returns the five best-scoring Pacino films, skipping four
/// better-scoring films the pattern rejects.
///
/// A *plain* pattern (the search element alone) keeps the LIMIT as a hard
/// candidate window: its outer WHERE, label or score predicate post-filters
/// the window without freeing slots (verified on Neo4j 2026.10.1:
/// `MATCH (x:Film) SEARCH x IN (... LIMIT 4) WHERE x.year < 2000` returns
/// the WHERE-passing rows *inside* the top four, not four passing rows).
///
/// A pre-bound binding variable follows the same rule — it widens under a
/// complex pattern until the pre-bound candidate appears (live 2026.10.1:
/// one row per incoming row), keeps the window under a plain one, and a
/// pre-bound null (an OPTIONAL MATCH that matched nothing) joins with no
/// candidate at all.
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
    // The binding is seeded per candidate, so a pre-bound binding's
    // partner may sit arbitrarily deep: a wider window always reaches
    // further (see [`top_k_grows`]).
    let grows = top_k_grows(search, true);
    let mut out = Vec::new();
    let mut extra = 0usize;
    // Candidates already matched in an earlier (narrower, hence identical
    // prefix of the same ranking) round.
    let mut processed = 0usize;
    let mut survivors = 0usize;
    while let Some(fetched) = search_spec_candidates(env, input, &search.spec, extra)? {
        check_binding_kind(binding_kind, fetched.kind)?;
        // A LIMIT-0 clause keeps no candidate at all: terminate before
        // any growth round -- with a zero base the widen step could
        // otherwise never leave `extra` at 0 and the loop would spin
        // forever (round-2 High: a pre-bound binding under a complex
        // pattern stays non-terminal while the window stays empty).
        if fetched.limit == 0 {
            break;
        }
        // The needed survivor count, stated once: the clause LIMIT, or
        // the single join partner a pre-bound binding can ever reach.
        let needed = if prebound.is_some() { 1 } else { fetched.limit };
        for (candidate, score) in fetched.candidates.into_iter().skip(processed) {
            processed += 1;
            if let Some(existing) = prebound {
                // A different non-null binding is dropped rather than
                // overwritten (live Neo4j 2026.09.0: `MATCH (p) SEARCH p IN
                // (VECTOR ...) WITH p MATCH (p) SEARCH p IN (FULLTEXT ...)`
                // intersects the two result sets).
                if !existing.eq_non_null(&candidate) {
                    continue;
                }
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
                continue;
            }
            out.extend(matched);
            survivors += 1;
            if survivors >= needed {
                break;
            }
        }
        if top_k_complete(grows, fetched.may_widen, survivors, needed) {
            break;
        }
        extra = widen_extra(extra, fetched.limit);
    }
    Ok(out)
}
