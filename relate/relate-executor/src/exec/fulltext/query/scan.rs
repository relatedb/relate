//! The fulltext query scanner: the query text split into whole words
//! (quotes and escapes resolved) and the `~N` / `^B` suffix grammar.
//! [`super::parse`] reads its words.

use relate_core::{Error, Result};

use super::super::analyzer::has_token;
use super::super::wildcard::Pattern;

/// Lucene's `FuzzyQuery` ceiling on edits (its Levenshtein automata stop at
/// distance 2) and the default a bare `~` selects.
pub(super) const MAX_FUZZY_EDITS: u8 = 2;

/// The fuzzy part of a `~` suffix before it meets its term's length.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Fuzz {
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
    pub(super) fn edits_for(self, chars: usize) -> u8 {
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
pub(super) struct Suffix {
    pub(super) fuzzy: Option<Fuzz>,
    pub(super) boost: f64,
}

/// The clause modifiers Lucene reads at a term's start (`+term`, `-term`,
/// `!term`) and this parser rejects — a shared vocabulary, because a literal
/// one must travel escaped for the parser's printed-form judgement.
pub(super) const CLAUSE_MODIFIERS: [char; 3] = ['+', '-', '!'];

/// Characters the later stages read as syntax; a literal one travels as `\c`
/// (the pattern parser unescapes it, the field splitter and the suffix
/// splitter skip it).
fn is_syntax(c: char) -> bool {
    matches!(c, '*' | '?' | '\\' | ':')
        || SUFFIX_MARKERS.contains(&c)
        || CLAUSE_MODIFIERS.contains(&c)
}

/// The Lucene construct an unsupported character belongs to, for the
/// rejection text; `None` for a character this parser reads as text.
fn unsupported(c: char) -> Option<&'static str> {
    match c {
        '(' | ')' => Some("grouping parentheses"),
        '[' | ']' | '{' | '}' => Some("range brackets"),
        _ => None,
    }
}

/// The markers that open a `~N` / `^B` suffix — the one spelling this
/// splitter and the parser's glue stage (`super::attach_suffixes`) share.
pub(super) const SUFFIX_MARKERS: [char; 2] = ['~', '^'];

/// The first position of one of `targets` in `text` that no backslash
/// escapes.
pub(super) fn find_unescaped(text: &str, targets: &[char]) -> Option<usize> {
    let mut chars = text.char_indices();
    while let Some((at, c)) = chars.next() {
        if c == '\\' {
            chars.next();
        } else if targets.contains(&c) {
            return Some(at);
        }
    }
    None
}

/// One scanned query word: its text, and whether it was written as literal
/// text (quoted, or containing an escaped character). A literal word is never
/// an operator, a clause modifier or a regular expression — its characters
/// are what the analyzer reads.
#[derive(Debug, Default)]
pub(super) struct Word {
    pub(super) text: String,
    pub(super) literal: bool,
}

impl Word {
    /// Push the character `chars` reads next as escaped text.
    fn push_escaped(&mut self, chars: &mut impl Iterator<Item = char>, query: &str) -> Result<()> {
        let Some(c) = chars.next() else {
            return Err(malformed(query, "a trailing `\\` escapes nothing"));
        };
        self.push_literal(c);
        Ok(())
    }

    /// Push one literal character: a syntax character keeps its backslash for
    /// the later stages, every other one is itself, and the word is marked
    /// literal — the flag every segment of the word inherits.
    fn push_literal(&mut self, c: char) {
        if is_syntax(c) {
            self.text.push('\\');
        }
        self.text.push(c);
        self.literal = true;
    }

    /// Push one character written unquoted and unescaped; the word's literal
    /// flag is untouched.
    fn push_live(&mut self, c: char) {
        self.text.push(c);
    }

    /// Append one quoted segment, up to its closing quote; an empty quote is
    /// no term at all.
    fn push_quoted(&mut self, chars: &mut impl Iterator<Item = char>, query: &str) -> Result<()> {
        let start = self.text.len();
        loop {
            let Some(c) = chars.next() else {
                return Err(malformed(query, "a `\"` is never closed"));
            };
            match c {
                '\\' => self.push_escaped(chars, query)?,
                '"' if self.text.len() == start => {
                    return Err(malformed(query, "an empty `\"\"` phrase is no term"))
                }
                '"' => return Ok(()),
                c => self.push_literal(c),
            }
        }
    }
}

/// Take the open word into `words` when it is non-empty (`Word::default`
/// starts the next one); its text is judged by the parser.
fn flush(words: &mut Vec<Word>, open: &mut Word) {
    if !open.text.is_empty() {
        words.push(std::mem::take(open));
    }
}

/// Scan the query into whole words, resolving quotes and escapes and nothing
/// else. A quoted character, or one after a backslash, is literal text: a
/// syntax character stays escaped for the later stages, `\"` is a quote that
/// never toggles quoting, and a separator after a backslash joins its word.
/// A trailing backslash, a quote left open at the end of the query, an
/// unsupported grouping/range character outside quotes, and an unsupported
/// word form are typed syntax errors.
pub(super) fn lex(query: &str) -> Result<Vec<Word>> {
    let mut words = Vec::new();
    let mut open = Word::default();
    // Whether the open word ends with an unescaped `:` — a `field:` prefix a
    // following quoted term attaches to (`title:"alpha"`). One assignment at
    // the loop's end owns it; the quote arm reads the previous character's
    // value.
    let mut field_prefix = false;
    let mut chars = query.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            open.push_escaped(&mut chars, query)?;
        } else if c == '"' {
            if !field_prefix {
                flush(&mut words, &mut open);
            }
            open.push_quoted(&mut chars, query)?;
            // The quoted term closes the word: a suffix right after it
            // (`title:"alpha"~2`) reattaches through the parser's glue, and
            // text glued to it is a word of its own.
            flush(&mut words, &mut open);
        } else if let Some(construct) = unsupported(c) {
            return Err(query_error(
                query,
                &format!("{construct} are not supported"),
            ));
        } else if c.is_whitespace() {
            flush(&mut words, &mut open);
        } else {
            open.push_live(c);
        }
        field_prefix = c == ':';
    }
    flush(&mut words, &mut open);
    Ok(words)
}

/// Split a word's text into its term, its `~N` / `^B` tail (`term~N^B`,
/// either part optional) and the text that must be read as a new word, if
/// any. A word with neither marker is returned untouched.
pub(super) fn split_suffix(text: &str) -> Result<(&str, Suffix, Option<&str>)> {
    // A bare `~` reaches the most edits Lucene allows.
    let bare_fuzz = Fuzz::Edits(MAX_FUZZY_EDITS);
    let mut suffix = Suffix {
        fuzzy: None,
        boost: 1.0,
    };
    let Some(at) = find_unescaped(text, &SUFFIX_MARKERS) else {
        return Ok((text, suffix, None));
    };
    let (term, mut rest) = text.split_at(at);
    if !Pattern::is_wildcard(term) && !has_token(term) {
        return Err(malformed(text, "a `~`/`^` suffix needs a term before it"));
    }
    if let Some(after) = rest.strip_prefix('~') {
        let (number, tail) = take_number(after);
        if !tail.is_empty() && !tail.starts_with('^') {
            // `~` then text that is not a boost: a bare `~`, the text is its
            // own word.
            suffix.fuzzy = Some(bare_fuzz);
            return Ok((term, suffix, Some(after)));
        }
        suffix.fuzzy = Some(if number.is_empty() {
            bare_fuzz
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
    Ok((term, suffix, None))
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

/// The rejection of one query word.
pub(super) fn malformed(word: &str, reason: &str) -> Error {
    Error::syntax(format!("Invalid fulltext query term '{word}': {reason}"))
}

/// The rejection of the query as a whole — the scanner's unsupported
/// constructs and the parser's operator and clause-count refusals, one
/// spelling.
pub(super) fn query_error(query: &str, reason: &str) -> Error {
    Error::syntax(format!("Invalid fulltext query '{query}': {reason}"))
}

/// What a `~N` suffix selects (`raw` is `digits[.digits]`).
///
/// Calibrated against Neo4j 2026.09-enterprise: an integer clamps to the
/// maximum of 2 (`~3` and `~99` match like `~2`), a value in `[0, 1)` is a
/// minimum similarity (`~0.8` matches like `~1` on an 8-character term), and
/// a non-integer of 1 or more (`~1.5`) is a parse error.
fn fuzz_spec(raw: &str) -> std::result::Result<Fuzz, &'static str> {
    let value: f64 = raw.parse().expect("digits[.digits] is a valid float");
    if value.is_infinite() {
        // A whole number too large for f64 is still past the cap.
        return Ok(Fuzz::Edits(MAX_FUZZY_EDITS));
    }
    if value < 1.0 {
        Ok(Fuzz::MinSimilarity(value))
    } else if value.fract() == 0.0 {
        // A whole number clamps to the maximum, however large it is spelled
        // (a 2^53-and-beyond distance is still only ever two edits).
        Ok(Fuzz::Edits(value.min(f64::from(MAX_FUZZY_EDITS)) as u8))
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

    /// One query's words, text only — the scanner's shape most tests
    /// assert on.
    fn lexed(query: &str) -> Vec<String> {
        lex(query)
            .expect("a well-formed query")
            .into_iter()
            .map(|word| word.text)
            .collect()
    }

    #[test]
    fn lex_keeps_escapes_strict_inside_and_outside_quotes() {
        // A quoted escaped wildcard stays one literal character, not a
        // literal backslash followed by a live wildcard.
        assert_eq!(lexed(r#""foo\*bar""#), [r"foo\*bar"]);
        // An escaped quote never toggles quoting: the `*` after it is live.
        assert_eq!(lexed(r#"foo\"*bar"#), [r#"foo"*bar"#]);
        // Quoted syntax characters are escaped; a quoted backslash is text.
        assert_eq!(lexed(r#""a*b""#), [r"a\*b"]);
        assert_eq!(lexed(r#""a\\b""#), [r"a\\b"]);
        // Outside quotes `\\` is a literal backslash and the `*` stays live.
        assert_eq!(lexed(r"a\\*"), [r"a\\*"]);
        // A separator is a word boundary, escaped or not: `a\ b` is a
        // literal term holding a separator, which the phrase rule refuses
        // (see `a_literal_term_the_analyzer_reads_as_several_is_a_phrase`).
    }

    #[test]
    fn an_unterminated_quote_is_a_typed_syntax_error() {
        let err = lex(r#"lamp "post"#).expect_err("the quote never closes");
        assert!(err.to_string().contains("never closed"), "{err}");
    }

    #[test]
    fn a_quoted_term_attaches_to_its_field_prefix_and_is_its_own_word() {
        // The quoted term closes the word: text glued after it is a word of
        // its own, and a suffix reattaches through the parser's glue.
        assert_eq!(lexed(r#"foo"bar"baz"#), ["foo", "bar", "baz"]);
        assert_eq!(lexed(r#"title:"alpha""#), ["title:alpha"]);
        // An escaped `:` is literal text, so no prefix attaches: `a\:"b"`
        // is the word `a\:` and the quoted word `b`.
        assert_eq!(lexed(r#"a\:"b""#), [r"a\:", "b"]);
    }

    #[test]
    fn a_quoted_word_is_literal() {
        // A quoted word keeps its characters and is marked literal; a quoted
        // `+` travels escaped like any literal syntax character.
        let words = lex(r#""AND" "+alpha" "/re/""#).expect("well-formed");
        assert!(words.iter().all(|word| word.literal), "{words:?}");
        assert_eq!(
            words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(),
            ["AND", "\\+alpha", "/re/"]
        );
    }

    #[test]
    fn a_trailing_backslash_is_a_typed_syntax_error() {
        let err = lex("lamp\\").expect_err("nothing to escape");
        assert!(err.to_string().contains("trailing"), "{err}");
    }
}
