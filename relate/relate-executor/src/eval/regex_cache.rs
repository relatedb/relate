//! The `=~` operator's compiled-pattern cache (LABS-1478).
//!
//! A regex compile costs microseconds to tens of microseconds; the match it
//! feeds costs tens of nanoseconds. `WHERE n.name =~ $p` used to compile on
//! every row. Compilation is a pure function of the pattern text — no row,
//! no snapshot, no clock — so the memo needs no stage scope: a small
//! per-thread table of recently used patterns, found by comparing the
//! pattern text itself (a short memcmp, no hash of the input, which would
//! cost as much as it saves). This is the memo API's tier 0 in its
//! simplest form: the "key" is the one constant a statement binds.
//!
//! An invalid pattern is never cached: every evaluation that reaches it
//! fails exactly as before.

use std::cell::RefCell;

use regex::Regex;

/// Patterns kept per thread. A statement binds a handful of `=~` patterns;
/// past this the oldest is dropped, so a stream of distinct dynamic
/// patterns costs one compile each, as before, and never grows the table.
const CACHED_PATTERNS: usize = 8;

thread_local! {
    static COMPILED: RefCell<Vec<(Box<str>, Regex)>> = const { RefCell::new(Vec::new()) };
}

/// Whether `text` matches `pattern` as a whole string.
pub(crate) fn regex_is_match(text: &str, pattern: &str) -> std::result::Result<bool, regex::Error> {
    COMPILED.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((_, re)) = cache.iter().find(|(known, _)| &**known == pattern) {
            return Ok(re.is_match(text));
        }
        // `=~` is a whole-string match (Neo4j's `Pattern.matches`), not a
        // substring search: `'21_val' =~ '1_.*'` is false. The non-capturing
        // group keeps an alternation (`a|b`) inside the anchors.
        let re = Regex::new(&format!("^(?:{pattern})$"))?;
        let matched = re.is_match(text);
        if cache.len() == CACHED_PATTERNS {
            cache.remove(0);
        }
        cache.push((pattern.into(), re));
        Ok(matched)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_whole_string_and_keeps_alternation_anchored() {
        assert!(regex_is_match("abc", "a.c").unwrap());
        assert!(!regex_is_match("21_val", "1_.*").unwrap());
        assert!(regex_is_match("b", "a|b").unwrap());
        assert!(!regex_is_match("bx", "a|b").unwrap());
    }

    #[test]
    fn a_cached_pattern_answers_like_a_fresh_one() {
        for _ in 0..3 {
            assert!(regex_is_match("hello", "h.*o").unwrap());
            assert!(!regex_is_match("help", "h.*o").unwrap());
        }
    }

    #[test]
    fn an_invalid_pattern_is_never_cached() {
        for _ in 0..3 {
            assert!(regex_is_match("x", "(").is_err());
        }
        COMPILED.with(|c| assert!(c.borrow().iter().all(|(p, _)| &**p != "(")));
    }

    #[test]
    fn the_table_stays_bounded_under_distinct_patterns() {
        for i in 0..(CACHED_PATTERNS * 4) {
            assert!(regex_is_match("a", &format!("a|b{i}")).unwrap());
        }
        COMPILED.with(|c| assert_eq!(c.borrow().len(), CACHED_PATTERNS));
        // The first pattern was evicted and still answers correctly.
        assert!(regex_is_match("a", "a|b0").unwrap());
    }
}
