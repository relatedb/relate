//! The fulltext query parser: Lucene-style clauses (`AND`/`OR`/`NOT`,
//! `property:term`, `term~N` fuzzy, `term*`/`te?m` wildcard, `term^B` boost)
//! folded into typed [`Clause`]s the scanner ranks; the query text is
//! scanned by the sibling [`scan`] module.
//!
//! The supported forms are the ones `docs/reference/cypher-syntax.md`
//! documents. Everything else Lucene accepts — grouping parentheses and
//! field groups, ranges (`[a TO b]`, `{a TO b}`), regular expressions
//! (`/re/`), the `+`/`-`/`!` clause modifiers and a quoted phrase of more
//! than one term — is an authored syntax error here, never a silent flat-OR
//! reading of the words it is built from.

mod scan;

use relate_core::Result;

use super::analyzer::tokenize;
use super::fold::fold;
use super::wildcard::Pattern;
#[cfg(test)]
use super::wildcard::MAX_WILDCARD_PARTS;
use super::Analyzer;
use scan::{
    find_unescaped, lex, malformed, query_error, split_suffix, Suffix, Word, CLAUSE_MODIFIERS,
    SUFFIX_MARKERS,
};

/// Split a word at its first unescaped `:` — the raw `field:` prefix and the
/// term after it; a word without one is all term.
fn split_field(text: &str) -> (Option<&str>, &str) {
    match find_unescaped(text, &[':']) {
        Some(at) => (Some(&text[..at]), &text[at + 1..]),
        None => (None, text),
    }
}

/// Refuse the leading forms Lucene admits and this parser does not: a
/// `+`/`-`/`!` clause modifier, and a `/re/` regular expression. The printed
/// text carries the provenance — a quoted or escaped modifier travels as
/// `\+`, so a raw one is a modifier — while `/` is not escaped by quoting, so
/// a literal word's `/` is text.
fn refuse_leading_form(text: &str, literal: bool) -> Result<()> {
    if let Some(modifier) = text.chars().next().filter(|c| CLAUSE_MODIFIERS.contains(c)) {
        return Err(malformed(
            text,
            &format!("a `{modifier}` clause modifier is not supported"),
        ));
    }
    if !literal && text.starts_with('/') {
        return Err(malformed(
            text,
            "a regular-expression term is not supported",
        ));
    }
    Ok(())
}

/// The most clauses one query may parse to: Lucene's default
/// `maxClauseCount`, which its boolean query enforces at build time. Each
/// fuzzy clause scans a field's whole vocabulary and each wildcard clause
/// every document's tokens, so the clause count multiplies the work of one
/// query; the bound is checked once at parse (ADR-0578).
const MAX_QUERY_CLAUSES: usize = 1024;

/// How one parsed query clause takes part in the boolean match: Lucene's
/// `Occur` flags after `AND`/`OR`/`NOT` are folded onto their neighbours.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Occur {
    Should,
    Must,
    MustNot,
}

/// Which indexed terms a clause matches.
#[derive(Debug, PartialEq)]
pub(super) enum Target {
    /// Exactly this analyzed term.
    Term(String),
    /// Every indexed term within `edits` edits of `term` (`term~n`).
    Fuzzy { term: String, edits: u8 },
    /// Every indexed term the pattern matches (`term*`, `te?m`).
    Wildcard(Pattern),
}

#[derive(Debug)]
pub(super) struct Clause {
    pub(super) occur: Occur,
    /// The `property:` prefix, if the query named one (the procedure's
    /// field-filter form; a name outside the index matches nothing).
    pub(super) property: Option<String>,
    pub(super) target: Target,
    /// The `^B` multiplier on the clause's score; `1.0` when absent.
    pub(super) boost: f64,
}

/// The position a query's words have reached — the one operator-placement
/// state: no word yet, a term last, or an operator last (with the occurrence
/// it imposes on the next term, and the word it was spelled as, which the
/// refusals quote).
enum Position<'w> {
    Nothing,
    Term,
    Operator { occur: Occur, word: &'w str },
}

struct Parser<'q, 'w> {
    /// The query text, for the refusals that name it.
    query: &'q str,
    analyzer: Analyzer,
    clauses: Vec<Clause>,
    position: Position<'w>,
}

impl Word {
    /// The `Occur` this word selects, when it is an operator: a literal
    /// (quoted or escaped) word is the characters themselves, never an
    /// operator — the one place that question is answered.
    fn operator(&self) -> Option<Occur> {
        (!self.literal).then(|| operator(&self.text)).flatten()
    }
}

/// A literal (quoted or escaped) term is one term: the analyzer reading
/// several tokens from it means the quote or escape spelled a phrase, which
/// this parser cannot match adjacently.
fn refuse_literal_phrase(tokens: usize, term: &str) -> Result<()> {
    if tokens > 1 {
        return Err(malformed(
            term,
            "a literal term the analyzer reads as several is a phrase, which is not supported",
        ));
    }
    Ok(())
}

/// The `Occur` an operator word selects (`AND`/`&&` → `Must`, `OR`/`||` →
/// `Should`, `NOT` → `MustNot`); `None` for an ordinary word. `!` is not an
/// operator: Lucene reads `!term` as a prohibited term, which this parser
/// rejects as an unsupported modifier rather than reading it as a lone `!`.
fn operator(word: &str) -> Option<Occur> {
    match word {
        "AND" | "&&" => Some(Occur::Must),
        "OR" | "||" => Some(Occur::Should),
        "NOT" => Some(Occur::MustNot),
        _ => None,
    }
}
/// Parse a fulltext query string into analyzer-produced clauses.
///
/// `AND`/`OR`/`NOT` (uppercase only — lowercase words stay terms, so
/// `'princess AND and NOT kingdom'` requires the token `and`) bind to
/// their neighbours the way Lucene's `Occur` flags do; juxtaposed terms
/// are `Should` (implicit OR). A malformed `~`/`^` suffix, an operator
/// without a term on each side, an empty query, and every construct the
/// module docs list as unsupported are typed syntax errors, never a stray
/// term or a flat OR.
pub(super) fn parse(query: &str, analyzer: Analyzer) -> Result<Vec<Clause>> {
    let words = lex(query)?;
    if words.is_empty() {
        return Err(query_error(query, "the query is empty"));
    }
    let words = attach_suffixes(words);
    let mut parser = Parser {
        query,
        analyzer,
        clauses: Vec::new(),
        position: Position::Nothing,
    };
    for word in &words {
        parser.word(word)?;
    }
    // Every operator-placement refusal is word-level, quoting the operator:
    // the position carries the operator word when the query ends on one.
    if let Position::Operator { word, .. } = parser.position {
        return Err(malformed(word, "an operator needs a term after it"));
    }
    Ok(parser.clauses)
}

/// Glue a detached suffix word (`term ~1`, `term ^2`, which Lucene reads
/// like `term~1`) onto the word before it. A suffix with no ordinary word
/// before it stays alone and fails as a missing term.
fn attach_suffixes(words: Vec<Word>) -> Vec<Word> {
    let mut out: Vec<Word> = Vec::with_capacity(words.len());
    for word in words {
        if word.text.starts_with(SUFFIX_MARKERS) {
            // A suffix binds to the word before it, which is never an
            // operator word.
            if let Some(last) = out
                .last_mut()
                .filter(|last| last.literal || operator(&last.text).is_none())
            {
                last.text.push_str(&word.text);
                continue;
            }
        }
        out.push(word);
    }
    out
}

impl<'w> Parser<'_, 'w> {
    /// One operator word's placement: `AND`/`OR` need a term on both sides,
    /// `NOT` a term after it, and the only operator that may follow another
    /// is `NOT` after `AND`/`OR` (`alpha AND NOT beta`).
    fn operator(&mut self, occur: Occur, word: &'w str) -> Result<()> {
        match self.position {
            // A doubled operator has no operand between the two; the one
            // operator that may follow another is `NOT` after `AND`/`OR`.
            Position::Operator {
                occur: previous, ..
            } if occur != Occur::MustNot || previous == Occur::MustNot => {
                return Err(malformed(
                    word,
                    "an operator needs a term between it and the previous operator",
                ))
            }
            // A leading `NOT` is Lucene's exclusion-only query; a leading
            // `AND`/`OR` has no left operand.
            Position::Nothing if occur != Occur::MustNot => {
                return Err(malformed(word, "an operator needs a term before it"))
            }
            _ => {}
        }
        // Lucene folds an `AND` onto the clause before it: a preceding
        // implicit `Should` becomes `Must`.
        if occur == Occur::Must {
            if let Some(last) = self
                .clauses
                .last_mut()
                .filter(|last| last.occur == Occur::Should)
            {
                last.occur = Occur::Must;
            }
        }
        self.position = Position::Operator { occur, word };
        Ok(())
    }

    fn word(&mut self, word: &'w Word) -> Result<()> {
        if let Some(occur) = word.operator() {
            return self.operator(occur, &word.text);
        }
        let mut occur = match std::mem::replace(&mut self.position, Position::Term) {
            Position::Operator { occur, .. } => occur,
            Position::Nothing | Position::Term => Occur::Should,
        };
        // Every term start is judged for a leading form in the loop below;
        // a `field:` word also judges its own start, which the field name
        // hides from the term.
        let (property, mut text) = split_field(&word.text);
        if property.is_some() {
            refuse_leading_form(&word.text, word.literal)?;
        }
        // The optional `field:` prefix: an empty name is malformed, and `*:`
        // names every field, like no prefix at all.
        let mut property = match property {
            Some("") => return Err(malformed(&word.text, "a `:` needs a field name before it")),
            Some("*") | None => None,
            Some(name) => Some(name),
        };
        // Read the text once: the term and its `~N`/`^B` suffix, then any
        // remainder as its own word. Text left after a fuzzy suffix that is
        // not a `^` boost starts a new word (`term~1x` is a bare `~` plus the
        // term `1x`, as Lucene's lexer reads it): it is its own implicit-OR
        // clause, with no field.
        // Each term keeps the word's provenance (any quoted or escaped
        // character marks the whole scanned word literal). `split_suffix`
        // never returns an empty term, so only a `field:` with nothing after
        // it can reach the loop empty.
        if text.is_empty() {
            return Err(malformed(
                &word.text,
                "a `:` field prefix needs a term after it",
            ));
        }
        let literal = word.literal;
        loop {
            refuse_leading_form(text, literal)?;
            let (term, suffix, remainder) = split_suffix(text)?;
            self.push_target(occur, property, term, &suffix, literal)?;
            let Some(rest) = remainder else { return Ok(()) };
            occur = Occur::Should;
            property = None;
            text = rest;
        }
    }

    /// Push one term's clauses — a wildcard, a fuzzy expansion or the
    /// analyzer's tokens — bounded by the clause cap.
    fn push_target(
        &mut self,
        occur: Occur,
        property: Option<&str>,
        term: &str,
        suffix: &Suffix,
        literal: bool,
    ) -> Result<()> {
        // The one place a clause is appended, so the clause bound holds
        // before an oversized term can allocate past it.
        let mut push = |target: Target| -> Result<()> {
            if self.clauses.len() >= MAX_QUERY_CLAUSES {
                return Err(query_error(
                    self.query,
                    &format!("more than {MAX_QUERY_CLAUSES} clauses"),
                ));
            }
            self.clauses.push(Clause {
                occur,
                property: property.map(str::to_string),
                target,
                boost: suffix.boost,
            });
            Ok(())
        };
        let prepared = if self.analyzer.folds() {
            fold(&term.to_lowercase())
        } else {
            term.to_string()
        };
        let Some(pattern) = Pattern::parse(&prepared)? else {
            // A literal term's analyzed tokens are the phrase rule's only
            // input, and the term arm below pushes them: one analyze, one
            // refusal.
            let analysed = literal.then(|| self.analyzer.analyze(term));
            if let Some(tokens) = &analysed {
                refuse_literal_phrase(tokens.len(), term)?;
            }
            return match suffix.fuzzy {
                // A fuzzy term is matched against the index's stored tokens
                // (lowercased, folded only by a folding index), so it skips the
                // analyzer: neither stemming nor folding (Lucene's multi-term
                // normalisation; calibrated: `eifrém~1` still reaches `eifrem`).
                Some(fuzz) => {
                    for term in tokenize(term) {
                        let edits = fuzz.edits_for(term.chars().count());
                        push(Target::Fuzzy { term, edits })?;
                    }
                    Ok(())
                }
                None => {
                    let tokens = analysed.unwrap_or_else(|| self.analyzer.analyze(term));
                    for token in tokens {
                        push(Target::Term(token))?;
                    }
                    Ok(())
                }
            };
        };
        // A wildcard term is lowercased, and folded under an analyzer that
        // folds (a deliberate deviation from live, which leaves it unfolded:
        // ADR-0583) — never stemmed or stop filtered — and ignores a `~N`
        // suffix (calibrated: `kolleg*~1` matches like `kolleg*`).
        push(Target::Wildcard(pattern))
    }
}

#[cfg(test)]
mod tests {
    use super::scan::MAX_FUZZY_EDITS;
    use super::*;
    use relate_core::Error;

    fn clauses(query: &str) -> Vec<Clause> {
        parse(query, Analyzer::Standard).expect("a well-formed query")
    }

    /// Assert `query` is refused as a typed syntax error under `analyzer`.
    fn assert_syntax_error_with(query: &str, analyzer: Analyzer) {
        let error = parse(query, analyzer).expect_err(query);
        assert!(matches!(error, Error::Syntax(_)), "{query}: {error:?}");
    }

    /// [`assert_syntax_error_with`] under the default analyzer.
    fn assert_syntax_error(query: &str) {
        assert_syntax_error_with(query, Analyzer::Standard);
    }

    fn term(clause: &Clause) -> &str {
        match &clause.target {
            Target::Term(term) | Target::Fuzzy { term, .. } => term,
            Target::Wildcard(_) => panic!("a wildcard clause has no single term"),
        }
    }

    fn edits(clause: &Clause) -> Option<u8> {
        match clause.target {
            Target::Fuzzy { edits, .. } => Some(edits),
            _ => None,
        }
    }

    fn wildcard(term: &str) -> Target {
        Target::Wildcard(Pattern::must_parse(term))
    }

    #[test]
    fn parse_folds_operators_onto_neighbours() {
        let parsed = clauses("princess AND NOT snow");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].occur, Occur::Must);
        assert_eq!(term(&parsed[0]), "princess");
        assert_eq!(parsed[1].occur, Occur::MustNot);
        assert_eq!(term(&parsed[1]), "snow");

        // Lowercase `and` stays a term: only uppercase words are operators.
        let parsed = clauses("princess AND and NOT kingdom");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[1].occur, Occur::Must);
        assert_eq!(term(&parsed[1]), "and");
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
        assert_eq!(term(&parsed[0]), "princess");
        assert_eq!(edits(&parsed[0]), None);
        assert_eq!(parsed[0].boost, 1.0);
    }

    #[test]
    fn escaped_and_quoted_syntax_characters_stay_inside_their_word() {
        // An escaped `:` is text, not a field separator: `ab\:` is one term
        // with no property.
        assert!(clauses(r"ab\:").iter().all(|c| c.property.is_none()));
        // A literal term the analyzer reads as several is a phrase (below),
        // and an escaped `~` is no fuzzy suffix either way.
        for query in [r"a\:b", r#""f:x""#] {
            assert_syntax_error(query);
        }
        assert_syntax_error(r"lamp\~1");
    }

    #[test]
    fn a_literal_term_the_analyzer_reads_as_several_is_a_phrase() {
        // Quoted or escaped, one term the analyzer splits is a phrase of
        // several terms — unsupported, never an implicit OR.
        for query in [
            r#""a b""#,
            r"a\ b",
            r#""a.b""#,
            r#""/re/ x""#,
            // Provenance survives the suffix grammar: the escaped remainder
            // is literal text too (an escaped space is stored raw, so the
            // word's flag — not its text — is what carries it).
            r"x~a\*b",
            r"x~a\ b",
        ] {
            assert_syntax_error(query);
        }
        // One token after analysis stays one term.
        assert!(parse(r#""a_b""#, Analyzer::Standard).is_ok());
    }

    #[test]
    fn fuzzy_suffix_forms_never_leak_digits_as_terms() {
        for (query, expected) in [
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
            assert_eq!(term(&parsed[0]), "kolleger", "{query}");
            assert_eq!(edits(&parsed[0]), Some(expected), "{query}");
            assert_eq!(parsed[0].boost, 1.0, "{query}");
        }
        // Operators still fold around a fuzzy clause.
        let parsed = clauses("kolleger~1 AND lamp");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].occur, Occur::Must);
        assert_eq!(edits(&parsed[0]), Some(1));
        assert_eq!(parsed[1].occur, Occur::Must);
        assert_eq!(edits(&parsed[1]), None);
    }

    #[test]
    fn boost_and_combined_suffixes() {
        let parsed = clauses("lamp^2.5");
        assert_eq!((edits(&parsed[0]), parsed[0].boost), (None, 2.5));
        let parsed = clauses("title:kolleger~1^3");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].property.as_deref(), Some("title"));
        assert_eq!(term(&parsed[0]), "kolleger");
        assert_eq!((edits(&parsed[0]), parsed[0].boost), (Some(1), 3.0));
        // A zero boost is legal; the clause still exists.
        assert_eq!(clauses("lamp^0")[0].boost, 0.0);
    }

    #[test]
    fn distance_above_the_maximum_clamps() {
        assert_eq!(edits(&clauses("lamp~7")[0]), Some(MAX_FUZZY_EDITS));
        // A digit run past f64's range is still a distance past the cap.
        let huge = format!("lamp~{}", "9".repeat(400));
        assert_eq!(edits(&clauses(&huge)[0]), Some(MAX_FUZZY_EDITS));
    }

    #[test]
    fn a_wildcard_term_over_the_part_cap_is_a_typed_syntax_error() {
        let at_cap = format!("{}*", "a".repeat(MAX_WILDCARD_PARTS - 1));
        assert!(parse(&at_cap, Analyzer::Standard).is_ok());
        let over = format!("{}*", "a".repeat(MAX_WILDCARD_PARTS));
        assert_syntax_error(&over);
        // Runs of `*` collapse before the count, so they never trip it.
        let stars = "*".repeat(10 * MAX_WILDCARD_PARTS);
        assert!(parse(&format!("a{stars}b"), Analyzer::Standard).is_ok());
    }

    #[test]
    fn a_query_over_the_clause_cap_is_a_typed_syntax_error() {
        let words = |n: usize| {
            (0..n)
                .map(|i| format!("t{i}~2"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(clauses(&words(MAX_QUERY_CLAUSES)).len(), MAX_QUERY_CLAUSES);
        assert_syntax_error(&words(MAX_QUERY_CLAUSES + 1));
    }

    #[test]
    fn one_word_cannot_append_past_the_clause_cap() {
        // A single word that analyzes into far more tokens than the cap.
        let word = (0..10 * MAX_QUERY_CLAUSES)
            .map(|i| format!("t{i}"))
            .collect::<Vec<_>>()
            .join("-");
        assert_syntax_error(&word);
    }

    #[test]
    fn placement_and_prefix_refusals_quote_the_whole_word() {
        let message = |query: &str| {
            parse(query, Analyzer::Standard)
                .expect_err(query)
                .to_string()
        };
        assert!(message("lamp AND").contains("'AND'"));
        assert!(message("title:").contains("'title:'"));
        assert!(message(":lamp").contains("':lamp'"));
    }

    #[test]
    fn malformed_suffixes_are_typed_syntax_errors() {
        for query in [
            "lamp^x", "lamp^", "lamp^-1", "lamp~1.5", "lamp^2~1", "lamp^2x", "~1", "^2",
            "title:~1", "AND ~1",
        ] {
            assert_syntax_error(query);
        }
    }

    #[test]
    fn fractions_are_minimum_similarity_relative_to_term_length() {
        // Calibrated against Neo4j 2026.09-enterprise: `~0.8` on the
        // 8-character `kolleger` matches like `~1`; 0 is exact; whole
        // numbers of 1 or more clamp like integers.
        assert_eq!(edits(&clauses("kolleger~0.8")[0]), Some(1));
        assert_eq!(edits(&clauses("kolleger~0.0")[0]), Some(0));
        assert_eq!(edits(&clauses("kolleger~0.1")[0]), Some(2));
        assert_eq!(edits(&clauses("abc~0.5")[0]), Some(1));
        assert_eq!(edits(&clauses("kolleger~2.0")[0]), Some(2));
    }

    #[test]
    fn text_after_a_tilde_is_a_new_word() {
        // `term~1x` is a bare `~` plus the separate term `1x`.
        let parsed = clauses("kolleger~1x");
        assert_eq!(parsed.len(), 2);
        assert_eq!((term(&parsed[0]), edits(&parsed[0])), ("kolleger", Some(2)));
        assert_eq!((term(&parsed[1]), edits(&parsed[1])), ("1x", None));
        assert_eq!(parsed[1].occur, Occur::Should);
    }

    #[test]
    fn a_detached_suffix_binds_to_the_previous_word() {
        // `term ~1` reads like `term~1`.
        let parsed = clauses("kolleger ~1");
        assert_eq!(parsed.len(), 1);
        assert_eq!((term(&parsed[0]), edits(&parsed[0])), ("kolleger", Some(1)));
        let parsed = clauses("a AND kolleger ~1 ^2");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].occur, Occur::Must);
        assert_eq!((edits(&parsed[1]), parsed[1].boost), (Some(1), 2.0));
        let parsed = clauses("kolleger ~");
        assert_eq!(edits(&parsed[0]), Some(2));
        let parsed = clauses("a AND kolleger ~1");
        assert_eq!(parsed[0].occur, Occur::Must);
    }

    #[test]
    fn wildcard_terms_are_patterns_lowercased() {
        let parsed = clauses("Kollegg* ?ollegger *gger");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].target, wildcard("kollegg*"));
        assert_eq!(parsed[1].target, wildcard("?ollegger"));
        assert_eq!(parsed[2].target, wildcard("*gger"));
        assert!(parsed.iter().all(|clause| clause.occur == Occur::Should));
    }

    #[test]
    fn wildcards_combine_with_operators_fields_and_boost() {
        let parsed = clauses("title:kolleg*^3 AND NOT lamp*");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].property.as_deref(), Some("title"));
        assert_eq!(parsed[0].target, wildcard("kolleg*"));
        assert_eq!((parsed[0].occur, parsed[0].boost), (Occur::Must, 3.0));
        assert_eq!(parsed[1].occur, Occur::MustNot);
        assert_eq!(parsed[1].target, wildcard("lamp*"));
        // A bare wildcard may carry a boost; `*:` names every field.
        let parsed = clauses("*:*^2");
        assert_eq!(parsed[0].property, None);
        assert_eq!(parsed[0].target, wildcard("*"));
        assert_eq!(parsed[0].boost, 2.0);
    }

    #[test]
    fn a_fuzzy_suffix_on_a_wildcard_is_ignored() {
        let parsed = clauses("kolleg*~1");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].target, wildcard("kolleg*"));
    }

    #[test]
    fn escaped_and_quoted_wildcard_characters_are_ordinary_text() {
        // `kollegger\*` is the term `kollegger`; `"tok1*"` is the term `tok1`
        // (a quoted word is plain text). Grouping parentheses are not
        // supported at all.
        assert_eq!(
            clauses(r"kollegger\*")[0].target,
            Target::Term("kollegger".into())
        );
        assert_eq!(clauses("\"tok1*\"")[0].target, Target::Term("tok1".into()));
        assert!(parse("(tok1*)", Analyzer::Standard).is_err());
    }

    #[test]
    fn a_quoted_operator_modifier_or_regex_is_a_term() {
        // Quoting makes the characters literal: no operator fold, no clause
        // modifier rejection, no regex rejection.
        for query in [r#""AND""#, r#""+alpha""#, r#""/re/""#] {
            let parsed = parse(query, Analyzer::Standard).expect(query);
            assert_eq!(parsed.len(), 1, "{query}");
        }
    }

    #[test]
    fn a_stop_word_is_still_a_term_on_the_left_of_an_operator() {
        // The english analyzer emits no clause for a stop word, but the word
        // is still a term: `the AND lamp` is well-formed; a leading `AND` is
        // not (REL8-2031 review).
        assert!(parse("the AND lamp", Analyzer::English).is_ok());
        assert_syntax_error_with("AND the", Analyzer::English);
    }

    #[test]
    fn unsupported_lucene_syntax_is_a_typed_syntax_error() {
        // Every construct the parser does not implement is rejected, never
        // read as a flat OR of the words inside it (issue 43).
        for query in [
            "(gamma OR one) AND beta",
            "beta AND (gamma OR one)",
            "body:(alpha OR delta)",
            "title:(alpha)",
            "title:[b TO c]",
            "title:{alpha TO carrot}",
            "/gam.*/",
            "title:/gam.*/",
            "+alpha +beta",
            "alpha -beta",
            "alpha !beta",
            "alpha AND !beta",
            "alpha AND -beta",
            "title:+alpha",
            "title:-alpha",
            "title:!alpha",
            "+title:\"alpha\"",
            "+\"alpha\"",
            // A term the suffix grammar mints is judged at its own start.
            "x~+beta",
            "\"alpha beta\"",
            "\"alpha beta\"~2",
            "\"\"",
            ":alpha",
            "title:",
        ] {
            assert_syntax_error(query);
        }
    }

    #[test]
    fn malformed_operator_and_empty_query_are_typed_syntax_errors() {
        for query in [
            "",
            "   ",
            "alpha AND",
            "alpha OR",
            "alpha NOT",
            "alpha OR OR gamma",
            "alpha AND AND beta",
            "alpha NOT NOT beta",
            "AND alpha",
            "OR alpha",
        ] {
            assert_syntax_error(query);
        }
        // `NOT` alone leads, and follows `AND`/`OR`; juxtaposition and an
        // explicit OR keep their folds.
        for query in [
            "NOT alpha",
            "alpha AND NOT beta",
            "alpha OR NOT beta",
            "alpha beta",
        ] {
            assert!(parse(query, Analyzer::Standard).is_ok(), "{query}");
        }
    }

    #[test]
    fn a_wildcard_folds_only_under_a_folding_analyzer() {
        let folding = |query: &str| parse(query, Analyzer::StandardFolding).unwrap();
        assert_eq!(folding("M\u{dc}LL*")[0].target, wildcard("mull*"));
        assert_eq!(folding("EIFR\u{c9}*")[0].target, wildcard("eifre*"));
        // The standard analyzer keeps the accent (lowercased only).
        let plain = clauses("M\u{dc}LL*");
        assert_eq!(plain[0].target, wildcard("m\u{fc}ll*"));
        assert_ne!(plain[0].target, wildcard("mull*"));
    }

    #[test]
    fn a_fuzzy_term_is_not_folded_by_the_query_parser() {
        let parsed = parse("Eifr\u{e9}m~1", Analyzer::StandardFolding).unwrap();
        assert_eq!(
            parsed[0].target,
            Target::Fuzzy {
                term: "eifr\u{e9}m".into(),
                edits: 1
            }
        );
        let parsed = parse("Eifr\u{e9}m", Analyzer::StandardFolding).unwrap();
        assert_eq!(parsed[0].target, Target::Term("eifrem".into()));
    }
}
