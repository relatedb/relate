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
//!
//! `term~N` (fuzzy, N in 0..=2, bare `~` = 2) expands against each field's
//! vocabulary by bounded Damerau-Levenshtein distance (see [`distance`]) and
//! `term^B` multiplies a clause's score; both are parsed in [`query`], their
//! scoring choices are calibrated against Neo4j 2026.09-enterprise.

use std::collections::{HashMap, HashSet};

use relate_core::{bare_type_name, Error, PropertyMap, Result, Value};

use crate::exec::candidates::{index_population, TopWindow};
use crate::overlay::ExecGraph;
use crate::redo::IndexDetailSpec;

mod distance;
mod query;
mod stemmer;

use distance::bounded_distance;
use query::{parse, Clause, Occur};
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

/// Lucene's `FuzzyQuery` default cap on the terms one fuzzy clause expands
/// to (`maxExpansions`); its `prefixLength` default of 0 means no leading
/// characters are pinned.
const MAX_FUZZY_EXPANSIONS: usize = 50;

/// One index term a clause may match in a field, with the weight its
/// closeness to the query term earns (`1.0` for an exact term).
struct Alternative {
    term: String,
    weight: f64,
}

/// A parsed clause resolved against the index vocabulary: per indexed field
/// the any-of group of terms it matches there. An exact clause is the group
/// of one; a fuzzy clause is its bounded expansion. The `property:` filter
/// is applied here, so a field outside it has no entry.
struct ResolvedClause<'a> {
    occur: Occur,
    boost: f64,
    by_field: HashMap<&'a str, Vec<Alternative>>,
}

/// The weight of a fuzzy candidate at edit distance `edits` from a query
/// term, Lucene's `FuzzyTermsEnum` similarity: `1 - edits / min(|query|,
/// |candidate|)` in characters, so a closer match outscores a farther one
/// and an exact match keeps weight 1. A candidate whose distance reaches its
/// shorter side's length weighs nothing and is not a match.
///
/// Calibrated against Neo4j 2026.09-enterprise: scores are exactly this
/// weight times the term's BM25 (0.875 at one edit, 0.75 at two on an
/// eight-character term). Lucene's document-frequency blending across the
/// expanded terms is not modelled — each expanded term scores with its own
/// idf.
fn fuzzy_weight(edits: u8, query_chars: usize, candidate_chars: usize) -> f64 {
    if edits == 0 {
        return 1.0;
    }
    1.0 - f64::from(edits) / query_chars.min(candidate_chars) as f64
}

/// The bounded expansion of a fuzzy term over one field's vocabulary: every
/// term within `max_edits`, closest first, then most frequent, then
/// lexicographic (determinism over the hash-ordered vocabulary), capped at
/// [`MAX_FUZZY_EXPANSIONS`].
fn expand_fuzzy(term: &str, max_edits: u8, stat: &FieldStats) -> Vec<Alternative> {
    let term_chars: Vec<char> = term.chars().collect();
    let query_chars = term_chars.len();
    let mut near: Vec<(u8, usize, &str, f64)> = stat
        .df
        .iter()
        .filter_map(|(candidate, df)| {
            let edits = bounded_distance(&term_chars, candidate, max_edits)?;
            let weight = fuzzy_weight(edits, query_chars, candidate.chars().count());
            (weight > 0.0).then_some((edits, *df, candidate.as_str(), weight))
        })
        .collect();
    near.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then_with(|| a.2.cmp(b.2)));
    near.truncate(MAX_FUZZY_EXPANSIONS);
    near.into_iter()
        .map(|(_, _, candidate, weight)| Alternative {
            term: candidate.to_string(),
            weight,
        })
        .collect()
}

fn resolve_clause<'a>(clause: &Clause, stats: &HashMap<&'a str, FieldStats>) -> ResolvedClause<'a> {
    let by_field = stats
        .iter()
        .filter(|(field, _)| {
            clause
                .property
                .as_deref()
                .is_none_or(|wanted| wanted == **field)
        })
        .map(|(field, stat)| {
            let alternatives = match clause.fuzzy {
                None => vec![Alternative {
                    term: clause.term.clone(),
                    weight: 1.0,
                }],
                Some(max_edits) => expand_fuzzy(&clause.term, max_edits, stat),
            };
            (*field, alternatives)
        })
        .collect();
    ResolvedClause {
        occur: clause.occur,
        boost: clause.boost,
        by_field,
    }
}

/// One clause's BM25 contribution on one document, summed over the fields
/// and alternatives it matched (a term present in both `title` and `plot`
/// scores from each, as Lucene's per-field clauses do; each fuzzy
/// alternative scales by its closeness weight), times the clause's boost;
/// `None` when the clause matches no field.
fn clause_score(
    clause: &ResolvedClause<'_>,
    doc: &[DocField<'_>],
    stats: &HashMap<&str, FieldStats>,
) -> Option<f64> {
    let mut score = 0.0;
    let mut matched = false;
    for field in doc {
        let Some(alternatives) = clause.by_field.get(field.property) else {
            continue;
        };
        for alternative in alternatives {
            let tf = field
                .tokens
                .iter()
                .filter(|token| **token == alternative.term)
                .count();
            if tf == 0 {
                continue;
            }
            matched = true;
            // Both lookups are invariants of the one statistics pass: every
            // indexed property has an entry, and a term this document
            // carries was counted into that field's document frequency.
            let stat = stats
                .get(field.property)
                .expect("statistics exist for every indexed property");
            let df = stat
                .df
                .get(&alternative.term)
                .copied()
                .expect("a carried token is in the field's document frequency table");
            let n = stat.docs as f64;
            let idf = (1.0 + (n - df as f64 + 0.5) / (df as f64 + 0.5)).ln();
            let dl = field.tokens.len() as f64;
            let avgdl = stat.total_len as f64 / n;
            let tf = tf as f64;
            score += alternative.weight * idf * tf / (tf + K1 * (1.0 - B + B * dl / avgdl));
        }
    }
    matched.then_some(score * clause.boost)
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
) -> Result<Vec<(i64, f64)>> {
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
) -> Result<Vec<(i64, f64)>> {
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
) -> Result<Vec<(i64, f64)>> {
    let clauses = parse(query, analyzer)?;
    if clauses.is_empty() {
        return Ok(Vec::new());
    }
    // The shared index population (exec::candidates); the document build
    // and the corpus statistics are this scan's own.
    let entities = index_population(graph, detail);
    let docs: Vec<(i64, Vec<DocField<'_>>)> = entities
        .iter()
        .map(|(id, properties)| (*id, doc_fields(&detail.properties, properties)))
        .collect();
    let stats = field_stats(&docs, &detail.properties);
    // Fuzzy clauses expand against each field's vocabulary once per scan.
    let clauses: Vec<ResolvedClause<'_>> = clauses
        .iter()
        .map(|clause| resolve_clause(clause, &stats))
        .collect();
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
    Ok(window.into_sorted_rows())
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
    fn tokenizer_splits_hyphens_and_lowercases() {
        assert_eq!(
            tokenize("A wish-granting GENIE."),
            ["a", "wish", "granting", "genie"]
        );
        // Live 2026.10.1: `foo_bar` is one token, and `foo` alone does not
        // match a document holding it.
        assert_eq!(tokenize("foo_bar baz"), ["foo_bar", "baz"]);
    }

    fn stat_of(terms: &[(&str, usize)]) -> FieldStats {
        FieldStats {
            docs: 10,
            total_len: 100,
            df: terms.iter().map(|(t, df)| (t.to_string(), *df)).collect(),
        }
    }

    fn terms(alternatives: &[Alternative]) -> Vec<&str> {
        alternatives.iter().map(|a| a.term.as_str()).collect()
    }

    #[test]
    fn expansion_orders_closest_then_frequent_then_lexicographic() {
        let stat = stat_of(&[
            ("kollegger", 1),
            ("kolleger", 3),
            ("kollegers", 5),
            ("kolleges", 5),
            ("unrelated", 9),
        ]);
        let expanded = expand_fuzzy("kolleger", 1, &stat);
        // Exact first; then distance 1 by df desc, ties lexicographic.
        assert_eq!(
            terms(&expanded),
            ["kolleger", "kollegers", "kolleges", "kollegger"]
        );
        assert_eq!(expanded[0].weight, 1.0);
        assert!(expanded[1].weight < 1.0 && expanded[1].weight > 0.0);
        // Distance 0 is the exact term only.
        assert_eq!(terms(&expand_fuzzy("kolleger", 0, &stat)), ["kolleger"]);
    }

    #[test]
    fn expansion_is_capped_and_deterministic() {
        let owned: Vec<(String, usize)> = (0..80).map(|i| (format!("lam{i:02}"), 1)).collect();
        let vocabulary: Vec<(&str, usize)> =
            owned.iter().map(|(t, df)| (t.as_str(), *df)).collect();
        let first = expand_fuzzy("lamp", 2, &stat_of(&vocabulary));
        assert_eq!(first.len(), MAX_FUZZY_EXPANSIONS);
        // Same vocabulary, different hash seeds / insertion order.
        let mut reversed = vocabulary.clone();
        reversed.reverse();
        let second = expand_fuzzy("lamp", 2, &stat_of(&reversed));
        assert_eq!(terms(&first), terms(&second));
        let mut sorted = terms(&first);
        sorted.sort();
        assert_eq!(terms(&first), sorted, "ties break lexicographically");
    }

    #[test]
    fn weight_falls_with_distance_and_never_reaches_zero_in_a_match() {
        assert_eq!(fuzzy_weight(0, 8, 8), 1.0);
        assert!(fuzzy_weight(1, 8, 9) > fuzzy_weight(2, 8, 9));
        // A 2-char candidate 2 edits away weighs nothing: not a match.
        let stat = stat_of(&[("ab", 1)]);
        assert!(expand_fuzzy("xy", 2, &stat).is_empty());
    }
}
