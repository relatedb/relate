//! CIP-224/225/278 SEARCH MATCH execution: the per-row source
//! specification machinery -- one index search's candidate window, or a
//! COMBINE's weighted reciprocal rank fusion over two or more (possibly
//! nested) sources -- and the `search_match_clause` entry the executor's
//! dispatch calls. The vector-similarity kernels live in
//! [`crate::vector_similarity`], the vector and fulltext scans'
//! orchestration in [`super::vector`] and [`super::fulltext`], and the
//! shared scan contract — index resolution, entity enumeration, survivor
//! materialization, ranked ordering, bounded top-k window — in
//! [`crate::exec::candidates`]; this module owns the SEARCH clause
//! semantics layered over them.

use std::collections::{BTreeMap, HashMap, HashSet};

use relate_core::{expected_type_detail, value_type_name, Error, Result, Value};
use relate_ir::{
    Expr, MatchMode, PathPattern, PatternPayload, SearchClause, SearchCombineSpec, SearchIndexSpec,
    SearchKind, SearchSpec,
};
use relate_model::EntityKind;
use relate_names::SmallName;

use crate::ctx::ExecCtx;
use crate::eval::{eval, where_clause_holds};
use crate::exec::candidates::{materialize_candidate, ranked_better_cmp, resolve_search_index};
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::fulltext;
use super::match_clause::{bind_pattern_nulls, match_clause};
use super::vector::{vector_index_candidates, vector_operand, VectorSearchFilter};

/// The SEARCH clause's legacy `InvalidValueType` detail channel: the class
/// token is spelled once for the helper's users here, the call site owns
/// only the detail. `candidate_entity_id` raises the same token through the
/// `Error::execution` channel, a different error class, and deliberately
/// spells it inline rather than routing through this helper.
fn invalid_value_type(detail: impl Into<String>) -> Error {
    Error::ty(format!("InvalidValueType: {}", detail.into()))
}

/// The SEARCH binding's entity kind must agree with the index the source
/// queries. The two failures stay distinct: an unbound name (the pattern
/// binds no node/relationship of that spelling — the front admits path
/// names) is not the same mistake as a node binding against a relationship
/// index.
fn check_binding_kind(binding: Option<EntityKind>, index: EntityKind) -> Result<()> {
    match binding {
        Some(kind) if kind == index => Ok(()),
        Some(_) => Err(invalid_value_type(
            "SEARCH binding entity does not match index",
        )),
        None => Err(invalid_value_type(
            "SEARCH binding variable must be bound as a node or relationship",
        )),
    }
}

/// Attach a SEARCH score to `row` under the clause's `SCORE AS` variable.
/// `None` (no score variable) leaves the row untouched; a `Value::Null`
/// score is the OPTIONAL null-row case.
fn attach_score(row: &mut Row, score_var: Option<&SmallName>, score: Value) {
    if let Some(score_var) = score_var {
        row.insert(score_var.to_string(), score);
    }
}

/// Evaluate a SEARCH window expression (`LIMIT` or `SKIP`/`OFFSET`),
/// requiring a non-negative INTEGER within `i32` range.
fn eval_window_int(
    graph: &ExecGraph<'_>,
    input: &Row,
    expr: &Expr,
    params: &BTreeMap<String, Value>,
    clause: &str,
) -> Result<usize> {
    match eval(graph, input, expr, params)? {
        Value::Int(value) if (0..=i32::MAX as i64).contains(&value) => Ok(value as usize),
        _ => Err(Error::syntax(format!(
            "NumberOutOfRange: SEARCH {clause} must be an INTEGER NOT NULL"
        ))),
    }
}

/// CIP-225 fulltext candidate generation: the shared [`super::fulltext`]
/// scan over the named FULLTEXT index ranks the matches by its live-fitted
/// BM25 score, then the CIP-225 offset window and LIMIT page them.
///
/// WHERE is rejected for FULLTEXT SEARCH by the front-end validator (Neo4j
/// error 42N14), so there is no inner filter here.
fn fulltext_index_candidates(
    ctx: ExecCtx<'_>,
    input: &Row,
    index: &SearchIndexSpec,
    query: &str,
    offset: usize,
    limit: usize,
) -> Result<(EntityKind, Vec<(Value, f64)>)> {
    let ExecCtx { graph, params, .. } = ctx;
    // The shared SEARCH index resolution (stellar round 3, std-6): the
    // schema read, the missing-index policy, and the wrong-kind
    // rejection are the one spelling in `exec::candidates`, shared with
    // the vector path. The options carry the index's analyzer.
    let (detail, options) = resolve_search_index(
        graph,
        &index.index_name,
        crate::redo::IndexKindSpec::Fulltext,
    )?;
    // The per-row `WITH ANALYZER` expression
    // (FulltextSearchAcceptance); an absent slot means the standard
    // analyzer, and the value's own NULL/unknown-name/type rejections are
    // `Analyzer::from_value`'s.
    let query_override = index
        .analyzer
        .as_ref()
        .map(|expr| fulltext::Analyzer::from_value(&eval(graph, input, expr, params)?))
        .transpose()?;
    let analyzers = fulltext::Analyzers::new(
        crate::exec::ddl_options::index_analyzer(&options, &index.index_name)?,
        query_override,
    );
    let kind = EntityKind::from_relationship_flag(detail.relationship);
    let candidates = fulltext::search(graph, &detail, query, analyzers, offset, limit)?
        .into_iter()
        .map(|(id, score)| (materialize_candidate(graph, kind, id), score))
        .collect();
    Ok((kind, candidates))
}

/// One source specification's ranked candidate list: the entity kind of
/// the source's index(es), the materialized `(entity, score)` rows,
/// best-first, the source's own LIMIT, and whether a wider window could
/// yield deeper candidates.
struct SpecCandidates {
    kind: EntityKind,
    candidates: Vec<(Value, f64)>,
    /// The source's own LIMIT: how many candidates the SEARCH clause
    /// keeps, counted over the ones that survive the pattern and WHERE.
    limit: usize,
    /// The candidate list came back as deep as the index goes (or the
    /// source cannot widen): the caller's growth loop stops.
    may_widen: bool,
}

/// The OPTIONAL rule's null row for one input row: the pattern's bound
/// variables nulled, the score column (if named) nulled, pushed once --
/// the one shape both OPTIONAL sites in `search_match_clause` emit.
fn push_optional_null_row(
    mut row: Row,
    patterns: &[PathPattern],
    score_var: Option<&SmallName>,
    out: &mut Vec<Row>,
) {
    bind_pattern_nulls(&mut row, patterns);
    attach_score(&mut row, score_var, Value::Null);
    out.push(row);
}

/// The candidates one `<source specification clause>` yields for
/// one incoming row of the driving table, ranked best-first
/// ([`ranked_better_cmp`]'s order: score descending, entity id ascending).
/// `None`: the source's query evaluated absent (a NULL or non-list VECTOR
/// query), so the source contributes no rows -- the caller applies the
/// OPTIONAL rule, exactly as the pre-CIP-278 top-level index path did.
///
/// `extra_candidates` is the caller's top-K growth demand: the window is
/// widened by that many entries so a value the pattern filtered out can
/// be replaced by the next-ranked one. Only an index source widens -- a
/// COMBINE's source LIMITs are fusion inputs (they fix each source's
/// ranks), so its fused list keeps its own window.
fn search_spec_candidates(
    ctx: ExecCtx<'_>,
    input: &Row,
    spec: &SearchSpec,
    extra_candidates: usize,
) -> Result<Option<SpecCandidates>> {
    match spec {
        SearchSpec::Index(index) => search_index_candidates(ctx, input, index, extra_candidates),
        SearchSpec::Combine(combine) => search_combine_candidates(ctx, input, combine),
    }
}

/// One `<index specification clause>`'s candidates for one
/// incoming row -- the pre-CIP-278 top-level index behavior, unchanged,
/// now also serving as a weighted source inside a COMBINE.
fn search_index_candidates(
    ctx: ExecCtx<'_>,
    input: &Row,
    index: &SearchIndexSpec,
    extra_candidates: usize,
) -> Result<Option<SpecCandidates>> {
    let ExecCtx { graph, params, .. } = ctx;
    let limit = eval_window_int(graph, input, &index.limit_expr, params, "LIMIT")?;
    // The caller's growth demand widens the fetched window only; the
    // clause's LIMIT stays the number of survivors it keeps.
    let window = limit.saturating_add(extra_candidates);
    // The `<offset clause>`, shared by both kinds.
    let offset = match &index.offset_expr {
        Some(expr) => eval_window_int(graph, input, expr.as_ref(), params, "SKIP/OFFSET")?,
        None => 0,
    };
    // The FOR expression is the query for both kinds — evaluated once,
    // each arm only interprets the resulting value.
    let query = eval(graph, input, &index.for_expr, params)?;
    let (kind, candidates) = match index.kind {
        SearchKind::Vector => {
            let Some(query) = vector_operand(&query)? else {
                return Ok(None);
            };
            vector_index_candidates(
                ctx,
                &index.index_name,
                &query,
                offset,
                window,
                VectorSearchFilter {
                    filters: &index.filters,
                    input,
                },
            )?
        }
        SearchKind::Fulltext => {
            // CIP-225: the fulltext query is a STRING (the per-index
            // analyzer decides its tokenization); the operand read is
            // `fulltext::operand`, the vector arm's `vector_operand` twin.
            let Some(query) = fulltext::operand(&query)? else {
                return Ok(None);
            };
            // #61's scan machinery (analyzers, live BM25) at this
            // branch's growth-loop depth: the window widens past the
            // source's own LIMIT, which SpecCandidates carries for the
            // survivor count.
            fulltext_index_candidates(ctx, input, index, query, offset, window)?
        }
    };
    // A full window means the scan may hold deeper candidates (the scan
    // reports every passing candidate up to the offset+window it kept).
    let may_widen = candidates.len() == window;
    Ok(Some(SpecCandidates {
        kind,
        candidates,
        limit,
        may_widen,
    }))
}

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
/// ascending, via [`ranked_better_cmp`]) and windowed by the combined
/// specification's own optional LIMIT (the n best fused results).
/// `None`: every source's query evaluated absent.
fn search_combine_candidates(
    ctx: ExecCtx<'_>,
    input: &Row,
    combine: &SearchCombineSpec,
) -> Result<Option<SpecCandidates>> {
    // Rules 3/4: the dampening constant, default 60.
    let constant = match &combine.constant_expr {
        Some(expr) => eval_wrrf_number(ctx, input, expr, "CONSTANT")?,
        None => 60.0,
    };
    let mut kind: Option<EntityKind> = None;
    // The fused scores keyed by entity id, so a later source adds its
    // contribution to the same entity instead of duplicating it; the
    // final order comes from the total sort below.
    let mut fused: HashMap<i64, (f64, Value)> = HashMap::new();
    for source in &combine.sources {
        // An absent source query contributes no ranks -- the CIP's
        // "missing from a source" rule (zero contribution).
        let Some(fetched) = search_spec_candidates(ctx, input, &source.spec, 0)? else {
            continue;
        };
        let source_kind = fetched.kind;
        // "Compatibility of indexes": every source must index
        // the same entity kind as the first.
        match kind {
            Some(kind) => {
                if kind != source_kind {
                    return Err(invalid_value_type(expected_type_detail(
                        kind.display_name(),
                        source_kind.display_name(),
                    )));
                }
            }
            None => kind = Some(source_kind),
        }
        // Rules 1/2: the source weight, default 1 (not normalized).
        let weight = match &source.weight_expr {
            Some(expr) => eval_wrrf_number(ctx, input, expr, "WEIGHT")?,
            None => 1.0,
        };
        for (rank, (candidate, _score)) in fetched.candidates.into_iter().enumerate() {
            let id = candidate_entity_id(&candidate)?;
            let contribution = weight / (constant + rank as f64 + 1.0);
            fused
                .entry(id)
                .and_modify(|(score, _)| *score += contribution)
                .or_insert((contribution, candidate));
        }
    }
    let Some(kind) = kind else {
        return Ok(None);
    };
    // The accumulate phase ends and the rank/window phase begins —
    // the binding name says so.
    let mut ranked: Vec<(i64, f64, Value)> = fused
        .into_iter()
        .map(|(id, (score, candidate))| (id, score, candidate))
        .collect();
    ranked.sort_by(|a, b| ranked_better_cmp(b.1, b.0, a.1, a.0));
    // The combined specification's own optional LIMIT keeps the n
    // highest-scoring fused results.
    let ExecCtx { graph, params, .. } = ctx;
    let limit = match &combine.limit_expr {
        Some(expr) => eval_window_int(graph, input, expr, params, "LIMIT")?,
        None => usize::MAX,
    };
    let candidates = ranked
        .into_iter()
        .take(limit)
        .map(|(_id, score, candidate)| (candidate, score))
        .collect();
    Ok(Some(SpecCandidates {
        kind,
        candidates,
        limit,
        // The fused window is a user constraint (keep the n best
        // fused results), not a fetched slice, so it never widens.
        may_widen: false,
    }))
}

/// Evaluates a combined specification's control expression (a WEIGHT or the
/// WRRF WITH CONSTANT): INTEGER NOT NULL or FLOAT NOT NULL, at least 0 and
/// at most Java's `Integer.MAX_VALUE` (2147483647). Any other value (NULL
/// included) is the authored invalid-type error.
fn eval_wrrf_number(ctx: ExecCtx<'_>, input: &Row, expr: &Expr, clause: &str) -> Result<f64> {
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

/// The entity id of one search candidate -- the WRRF fusion key. A
/// non-entity candidate is the same catalog-drift contract the candidate
/// seek already carries, not a silently skipped entry.
fn candidate_entity_id(candidate: &Value) -> Result<i64> {
    match candidate {
        Value::Node(node) => Ok(node.id),
        Value::Relationship(rel) => Ok(rel.id),
        _ => Err(Error::execution(
            "InvalidValueType: SEARCH candidate is not a node or relationship",
        )),
    }
}

/// Whether the SEARCH source specification reads a variable this MATCH's
/// own pattern binds (a sibling component, `(seed:Person), (p:Person)
/// SEARCH p IN (… FOR seed.bio …)`). Such a source cannot be evaluated
/// against the incoming row; the clause then matches the pattern first and
/// filters the bound binding variable against the per-row candidate list.
fn search_reads_pattern(search: &SearchClause, patterns: &[PathPattern]) -> bool {
    let mut pattern_names = HashSet::new();
    for pattern in patterns {
        relate_ir::path_pattern_mentions(pattern, &mut pattern_names);
    }
    let mut spec_vars = HashSet::new();
    for expr in search.spec.exprs() {
        relate_ir::expr_vars(expr, &mut spec_vars);
    }
    spec_vars.iter().any(|name| pattern_names.contains(name))
}

/// Which entity kind this MATCH's pattern binds `name` as, if any. The IR's
/// canonical [`relate_ir::walk_pattern_payloads`] dispatch owns the
/// compact-view-vs-elements precedence, so the answer cannot drift from
/// every other pattern reader. `None`: no node or relationship payload
/// carries that binding — a path name, or a name the pattern never binds.
fn path_pattern_binding_kind(patterns: &[PathPattern], name: &str) -> Option<EntityKind> {
    patterns.iter().find_map(|pattern| {
        let mut kind = None;
        relate_ir::walk_pattern_payloads(pattern, &mut |payload| match payload {
            PatternPayload::Node(node) if node.variable.as_ref().is_some_and(|v| v == name) => {
                kind = kind.or(Some(EntityKind::Node));
            }
            PatternPayload::Relationship(rel)
                if rel.variable.as_ref().is_some_and(|v| v == name) =>
            {
                kind = kind.or(Some(EntityKind::Relationship));
            }
            PatternPayload::Node(_)
            | PatternPayload::Relationship(_)
            | PatternPayload::Quantified(_) => {}
        });
        kind
    })
}

pub(crate) fn search_match_clause(
    ctx: ExecCtx<'_>,
    rows: Vec<Row>,
    optional: bool,
    match_mode: MatchMode,
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    search: &SearchClause,
) -> Result<Vec<Row>> {
    // The binding's own typed kind (stellar round 2, std-2): compare the
    // enum directly, no bool projection of either side. A pure function of
    // the pattern and the binding name -- loop-invariant, so it is resolved
    // once, not per input row, through the IR's canonical pattern walk.
    let binding_kind = search
        .binding_var
        .with_str(|text| path_pattern_binding_kind(patterns, text));
    let deferred = search_reads_pattern(search, patterns);
    let mut out = Vec::new();
    for input in rows {
        let matched = if deferred {
            // The source reads a variable bound by this same MATCH, so match
            // the pattern first (binding the search variable freely) and keep
            // the rows whose binding is among the row's own candidates.
            deferred_search_rows(
                ctx,
                &input,
                binding_kind,
                match_mode,
                patterns,
                where_clause,
                search,
            )?
        } else {
            direct_search_rows(
                ctx,
                &input,
                binding_kind,
                match_mode,
                patterns,
                where_clause,
                search,
            )?
        };
        // The OPTIONAL null-row rule, stated once (stellar round 3): no
        // source match and no produced row both mean the input row rides
        // through with the score null.
        if matched.is_empty() && optional {
            push_optional_null_row(input, patterns, search.score_var.as_ref(), &mut out);
        } else {
            out.extend(matched);
        }
    }
    Ok(out)
}

/// The top-K growth policy, the one spelling both search paths follow:
/// only a complex pattern widens its window (a plain pattern's LIMIT is a
/// hard candidate window), and only while a wider window can reach a
/// candidate the current one cannot — always true in the direct path
/// (the binding is seeded per candidate, so a pre-bound binding's
/// partner may sit arbitrarily deep), never true in the deferred path
/// once the binding is pre-bound (its rows already carry their only
/// possible candidate, so no wider window can add a survivor).
fn top_k_grows(search: &SearchClause, wider_reaches_new_candidate: bool) -> bool {
    search.complex_pattern && wider_reaches_new_candidate
}

/// The top-K completion test, stated once for both paths: the growth
/// loop ends when the clause does not grow, the index cannot widen, or
/// enough candidates have a surviving row.
fn top_k_complete(grows: bool, may_widen: bool, survivors: usize, needed: usize) -> bool {
    !grows || !may_widen || survivors >= needed
}

/// The next growth round's extra-candidate budget: geometric doubling
/// from `base` — which is at least 1, guaranteed by the caller's
/// zero-LIMIT break — so the window grows toward the index's depth in
/// logarithmic rounds.
fn widen_extra(extra: usize, base: usize) -> usize {
    extra.saturating_add(extra).max(base)
}

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
fn direct_search_rows(
    ctx: ExecCtx<'_>,
    input: &Row,
    binding_kind: Option<EntityKind>,
    match_mode: MatchMode,
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    search: &SearchClause,
) -> Result<Vec<Row>> {
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
    while let Some(fetched) = search_spec_candidates(ctx, input, &search.spec, extra)? {
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

/// The deferred shape: matching the pattern first and filtering each row's
/// binding against its own candidate list. The same complex-pattern top-K
/// rule applies — the LIMIT counts distinct candidates that have a row — so
/// a window with too few surviving candidates is widened, and the LIMIT
/// keeps the best-ranked survivors (every row of a kept candidate rides
/// along).
fn deferred_search_rows(
    ctx: ExecCtx<'_>,
    input: &Row,
    binding_kind: Option<EntityKind>,
    match_mode: MatchMode,
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    search: &SearchClause,
) -> Result<Vec<Row>> {
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
                deferred_row_binding(ctx, row, extra, binding_kind, search, &mut round)?
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
    ctx: ExecCtx<'_>,
    row: &Row,
    extra: usize,
    binding_kind: Option<EntityKind>,
    search: &SearchClause,
    round: &mut DeferredRound,
) -> Result<Option<(i64, usize, f64)>> {
    let Some(fetched) = search_spec_candidates(ctx, row, &search.spec, extra)? else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use relate_ir::{Direction, NodePattern, PatternElement, RelPattern};

    fn node(variable: &str) -> NodePattern {
        NodePattern {
            variable: Some(variable.into()),
            labels: Vec::new(),
            label_expr: None,
            properties: None,
            prop_keys: Vec::new(),
            where_clause: None,
            force_scan: false,
        }
    }

    fn rel(variable: &str) -> RelPattern {
        RelPattern {
            variable: Some(variable.into()),
            types: Vec::new(),
            type_expr: None,
            direction: Direction::Outgoing,
            properties: None,
            prop_keys: Vec::new(),
            var_length: false,
            min_hops: 0,
            max_hops: None,
            where_clause: None,
            quantified: false,
        }
    }

    fn pattern(nodes: Vec<NodePattern>, rels: Vec<RelPattern>) -> PathPattern {
        PathPattern {
            variable: None,
            subpath_variable: None,
            elements: Vec::new(),
            nodes,
            rels,
            where_clause: None,
            mode: None,
            selector: None,
            is_shortest_path_function: false,
        }
    }

    /// The binding kind comes from the canonical pattern walk, so both the
    /// compact view and the elements sequence resolve the same name, and a
    /// name no node/relationship binds (a path name) is `None`.
    #[test]
    fn binding_kind_reads_the_canonical_pattern_walk() {
        let compact = pattern(vec![node("n")], vec![rel("r")]);
        assert_eq!(
            path_pattern_binding_kind(std::slice::from_ref(&compact), "n"),
            Some(EntityKind::Node)
        );
        assert_eq!(
            path_pattern_binding_kind(std::slice::from_ref(&compact), "r"),
            Some(EntityKind::Relationship)
        );
        assert_eq!(
            path_pattern_binding_kind(std::slice::from_ref(&compact), "p"),
            None
        );
        let elements = PathPattern {
            elements: vec![
                PatternElement::Node(node("n")),
                PatternElement::Relationship(rel("r")),
            ],
            ..pattern(Vec::new(), Vec::new())
        };
        assert_eq!(
            path_pattern_binding_kind(&[elements], "r"),
            Some(EntityKind::Relationship)
        );
    }

    /// F-2-5: an unbound binding name and a kind mismatch are distinct
    /// rejections, not one folded message.
    #[test]
    fn check_binding_kind_separates_mismatch_from_unbound() {
        assert!(check_binding_kind(Some(EntityKind::Node), EntityKind::Node).is_ok());
        let mismatch =
            check_binding_kind(Some(EntityKind::Relationship), EntityKind::Node).unwrap_err();
        assert!(
            mismatch.to_string().contains("does not match index"),
            "{mismatch}"
        );
        let unbound = check_binding_kind(None, EntityKind::Node).unwrap_err();
        assert!(
            unbound
                .to_string()
                .contains("must be bound as a node or relationship"),
            "{unbound}"
        );
    }

    /// F-2-4: one score-attachment policy, including the OPTIONAL null row
    /// and the unnamed-score case.
    #[test]
    fn attach_score_writes_only_a_named_score_column() {
        let mut unnamed = Row::new();
        attach_score(&mut unnamed, None, Value::Float(1.0));
        assert!(unnamed.get("score").is_none());

        let mut scored = Row::new();
        attach_score(
            &mut scored,
            Some(&SmallName::from_name("score")),
            Value::Float(1.0),
        );
        assert_eq!(scored.get("score"), Some(&Value::Float(1.0)));

        let mut nulled = Row::new();
        attach_score(
            &mut nulled,
            Some(&SmallName::from_name("score")),
            Value::Null,
        );
        assert_eq!(nulled.get("score"), Some(&Value::Null));
    }
}
