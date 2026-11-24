//! The compiled-pattern cache behind `=~` (LABS-1478) and
//! `string.regexReplace` (REL8-618).
//!
//! A regex compile costs microseconds to tens of microseconds; the match it
//! feeds costs tens of nanoseconds. `WHERE n.name =~ $p` used to compile on
//! every row. Compilation is a pure function of the pattern text — no row,
//! no snapshot, no clock — so the memo needs no stage scope: a small
//! per-thread table of recently used patterns, found by comparing the
//! pattern text itself (a short memcmp, no hash of the input, which would
//! cost as much as it saves). The "key" is the one constant a statement
//! binds.
//!
//! An invalid pattern is never cached: every evaluation that reaches it
//! fails exactly as before. A pattern whose program is past the cached
//! budget is remembered as such (no program retained), so it skips the
//! failed budgeted compile and pays only the one default-limit compile.

use std::borrow::Cow;
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
struct PatternCache(Vec<Slot>);

/// One cached pattern: the key (pattern text and mode) and what the table
/// knows about it.
struct Slot {
    pattern: Box<str>,
    mode: Mode,
    entry: Entry,
}

impl Slot {
    /// Whether this slot is the one for `pattern` compiled under `mode`.
    fn matches(&self, pattern: &str, mode: Mode) -> bool {
        self.mode == mode && &*self.pattern == pattern
    }
}

/// How a cached pattern is spelled to the regex crate: the same pattern
/// text compiles to two different programs, so the mode is part of the key.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::eval) enum Mode {
    /// `=~`: a whole-string match, the pattern wrapped in anchors.
    Anchored,
    /// `string.regexReplace`: the pattern as written, searched anywhere.
    Search,
}

/// What the table knows about a pattern.
enum Entry {
    /// The compiled program, within the cached budget.
    Compiled(Regex),
    /// Valid, but past the cached budget: answered by an uncached
    /// default-limit compile, remembered so the budgeted attempt is not
    /// repeated on every evaluation.
    OverBudget,
}

impl PatternCache {
    /// The refreshed entry for `pattern` — recency moves it to the back —
    /// or `None` when the table holds no such pattern. The caller owns the
    /// match, so this API never takes the subject text.
    fn hit(&mut self, pattern: &str, mode: Mode) -> Option<&Entry> {
        let pos = self.0.iter().position(|slot| slot.matches(pattern, mode))?;
        let slot = self.0.remove(pos);
        self.0.push(slot);
        self.0.last().map(|slot| &slot.entry)
    }

    /// A newly classified pattern takes the back; a full table first drops
    /// its least recently used front entry.
    fn insert(&mut self, pattern: &str, mode: Mode, entry: Entry) {
        if self.0.len() == CACHED_PATTERNS {
            self.0.remove(0);
        }
        self.0.push(Slot {
            pattern: pattern.into(),
            mode,
            entry,
        });
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

/// The regex source a pattern compiles from under `mode`.
fn source(pattern: &str, mode: Mode) -> Cow<'_, str> {
    match mode {
        Mode::Anchored => Cow::Owned(anchored(pattern)),
        Mode::Search => Cow::Borrowed(pattern),
    }
}

/// The one default-limit compile for a pattern the table cannot keep —
/// too long to store, or past the cached program budget — used, never
/// stored.
fn uncached<R>(
    pattern: &str,
    mode: Mode,
    use_regex: impl FnOnce(&Regex) -> R,
) -> std::result::Result<R, regex::Error> {
    Regex::new(&source(pattern, mode)).map(|re| use_regex(&re))
}

/// Runs `use_regex` on the compiled program for `pattern` under `mode`,
/// compiling it once into the per-thread table when it fits the budget.
/// An invalid pattern's compile error is returned, never cached.
pub(in crate::eval) fn with_regex<R>(
    pattern: &str,
    mode: Mode,
    use_regex: impl FnOnce(&Regex) -> R,
) -> std::result::Result<R, regex::Error> {
    // The admission rule first: a pattern too long to keep never consults
    // the table (it can never be in it).
    if pattern.len() > MAX_CACHED_PATTERN_BYTES {
        return uncached(pattern, mode, use_regex);
    }
    COMPILED.with(|cache| {
        match cache.borrow_mut().hit(pattern, mode) {
            Some(Entry::Compiled(re)) => return Ok(use_regex(re)),
            Some(Entry::OverBudget) => return uncached(pattern, mode, use_regex),
            None => {}
        }
        let re = match RegexBuilder::new(&source(pattern, mode))
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
            Err(regex::Error::CompiledTooBig(_)) => {
                cache.borrow_mut().insert(pattern, mode, Entry::OverBudget);
                return uncached(pattern, mode, use_regex);
            }
            // An invalid pattern fails exactly as before, never cached.
            Err(error) => return Err(error),
        };
        let used = use_regex(&re);
        cache
            .borrow_mut()
            .insert(pattern, mode, Entry::Compiled(re));
        Ok(used)
    })
}

/// Whether `text` matches `pattern` as a whole string.
pub(super) fn regex_is_match(text: &str, pattern: &str) -> Result<bool> {
    with_regex(pattern, Mode::Anchored, |re| re.is_match(text)).map_err(|_| invalid_regex())
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
    fn the_search_mode_is_keyed_apart_from_the_anchored_one() {
        // The same text is two programs: whole-string for `=~`, anywhere
        // for `string.regexReplace`; each is cached under its own mode.
        assert!(!regex_is_match("xabcx", "abc").unwrap());
        for _ in 0..2 {
            assert!(with_regex("abc", Mode::Search, |re| re.is_match("xabcx")).unwrap());
        }
        assert!(!regex_is_match("xabcx", "abc").unwrap());
        COMPILED.with(|c| {
            let table = &c.borrow().0;
            assert_eq!(
                table.iter().filter(|slot| &*slot.pattern == "abc").count(),
                2
            );
        });
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
        COMPILED.with(|c| assert!(c.borrow().0.iter().all(|slot| &*slot.pattern != "(")));
    }

    #[test]
    fn an_over_budget_program_answers_uncached_from_the_default_limits() {
        // `a{100000}`'s compiled program is past the cached budget but
        // under the crate default, so it answers exactly as before; the
        // table remembers it as over budget, never as a program.
        let pattern = "a{100000}";
        let text = "a".repeat(100_000);
        for _ in 0..2 {
            assert!(regex_is_match(&text, pattern).unwrap());
        }
        COMPILED.with(|c| {
            let table = &c.borrow().0;
            assert!(table
                .iter()
                .filter(|slot| &*slot.pattern == pattern)
                .all(|slot| matches!(slot.entry, Entry::OverBudget)));
            assert_eq!(
                table
                    .iter()
                    .filter(|slot| &*slot.pattern == pattern)
                    .count(),
                1
            );
        });
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
                .all(|slot| slot.pattern.len() <= MAX_CACHED_PATTERN_BYTES))
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
            assert!(table.iter().any(|slot| &*slot.pattern == "a|b"));
            assert!(table.iter().all(|slot| &*slot.pattern != "a|p0"));
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
