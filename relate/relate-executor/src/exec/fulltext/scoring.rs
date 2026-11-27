//! The calibrated scoring kernel: the BM25 field scoring (live Neo4j's
//! fitted `K1`/`B` parameters), the fuzzy-clause expansion with its
//! ordering, and the clause resolution a scan's query text goes through
//! — the math [`super`]'s scan orchestration drives over the corpus
//! subsystem's documents and per-field statistics ([`super::corpus`]).

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;

use super::corpus::{DocField, FieldStats};
use super::distance::bounded_distance;
use super::query::{Clause, Occur, Target};
use super::wildcard::Pattern;

/// Live Neo4j's fulltext similarity parameters (fitted on 2026.10.1
/// enterprise, see the module docs).
const K1: f64 = 1.2;
const B: f64 = 0.75;

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

/// The any-of group of terms a clause matches in one field, and how the
/// match scores.
enum FieldMatch {
    /// An exact term is the group of one, a fuzzy term its bounded
    /// expansion; each alternative scores its own weighted BM25.
    Weighted(Vec<Alternative>),
    /// A wildcard (uncapped): the field scores a constant 1.0 when any of
    /// its tokens matches the pattern, however many or how often. The
    /// pattern is carried, not its expansion: a vocabulary-sized set per
    /// clause and field would multiply memory by the clause count.
    Constant(Pattern),
}

/// A parsed clause resolved against the index vocabulary: per indexed field
/// the any-of group of terms it matches there. The `property:` filter is
/// applied here, so a field outside it has no entry.
pub(super) struct ResolvedClause<'a> {
    pub(super) occur: Occur,
    boost: f64,
    by_field: HashMap<&'a str, FieldMatch>,
}

/// The weight of a fuzzy candidate at edit distance `edits` from a query
/// term, Lucene's `FuzzyTermsEnum` similarity: `1 - edits / min(|query|,
/// |candidate|)` in characters, so a closer match outscores a farther one
/// and an exact match keeps weight 1. A candidate whose distance reaches its
/// shorter side's length weighs nothing and is not a match: `None`, the one
/// place the match rule is decided.
///
/// Calibrated against Neo4j 2026.09-enterprise: scores are exactly this
/// weight times the term's BM25 (0.875 at one edit, 0.75 at two on an
/// eight-character term). Lucene's document-frequency blending across the
/// expanded terms is not modelled — each expanded term scores with its own
/// idf.
fn fuzzy_weight(edits: u8, query_chars: usize, candidate_chars: usize) -> Option<f64> {
    let shorter = query_chars.min(candidate_chars);
    (usize::from(edits) < shorter).then(|| 1.0 - f64::from(edits) / shorter as f64)
}

/// One in-range fuzzy candidate: its heap key in the expansion's result
/// order (edits, then frequency, then term) beside the weight the match
/// rule produced for it. Ordering reads the key alone: terms are unique
/// within a vocabulary, so the key is already total and the weight (a
/// function of the key and the term's length) never breaks a tie.
struct FuzzyCandidate<'a> {
    key: (u8, Reverse<usize>, &'a str),
    weight: f64,
}

impl PartialEq for FuzzyCandidate<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for FuzzyCandidate<'_> {}

impl PartialOrd for FuzzyCandidate<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FuzzyCandidate<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key.cmp(&other.key)
    }
}

/// The bounded expansion of a fuzzy term over one field's vocabulary: every
/// term within `max_edits`, closest first, then most frequent, then
/// lexicographic (determinism over the hash-ordered vocabulary), capped at
/// [`MAX_FUZZY_EXPANSIONS`].
fn expand_fuzzy(term: &str, max_edits: u8, stat: &FieldStats) -> Vec<Alternative> {
    let term_chars: Vec<char> = term.chars().collect();
    let query_chars = term_chars.len();
    // A max-heap on the result order keeps the best `MAX_FUZZY_EXPANSIONS`
    // candidates: the worst kept one sits on top and is evicted first, so
    // memory is bounded by the cap, not by how many terms are in range.
    let mut best: BinaryHeap<FuzzyCandidate<'_>> = BinaryHeap::new();
    for (candidate, df) in &stat.df {
        let Some(edits) = bounded_distance(&term_chars, candidate, max_edits) else {
            continue;
        };
        let Some(weight) = fuzzy_weight(edits, query_chars, candidate.chars().count()) else {
            continue;
        };
        best.push(FuzzyCandidate {
            key: (edits, Reverse(*df), candidate.as_str()),
            weight,
        });
        if best.len() > MAX_FUZZY_EXPANSIONS {
            best.pop();
        }
    }
    best.into_sorted_vec()
        .into_iter()
        .map(|kept| Alternative {
            term: kept.key.2.to_string(),
            weight: kept.weight,
        })
        .collect()
}

pub(super) fn resolve_clause<'a>(
    clause: &Clause,
    stats: &'a HashMap<Arc<str>, FieldStats>,
) -> ResolvedClause<'a> {
    let by_field = stats
        .iter()
        .filter(|(field, _)| {
            clause
                .property
                .as_deref()
                .is_none_or(|wanted| wanted == &***field)
        })
        .map(|(field, stat)| {
            let matched = match &clause.target {
                Target::Term(term) => FieldMatch::Weighted(vec![Alternative {
                    term: term.clone(),
                    weight: 1.0,
                }]),
                Target::Fuzzy { term, edits } => {
                    FieldMatch::Weighted(expand_fuzzy(term, *edits, stat))
                }
                Target::Wildcard(pattern) => FieldMatch::Constant(pattern.clone()),
            };
            (&**field, matched)
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
/// alternative scales by its closeness weight; a wildcard scores a constant
/// 1.0 per field it matches), times the clause's boost; `None` when the
/// clause matches no field.
pub(super) fn clause_score(
    clause: &ResolvedClause<'_>,
    doc: &[DocField],
    stats: &HashMap<Arc<str>, FieldStats>,
) -> Option<f64> {
    let fields = doc.iter().filter_map(|field| {
        let field_match = clause.by_field.get(&*field.property)?;
        field_score(field_match, field, stats)
    });
    sum_matches(fields).map(|score| score * clause.boost)
}

/// The sum of the `Some` scores in `scores`, `None` when none matched — the
/// "matched anywhere, then added up" rule the clause and field scores share.
fn sum_matches(scores: impl Iterator<Item = f64>) -> Option<f64> {
    scores.fold(None, |sum, score| Some(sum.unwrap_or(0.0) + score))
}

/// One clause's score on one document field, `None` when the field holds no
/// term the clause matches there.
fn field_score(
    field_match: &FieldMatch,
    field: &DocField,
    stats: &HashMap<Arc<str>, FieldStats>,
) -> Option<f64> {
    let alternatives = match field_match {
        FieldMatch::Weighted(alternatives) => alternatives,
        FieldMatch::Constant(pattern) => {
            return field
                .tokens
                .iter()
                .any(|token| pattern.matches(token))
                .then_some(1.0);
        }
    };
    // An invariant of the one statistics pass: every indexed property has
    // an entry.
    let stat = stats
        .get(&*field.property)
        .expect("statistics exist for every indexed property");
    let n = stat.docs as f64;
    let dl = field.tokens.len() as f64;
    let avgdl = stat.total_len as f64 / n;
    let scores = alternatives.iter().filter_map(|alternative| {
        let tf = field
            .tokens
            .iter()
            .filter(|token| **token == alternative.term)
            .count();
        if tf == 0 {
            return None;
        }
        // A term this document carries was counted into that field's
        // document frequency.
        let df = stat
            .df
            .get(&alternative.term)
            .copied()
            .expect("a carried token is in the field's document frequency table");
        let idf = (1.0 + (n - df as f64 + 0.5) / (df as f64 + 0.5)).ln();
        let tf = tf as f64;
        Some(alternative.weight * idf * tf / (tf + K1 * (1.0 - B + B * dl / avgdl)))
    });
    sum_matches(scores)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wildcard_group_scores_a_constant_per_field() {
        let stat = stat_of(&[("lampa", 1), ("lampb", 1), ("other", 1)]);
        let stats = HashMap::from([(Arc::<str>::from("name"), stat)]);
        let clause = ResolvedClause {
            occur: Occur::Should,
            boost: 2.0,
            by_field: HashMap::from([("name", FieldMatch::Constant(Pattern::must_parse("lamp?")))]),
        };
        let field = |tokens: &[&str]| DocField {
            property: Arc::from("name"),
            tokens: tokens.iter().map(|t| t.to_string()).collect(),
        };
        // Two matching terms, repeated, in a long field: still 1.0 * boost.
        let doc = [field(&["lampa", "lampa", "lampb", "x", "y", "z"])];
        assert_eq!(clause_score(&clause, &doc, &stats), Some(2.0));
        assert_eq!(clause_score(&clause, &[field(&["other"])], &stats), None);
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
        assert_eq!(fuzzy_weight(0, 8, 8), Some(1.0));
        let (one_edit, two_edits) = (
            fuzzy_weight(1, 8, 9).expect("one edit on a nine-character candidate matches"),
            fuzzy_weight(2, 8, 9).expect("two edits on a nine-character candidate match"),
        );
        assert!(one_edit > two_edits);
        assert_eq!(fuzzy_weight(2, 2, 9), None);
        assert_eq!(fuzzy_weight(0, 0, 3), None);
        // A 2-char candidate 2 edits away weighs nothing: not a match.
        let stat = stat_of(&[("ab", 1)]);
        assert!(expand_fuzzy("xy", 2, &stat).is_empty());
    }
}
