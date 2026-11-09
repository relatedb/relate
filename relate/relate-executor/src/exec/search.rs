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
use crate::eval::eval;
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
    cypher_version: Option<u32>,
    clause: &str,
) -> Result<usize> {
    match eval(graph, input, expr, params, cypher_version)? {
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
    let ExecCtx {
        graph,
        params,
        cypher_version,
        ..
    } = ctx;
    // The shared SEARCH index resolution (stellar round 3, std-6): the
    // schema read, the missing-index policy, and the wrong-kind
    // rejection are the one spelling in `exec::candidates`, shared with
    // the vector path. The fulltext scan reads no options, so the
    // resolution's options half is discarded here.
    let (detail, _options) = resolve_search_index(
        graph,
        &index.index_name,
        crate::redo::IndexKindSpec::Fulltext,
    )?;
    // The per-row `WITH ANALYZER` expression (CIP-225 Rule 7,
    // FulltextSearchAcceptance [57]); an absent slot means the standard
    // analyzer, and the value's own NULL/unknown-name/type rejections are
    // `Analyzer::from_value`'s.
    let analyzer = match &index.analyzer {
        Some(expr) => {
            fulltext::Analyzer::from_value(&eval(graph, input, expr, params, cypher_version)?)?
        }
        None => fulltext::Analyzer::Standard,
    };
    let kind = EntityKind::from_relationship_flag(detail.relationship);
    let candidates = fulltext::search(graph, &detail, query, analyzer, offset, limit)
        .into_iter()
        .map(|(id, score)| (materialize_candidate(graph, kind, id), score))
        .collect();
    Ok((kind, candidates))
}

/// One source specification's ranked candidate list: the entity kind of
/// the source's index(es), plus the materialized `(entity, score)` rows,
/// best-first. `None`: the source's query evaluated absent.
type SpecCandidates = Option<(EntityKind, Vec<(Value, f64)>)>;

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

/// The candidates one CIP-278 `<source specification clause>` yields for
/// one incoming row of the driving table, ranked best-first
/// ([`ranked_better_cmp`]'s order: score descending, entity id ascending).
/// `None`: the source's query evaluated absent (a NULL or non-list VECTOR
/// query), so the source contributes no rows -- the caller applies the
/// OPTIONAL rule, exactly as the pre-CIP-278 top-level index path did.
fn search_spec_candidates(
    ctx: ExecCtx<'_>,
    input: &Row,
    spec: &SearchSpec,
) -> Result<SpecCandidates> {
    match spec {
        SearchSpec::Index(index) => search_index_candidates(ctx, input, index),
        SearchSpec::Combine(combine) => search_combine_candidates(ctx, input, combine),
    }
}

/// One CIP-225 `<index specification clause>`'s candidates for one
/// incoming row -- the pre-CIP-278 top-level index behavior, unchanged,
/// now also serving as a weighted source inside a COMBINE.
fn search_index_candidates(
    ctx: ExecCtx<'_>,
    input: &Row,
    index: &SearchIndexSpec,
) -> Result<SpecCandidates> {
    let ExecCtx {
        graph,
        params,
        cypher_version,
        ..
    } = ctx;
    let limit = eval_window_int(
        graph,
        input,
        &index.limit_expr,
        params,
        cypher_version,
        "LIMIT",
    )?;
    // CIP-225 `<offset clause>`, shared by both kinds.
    let offset = match &index.offset_expr {
        Some(expr) => eval_window_int(
            graph,
            input,
            expr.as_ref(),
            params,
            cypher_version,
            "SKIP/OFFSET",
        )?,
        None => 0,
    };
    // The FOR expression is the query for both kinds — evaluated once,
    // each arm only interprets the resulting value.
    let query = eval(graph, input, &index.for_expr, params, cypher_version)?;
    match index.kind {
        SearchKind::Vector => {
            let Some(query) = vector_operand(&query)? else {
                return Ok(None);
            };
            Ok(Some(vector_index_candidates(
                ctx,
                &index.index_name,
                &query,
                offset,
                limit,
                VectorSearchFilter {
                    filters: &index.filters,
                    input,
                },
            )?))
        }
        SearchKind::Fulltext => {
            // CIP-225: the fulltext query is a STRING (the per-index
            // analyzer decides its tokenization); the operand read is
            // `fulltext::operand`, the vector arm's `vector_operand` twin.
            let Some(query) = fulltext::operand(&query)? else {
                return Ok(None);
            };
            Ok(Some(fulltext_index_candidates(
                ctx, input, index, query, offset, limit,
            )?))
        }
    }
}

/// One CIP-278 `<combined specification clause>`'s fused candidates for
/// one incoming row: weighted reciprocal rank fusion (WRRF) over the
/// ranked lists of its weighted sources (which may themselves be nested
/// combined specifications, Example P3). Each source's own list is ranked
/// after its own SKIP/OFFSET/LIMIT window; a source's rank-`i` entry
/// (0-based) contributes `weight / (constant + i + 1)` (the CIP's
/// `weight / (rrfConstant + rankIndex + 1)`), and an entity missing from a
/// source contributes nothing from that source. The fused list is ordered
/// by the fused score (the CIP's documented tie-breaker was removed --
/// implementation-dependent, CLG-102; this engine ties on the entity id,
/// ascending, via [`ranked_better_cmp`]) and windowed by the combined
/// specification's own optional LIMIT (Rule 7). `None`: every source's
/// query evaluated absent.
fn search_combine_candidates(
    ctx: ExecCtx<'_>,
    input: &Row,
    combine: &SearchCombineSpec,
) -> Result<SpecCandidates> {
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
        let Some((source_kind, candidates)) = search_spec_candidates(ctx, input, &source.spec)?
        else {
            continue;
        };
        // "Compatibility of indexes"/Example O1.1: every source must index
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
        for (rank, (candidate, _score)) in candidates.into_iter().enumerate() {
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
    // Rule 7: the combined specification's own optional LIMIT keeps the n
    // highest-scoring fused results.
    let ExecCtx {
        graph,
        params,
        cypher_version,
        ..
    } = ctx;
    let limit = match &combine.limit_expr {
        Some(expr) => eval_window_int(graph, input, expr, params, cypher_version, "LIMIT")?,
        None => usize::MAX,
    };
    let candidates = ranked
        .into_iter()
        .take(limit)
        .map(|(_id, score, candidate)| (candidate, score))
        .collect();
    Ok(Some((kind, candidates)))
}

/// Evaluates a CIP-278 Rule 1/Rule 3 control expression (a WEIGHT or the
/// WRRF WITH CONSTANT): INTEGER NOT NULL or FLOAT NOT NULL, at least 0 and
/// at most Java's `Integer.MAX_VALUE` (2147483647). Any other value (NULL
/// included) is the CIP's invalid-type error.
fn eval_wrrf_number(ctx: ExecCtx<'_>, input: &Row, expr: &Expr, clause: &str) -> Result<f64> {
    let ExecCtx {
        graph,
        params,
        cypher_version,
        ..
    } = ctx;
    let value = eval(graph, input, expr, params, cypher_version)?;
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
        // The CIP-278 source specification's candidate list -- one index
        // search or a COMBINE's fused list, ranked best-first. `None`: the
        // source's query evaluated absent, so it contributes no rows and
        // the single OPTIONAL rule below covers the input row (exactly the
        // pre-CIP-278 null-vector-query behavior).
        let mut produced = 0;
        if deferred {
            // The source reads a variable bound by this same MATCH, so match
            // the pattern first (binding the search variable freely) and keep
            // the rows whose binding is among the row's own candidates.
            let matched = match_clause(
                ctx,
                vec![input.clone()],
                false,
                match_mode,
                patterns,
                where_clause,
                None,
                None,
            )?;
            for row in matched {
                let Some((index_kind, candidates)) =
                    search_spec_candidates(ctx, &row, &search.spec)?
                else {
                    continue;
                };
                check_binding_kind(binding_kind, index_kind)?;
                let score = row.get_name(&search.binding_var).and_then(|bound| {
                    candidates
                        .iter()
                        .find(|(candidate, _)| bound.eq_non_null(candidate))
                        .map(|(_, score)| *score)
                });
                let Some(score) = score else { continue };
                let mut matched_row = row;
                attach_score(
                    &mut matched_row,
                    search.score_var.as_ref(),
                    Value::Float(score),
                );
                out.push(matched_row);
                produced += 1;
            }
        } else if let Some((index_kind, candidates)) =
            search_spec_candidates(ctx, &input, &search.spec)?
        {
            check_binding_kind(binding_kind, index_kind)?;
            for (candidate, score) in candidates {
                // When the binding variable is already bound on the input row
                // (e.g. a hybrid query composing two SEARCH clauses through
                // WITH), a candidate must match that binding; a different
                // candidate is dropped rather than overwriting it. Verified on
                // Neo4j 2026.09.0: `MATCH (p) SEARCH p IN (VECTOR ...) WITH p
                // MATCH (p) SEARCH p IN (FULLTEXT ...)` intersects the two
                // result sets.
                if let Some(existing) = input.get_name(&search.binding_var) {
                    if !existing.is_null() && !existing.eq_non_null(&candidate) {
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
                )?;
                produced += matched.len();
                out.extend(matched);
            }
        }
        // The OPTIONAL null-row rule, stated once (stellar round 3): no
        // source match and no produced row both mean the input row rides
        // through with the score null.
        if produced == 0 && optional {
            push_optional_null_row(input, patterns, search.score_var.as_ref(), &mut out);
        }
    }
    Ok(out)
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
