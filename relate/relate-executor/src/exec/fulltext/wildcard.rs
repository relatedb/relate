//! Lucene wildcard terms: `*` matches any run of characters (including
//! none), `?` exactly one, `\` makes the next character literal. A pattern
//! is matched against whole indexed terms.

use relate_core::{Error, Result};

/// The most parts one wildcard term may hold after `**` collapses (each
/// literal character, `?` and `*` is one part). Matching a pattern costs
/// time proportional to its parts times the token length for every scanned
/// token, so the bound is checked once per query, never per document. Real
/// terms hold a few dozen parts; Lucene and tantivy likewise reject a
/// wildcard or regex too large to compile, at parse time (ADR-0583).
pub(super) const MAX_WILDCARD_PARTS: usize = 256;

/// One element of a parsed pattern.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Part {
    Literal(char),
    /// `*`
    Run,
    /// `?`
    One,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Pattern(Vec<Part>);

impl Pattern {
    /// The pattern of `term`, `None` when it holds no unescaped `*` or
    /// `?` (an ordinary term), or a typed syntax error when it holds more
    /// than [`MAX_WILDCARD_PARTS`] parts. The term is lowercased first: Lucene
    /// normalises wildcard terms with the lowercase filter only. A caller
    /// whose analyzer folds folds the term before parsing (folding leaves
    /// `*`, `?` and `\` alone), so `?` counts one character of the folded
    /// token: `stra?e` does not match `strasse`, deliberately not special-
    /// cased.
    pub(super) fn parse(term: &str) -> Result<Option<Self>> {
        let mut parts = Vec::new();
        let mut wild = false;
        let mut chars = term.chars().flat_map(char::to_lowercase);
        while let Some(c) = chars.next() {
            parts.push(match c {
                '*' => {
                    wild = true;
                    // `**` is `*`.
                    if parts.last() == Some(&Part::Run) {
                        continue;
                    }
                    Part::Run
                }
                '?' => {
                    wild = true;
                    Part::One
                }
                '\\' => match chars.next() {
                    Some(escaped) => Part::Literal(escaped),
                    None => Part::Literal('\\'),
                },
                other => Part::Literal(other),
            });
        }
        if !wild {
            return Ok(None);
        }
        if parts.len() > MAX_WILDCARD_PARTS {
            return Err(Error::syntax(format!(
                "Invalid fulltext query: a wildcard term holds {} characters, over the \
                 limit of {MAX_WILDCARD_PARTS}",
                parts.len()
            )));
        }
        Ok(Some(Self(parts)))
    }

    /// The pattern of a test's wildcard term, which must be one and within
    /// the part cap.
    #[cfg(test)]
    pub(super) fn must_parse(term: &str) -> Self {
        Self::parse(term)
            .expect("within the part cap")
            .expect("a wildcard term")
    }

    /// Whether the whole of `term` matches the pattern (iterative glob with
    /// single-star backtracking: linear in practice, no recursion).
    pub(super) fn matches(&self, term: &str) -> bool {
        // Walk `term` by byte offset, stepping one `char` at a time, so the
        // per-token scan allocates nothing.
        let step = |at: usize| -> (char, usize) {
            let c = term[at..]
                .chars()
                .next()
                .expect("an offset below the term length starts a char");
            (c, at + c.len_utf8())
        };
        let (mut p, mut t) = (0, 0);
        // The most recent `*` and the text offset it currently absorbs to.
        let mut resume: Option<(usize, usize)> = None;
        while t < term.len() {
            let (c, next) = step(t);
            match self.0.get(p) {
                Some(Part::Run) => {
                    resume = Some((p, t));
                    p += 1;
                }
                Some(Part::One) => {
                    p += 1;
                    t = next;
                }
                Some(Part::Literal(l)) if *l == c => {
                    p += 1;
                    t = next;
                }
                _ => match resume {
                    Some((star, at)) => {
                        let (_, after) = step(at);
                        resume = Some((star, after));
                        p = star + 1;
                        t = after;
                    }
                    None => return false,
                },
            }
        }
        self.0[p..].iter().all(|part| *part == Part::Run)
    }
}

#[cfg(test)]
mod tests {
    use super::Pattern;

    fn pattern(term: &str) -> Pattern {
        Pattern::must_parse(term)
    }

    #[test]
    fn plain_terms_are_not_patterns() {
        assert_eq!(Pattern::parse("kollegger").unwrap(), None);
        // An escaped wildcard character is literal, so no wildcard remains.
        assert_eq!(Pattern::parse(r"kollegger\*").unwrap(), None);
    }

    #[test]
    fn star_runs_and_question_marks_match_whole_terms() {
        assert!(pattern("kollegg*").matches("kollegger"));
        assert!(pattern("kollegg*").matches("kollegg"));
        assert!(!pattern("kollegg*").matches("kolleger"));
        assert!(pattern("*gger").matches("kollegger"));
        assert!(pattern("*olleg*").matches("kollegium"));
        assert!(pattern("k?llegger").matches("kollegger"));
        assert!(!pattern("k?llegger").matches("kllegger"));
        assert!(!pattern("kolleg?").matches("kollegger"));
        assert!(pattern("koll*gger").matches("kollegger"));
        assert!(pattern("k*r").matches("kollegger"));
        assert!(pattern("*").matches("x"));
        assert!(!pattern("?").matches("xy"));
        // Multi-byte characters step by char, not byte, including on a
        // backtrack past a `*`.
        assert!(pattern("k?ln*").matches("kölner"));
        assert!(pattern("*ß*c").matches("aßßc"));
        assert!(!pattern("k?ln").matches("kölner"));
        assert!(pattern("a*b*c").matches("aXbYbZc"));
        assert!(!pattern("a*b*c").matches("aXbYbZ"));
    }

    #[test]
    fn terms_are_lowercased_and_stars_collapse() {
        assert_eq!(pattern("KOLLEGG*"), pattern("kollegg*"));
        assert_eq!(pattern("kolleg**"), pattern("kolleg*"));
    }

    #[test]
    fn escaped_wildcards_are_literal_characters() {
        let escaped = pattern(r"a\*b*");
        assert!(escaped.matches("a*bxyz"));
        assert!(!escaped.matches("axbxyz"));
    }
}
