//! The fulltext test family's shared scaffolding, used by exactly the two
//! fulltext integration-test crates (`fulltext_search_semantics.rs` and
//! `fulltext_folding.rs`): the one spelling of each entry-point statement
//! (the SEARCH-clause prefix and the `queryNodes` call), the two
//! entry-point wrappers over them, the entry-point agreement helper, the
//! ranked-hit extractors, and the shared query-syntax rejection check.

use super::{assert_syntax_error, run};

/// Every result row as a `(String, f64)` score pair — the ranked-hit
/// shape the fulltext test crates assert on (name plus score, best
/// first).
pub fn scored(result: &relate::QueryResult) -> Vec<(String, f64)> {
    result
        .rows
        .iter()
        .map(|row| match (&row[0], &row[1]) {
            (relate::Value::String(name), relate::Value::Float(score)) => {
                (name.to_string(), *score)
            }
            other => panic!("expected (STRING, FLOAT), got {other:?}"),
        })
        .collect()
}

/// The names of scored hits in ranked order.
pub fn scored_names(hits: &[(String, f64)]) -> Vec<String> {
    hits.iter().map(|(name, _)| name.clone()).collect()
}

/// The names of scored hits, sorted — the order-free equality the
/// fulltext entry-point agreement checks use.
pub fn sorted_names(hits: &[(String, f64)]) -> Vec<String> {
    let mut names = scored_names(hits);
    names.sort();
    names
}

/// The SEARCH-clause statement prefix for one fulltext query — the one
/// spelling of the clause the family issues; the caller appends its own
/// projection. `analyzer` names a `WITH ANALYZER` override; `None` omits
/// the clause, so the query follows the index's own analyzer (ADR-0636).
pub fn search_statement(
    label: &str,
    index: &str,
    query: &str,
    analyzer: Option<&str>,
    limit: u32,
) -> String {
    let analyzer = analyzer
        .map(|name| format!(" WITH ANALYZER '{name}'"))
        .unwrap_or_default();
    format!(
        "MATCH (n:{label}) \
         SEARCH n IN (FULLTEXT INDEX {index} FOR '{query}'{analyzer} LIMIT {limit})"
    )
}

/// The `db.index.fulltext.queryNodes` statement prefix — the one spelling
/// of the procedure call; the caller appends its own projection. The
/// procedure has no analyzer form: the index's own analyzer always
/// analyzes its query.
pub fn procedure_statement(index: &str, query: &str) -> String {
    format!("CALL db.index.fulltext.queryNodes('{index}', '{query}')")
}

/// The SEARCH-clause scan of one fulltext query over `(label, index,
/// property)`: ranked `(name, score)` pairs, best first, name-ascending
/// within a score. `analyzer` is the `WITH ANALYZER` override; `None`
/// runs the index's own analyzer.
pub fn fulltext_search(
    c: &mut relate::Connection,
    label: &str,
    index: &str,
    property: &str,
    limit: u32,
    query: &str,
    analyzer: Option<&str>,
) -> Vec<(String, f64)> {
    let result = run(
        c,
        &format!(
            "{} \
             SCORE AS score RETURN n.{property} AS name, score ORDER BY score DESC, name ASC",
            search_statement(label, index, query, analyzer, limit)
        ),
    );
    scored(&result)
}

/// The `db.index.fulltext.queryNodes` scan of the same query: every hit,
/// ranked the same way — no analyzer form exists on this side.
pub fn fulltext_procedure(
    c: &mut relate::Connection,
    index: &str,
    property: &str,
    query: &str,
) -> Vec<(String, f64)> {
    let result = run(
        c,
        &format!(
            "{} \
             YIELD node, score RETURN node.{property} AS name, score ORDER BY score DESC, name ASC",
            procedure_statement(index, query)
        ),
    );
    scored(&result)
}

/// The one entry-point-agreement policy of the fulltext test family: both
/// entry points must answer the same ranked `(name, score)` pairs. The
/// clause's `limit` is a paging window that must cover the query's whole
/// answer here — the procedure returns every hit — so a window too small
/// fails loudly instead of truncating the comparison. No analyzer slot:
/// the procedure has no `WITH ANALYZER` form, so an override cannot
/// agree. Returns the hits so a caller can read the scores; name-only
/// checks derive through [`scored_names`] (ranked) or [`sorted_names`]
/// (order-free).
pub fn fulltext_agree(
    c: &mut relate::Connection,
    label: &str,
    index: &str,
    property: &str,
    limit: u32,
    query: &str,
) -> Vec<(String, f64)> {
    let clause = fulltext_search(c, label, index, property, limit, query, None);
    let procedure = fulltext_procedure(c, index, property, query);
    assert!(
        procedure.len() <= limit as usize,
        "{label} {index} {query}: LIMIT {limit} truncates the {} procedure hits",
        procedure.len()
    );
    assert_eq!(clause, procedure, "{label} {index} {query}");
    clause
}

/// Each query is a syntax error through the SEARCH clause and through
/// `queryNodes` on `(label, index)` — the shared rejection check of the
/// fulltext query-syntax test crates, over the same statement builders.
pub fn assert_syntax_error_on_both_paths(
    c: &mut relate::Connection,
    (label, index): (&str, &str),
    queries: &[&str],
) {
    for query in queries {
        let error = c
            .query(&format!(
                "{} RETURN n.name",
                search_statement(label, index, query, None, 5)
            ))
            .expect_err(query);
        assert_syntax_error(&error);
        let error = c
            .query(&format!(
                "{} YIELD node RETURN node.name",
                procedure_statement(index, query)
            ))
            .expect_err(query);
        assert_syntax_error(&error);
    }
}
