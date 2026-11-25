//! CIP-225 fulltext query semantics: the Lucene-style query parser
//! (implicit OR between terms, `AND`/`OR`/`NOT`, `property:term`), the two
//! differentiated search analyzers (`standard`, `english`), and the
//! relevance scoring both the SEARCH clause and the
//! `db.index.fulltext.query*` procedures share. The corpus subsystem
//! (the tokenized documents, its retained-byte estimate, and the
//! bounded-recency memo that caches them) lives in
//! [`corpus`](self::corpus), and the calibrated scoring kernel (the
//! BM25 and fuzzy math) in [`scoring`](self::scoring) — both
//! re-exported below like the analyzer and query modules.
//!
//! Scoring is calibrated against live Neo4j 2026.10.1 (enterprise): the
//! observed `SCORE AS` values reproduce exactly with
//!
//! ```text
//! score = Σ over matching (property, term): idf · tf / (tf + k1·(1 - b + b·dl/avgdl))
//! idf   = ln(1 + (N - df + 0.5) / (df + 0.5))
//! ```
//!
//! `k1 = 1.2`, `b = 0.75`; `N`, `df` and `avgdl` are per indexed property
//! (field) over the index's own entity population, and the `(k1 + 1)`
//! numerator of textbook BM25 is absent (live-vs-model fit: a constant
//! 1/2.2 scale across every probe corpus).
//!
//! The query syntax accepts the forms `docs/reference/cypher-syntax.md`
//! documents and rejects the rest with an authored syntax error — grouping
//! parentheses, field groups, ranges, regular expressions and
//! `+`/`-`/`!` clause modifiers are never silently read as a flat OR (see
//! [`query`]). The index's own
//! `fulltext.analyzer` config runs its full analysis on the stored tokens —
//! accent folding for `standard-folding`, stop-word filtering and Porter
//! stemming for `english`, CJK bigrams for `cjk` — and a query without
//! `WITH ANALYZER` is analyzed
//! by that same index analyzer; `WITH ANALYZER` overrides the query side
//! only (ADR-0636). Every live-admitted analyzer
//! name is accepted (the one [`analyzer::ANALYZERS`] table below — the same table
//! `exec::ddl_options`' config validation reads); the names other than
//! `english`, `standard-folding` and `cjk` share the standard tokenizer, so
//! their stored tokens and queries are unstemmed. The standard tokenizer
//! splits the scripts written without spaces (Han, Hiragana, Katakana) the
//! way UAX #29 has it, so CJK text is searchable by its words (ADR-0672).
//! `standard-folding` ASCII-folds Latin diacritics and ligatures (see
//! [`fold`]) in the index's stored tokens and in analyzed query terms.
//!
//! `term*` / `te?m` wildcard terms (see [`wildcard`]; folded under a folding
//! analyzer, unlike live) match each document's tokens and score a constant
//! 1.0 per matching field (times the boost), with no expansion cap and no
//! materialized expansion set — all calibrated
//! against Neo4j 2026.09-enterprise (ADR-0583).
//!
//! `term~N` (fuzzy, N in 0..=2, bare `~` = 2) expands against each field's
//! vocabulary by bounded Damerau-Levenshtein distance (see [`distance`]) and
//! `term^B` multiplies a clause's score; both are parsed in [`query`], their
//! scoring choices are calibrated against Neo4j 2026.09-enterprise.

use relate_core::{Error, Result, Value};
use relate_ir::SearchIndexSpec;
use relate_model::EntityKind;

use crate::ctx::ExecCtx;
use crate::eval::eval;
use crate::exec::candidates::{ranked_candidates, resolve_search_index, TopWindow};
use crate::exec::ddl_options::{index_analyzer, index_eventually_consistent};
use crate::exec::search_filters::{prepare_search_filters, PreparedSearchFilter};
use crate::overlay::ExecGraph;
use crate::redo::{IndexDetailSpec, IndexKindSpec};

use crate::row::Row;

use corpus::CorpusDoc;
pub(crate) use corpus::CorpusMemo;

mod analyzer;
mod corpus;
mod distance;
mod fold;
mod query;
mod scoring;
mod stemmer;
mod wildcard;

pub(crate) use analyzer::{analyzer_catalog, Analyzer, Analyzers};
use query::{parse, Occur};
use scoring::{clause_score, resolve_clause, ResolvedClause};

/// The SEARCH clause's fulltext candidate generation — the vector
/// orchestration's twin ([`crate::exec::vector::index_candidates`]):
/// the shared catalog-descriptor phase (index resolution, the per-row
/// `WITH ANALYZER` override), the inner WHERE's single-stage gate
/// prepared per input row, the live BM25 scan, survivor materialization
/// — living beside [`search`] (the scan it orchestrates).
pub(crate) fn index_candidates(
    ctx: ExecCtx<'_>,
    corpora: &CorpusMemo,
    input: &Row,
    index: &SearchIndexSpec,
    query: &str,
    window: usize,
) -> Result<(EntityKind, Vec<(Value, f64)>)> {
    let ExecCtx { graph, params, .. } = ctx;
    // The per-row `WITH ANALYZER` expression; an absent slot — or a NULL
    // value — is no override, so the query follows the index's analyzer
    // (ADR-0636), and the value's own unknown-name/type rejections are
    // `Analyzer::from_value`'s.
    let query_override = match &index.analyzer {
        None => None,
        Some(expr) => Analyzer::from_value(&eval(graph, input, expr, params)?)?,
    };
    // The SEARCH arm's catalog-descriptor entry ([`resolve_for_search`]):
    // the one resolution, with the SEARCH-only eventually-consistent
    // rejection raised inside it (ADR-0680; the procedures stay the
    // supported surface for such an index, through [`resolve_analyzers`]).
    let scan = resolve_for_search(graph, &index.index_name, query_override)?;
    // The inner WHERE's single-stage gate, prepared per input row (the
    // RHS of every predicate is row-correlated) and applied to each
    // matching document before the window — the same placement the
    // vector scan gives its filters. The preparation returns the entity
    // kind with the prepared predicates — the one derivation — and the
    // survivor materialization below uses that returned kind.
    let (kind, prepared) =
        prepare_search_filters(&index.filters, &scan.detail, graph, input, params)?;
    let candidates = ranked_candidates(
        graph,
        kind,
        search(corpora, graph, scan, query, 0, window, &prepared)?,
    );
    Ok((kind, candidates))
}

/// The resolved facts one fulltext scan needs, from the one canonical
/// descriptor phase ([`resolve_analyzers`]) — `exec/vector.rs`'s
/// [`crate::exec::vector::VectorScan`] twin: the catalog detail (whose
/// relationship flag is the entity kind's fact, one derivation in
/// [`crate::redo::IndexDetailSpec::entity_kind`], and whose labels gate
/// the corpus population) and the two-sided [`Analyzers`] with the
/// query-side override applied.
pub(crate) struct FulltextScan {
    pub(crate) detail: IndexDetailSpec,
    pub(crate) analyzers: Analyzers,
}

/// The fulltext catalog-descriptor phase both call kinds share — the
/// SEARCH clause's orchestration ([`index_candidates`]) and the
/// `db.index.fulltext.query*` procedures: resolve the index (the one
/// shared spelling, missing-index and wrong-kind policies included),
/// read the stored analyzer options, and build the two-sided
/// [`Analyzers`] with the caller's query-side override (an absent
/// override follows the index's own analyzer, ADR-0636).
pub(crate) fn resolve_analyzers(
    graph: &ExecGraph<'_>,
    index_name: &str,
    query_analyzer: Option<Analyzer>,
) -> Result<FulltextScan> {
    let (detail, options) = resolve_search_index(graph, index_name, IndexKindSpec::Fulltext)?;
    analyzers_from(detail, &options, index_name, query_analyzer)
}

/// The SEARCH clause's resolution entry (ADR-0680): the same one catalog
/// read [`resolve_analyzers`] performs, with the SEARCH-only
/// eventually-consistent rejection raised inside it — the
/// `db.index.fulltext.query*` procedures admit such an index, so the
/// policy sits at its own entry, never on the resolved scan every caller
/// carries.
fn resolve_for_search(
    graph: &ExecGraph<'_>,
    index_name: &str,
    query_analyzer: Option<Analyzer>,
) -> Result<FulltextScan> {
    let (detail, options) = resolve_search_index(graph, index_name, IndexKindSpec::Fulltext)?;
    if index_eventually_consistent(&options, index_name)? {
        return Err(Error::execution(format!(
            "EventuallyConsistentIndexNotSupported: The index `{index_name}` is eventually \
             consistent, which is not currently supported with `SEARCH`"
        )));
    }
    analyzers_from(detail, &options, index_name, query_analyzer)
}

/// The two-sided [`Analyzers`] from the resolved options — the one
/// construction both resolution entries end with.
fn analyzers_from(
    detail: IndexDetailSpec,
    options: &std::collections::BTreeMap<String, Value>,
    index_name: &str,
    query_analyzer: Option<Analyzer>,
) -> Result<FulltextScan> {
    let analyzers = Analyzers::new(index_analyzer(options, index_name)?, query_analyzer);
    Ok(FulltextScan { detail, analyzers })
}

/// The one fulltext scan, windowed: rank the index's matching entities by
/// the calibrated BM25 score, best-first under
/// [`crate::exec::candidates::ranked_better_cmp`] (score descending,
/// element id ascending), through the shared CIP-225 [`TopWindow`], in the
/// family's named window (`offset` before `limit`, never a positional
/// pair). The two callers state different windows: the SEARCH clause
/// passes a limit-only window (`index_candidates` always passes offset 0 —
/// the clause's SKIP/OFFSET pages entity survivors in the caller, over the
/// entities the whole MATCH keeps), and the procedures' `options` supply
/// both `skip` and `limit`.
pub(crate) fn search(
    memo: &CorpusMemo,
    graph: &ExecGraph<'_>,
    scan: FulltextScan,
    query: &str,
    offset: usize,
    limit: usize,
    filters: &[PreparedSearchFilter<'_>],
) -> Result<Vec<(i64, f64)>> {
    rank(
        memo,
        graph,
        scan,
        query,
        TopWindow::new(offset, limit),
        filters,
    )
}

/// The fulltext arm's operand read, the twin of the vector arm's
/// `vector_operand` (exec::vector): NULL matches nothing (the
/// null-propagates-to-no-candidates policy), a STRING is the query, every
/// other type is the typed rejection.
pub(crate) fn operand(value: &Value) -> Result<Option<&str>> {
    match value {
        Value::Null => Ok(None),
        Value::String(query) => Ok(Some(query)),
        _ => Err(Error::ty(
            "InvalidValueType: fulltext SEARCH requires a STRING query",
        )),
    }
}

fn rank(
    memo: &CorpusMemo,
    graph: &ExecGraph<'_>,
    scan: FulltextScan,
    query: &str,
    mut window: TopWindow,
    filters: &[PreparedSearchFilter<'_>],
) -> Result<Vec<(i64, f64)>> {
    let FulltextScan { detail, analyzers } = scan;
    let clauses = parse(query, analyzers.query)?;
    if clauses.is_empty() {
        return Ok(Vec::new());
    }
    let corpus = memo.corpus(graph, &detail, analyzers.index);
    let (docs, stats) = (&corpus.docs, &corpus.stats);
    // Fuzzy clauses expand against each field's vocabulary once per scan.
    let clauses: Vec<ResolvedClause<'_>> = clauses
        .iter()
        .map(|clause| resolve_clause(clause, stats))
        .collect();
    // A query with a MUST clause needs no SHOULD to match; a should-only
    // query needs one (Lucene's BooleanQuery contract).
    let needs_should = !clauses.iter().any(|clause| clause.occur == Occur::Must);
    for CorpusDoc {
        id,
        properties,
        fields,
    } in docs
    {
        let mut musts_hold = true;
        let mut should_holds = false;
        let mut prohibited = false;
        let mut score = 0.0;
        for clause in &clauses {
            let contribution = clause_score(clause, fields, stats);
            if let Some(contribution) = contribution {
                score += contribution;
            }
            match clause.occur {
                Occur::MustNot => prohibited |= contribution.is_some(),
                Occur::Must => musts_hold &= contribution.is_some(),
                Occur::Should => should_holds |= contribution.is_some(),
            }
        }
        // One acceptance statement: the query's clause semantics hold and
        // the SEARCH clause's inner WHERE passes — the gate reads the
        // document's own properties as the corpus carries them, and an
        // empty filter list (the procedure callers carry no SEARCH
        // clause) is the callee's own ungated answer, so the loop states
        // no guard of its own (ADR-0678).
        if musts_hold
            && !prohibited
            && (should_holds || !needs_should)
            && crate::exec::search_filters::search_filter_passes(graph, *id, properties, filters)?
        {
            window.push(score, *id);
        }
    }
    Ok(window.into_sorted_rows())
}
