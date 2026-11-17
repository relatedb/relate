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

use regex::{Regex, RegexBuilder};
use relate_core::{Error, Result};

/// Longest pattern text that is cached.
const MAX_CACHED_PATTERN_BYTES: usize = 1024;
/// Compiled-program budget for a cached pattern: it bounds both the compiled
/// NFA and the lazy-DFA match cache (which defaults to 2 MiB per pattern),
/// so the eight cached patterns a connection thread can pin after the
/// statement stay within a few MiB.
const MAX_CACHED_PROGRAM_BYTES: usize = 256 * 1024;

/// Patterns kept per thread. A statement binds a handful of `=~` patterns;
/// past this the oldest is dropped, so a stream of distinct dynamic
/// patterns costs one compile each, as before, and never grows the table.
const CACHED_PATTERNS: usize = 8;

/// The one recency policy: the back holds the recently used pattern, the
/// front is the eviction victim. A hit refreshes its entry to the back;
/// an insert into a full table first drops the front. Recency and
/// eviction live here, never at a call site.
struct PatternCache(Vec<(Box<str>, Regex)>);

impl PatternCache {
    /// The refreshed entry for `pattern` — recency moves it to the back —
    /// or `None` when the table holds no such pattern. The caller owns the
    /// match, so this API never takes the subject text.
    fn hit(&mut self, pattern: &str) -> Option<&Regex> {
        let pos = self.0.iter().position(|(known, _)| &**known == pattern)?;
        let entry = self.0.remove(pos);
        self.0.push(entry);
        self.0.last().map(|(_, re)| re)
    }

    /// A newly compiled pattern takes the back; a full table first drops
    /// its least recently used front entry.
    fn insert(&mut self, pattern: &str, re: Regex) {
        if self.0.len() == CACHED_PATTERNS {
            self.0.remove(0);
        }
        self.0.push((pattern.into(), re));
    }
}

thread_local! {
    static COMPILED: RefCell<PatternCache> = const { RefCell::new(PatternCache(Vec::new())) };
}

/// The one authored rejection an invalid `=~` pattern reports — 22N11, an
/// invalid-argument data exception — spelled once so the crate's own error
/// type never crosses this module's seam.
fn invalid_regex() -> Error {
    Error::argument_error("invalid regular expression")
}

/// The one anchored spelling of a pattern: `=~` is a whole-string match
/// (Neo4j's `Pattern.matches`), not a substring search — `'21_val' =~
/// '1_.*'` is false. The non-capturing group keeps an alternation (`a|b`)
/// inside the anchors.
fn anchored(pattern: &str) -> String {
    format!("^(?:{pattern})$")
}

/// The one default-limit compile for a pattern the table cannot keep —
/// too long to store, or past the cached program budget — anchored,
/// matched, never stored.
fn uncached_match(text: &str, pattern: &str) -> Result<bool> {
    Regex::new(&anchored(pattern))
        .map(|re| re.is_match(text))
        .map_err(|_| invalid_regex())
}

/// Whether `text` matches `pattern` as a whole string.
pub(super) fn regex_is_match(text: &str, pattern: &str) -> Result<bool> {
    // The admission rule first: a pattern too long to keep never consults
    // the table (it can never be in it) and never pays the anchoring copy.
    if pattern.len() > MAX_CACHED_PATTERN_BYTES {
        return uncached_match(text, pattern);
    }
    COMPILED.with(|cache| {
        if let Some(re) = cache.borrow_mut().hit(pattern) {
            return Ok(re.is_match(text));
        }
        let anchored = anchored(pattern);
        let re = match RegexBuilder::new(&anchored)
            .size_limit(MAX_CACHED_PROGRAM_BYTES)
            // The matcher's lazy-DFA cache would otherwise default to
            // 2 MiB per cached pattern; bounded here so a cached program's
            // total retention matches the budget above. A cache that
            // outgrows it falls back to the slower NFA engine — never a
            // wrong answer, never a build failure.
            .dfa_size_limit(MAX_CACHED_PROGRAM_BYTES)
            .build()
        {
            Ok(re) => re,
            Err(regex::Error::CompiledTooBig(_)) => return uncached_match(text, pattern),
            // An invalid pattern fails exactly as before, never cached.
            Err(_) => return Err(invalid_regex()),
        };
        let matched = re.is_match(text);
        cache.borrow_mut().insert(pattern, re);
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
        COMPILED.with(|c| assert!(c.borrow().0.iter().all(|(p, _)| &**p != "(")));
    }

    #[test]
    fn an_over_budget_program_answers_uncached_from_the_default_limits() {
        // `a{100000}`'s compiled program is past the cached budget but
        // under the crate default, so it answers exactly as before and is
        // never stored.
        let pattern = "a{100000}";
        let text = "a".repeat(100_000);
        for _ in 0..2 {
            assert!(regex_is_match(&text, pattern).unwrap());
        }
        COMPILED.with(|c| assert!(c.borrow().0.iter().all(|(p, _)| &**p != pattern)));
    }

    #[test]
    fn a_long_pattern_matches_but_is_never_cached() {
        let long = format!("{}x", "a".repeat(MAX_CACHED_PATTERN_BYTES));
        let text = format!("{}x", "a".repeat(MAX_CACHED_PATTERN_BYTES));
        for _ in 0..3 {
            assert!(regex_is_match(&text, &long).unwrap());
        }
        COMPILED.with(|c| {
            assert!(c
                .borrow()
                .0
                .iter()
                .all(|(p, _)| p.len() <= MAX_CACHED_PATTERN_BYTES))
        });
    }

    #[test]
    fn a_hit_pattern_is_not_the_next_eviction_victim() {
        // Prime a full table with "a|b" sunk to the front: the next
        // eviction victim. The hit that follows refreshes it to the back,
        // so the one distinct insert evicts the oldest primed pattern
        // instead — without the refresh, "a|b" itself would be gone.
        assert!(regex_is_match("b", "a|b").unwrap());
        for i in 0..(CACHED_PATTERNS - 1) {
            assert!(regex_is_match("a", &format!("a|p{i}")).unwrap());
        }
        assert!(regex_is_match("b", "a|b").unwrap());
        assert!(regex_is_match("a", "a|q").unwrap());
        COMPILED.with(|c| {
            let table = &c.borrow().0;
            assert!(table.iter().any(|(p, _)| &**p == "a|b"));
            assert!(table.iter().all(|(p, _)| &**p != "a|p0"));
            assert_eq!(table.len(), CACHED_PATTERNS);
        });
    }

    #[test]
    fn the_table_stays_bounded_under_distinct_patterns() {
        for i in 0..(CACHED_PATTERNS * 4) {
            assert!(regex_is_match("a", &format!("a|b{i}")).unwrap());
        }
        COMPILED.with(|c| assert_eq!(c.borrow().0.len(), CACHED_PATTERNS));
        // The first pattern was evicted and still answers correctly.
        assert!(regex_is_match("a", "a|b0").unwrap());
    }
}
