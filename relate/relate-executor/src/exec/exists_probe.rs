//! Depth-first existence over planner-ordered MATCH patterns.
//!
//! [`match_paths`](super::match_clause::match_paths) finishes one pattern's
//! `Vec` before starting the next, so a row cap on the last pattern cannot
//! stop the first. This probe offers each complete binding of pattern N to
//! pattern N+1 immediately and returns at the first binding that satisfies
//! the predicate.

use crate::UsedSet;
use std::cell::Cell;
use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::{
    classify_aggregate, AggregateKind, BinaryOp, Clause, Expr, MatchMode, PathPattern, Projection,
};

use crate::ctx::ExecCtx;
use crate::eval::eval;
use crate::overlay::ExecGraph;
use crate::pattern_walk::resolved_endpoints_exist;
use crate::row::Row;

use super::one_path::{match_one_path, PathFilters};
use super::pattern_policies::MatchTail;
use super::pushdown::{flatten_and_owned, match_scope, FoldedReturn, Residual};
use super::rider::{PathProbe, Rider};

/// What the existence probe counts as a hit — the two shapes it serves:
/// every complete binding (a multi-pattern `EXISTS { MATCH … }`), or the
/// boolean-count fold's `count(DISTINCT expr)` being non-null.
#[derive(Clone, Copy)]
pub(crate) enum ExistencePred<'a> {
    /// Every complete binding counts.
    Any,
    /// A complete binding counts when the expression is non-null (the
    /// boolean-count fold's counted expression).
    Counted(&'a Expr),
}

impl ExistencePred<'_> {
    /// Whether one complete binding counts — the row-shaped evaluation
    /// the probe's DFS makes per binding.
    fn satisfied(
        &self,
        graph: &ExecGraph<'_>,
        row: &Row,
        params: &BTreeMap<String, Value>,
        cypher_version: Option<u32>,
    ) -> Result<bool> {
        match self {
            ExistencePred::Any => Ok(true),
            ExistencePred::Counted(expr) => {
                Ok(!eval(graph, row, expr, params, cypher_version)?.is_null())
            }
        }
    }
}

/// `Ok(None)` when the patterns are not walkable by the probe (empty, or
/// a shortest-path function whose uniqueness semantics are the GPM
/// route's). GPM patterns (every var-length spelling since ADR-0471)
/// walk the GPM engine with a probe rider — restoring the early exit the
/// unification's Consequences record losing; the classic shapes keep the
/// LABS-193/LABS-249 walk below.
pub(crate) fn patterns_exist(
    ctx: ExecCtx<'_>,
    patterns: &[PathPattern],
    wheres: &[Expr],
    row: Row,
    pred: ExistencePred<'_>,
) -> Result<Option<bool>> {
    if patterns.is_empty() || patterns.iter().any(|path| path.is_shortest_path_function) {
        return Ok(None);
    }
    // ADR-0473: a contracted kernel takes the existence walk when the
    // plan matches a family and the cost gate pays — the hub-driven
    // per-hub combination with its early exit (q30's shape), or the
    // unrolled chain. `None` keeps the probe's depth-first seek. Every
    // caller's patterns came through `plain_match`, which guarantees the
    // `DIFFERENT RELATIONSHIPS` mode the kernel's uniqueness rule needs.
    let counted = match &pred {
        ExistencePred::Counted(expr) => Some(*expr),
        ExistencePred::Any => None,
    };
    if let Some(hit) = super::kernels::try_exists_kernel(
        ctx,
        patterns,
        wheres,
        &row,
        MatchMode::DifferentRelationships,
        counted,
    )? {
        // The kernel arm serves the same full-pattern walk the seek below
        // is — the clause-engine pass it replaces — so it ticks the
        // nested-evaluation budget once per evaluation too (ADR-0395 over
        // ADR-0473): no arm of this probe is the evaluation route that
        // escapes the accounting. The tick rides the engaged answer, never
        // the decline — a declined kernel falls through to the seek's own
        // tick below, so one evaluation spends exactly one either way. It
        // lands after the walk (the seek's tick lands before its walk)
        // because the kernel's own cost gate bounds the walk it admits,
        // while the seek's walk is unbounded a priori — that bound is what
        // the pre-walk tick buys there.
        ctx.graph.nested_evals().tick()?;
        return Ok(Some(hit));
    }
    let conjuncts: Vec<Expr> = wheres.iter().flat_map(flatten_and_owned).collect();
    let scope = match_scope(&row, patterns);
    let residual = Residual {
        conjuncts: &conjuncts,
        scope: &scope,
    };
    // Unlike the bounded LABS-193 anchored probe (which answers without
    // ticking, ADR-0395), this depth-first probe is a full pattern walk —
    // the clause-engine pass it replaces — so it ticks the
    // nested-evaluation budget once per evaluation, keeping the security
    // bound on compounding EXISTS chains (the classic and GPM arms alike;
    // the kernel arm ticks at its own return above).
    ctx.graph.nested_evals().tick()?;
    Ok(Some(seek(
        ctx,
        patterns,
        0,
        row,
        UsedSet::default(),
        &residual,
        pred,
    )?))
}

fn seek(
    ctx: ExecCtx<'_>,
    patterns: &[PathPattern],
    idx: usize,
    row: Row,
    used: UsedSet,
    residual: &Residual<'_>,
    pred: ExistencePred<'_>,
) -> Result<bool> {
    if idx >= patterns.len() {
        let graph = ctx.graph;
        return pred.satisfied(graph, &row, ctx.params, ctx.cypher_version);
    }
    let halt = Cell::new(false);
    let graph = ctx.graph;
    let params = ctx.params;
    let cypher_version = ctx.cypher_version;
    let accept = |row2: Row, used2: UsedSet| -> Result<bool> {
        seek(
            ExecCtx::new(graph, params, cypher_version),
            patterns,
            idx + 1,
            row2,
            used2,
            residual,
            pred,
        )
    };
    // The one engine fork is `match_one_path`'s — this probe routes
    // through it with a probe as its rider (F-1-3), never a fork of its
    // own. A GPM pattern (a quantified element, a path mode or selector,
    // an element or path predicate — every var-length spelling among
    // them) walks the GPM engine with the early-exit rider: the first
    // complete row the chain accepts halts the enumeration, so the
    // remaining frontier is never evaluated (the ADR-0471 Consequences
    // follow-up; `match_gpm_path` applies the residual's ready conjuncts
    // at the walk's binding points and the rest on each offered row). A
    // classic pattern walks the classic engine, where the same rider
    // becomes the probe sink — the same early exit, one spelling of the
    // engine choice (LABS-161: the first-hop rel seek narrows the
    // probe's walk the same way it narrows every classic match's; a Gone
    // seek probes false).
    let probe = PathProbe::new(&accept, &halt, &patterns[idx]);
    let mut rider = Some(Rider::Probe(&probe));
    // The probe's tail is the carry (`MatchTail::Carry`,
    // pattern_policies): the named fact that a probe route never reduces
    // — a probe answers a verdict through its halt cell, and no probe
    // consumer reads the tail. The filters resolve once per probe
    // (LABS-801), beside the probe's own construction.
    let filters =
        PathFilters::resolve(ctx.graph, &patterns[idx], MatchMode::DifferentRelationships);
    match_one_path(
        ctx,
        &patterns[idx],
        &filters,
        row,
        used,
        MatchMode::DifferentRelationships,
        residual,
        rider.as_mut(),
        MatchTail::Carry,
    )?;
    Ok(halt.get())
}

struct PlainMatch<'a> {
    patterns: &'a [PathPattern],
    where_clause: Option<&'a Expr>,
}

/// A non-optional, non-search `MATCH` in `DIFFERENT RELATIONSHIPS` mode.
/// The one decoder of the plain-MATCH vocabulary (optional, search, walk
/// mode): every eligibility in this module — the boolean-count span, the
/// probe ladder's endpoint rung, the depth-first walk — reads it, never a
/// second spelling of the discrimination.
fn plain_match(clause: &Clause) -> Option<PlainMatch<'_>> {
    let Clause::Match {
        optional,
        match_mode,
        patterns,
        where_clause,
        search,
        using_index: _,
        using_join: _,
    } = clause
    else {
        return None;
    };
    if *optional || search.is_some() || *match_mode != MatchMode::DifferentRelationships {
        return None;
    }
    Some(PlainMatch {
        patterns,
        where_clause: where_clause.as_ref(),
    })
}

struct BoolCount<'a> {
    patterns: Vec<PathPattern>,
    wheres: Vec<Expr>,
    /// MATCH clauses after the one currently being dispatched.
    extra_matches: usize,
    output: String,
    counted: &'a Expr,
}

/// One or more plain `MATCH` clauses followed by an ungrouped
/// `RETURN count(DISTINCT expr) > 0` or `>= 1` — the threshold literal or
/// a parameter resolving to it (see [`counted_existence`]).
fn boolean_count_span<'a>(
    clauses: &'a [Clause],
    params: &BTreeMap<String, Value>,
) -> Option<BoolCount<'a>> {
    let mut patterns = Vec::new();
    let mut wheres = Vec::new();
    let mut matches = 0usize;
    for clause in clauses {
        match clause {
            Clause::Match { .. } => {
                let plain = plain_match(clause)?;
                if plain.patterns.is_empty() {
                    return None;
                }
                patterns.extend(plain.patterns.iter().cloned());
                if let Some(where_clause) = plain.where_clause {
                    wheres.push(where_clause.clone());
                }
                matches += 1;
            }
            Clause::Return {
                distinct,
                group_by,
                items,
                order,
                skip,
                limit,
            } => {
                if matches == 0
                    || *distinct
                    || group_by.is_some()
                    || !order.is_empty()
                    || skip.is_some()
                    || limit.is_some()
                    || items.len() != 1
                {
                    return None;
                }
                let Projection { expr, .. } = &items[0];
                let counted = counted_existence(expr, params)?;
                return Some(BoolCount {
                    patterns,
                    wheres,
                    extra_matches: matches - 1,
                    output: items[0].output_name(0),
                    counted,
                });
            }
            _ => return None,
        }
    }
    None
}

/// `count(DISTINCT expr) > 0` or `count(DISTINCT expr) >= 1` — the
/// threshold as a literal, or as a parameter that resolves to it for this
/// execution (ADR-0503: an auto-parameterized `> $__AUTO_INT_n__` binds 0
/// or 1 here exactly as the spelled literal did, and a caller's own
/// `> $threshold` gains the same recognition; a parameter resolving to
/// anything else, or absent, keeps the full-count path — the safe arm of
/// every decline in this recognizer).
fn counted_existence<'a>(expr: &'a Expr, params: &BTreeMap<String, Value>) -> Option<&'a Expr> {
    let Expr::Binary { op, left, right } = expr else {
        return None;
    };
    let threshold = match op {
        BinaryOp::Gt => 0,
        BinaryOp::Gte => 1,
        _ => return None,
    };
    // The one constant policy (`Expr::seek_value`: literals fold,
    // parameters resolve) — a non-int literal or a non-int/missing
    // parameter declines here exactly as before.
    if right.seek_value(params) != Some(Value::Int(threshold)) {
        return None;
    }
    let Expr::Function {
        name,
        distinct,
        args,
    } = left.as_ref()
    else {
        return None;
    };
    if !*distinct
        || args.len() != 1
        || name.with_str(classify_aggregate) != Some(AggregateKind::Count)
    {
        return None;
    }
    Some(&args[0])
}

/// `Ok(None)` keeps the full-match path. The returned fold already includes
/// the consumed `RETURN`.
pub(crate) fn try_fold_boolean_count(
    graph: &ExecGraph<'_>,
    rows: &[Row],
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Option<FoldedReturn>> {
    let Some(span) = boolean_count_span(clauses, params) else {
        return Ok(None);
    };
    // WHERE conjuncts are the residual on the walk. This predicate only
    // decides whether the counted expression is non-null.
    let pred = ExistencePred::Counted(span.counted);
    let mut hit = false;
    for row in rows {
        graph.cancel_tick()?;
        match patterns_exist(
            ExecCtx::new(graph, params, cypher_version),
            &span.patterns,
            &span.wheres,
            row.clone(),
            pred,
        )? {
            None => return Ok(None),
            Some(true) => {
                hit = true;
                break;
            }
            Some(false) => {}
        }
    }
    let mut out = Row::new();
    out.insert(span.output.clone(), Value::Bool(hit));
    Ok(Some(FoldedReturn {
        result: super::QueryResult::intermediate(
            vec![span.output],
            vec![vec![Value::Bool(hit)]],
            graph.stats(),
        ),
        rows: vec![out],
        // Remaining MATCH clauses plus the RETURN this fold consumed.
        consumed_after: span.extra_matches + 1,
    }))
}

/// `EXISTS { MATCH ... }` bodies of plain (non-optional, non-search,
/// WHERE-less, DIFFERENT RELATIONSHIPS) MATCH clauses, one pattern or
/// many — the one clause-level probe ladder. The clauses decode through
/// [`plain_match`] exactly once here; a decoded list holding exactly one
/// pattern answers on the LABS-193 endpoint probe first, and a declined
/// shape (multi-hop, var-length, GPM) and every other eligible body walk
/// the depth-first probe, which stops at the first hit. `Ok(None)` keeps
/// the subquery engine; the empty-patterns decline is
/// [`patterns_exist`]'s own gate, not spelled again here.
pub(crate) fn probe_multi_pattern_exists(
    graph: &ExecGraph<'_>,
    row: &Row,
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Option<bool>> {
    let mut patterns = Vec::new();
    for clause in clauses {
        let Some(plain) = plain_match(clause) else {
            return Ok(None);
        };
        if plain.where_clause.is_some() {
            return Ok(None);
        }
        patterns.extend(plain.patterns.iter().cloned());
    }
    // The LABS-193 endpoint rung. GPM eligibility is decided where it is
    // owned: needs_gpm in resolved_endpoints_exist (the path-level
    // gates), the walk mode in the plain-MATCH vocabulary plain_match
    // owns. A decline falls through to the depth-first walk below —
    // through the GPM engine with the early-exit rider for GPM shapes
    // (ADR-0471's lost early exit restored), through the classic
    // ProbeSink otherwise.
    if let [pattern] = &patterns[..] {
        if let Some(hit) = resolved_endpoints_exist(graph, row, pattern, params, cypher_version)? {
            return Ok(Some(hit));
        }
    }
    patterns_exist(
        ExecCtx::new(graph, params, cypher_version),
        &patterns,
        &[],
        row.clone(),
        ExistencePred::Any,
    )
}

#[cfg(test)]
mod threshold_tests {
    use std::collections::BTreeMap;

    use relate_core::Value;
    use relate_ir::{BinaryOp, Expr};

    use super::counted_existence;

    fn count_cmp(op: BinaryOp, bound: Expr) -> Expr {
        Expr::Binary {
            op,
            left: Box::new(Expr::Function {
                name: "count".into(),
                distinct: true,
                args: vec![Expr::Variable("n".into())],
            }),
            right: Box::new(bound),
        }
    }

    fn count_gt(bound: Expr) -> Expr {
        count_cmp(BinaryOp::Gt, bound)
    }

    fn no_params() -> BTreeMap<String, Value> {
        BTreeMap::new()
    }

    fn params(name: &str, value: Value) -> BTreeMap<String, Value> {
        BTreeMap::from([(name.to_string(), value)])
    }

    /// The literal thresholds keep their recognition: `> 0` and `>= 1`.
    #[test]
    fn literal_thresholds_recognize() {
        assert!(counted_existence(&count_gt(Expr::Int(0)), &no_params()).is_some());
        let gte = count_cmp(BinaryOp::Gte, Expr::Int(1));
        assert!(counted_existence(&gte, &no_params()).is_some());
        // Any other literal declines.
        assert!(counted_existence(&count_gt(Expr::Int(1)), &no_params()).is_none());
    }

    /// ADR-0503: a parameter resolving to the threshold recognizes exactly
    /// as the literal did — the auto-parameterized `> $__AUTO_INT_n__`
    /// binds 0, and a caller's own `> $zero` gains the same early exit.
    /// Any other value, a missing parameter, or a non-integer binding
    /// keeps the full-count path.
    #[test]
    fn parameter_thresholds_resolve_or_decline() {
        let gt_param = count_gt(Expr::Parameter("zero".into()));
        assert!(counted_existence(&gt_param, &params("zero", Value::Int(0))).is_some());
        assert!(counted_existence(&gt_param, &params("zero", Value::Int(1))).is_none());
        assert!(counted_existence(&gt_param, &params("other", Value::Int(0))).is_none());
        assert!(counted_existence(&gt_param, &params("zero", Value::String("0".into()))).is_none());
        assert!(counted_existence(&gt_param, &no_params()).is_none());
    }
}
