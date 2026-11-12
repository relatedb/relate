//! The fulltext query parser: Lucene-style clauses (`AND`/`OR`/`NOT`,
//! `property:term`, `term~N` fuzzy, `term^B` boost) folded into typed
//! [`Clause`]s the scanner ranks.

use relate_core::{Error, Result};

use super::{tokenize, Analyzer};

/// Lucene's `FuzzyQuery` ceiling on edits (its Levenshtein automata stop at
/// distance 2) and the default a bare `~` selects.
pub(super) const MAX_FUZZY_EDITS: u8 = 2;

/// How one parsed query clause takes part in the boolean match: Lucene's
/// `Occur` flags after `AND`/`OR`/`NOT` are folded onto their neighbours.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Occur {
    Should,
    Must,
    MustNot,
}

#[derive(Debug)]
pub(super) struct Clause {
    pub(super) occur: Occur,
    /// The `property:` prefix, if the query named one (the procedure's
    /// field-filter form; a name outside the index matches nothing).
    pub(super) property: Option<String>,
    pub(super) term: String,
    /// `Some(n)`: match every indexed term within `n` edits (`term~n`);
    /// `None`: the exact term.
    pub(super) fuzzy: Option<u8>,
    /// The `^B` multiplier on the clause's score; `1.0` when absent.
    pub(super) boost: f64,
}

/// The fuzzy part of a `~` suffix before it meets its term's length.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Fuzz {
    /// `~N`: an edit bound (already clamped to [`MAX_FUZZY_EDITS`]).
    Edits(u8),
    /// `~x`, `0 <= x < 1`: Lucene's minimum similarity, a bound relative to
    /// the term's length.
    MinSimilarity(f64),
}

impl Fuzz {
    /// The edit bound for a term of `chars` characters (Lucene's
    /// `FuzzyQuery.floatToEdits`: similarity 0 is exact, otherwise
    /// `floor((1 - s) * len)` capped at the maximum).
    fn edits_for(self, chars: usize) -> u8 {
        match self {
            Self::Edits(edits) => edits,
            Self::MinSimilarity(0.0) => 0,
            Self::MinSimilarity(s) => {
                // In [0, MAX_FUZZY_EDITS] after the cap, so the cast is exact.
                ((1.0 - s) * chars as f64)
                    .floor()
                    .min(f64::from(MAX_FUZZY_EDITS)) as u8
            }
        }
    }
}

/// The `~N` / `^B` tail of one query word.
struct Suffix {
    fuzzy: Option<Fuzz>,
    boost: f64,
}

struct Parser {
    analyzer: Analyzer,
    clauses: Vec<Clause>,
    pending: Occur,
}

/// The `Occur` an operator word selects (`AND`/`&&` → `Must`, `OR`/`||` →
/// `Should`, `NOT`/`!` → `MustNot`); `None` for an ordinary word.
fn operator(word: &str) -> Option<Occur> {
    match word {
        "AND" | "&&" => Some(Occur::Must),
        "OR" | "||" => Some(Occur::Should),
        "NOT" | "!" => Some(Occur::MustNot),
        _ => None,
    }
}

/// The query's words, with a detached suffix word (`term ~1`, `term ^2`,
/// which Lucene reads like `term~1`) glued onto the word before it. A suffix
/// with no ordinary word before it stays alone and fails as a missing term.
fn words(query: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let raw = query.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '"'));
    for word in raw.filter(|word| !word.is_empty()) {
        if word.starts_with(['~', '^']) {
            if let Some(last) = words.last_mut().filter(|last| operator(last).is_none()) {
                last.push_str(word);
                continue;
            }
        }
        words.push(word.to_string());
    }
    words
}

/// Parse a fulltext query string into analyzer-produced clauses.
///
/// `AND`/`OR`/`NOT` (uppercase only — lowercase words stay terms, so
/// `'princess AND and NOT kingdom'` requires the token `and`) bind to
/// their neighbours the way Lucene's `Occur` flags do; juxtaposed terms
/// are `Should` (implicit OR). Parentheses and quotes are separators (see
/// the module's shortcut note). A malformed `~`/`^` suffix is a typed
/// syntax error, never a stray term.
pub(super) fn parse(query: &str, analyzer: Analyzer) -> Result<Vec<Clause>> {
    let mut parser = Parser {
        analyzer,
        clauses: Vec::new(),
        pending: Occur::Should,
    };
    for word in words(query) {
        parser.word(&word)?;
    }
    Ok(parser.clauses)
}

impl Parser {
    fn word(&mut self, word: &str) -> Result<()> {
        if let Some(occur) = operator(word) {
            if occur == Occur::Must {
                if let Some(last) = self.clauses.last_mut() {
                    if last.occur == Occur::Should {
                        last.occur = Occur::Must;
                    }
                }
            }
            self.pending = occur;
            return Ok(());
        }
        let (property, text) = match word.split_once(':') {
            Some((property, text)) => (Some(property), text),
            None => (None, word),
        };
        let mut occur = self.pending;
        self.pending = Occur::Should;
        // Text left after a fuzzy suffix that is not a `^` boost starts a
        // new word (`term~1x` is a bare `~` plus the term `1x`, as Lucene's
        // lexer reads it): it is its own implicit-OR clause.
        let mut rest = Some(text);
        let mut property = property;
        while let Some(text) = rest {
            rest = self.push_text(occur, property, text)?;
            occur = Occur::Should;
            property = None;
        }
        Ok(())
    }

    /// Push the clauses of one word's text; returns the text that must be
    /// read as a following word.
    fn push_text<'t>(
        &mut self,
        occur: Occur,
        property: Option<&str>,
        text: &'t str,
    ) -> Result<Option<&'t str>> {
        let (term, suffix, remainder) = split_suffix(text)?;
        // A fuzzy term is matched against the index's stored tokens, which
        // are lowercased but never stemmed or stop-filtered, so it skips the
        // analyzer's stemming (Lucene's multi-term normalisation).
        let terms = if suffix.fuzzy.is_some() {
            tokenize(term)
        } else {
            self.analyzer.analyze(term)
        };
        for term in terms {
            self.clauses.push(Clause {
                occur,
                property: property.map(str::to_string),
                fuzzy: suffix
                    .fuzzy
                    .map(|fuzz| fuzz.edits_for(term.chars().count())),
                term,
                boost: suffix.boost,
            });
        }
        Ok((!remainder.is_empty()).then_some(remainder))
    }
}

/// Split a word's text into its term, its `~N` / `^B` tail (`term~N^B`,
/// either part optional) and any text that must be read as a new word. A
/// word with neither marker is returned untouched.
fn split_suffix(text: &str) -> Result<(&str, Suffix, &str)> {
    let mut suffix = Suffix {
        fuzzy: None,
        boost: 1.0,
    };
    let Some(at) = text.find(['~', '^']) else {
        return Ok((text, suffix, ""));
    };
    let (term, mut rest) = text.split_at(at);
    if tokenize(term).is_empty() {
        return Err(malformed(text, "a `~`/`^` suffix needs a term before it"));
    }
    if let Some(after) = rest.strip_prefix('~') {
        let (number, tail) = take_number(after);
        if !tail.is_empty() && !tail.starts_with('^') {
            // `~` then text that is not a boost: a bare `~`, the text is its
            // own word.
            suffix.fuzzy = Some(Fuzz::Edits(MAX_FUZZY_EDITS));
            return Ok((term, suffix, after));
        }
        suffix.fuzzy = Some(if number.is_empty() {
            Fuzz::Edits(MAX_FUZZY_EDITS)
        } else {
            fuzz_spec(number).map_err(|reason| malformed(text, reason))?
        });
        rest = tail;
    }
    if let Some(after) = rest.strip_prefix('^') {
        let (number, tail) = take_number(after);
        suffix.boost = boost_value(number).map_err(|reason| malformed(text, reason))?;
        if !tail.is_empty() {
            return Err(malformed(
                text,
                "unexpected text after the `^` boost (expected term~N^B)",
            ));
        }
    }
    Ok((term, suffix, ""))
}

/// The leading `digits[.digits]` of `s` and what follows it (the number is
/// empty when `s` does not start with a digit).
fn take_number(s: &str) -> (&str, &str) {
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    let mut end = digits;
    if digits > 0 && s[digits..].starts_with('.') {
        let fraction = s[digits + 1..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        if fraction > 0 {
            end = digits + 1 + fraction;
        }
    }
    s.split_at(end)
}

fn malformed(word: &str, reason: &str) -> Error {
    Error::syntax(format!("Invalid fulltext query term '{word}': {reason}"))
}

/// What a `~N` suffix selects (`raw` is `digits[.digits]`).
///
/// Calibrated against Neo4j 2026.09-enterprise: an integer clamps to the
/// maximum of 2 (`~3` and `~99` match like `~2`), a value in `[0, 1)` is a
/// minimum similarity (`~0.8` matches like `~1` on an 8-character term), and
/// a non-integer of 1 or more (`~1.5`) is a parse error.
fn fuzz_spec(raw: &str) -> std::result::Result<Fuzz, &'static str> {
    if !raw.contains('.') {
        // `raw` is all digits, so only overflow can saturate.
        let n = raw.bytes().fold(0u64, |acc, digit| {
            acc.saturating_mul(10)
                .saturating_add(u64::from(digit - b'0'))
        });
        return Ok(Fuzz::Edits(
            u8::try_from(n.min(u64::from(MAX_FUZZY_EDITS))).expect("clamped to the u8 maximum"),
        ));
    }
    let value: f64 = raw.parse().expect("digits.digits is a valid float");
    if value < 1.0 {
        Ok(Fuzz::MinSimilarity(value))
    } else if value.fract() == 0.0 {
        // A whole decimal (`~1.0`) means the same as its integer spelling.
        let edits = value.min(f64::from(MAX_FUZZY_EDITS));
        Ok(Fuzz::Edits(edits as u8))
    } else {
        Err("a fuzzy distance of 1 or more must be a whole number")
    }
}

/// The multiplier a `^B` suffix selects (`raw` is `digits[.digits]`, empty
/// when nothing numeric followed the caret).
///
/// Calibrated against Neo4j 2026.09-enterprise: `^0` is legal (the clause
/// still matches and contributes 0), `^1.5` scales by 1.5, and `^`, `^x`
/// and `^-1` are parse errors.
fn boost_value(raw: &str) -> std::result::Result<f64, &'static str> {
    if raw.is_empty() {
        return Err("a `^` boost needs a non-negative number");
    }
    let boost: f64 = raw.parse().expect("digits[.digits] is a valid float");
    if boost.is_finite() {
        Ok(boost)
    } else {
        Err("the `^` boost is out of range")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clauses(query: &str) -> Vec<Clause> {
        parse(query, Analyzer::Standard).expect("a well-formed query")
    }

    #[test]
    fn parse_folds_operators_onto_neighbours() {
        let parsed = clauses("princess AND NOT snow");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].occur, Occur::Must);
        assert_eq!(parsed[0].term, "princess");
        assert_eq!(parsed[1].occur, Occur::MustNot);
        assert_eq!(parsed[1].term, "snow");

        // Lowercase `and` stays a term: only uppercase words are operators.
        let parsed = clauses("princess AND and NOT kingdom");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[1].occur, Occur::Must);
        assert_eq!(parsed[1].term, "and");
        assert_eq!(parsed[2].occur, Occur::MustNot);

        // Juxtaposed terms are implicit OR (all Should).
        assert!(clauses("lamp lamplighter")
            .iter()
            .all(|clause| clause.occur == Occur::Should));

        // An explicit OR clears a preceding AND's carry-over.
        assert_eq!(
            clauses("a AND b OR c")
                .iter()
                .map(|clause| clause.occur)
                .collect::<Vec<_>>(),
            [Occur::Must, Occur::Must, Occur::Should]
        );
    }

    #[test]
    fn parse_splits_a_property_prefix_per_term() {
        let parsed = clauses("plot:princess");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].property.as_deref(), Some("plot"));
        assert_eq!(parsed[0].term, "princess");
        assert_eq!(parsed[0].fuzzy, None);
        assert_eq!(parsed[0].boost, 1.0);
    }

    #[test]
    fn fuzzy_suffix_forms_never_leak_digits_as_terms() {
        for (query, edits) in [
            ("kolleger~", 2),
            ("kolleger~0", 0),
            ("kolleger~1", 1),
            ("kolleger~2", 2),
            ("kolleger~1.0", 1),
            ("kolleger~2.0", 2),
            ("kolleger~3.0", 2),
        ] {
            let parsed = clauses(query);
            assert_eq!(parsed.len(), 1, "{query}");
            assert_eq!(parsed[0].term, "kolleger", "{query}");
            assert_eq!(parsed[0].fuzzy, Some(edits), "{query}");
            assert_eq!(parsed[0].boost, 1.0, "{query}");
        }
        // Operators still fold around a fuzzy clause.
        let parsed = clauses("kolleger~1 AND lamp");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].occur, Occur::Must);
        assert_eq!(parsed[0].fuzzy, Some(1));
        assert_eq!(parsed[1].occur, Occur::Must);
        assert_eq!(parsed[1].fuzzy, None);
    }

    #[test]
    fn boost_and_combined_suffixes() {
        let parsed = clauses("lamp^2.5");
        assert_eq!((parsed[0].fuzzy, parsed[0].boost), (None, 2.5));
        let parsed = clauses("title:kolleger~1^3");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].property.as_deref(), Some("title"));
        assert_eq!(parsed[0].term, "kolleger");
        assert_eq!((parsed[0].fuzzy, parsed[0].boost), (Some(1), 3.0));
        // A zero boost is legal; the clause still exists.
        assert_eq!(clauses("lamp^0")[0].boost, 0.0);
    }

    #[test]
    fn distance_above_the_maximum_clamps() {
        assert_eq!(clauses("lamp~7")[0].fuzzy, Some(MAX_FUZZY_EDITS));
        assert_eq!(
            clauses("lamp~99999999999999999999999")[0].fuzzy,
            Some(MAX_FUZZY_EDITS)
        );
    }

    #[test]
    fn malformed_suffixes_are_typed_syntax_errors() {
        for query in [
            "lamp^x", "lamp^", "lamp^-1", "lamp~1.5", "lamp^2~1", "lamp^2x", "~1", "^2",
            "title:~1", "AND ~1",
        ] {
            let error = parse(query, Analyzer::Standard).expect_err(query);
            assert!(matches!(error, Error::Syntax(_)), "{query}: {error:?}");
        }
    }

    #[test]
    fn fractions_are_minimum_similarity_relative_to_term_length() {
        // Calibrated against Neo4j 2026.09-enterprise: `~0.8` on the
        // 8-character `kolleger` matches like `~1`; 0 is exact; whole
        // numbers of 1 or more clamp like integers.
        assert_eq!(clauses("kolleger~0.8")[0].fuzzy, Some(1));
        assert_eq!(clauses("kolleger~0.0")[0].fuzzy, Some(0));
        assert_eq!(clauses("kolleger~0.1")[0].fuzzy, Some(2));
        assert_eq!(clauses("abc~0.5")[0].fuzzy, Some(1));
        assert_eq!(clauses("kolleger~2.0")[0].fuzzy, Some(2));
    }

    #[test]
    fn text_after_a_tilde_is_a_new_word() {
        // `term~1x` is a bare `~` plus the separate term `1x`.
        let parsed = clauses("kolleger~1x");
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            (parsed[0].term.as_str(), parsed[0].fuzzy),
            ("kolleger", Some(2))
        );
        assert_eq!((parsed[1].term.as_str(), parsed[1].fuzzy), ("1x", None));
        assert_eq!(parsed[1].occur, Occur::Should);
    }

    #[test]
    fn a_detached_suffix_binds_to_the_previous_word() {
        // `term ~1` reads like `term~1`.
        let parsed = clauses("kolleger ~1");
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            (parsed[0].term.as_str(), parsed[0].fuzzy),
            ("kolleger", Some(1))
        );
        let parsed = clauses("a AND kolleger ~1 ^2");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].occur, Occur::Must);
        assert_eq!((parsed[1].fuzzy, parsed[1].boost), (Some(1), 2.0));
        let parsed = clauses("kolleger ~");
        assert_eq!(parsed[0].fuzzy, Some(2));
        let parsed = clauses("a AND kolleger ~1");
        assert_eq!(parsed[0].occur, Occur::Must);
    }
}
