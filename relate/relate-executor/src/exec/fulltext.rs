//! CIP-225 fulltext query semantics: the Lucene-style query parser
//! (implicit OR between terms, `AND`/`OR`/`NOT`, `property:term`), the two
//! differentiated search analyzers (`standard`, `english`), and the
//! relevance scoring both the SEARCH clause and the
//! `db.index.fulltext.query*` procedures share.
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
//! Shortcuts, all live-documented and outside the merged corpora's shapes:
//! a quoted phrase adds no adjacency constraint (its words stay separate
//! terms), `+`/`-` clause modifiers are not read, and the index's own
//! `fulltext.analyzer` config is not applied to stored tokens — only the
//! query-side analyzer is differentiated. Every live-admitted analyzer
//! name is accepted (the one [`ANALYZERS`] table below — the same table
//! `exec::ddl_options`' config validation reads); the names other than
//! `english` share the standard tokenizer.

use std::collections::{HashMap, HashSet};

use relate_core::{bare_type_name, Error, PropertyMap, Result, Value};

use crate::exec::candidates::{index_population, TopWindow};
use crate::overlay::ExecGraph;
use crate::redo::IndexDetailSpec;

mod stemmer;

use stemmer::porter_stem;

/// Live Neo4j's fulltext similarity parameters (fitted on 2026.10.1
/// enterprise, see the module docs).
const K1: f64 = 1.2;
const B: f64 = 0.75;

/// EnglishAnalyzer's default stop set (Lucene `DefaultEnglishStopWordSet`)
/// — live-verified: `FOR 'the' WITH ANALYZER 'english'` matches nothing
/// while the standard analyzer matches every plot containing `the`.
const ENGLISH_STOP_WORDS: [&str; 33] = [
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "if", "in", "into", "is", "it",
    "no", "not", "of", "on", "or", "such", "that", "the", "their", "then", "there", "these",
    "they", "this", "to", "was", "will", "with",
];

/// The search-side analyzers this engine differentiates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Analyzer {
    Standard,
    English,
}

/// The one analyzer vocabulary: every fulltext analyzer name live Neo4j
/// admits (live-verified 2026-11-02 on neo4j:2026.09-enterprise) with the
/// behaviour it selects — `english` drops English stop words and
/// Porter-stems, every other admitted name shares the standard
/// tokenizer. The name admission, the rejection sentence's list and
/// `exec::ddl_options`' `fulltext.analyzer` config validation all read
/// these rows; the name list and the name→behaviour mapping are never
/// split across modules again.
const ANALYZERS: [(&str, Analyzer); 45] = [
    ("arabic", Analyzer::Standard),
    ("armenian", Analyzer::Standard),
    ("basque", Analyzer::Standard),
    ("brazilian", Analyzer::Standard),
    ("bulgarian", Analyzer::Standard),
    ("catalan", Analyzer::Standard),
    ("cjk", Analyzer::Standard),
    ("classic", Analyzer::Standard),
    ("czech", Analyzer::Standard),
    ("danish", Analyzer::Standard),
    ("dutch", Analyzer::Standard),
    ("email", Analyzer::Standard),
    ("english", Analyzer::English),
    ("finnish", Analyzer::Standard),
    ("french", Analyzer::Standard),
    ("galician", Analyzer::Standard),
    ("german", Analyzer::Standard),
    ("greek", Analyzer::Standard),
    ("hindi", Analyzer::Standard),
    ("hungarian", Analyzer::Standard),
    ("indonesian", Analyzer::Standard),
    ("irish", Analyzer::Standard),
    ("italian", Analyzer::Standard),
    ("keyword", Analyzer::Standard),
    ("latvian", Analyzer::Standard),
    ("lithuanian", Analyzer::Standard),
    ("norwegian", Analyzer::Standard),
    ("persian", Analyzer::Standard),
    ("portuguese", Analyzer::Standard),
    ("romanian", Analyzer::Standard),
    ("russian", Analyzer::Standard),
    ("simple", Analyzer::Standard),
    ("sorani", Analyzer::Standard),
    ("spanish", Analyzer::Standard),
    ("standard", Analyzer::Standard),
    ("standard-folding", Analyzer::Standard),
    ("standard-no-stop-words", Analyzer::Standard),
    ("stop", Analyzer::Standard),
    ("swedish", Analyzer::Standard),
    ("thai", Analyzer::Standard),
    ("turkish", Analyzer::Standard),
    ("unicode_whitespace", Analyzer::Standard),
    ("url", Analyzer::Standard),
    ("url_or_email", Analyzer::Standard),
    ("whitespace", Analyzer::Standard),
];

impl Analyzer {
    /// Resolve one evaluated `WITH ANALYZER` value against the canonical
    /// analyzer-name table. A NULL value is CIP-225's "no override" (live
    /// 2026.10.1 returns the standard-analyzer hits, no error), a STRING
    /// must name a live-admitted analyzer (live 22N27), and every other
    /// type is live's 22NB1 string type mismatch.
    pub(crate) fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Null => Ok(Self::Standard),
            Value::String(name) => Self::from_name(name).ok_or_else(|| {
                Error::argument_error(format!(
                    "Invalid input '{name}' for analyzer name. Expected to be {}.",
                    analyzer_names()
                ))
            }),
            other => Err(Error::ty(format!(
                "Type mismatch: expected to be STRING but was '{}'.",
                bare_type_name(other)
            ))),
        }
    }

    /// The known-name lookup against the one [`ANALYZERS`] table: a name
    /// is admitted exactly when it has a row, and the row's behaviour is
    /// the analyzer it selects (case-insensitive, like the config side).
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        ANALYZERS
            .iter()
            .find(|(admitted, _)| *admitted == name.as_str())
            .map(|(_, behavior)| *behavior)
    }

    /// The analyzer's tokens: `standard` is the tokenizer alone (the
    /// default `standard-no-stop-words` index analyzer), `english` drops
    /// English stop words and Porter-stems what remains.
    fn analyze(self, text: &str) -> Vec<String> {
        let tokens = tokenize(text);
        match self {
            Self::Standard => tokens,
            Self::English => tokens
                .into_iter()
                .filter(|token| !ENGLISH_STOP_WORDS.contains(&token.as_str()))
                .map(|token| porter_stem(&token))
                .collect(),
        }
    }
}

/// The admitted analyzer names, rendered as the rejection sentence's list
/// in the one [`ANALYZERS`] table's order.
fn analyzer_names() -> String {
    let names: Vec<&str> = ANALYZERS.iter().map(|(name, _)| *name).collect();
    let (last, rest) = names.split_last().expect("the analyzer table is non-empty");
    format!("{} or {last}", rest.join(", "))
}

/// Lowercase, split on any run of non-alphanumeric characters except `_` —
/// the standard tokenizer's shape on this engine's corpora. Live-fitted:
/// the default analyzer keeps stop words and underscores (`foo_bar` is one
/// token; `foo` alone matches nothing) and splits hyphenated words
/// (`wish-granting` is two tokens), which is what the length statistics of
/// the calibrated scores require.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

/// How one parsed query clause takes part in the boolean match: Lucene's
/// `Occur` flags after `AND`/`OR`/`NOT` are folded onto their neighbours.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Occur {
    Should,
    Must,
    MustNot,
}

#[derive(Debug)]
struct Clause {
    occur: Occur,
    /// The `property:` prefix, if the query named one (the procedure's
    /// field-filter form; a name outside the index matches nothing).
    property: Option<String>,
    term: String,
}

/// Parse a fulltext query string into analyzer-produced clauses.
///
/// `AND`/`OR`/`NOT` (uppercase only — lowercase words stay terms, so
/// `'princess AND and NOT kingdom'` requires the token `and`) bind to
/// their neighbours the way Lucene's `Occur` flags do; juxtaposed terms
/// are `Should` (implicit OR). Parentheses and quotes are separators (see
/// the module's shortcut note).
fn parse(query: &str, analyzer: Analyzer) -> Vec<Clause> {
    let mut clauses: Vec<Clause> = Vec::new();
    let mut pending = Occur::Should;
    let words = query.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '"'));
    for word in words.filter(|word| !word.is_empty()) {
        match word {
            "AND" | "&&" => {
                if let Some(last) = clauses.last_mut() {
                    if last.occur == Occur::Should {
                        last.occur = Occur::Must;
                    }
                }
                pending = Occur::Must;
                continue;
            }
            "OR" | "||" => {
                pending = Occur::Should;
                continue;
            }
            "NOT" | "!" => {
                pending = Occur::MustNot;
                continue;
            }
            _ => {}
        }
        let (property, text) = match word.split_once(':') {
            Some((property, text)) => (Some(property.to_string()), text),
            None => (None, word),
        };
        for term in analyzer.analyze(text) {
            clauses.push(Clause {
                occur: pending,
                property: property.clone(),
                term,
            });
        }
        pending = Occur::Should;
    }
    clauses
}

/// One indexed property's tokens for one document. The property travels
/// with its tokens, so a clause's `property:` filter reads fields directly
/// instead of indexing back into `detail.properties`.
struct DocField<'a> {
    property: &'a str,
    tokens: Vec<String>,
}

/// Per-field corpus statistics: how many indexed entities carry the field,
/// the field's total token count (for `avgdl`), and the document frequency
/// of each term.
#[derive(Default)]
struct FieldStats {
    docs: usize,
    total_len: usize,
    df: HashMap<String, usize>,
}

/// The tokens a value contributes to a fulltext field: a string as itself,
/// a list element-wise, recursively (Neo4j fulltext indexes string arrays),
/// anything else nothing. The recursion is unreachable for stored data — a
/// nested list cannot be a property value at all (live 2026.10.1 rejects it
/// with 22N39, "cannot be stored in properties") — but it keeps the walker
/// total over the value model instead of silently dropping an inner list.
fn value_tokens(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => tokenize(text),
        Value::List(values) => values.iter().flat_map(value_tokens).collect(),
        _ => Vec::new(),
    }
}

fn doc_fields<'a>(properties: &'a [String], values: &PropertyMap) -> Vec<DocField<'a>> {
    properties
        .iter()
        .map(|property| DocField {
            property,
            tokens: values.get(property).map(value_tokens).unwrap_or_default(),
        })
        .collect()
}

/// The corpus statistics pass over the scan's documents: how many
/// documents carry each indexed property, each field's total token count
/// (for `avgdl`), and every carried term's document frequency. Every
/// indexed property gets an entry — empty fields included — so the score
/// read is an invariant, not a lookup that can miss.
fn field_stats<'a>(
    docs: &[(i64, Vec<DocField<'_>>)],
    properties: &'a [String],
) -> HashMap<&'a str, FieldStats> {
    let mut stats: HashMap<&str, FieldStats> = properties
        .iter()
        .map(|property| (property.as_str(), FieldStats::default()))
        .collect();
    for (_, doc) in docs {
        for field in doc {
            if field.tokens.is_empty() {
                continue;
            }
            let stat = stats
                .get_mut(field.property)
                .expect("statistics exist for every indexed property");
            stat.docs += 1;
            stat.total_len += field.tokens.len();
            let mut seen = HashSet::new();
            for token in &field.tokens {
                if seen.insert(token.as_str()) {
                    *stat.df.entry(token.clone()).or_insert(0) += 1;
                }
            }
        }
    }
    stats
}

/// One clause's BM25 contribution on one document, summed over the fields it
/// matched (a term present in both `title` and `plot` scores from each, as
/// Lucene's per-field clauses do); `None` when the clause matches no field.
fn clause_score(
    clause: &Clause,
    doc: &[DocField<'_>],
    stats: &HashMap<&str, FieldStats>,
) -> Option<f64> {
    let mut score = 0.0;
    let mut matched = false;
    for field in doc {
        if clause
            .property
            .as_ref()
            .is_some_and(|wanted| wanted != field.property)
        {
            continue;
        }
        let tf = field
            .tokens
            .iter()
            .filter(|token| **token == clause.term)
            .count();
        if tf == 0 {
            continue;
        }
        matched = true;
        // Both lookups are invariants of the one statistics pass: every
        // indexed property has an entry, and a term this document carries
        // was counted into that field's document frequency.
        let stat = stats
            .get(field.property)
            .expect("statistics exist for every indexed property");
        let df = stat
            .df
            .get(&clause.term)
            .copied()
            .expect("a carried token is in the field's document frequency table");
        let n = stat.docs as f64;
        let idf = (1.0 + (n - df as f64 + 0.5) / (df as f64 + 0.5)).ln();
        let dl = field.tokens.len() as f64;
        let avgdl = stat.total_len as f64 / n;
        let tf = tf as f64;
        score += idf * tf / (tf + K1 * (1.0 - B + B * dl / avgdl));
    }
    matched.then_some(score)
}

/// The one fulltext scan, windowed: rank the index's matching entities by
/// the calibrated BM25 score, best-first under
/// [`crate::exec::candidates::ranked_better_cmp`] (score descending, element
/// id ascending), through the shared CIP-225 [`TopWindow`] — the SEARCH
/// clause's `SKIP`/`OFFSET`/`LIMIT` page.
pub(crate) fn search(
    graph: &ExecGraph<'_>,
    detail: &IndexDetailSpec,
    query: &str,
    analyzer: Analyzer,
    offset: usize,
    limit: usize,
) -> Vec<(i64, f64)> {
    rank(
        graph,
        detail,
        query,
        analyzer,
        TopWindow::new(offset, limit),
    )
}

/// The same scan over the whole ranking — the `db.index.fulltext.query*`
/// procedures return every hit, not a page.
pub(crate) fn search_all(
    graph: &ExecGraph<'_>,
    detail: &IndexDetailSpec,
    query: &str,
    analyzer: Analyzer,
) -> Vec<(i64, f64)> {
    rank(graph, detail, query, analyzer, TopWindow::ranking_all())
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
    graph: &ExecGraph<'_>,
    detail: &IndexDetailSpec,
    query: &str,
    analyzer: Analyzer,
    mut window: TopWindow,
) -> Vec<(i64, f64)> {
    let clauses = parse(query, analyzer);
    if clauses.is_empty() {
        return Vec::new();
    }
    // The shared index population (exec::candidates); the document build
    // and the corpus statistics are this scan's own.
    let entities = index_population(graph, detail);
    let docs: Vec<(i64, Vec<DocField<'_>>)> = entities
        .iter()
        .map(|(id, properties)| (*id, doc_fields(&detail.properties, properties)))
        .collect();
    let stats = field_stats(&docs, &detail.properties);
    // A query with a MUST clause needs no SHOULD to match; a should-only
    // query needs one (Lucene's BooleanQuery contract).
    let needs_should = !clauses.iter().any(|clause| clause.occur == Occur::Must);
    for (id, doc) in &docs {
        let mut musts_hold = true;
        let mut should_holds = false;
        let mut prohibited = false;
        let mut score = 0.0;
        for clause in &clauses {
            let contribution = clause_score(clause, doc, &stats);
            if let Some(contribution) = contribution {
                score += contribution;
            }
            match clause.occur {
                Occur::MustNot => prohibited |= contribution.is_some(),
                Occur::Must => musts_hold &= contribution.is_some(),
                Occur::Should => should_holds |= contribution.is_some(),
            }
        }
        if musts_hold && !prohibited && (should_holds || !needs_should) {
            window.push(score, *id);
        }
    }
    window.into_sorted_rows()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyzers_differ_on_stop_words_and_stemming() {
        assert_eq!(
            Analyzer::Standard.analyze("The princesses"),
            ["the", "princesses"]
        );
        assert_eq!(Analyzer::English.analyze("The princesses"), ["princess"]);
    }

    #[test]
    fn analyzer_values_are_validated_like_live() {
        // Live 2026.10.1: a NULL analyzer is no override (standard hits),
        // an unknown name is 22N27, a non-STRING value is 22NB1.
        assert_eq!(
            Analyzer::from_value(&Value::Null).unwrap(),
            Analyzer::Standard
        );
        assert_eq!(
            Analyzer::from_value(&Value::String("ENGLISH".into())).unwrap(),
            Analyzer::English
        );
        assert_eq!(
            Analyzer::from_value(&Value::String("not-a-real-analyzer".into())).unwrap_err(),
            Error::argument_error(format!(
                "Invalid input 'not-a-real-analyzer' for analyzer name. Expected to be {}.",
                analyzer_names()
            ))
        );
        assert_eq!(
            Analyzer::from_value(&Value::Int(42)).unwrap_err(),
            Error::ty("Type mismatch: expected to be STRING but was 'INTEGER'.")
        );
    }

    #[test]
    fn parse_folds_operators_onto_neighbours() {
        let clauses = parse("princess AND NOT snow", Analyzer::Standard);
        assert_eq!(clauses.len(), 2);
        assert_eq!(clauses[0].occur, Occur::Must);
        assert_eq!(clauses[0].term, "princess");
        assert_eq!(clauses[1].occur, Occur::MustNot);
        assert_eq!(clauses[1].term, "snow");

        // Lowercase `and` stays a term: only uppercase words are operators.
        let clauses = parse("princess AND and NOT kingdom", Analyzer::Standard);
        assert_eq!(clauses.len(), 3);
        assert_eq!(clauses[1].occur, Occur::Must);
        assert_eq!(clauses[1].term, "and");
        assert_eq!(clauses[2].occur, Occur::MustNot);

        // Juxtaposed terms are implicit OR (all Should).
        let clauses = parse("lamp lamplighter", Analyzer::Standard);
        assert!(clauses.iter().all(|clause| clause.occur == Occur::Should));

        // An explicit OR clears a preceding AND's carry-over.
        let clauses = parse("a AND b OR c", Analyzer::Standard);
        assert_eq!(
            clauses
                .iter()
                .map(|clause| clause.occur)
                .collect::<Vec<_>>(),
            [Occur::Must, Occur::Must, Occur::Should]
        );
    }

    #[test]
    fn parse_splits_a_property_prefix_per_term() {
        let clauses = parse("plot:princess", Analyzer::Standard);
        assert_eq!(clauses.len(), 1);
        assert_eq!(clauses[0].property.as_deref(), Some("plot"));
        assert_eq!(clauses[0].term, "princess");
    }

    #[test]
    fn tokenizer_splits_hyphens_and_lowercases() {
        assert_eq!(
            tokenize("A wish-granting GENIE."),
            ["a", "wish", "granting", "genie"]
        );
        // Live 2026.10.1: `foo_bar` is one token, and `foo` alone does not
        // match a document holding it.
        assert_eq!(tokenize("foo_bar baz"), ["foo_bar", "baz"]);
    }
}
