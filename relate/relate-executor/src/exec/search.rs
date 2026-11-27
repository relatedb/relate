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

use std::collections::{BTreeMap, HashSet};

use relate_core::{Error, Result, Value};
use relate_ir::{
    Expr, PathPattern, PatternPayload, SearchClause, SearchIndexSpec, SearchKind, SearchSpec,
};
use relate_model::EntityKind;
use relate_names::SmallName;

use crate::ctx::ExecCtx;
use crate::eval::{eval, filter_rows_where_clause};
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::fulltext;
use super::match_clause::{bind_pattern_nulls, MatchClauseInputs};
use super::pattern_policies::RowFold;
use super::vector::{self, vector_operand};

mod combine;
mod deferred;
mod direct;
mod growth;

use combine::combine_candidates;
use deferred::deferred_search_rows;
use direct::{direct_search_rows, pre_bound_search_rows};

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

/// What one SEARCH clause's scans share: the execution context and the
/// fulltext corpora, built on the first scan of an index and reused by every
/// later row and growth round of the clause.
pub(super) struct SearchEnv<'a> {
    ctx: ExecCtx<'a>,
    corpora: fulltext::CorpusMemo,
}

/// What one fetch answers, from either producer — an index scan's ranked
/// candidate list or a fused rank list: the entity kind the source
/// served, the `(entity, score)` rows best-first, and whether a wider
/// fetched window could reach a candidate the current one cannot. The
/// caller's window is deliberately absent — every caller holds the
/// [`SearchWindow`] it stated for the row, so a fetch result never
/// echoes it back.
pub(super) struct SpecCandidates {
    kind: EntityKind,
    candidates: Vec<(Value, f64)>,
    /// For an index fetch: `true` when the fetch filled its depth, so a
    /// wider fetch may reach deeper candidates and the growth loop
    /// continues; `false` when the scan is exhausted or the source
    /// cannot widen, and the loop stops. A fused rank list is a fixed
    /// window, so the fusion always answers `false`.
    may_widen: bool,
}

/// The OPTIONAL rule's null row for one input row: the pattern's bound
/// variables nulled, the score column (if named) nulled -- the one shape
/// `search_match_clause` emits for an input row its sources left unmatched.
fn optional_null_row(mut row: Row, patterns: &[PathPattern], score_var: Option<&SmallName>) -> Row {
    bind_pattern_nulls(&mut row, patterns);
    attach_score(&mut row, score_var, Value::Null);
    row
}

/// The growth demand one fetch carries: the growth loops widen an index
/// source's fetched depth by the widening so far ([`Growth::Widen`]) so a
/// value the pattern filtered out can be replaced by the next-ranked one;
/// the fusion's source fetches are fixed-window ([`Growth::Fixed`]) — a
/// fused list never widens, so the growth demand lives on the index arm
/// alone, never dropped by the combine arm.
#[derive(Clone, Copy, Debug)]
pub(super) enum Growth {
    /// A fixed-window fetch: no widening (the fusion's source fetches).
    Fixed,
    /// The growth loops' demand: widen the index fetch by this many
    /// entries past the window.
    Widen(usize),
}

impl Growth {
    /// The extra-candidate budget this demand adds to an index fetch.
    fn extra_candidates(self) -> usize {
        match self {
            Growth::Fixed => 0,
            Growth::Widen(extra) => extra,
        }
    }
}

/// The candidates one `<source specification clause>` yields for
/// one incoming row of the driving table, ranked best-first
/// ([`crate::exec::candidates::ranked_better_cmp`]'s order: score
/// descending, entity id ascending). `None`: the source's query
/// evaluated absent (a NULL or non-list VECTOR query), so the source
/// contributes no rows -- the caller applies the OPTIONAL rule, exactly
/// as the pre-CIP-278 top-level index path did.
fn search_spec_candidates(
    env: &SearchEnv<'_>,
    input: &Row,
    spec: &SearchSpec,
    window: SearchWindow,
    growth: Growth,
) -> Result<Option<SpecCandidates>> {
    match spec {
        SearchSpec::Index(index) => {
            search_index_candidates(env, input, index, window, growth.extra_candidates())
        }
        // A combined specification's fused list is a fixed window: its
        // sources' LIMITs are fusion inputs that fix each source's ranks,
        // and its fused results never widen — no growth reaches it.
        SearchSpec::Combine(combine) => combine_candidates(env, input, combine, window),
    }
}

/// The clause's own window as a named pair — `skip` before `limit`,
/// never a positional tuple at a call site. The `limit` field's one
/// sentinel: `usize::MAX` is an absent fused LIMIT ("keep every fused
/// result"), built only by [`SearchWindow::unbounded`] and documented
/// there — an index source's LIMIT is mandatory, so every other
/// `SearchWindow` carries a real bound.
#[derive(Clone, Copy, Debug)]
pub(super) struct SearchWindow {
    pub(super) skip: usize,
    pub(super) limit: usize,
}

impl SearchWindow {
    /// The absent fused LIMIT: keep every fused result. The one
    /// constructor of the `usize::MAX` sentinel.
    fn unbounded() -> Self {
        Self {
            skip: 0,
            limit: usize::MAX,
        }
    }

    /// The window is satisfied once `survivors` entities have been kept:
    /// the one spelling of "enough survivors", LIMIT 0 included (satisfied
    /// before any candidate is seen).
    fn satisfied(self, survivors: usize) -> bool {
        survivors >= self.limit
    }

    /// The window a pre-bound entity can never fill: a SKIP window consumes
    /// the one entity that could survive, and a LIMIT-0 window keeps
    /// nothing, so the answer is empty without fetching.
    fn empty_for_one_entity(self) -> bool {
        self.skip > 0 || self.limit == 0
    }
}

/// The clause's own window — the SKIP and LIMIT a source specification
/// states, evaluated against the input row (never a pattern binding: the
/// front rejects window expressions that read one). The one spelling of
/// the window statement: every fetch is handed the window its caller
/// stated once for that row, so no path re-derives it per growth round.
/// An index source's window is its optional SKIP (fulltext-only; the
/// front rejects SKIP on a VECTOR source — the scan never applies it,
/// the SKIP window pages over the entities the whole MATCH keeps in the
/// caller's loop) beside its mandatory LIMIT; a combined specification's
/// own window is its optional LIMIT with no SKIP (its sources' SKIP
/// pages their own ranked lists before the fusion).
fn spec_window(env: &SearchEnv<'_>, input: &Row, spec: &SearchSpec) -> Result<SearchWindow> {
    let ExecCtx { graph, params, .. } = env.ctx;
    match spec {
        SearchSpec::Index(index) => {
            let limit = eval_window_int(graph, input, &index.limit_expr, params, "LIMIT")?;
            let skip = match &index.offset_expr {
                Some(expr) => eval_window_int(graph, input, expr.as_ref(), params, "SKIP/OFFSET")?,
                None => 0,
            };
            Ok(SearchWindow { skip, limit })
        }
        SearchSpec::Combine(combine) => {
            let limit = match &combine.limit_expr {
                Some(expr) => eval_window_int(graph, input, expr, params, "LIMIT")?,
                None => return Ok(SearchWindow::unbounded()),
            };
            Ok(SearchWindow { skip: 0, limit })
        }
    }
}

/// One `<index specification clause>`'s candidates for one
/// incoming row -- the pre-CIP-278 top-level index behavior, unchanged,
/// now also serving as a weighted source inside a COMBINE. The window is
/// the caller's once-stated [`SearchWindow`] for this row (see
/// [`spec_window`]).
fn search_index_candidates(
    env: &SearchEnv<'_>,
    input: &Row,
    index: &SearchIndexSpec,
    window: SearchWindow,
    extra_candidates: usize,
) -> Result<Option<SpecCandidates>> {
    let ExecCtx { graph, params, .. } = env.ctx;
    // The fetch depth spans the source's own SKIP and LIMIT plus the
    // caller's growth demand: the growth loops need skip+limit survivors
    // from the fetched list, and a COMBINE source's list is paged raw
    // (its SKIP drops the fetched prefix, so the fetch must reach
    // skip+limit deep to keep `limit` entries — the rank semantics the
    // fusion counts on). The clause's LIMIT stays the number of survivors
    // it keeps. The window stays in its named shape — `skip` and `limit`
    // read as fields at their use sites, the family's one policy.
    let fetch_depth = window
        .skip
        .saturating_add(window.limit)
        .saturating_add(extra_candidates);
    // The FOR expression is the query for both kinds — evaluated once,
    // each arm only interprets the resulting value.
    let query = eval(graph, input, &index.for_expr, params)?;
    let (kind, candidates) = match index.kind {
        SearchKind::Vector => {
            let Some(query) = vector_operand(&query)? else {
                return Ok(None);
            };
            // The scan orchestration (index resolution, per-row filter
            // preparation, the similarity scan, survivor materialization)
            // lives in `exec/vector.rs` — the fulltext arm's twin; this
            // arm states only the growth-loop depth it asks for.
            vector::index_candidates(env.ctx, index, input, &query, fetch_depth)?
        }
        SearchKind::Fulltext => {
            // CIP-225: the fulltext query is a STRING (the per-index
            // analyzer decides its tokenization); the operand read is
            // `fulltext::operand`, the vector arm's `vector_operand` twin.
            let Some(query) = fulltext::operand(&query)? else {
                return Ok(None);
            };
            // The fulltext scan orchestration (index resolution,
            // analyzers, live BM25, the inner WHERE's single-stage gate)
            // lives beside `fulltext::search` in `exec/fulltext.rs`;
            // this arm states only the growth-loop depth it asks for.
            fulltext::index_candidates(env.ctx, &env.corpora, input, index, query, fetch_depth)?
        }
    };
    // `true` when the fetch filled its depth, so a wider fetch may reach
    // deeper candidates and the growth loop continues; `false` when the
    // scan is exhausted (fewer passing candidates than the depth) or the
    // source cannot widen, and the loop stops.
    let may_widen = candidates.len() == fetch_depth;
    Ok(Some(SpecCandidates {
        kind,
        candidates,
        may_widen,
    }))
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
    clause: MatchClauseInputs<'_>,
    search: &SearchClause,
    fold: Option<&dyn RowFold>,
) -> Result<Vec<Row>> {
    let MatchClauseInputs {
        optional,
        match_mode,
        patterns,
        where_clause,
    } = clause;
    // The binding's own typed kind (stellar round 2, std-2): compare the
    // enum directly, no bool projection of either side. A pure function of
    // the pattern and the binding name -- loop-invariant, so it is resolved
    // once, not per input row, through the IR's canonical pattern walk.
    let binding_kind = search
        .binding_var
        .with_str(|text| path_pattern_binding_kind(patterns, text));
    // The row source follows the clause facts resolved once beside
    // `binding_kind`: the source reads a variable this MATCH's own
    // pattern binds (`deferred` — the pattern matches first and each
    // row's binding is filtered against its own candidate list); else the
    // input row already binds the SEARCH variable (`pre-bound` — a
    // single-entity membership question); else each candidate seeds the
    // binding and the pattern matches per candidate (`direct`).
    let reads_pattern = search_reads_pattern(search, patterns);
    let corpora = fulltext::CorpusMemo::new(ctx.graph);
    let env = SearchEnv { ctx, corpora };
    let mut out = Vec::new();
    for input in rows {
        let source = if reads_pattern {
            deferred_search_rows
        } else if input.get_name(&search.binding_var).is_some() {
            pre_bound_search_rows
        } else {
            direct_search_rows
        };
        let mut matched = source(
            &env,
            &input,
            binding_kind,
            match_mode,
            patterns,
            where_clause,
            search,
        )?;
        // The outer WHERE's score-referencing half post-filters the kept
        // rows (ADR-0678's window split): it rides the SEARCH clause, the
        // score is on each row the paths attached, and the entity-side
        // conjuncts already ran inside them as the window's candidate
        // restriction. The canonical optional-WHERE row filter owns the
        // whole policy (a non-boolean predicate is the typed rejection,
        // never a silent drop; the empty half passes through).
        matched =
            filter_rows_where_clause(ctx.graph, ctx.params, search.post_where.as_ref(), matched)?;
        // The OPTIONAL null-row rule, stated once (stellar round 3): no
        // source match and no produced row both mean the input row rides
        // through with the score null.
        let matched = if matched.is_empty() && optional {
            vec![optional_null_row(
                input,
                patterns,
                search.score_var.as_ref(),
            )]
        } else {
            matched
        };
        // A consumer that folds rows as they are produced (the following
        // aggregation, a run of stages) takes each matched row as its
        // input row's walk yields it: the clause's output is never held
        // (ADR-0555, ADR-0660's `expand` disposition).
        for row in matched {
            match fold {
                Some(fold) => fold.offer(env.ctx.graph, row)?,
                None => out.push(row),
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::{
        exec_autocommit, exec_cypher, named_node, profiled, try_exec_cypher,
    };
    use crate::graph::Graph;
    use crate::metrics::KernelRung;
    use relate_core::CapacityKind;
    use relate_ir::{Direction, NodePattern, PatternElement, RelPattern};

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
        let compact = pattern(vec![named_node("n")], vec![rel("r")]);
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
                PatternElement::Node(named_node("n")),
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

    /// Eight `Person`s with two-dimension embeddings indexed by `vidx`: a
    /// four-entry SEARCH window has four candidates for every driving row.
    fn vector_index_graph() -> Graph {
        let g = Graph::default();
        exec_autocommit(&g, "CREATE VECTOR INDEX vidx FOR (p:Person) ON p.embed").unwrap();
        g.begin_write().unwrap();
        for i in 0..8 {
            g.create_node(
                vec!["Person".into()],
                BTreeMap::from([
                    ("name".into(), Value::String(format!("p{i}").into())),
                    (
                        "embed".into(),
                        Value::List(vec![Value::Float(1.0), Value::Float(i as f64)].into()),
                    ),
                ]),
            )
            .unwrap();
        }
        g.commit_write();
        g
    }

    /// The driving-table statement: `driving` rows each SEARCHing a
    /// four-entry window, with `tail` as the consumer.
    fn search_over_driving(driving: usize, tail: &str) -> String {
        format!(
            "UNWIND range(1, {driving}) AS i \
             MATCH (p:Person) \
             SEARCH p IN (VECTOR INDEX vidx FOR [1.0, 0.0] LIMIT 4) {tail}"
        )
    }

    /// ADR-0555/ADR-0660: a SEARCH MATCH feeding a folding consumer emits
    /// each matched row as its walk produces it, so the clause holds nothing
    /// that grows with the driving row count — 120,000 matched rows pass
    /// under a 4 MiB expression cap. The same clause holding its output for a
    /// plain `RETURN` is refused at the cap.
    #[test]
    fn a_search_match_feeding_a_fold_holds_no_rows() {
        const DRIVING: usize = 30_000;
        const CAP: u64 = 4 * 1024 * 1024;
        let mut g = vector_index_graph();
        g.configure_expression_memory_limit(Some(CAP));
        let rows = exec_cypher(&g, &search_over_driving(DRIVING, "RETURN count(*) AS c")).rows;
        assert_eq!(rows[0][0], Value::Int(4 * DRIVING as i64));
        let error = try_exec_cypher(&g, &search_over_driving(DRIVING, "RETURN p.name AS name"))
            .err()
            .expect("the materializing form must be refused at the cap");
        assert!(
            matches!(
                error,
                Error::Capacity {
                    kind: CapacityKind::Expression,
                    ..
                }
            ),
            "{error}"
        );
    }

    /// PROFILE keeps reporting the SEARCH clause's own facts when its rows
    /// stream: the walker rung that served it, the store reads its
    /// per-candidate walks made, and the rows it emitted into the fold (a
    /// streamed clause holds nothing).
    #[test]
    fn a_streamed_search_match_reports_the_walker_and_its_reads() {
        let g = vector_index_graph();
        let records = profiled(&g, &search_over_driving(3, "RETURN count(*) AS c"));
        let record = records
            .iter()
            .find(|record| record.kernel.is_some())
            .expect("the SEARCH MATCH records its rung");
        assert_eq!(record.kernel, Some(KernelRung::Walker));
        // Three driving rows × the four-entry window: one node-store read
        // per candidate the walk examined, and one row emitted per candidate.
        assert_eq!(record.reads.node_reads, 12, "{:?}", record.reads);
        assert_eq!(record.rows, 12, "the rows the clause emitted into the fold");
        assert_eq!(record.memory_bytes, 0, "a streamed clause holds nothing");
    }

    /// A pre-bound row's window of one is decided before any fetch: a
    /// LIMIT-0 window keeps nothing, so the clause answers empty without
    /// reaching the index — the entity-kind rejection a fetch would raise
    /// (a relationship binding against a node index) is not reached.
    /// Deliberate, ticket-prescribed (REL8-2128); the control shows the
    /// same statement with LIMIT 1 does reach the index and is rejected.
    #[test]
    fn a_pre_bound_limit_zero_window_answers_before_the_index_is_read() {
        let g = vector_index_graph();
        exec_autocommit(&g, "CREATE (:Person)-[:KNOWS]->(:Person)").unwrap();
        let search = |limit: u32| {
            format!(
                "MATCH ()-[r:KNOWS]->() WITH r MATCH ()-[r]->() \
                 SEARCH r IN (VECTOR INDEX vidx FOR [1.0, 0.0] LIMIT {limit}) \
                 RETURN count(*) AS c"
            )
        };
        let rejected = try_exec_cypher(&g, &search(1))
            .err()
            .expect("a LIMIT-1 window reads the index and rejects the kind mismatch");
        assert!(
            rejected.to_string().contains("does not match index"),
            "{rejected}"
        );
        let rows = exec_cypher(&g, &search(0)).rows;
        assert_eq!(rows[0][0], Value::Int(0));
    }
}
