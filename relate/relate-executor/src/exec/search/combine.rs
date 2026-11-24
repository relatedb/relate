//! The `<combined specification clause>`'s WRRF fusion: weighted
//! reciprocal rank fusion over the ranked lists of a COMBINE's weighted
//! sources (which may themselves be nested combined specifications),
//! split from the SEARCH driver the way its sibling concerns are
//! (`mod deferred; mod direct; mod growth;`).

use std::collections::HashMap;

use relate_core::{expected_type_detail, value_type_name, Error, Result, Value};
use relate_ir::{Expr, SearchCombineSpec};
use relate_model::EntityKind;

use crate::ctx::ExecCtx;
use crate::eval::eval;
use crate::exec::candidates::{ranked_candidates, TopWindow};
use crate::row::Row;

use super::{
    candidate_entity_id, invalid_value_type, search_spec_candidates, spec_window, Growth,
    SearchEnv, SearchWindow, SpecCandidates,
};

/// One `<combined specification clause>`'s fused candidates for
/// one incoming row: weighted reciprocal rank fusion (WRRF) over the
/// ranked lists of its weighted sources (which may themselves be nested
/// combined specifications, Example P3). Each source's own list is ranked
/// after its own SKIP/OFFSET/LIMIT window; a source's rank-`i` entry
/// (0-based) contributes `weight / (constant + i + 1)` (the CIP's
/// `weight / (rrfConstant + rankIndex + 1)`), and an entity missing from a
/// source contributes nothing from that source. The fused list is ordered
/// by the fused score (the CIP's documented tie-breaker was removed --
/// implementation-dependent; this engine ties on the entity id,
/// ascending, via [`crate::exec::candidates::ranked_better_cmp`]) and
/// windowed by the combined specification's own optional LIMIT (the n
/// best fused results) through the shared [`TopWindow`].
/// `None`: every source's query evaluated absent.
pub(super) fn combine_candidates(
    env: &SearchEnv<'_>,
    input: &Row,
    combine: &SearchCombineSpec,
    window: SearchWindow,
) -> Result<Option<SpecCandidates>> {
    // Rules 3/4: the dampening constant, default 60.
    let constant = eval_wrrf_number(
        env.ctx,
        input,
        combine.constant_expr.as_deref(),
        60.0,
        "CONSTANT",
    )?;
    let mut kind: Option<EntityKind> = None;
    // The fused scores keyed by entity id, so a later source adds its
    // contribution to the same entity instead of duplicating it —
    // scores only; the entities materialize from the windowed ids at
    // the end, never carried per source.
    let mut fused: HashMap<i64, f64> = HashMap::new();
    for source in &combine.sources {
        // An absent source query contributes no ranks -- the CIP's
        // "missing from a source" rule (zero contribution).
        // Each source's own window, stated once for this row (see
        // [`spec_window`]): the fusion fetches with each source's own
        // SKIP and LIMIT, so the clause-level statement does not reach
        // inside.
        let source_window = spec_window(env, input, &source.spec)?;
        let Some(fetched) =
            search_spec_candidates(env, input, &source.spec, source_window, Growth::Fixed)?
        else {
            continue;
        };
        // "Compatibility of indexes": the first source's kind becomes the
        // fused kind; every later source must index that same kind.
        let fused_kind = *kind.get_or_insert(fetched.kind);
        if fused_kind != fetched.kind {
            return Err(invalid_value_type(expected_type_detail(
                fused_kind.display_name(),
                fetched.kind.display_name(),
            )));
        }
        // Rules 1/2: the source weight, default 1 (not normalized).
        let weight =
            eval_wrrf_number(env.ctx, input, source.weight_expr.as_deref(), 1.0, "WEIGHT")?;
        // A source's SKIP pages its own ranked list before the fusion
        // sees it: each source's ranks fix its contributions, so the
        // skipped prefix contributes no ranks. The fetch spans
        // skip+limit (see `search_index_candidates`), so the page keeps
        // the `limit` entries ranked after the SKIP — the same ranks the
        // scan-side offset once selected (ADR-0678). The fused window is
        // the combined specification's own LIMIT below.
        for (rank, (candidate, _score)) in fetched
            .candidates
            .into_iter()
            .skip(source_window.skip)
            .enumerate()
        {
            let id = candidate_entity_id(&candidate)?;
            let contribution = weight / (constant + rank as f64 + 1.0);
            fused
                .entry(id)
                .and_modify(|score| *score += contribution)
                .or_insert(contribution);
        }
    }
    let Some(kind) = kind else {
        return Ok(None);
    };
    // The accumulate phase ends and the rank/window phase begins —
    // the binding name says so. The canonical bounded collector keeps
    // the n best fused results ([`TopWindow`]'s own contract: score
    // descending, id ascending on ties) without ever holding the whole
    // fused set sorted — the window is the caller's once-stated fused
    // window (see [`spec_window`]).
    let mut top = TopWindow::new(0, window.limit);
    for (id, score) in &fused {
        top.push(*score, *id);
    }
    // The windowed survivors materialize through the one ranked
    // composition (exec::candidates) with the fused kind.
    let candidates = ranked_candidates(env.ctx.graph, kind, top.into_sorted_rows());
    Ok(Some(SpecCandidates {
        kind,
        candidates,
        // The fused window is a user constraint (keep the n best
        // fused results), not a fetched slice, so it never widens.
        may_widen: false,
    }))
}

/// Evaluates a combined specification's control expression (a WEIGHT or
/// the WRRF WITH CONSTANT) with its absent-expression default: the one
/// stated policy for "an absent control expression takes this default" —
/// present, the value is INTEGER NOT NULL or FLOAT NOT NULL, at least 0
/// and at most Java's `Integer.MAX_VALUE` (2147483647); any other value
/// (NULL included) is the authored invalid-type error.
fn eval_wrrf_number(
    ctx: ExecCtx<'_>,
    input: &Row,
    expr: Option<&Expr>,
    default: f64,
    clause: &str,
) -> Result<f64> {
    let Some(expr) = expr else {
        return Ok(default);
    };
    let ExecCtx { graph, params, .. } = ctx;
    let value = eval(graph, input, expr, params)?;
    let number = match &value {
        Value::Int(number) => *number as f64,
        Value::Float(number) => *number,
        _ => {
            return Err(invalid_value_type(format!(
                "{} (SEARCH {clause})",
                expected_type_detail(
                    "INTEGER NOT NULL or FLOAT NOT NULL",
                    &value_type_name(&value),
                )
            )))
        }
    };
    if !(0.0..=f64::from(i32::MAX)).contains(&number) {
        return Err(Error::syntax(format!(
            "NumberOutOfRange: SEARCH {clause} must be between 0 and 2147483647"
        )));
    }
    Ok(number)
}
