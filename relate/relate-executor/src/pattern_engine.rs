//! The diagnostic pattern-engine switch (ADR-0519): the typed value the
//! kernels' routing consults, never re-derived at a rung. Resolved once
//! at `Database::open` from `RELATE_PATTERN_ENGINE` (the
//! `RELATE_PARSER` precedent's resolution site and precedence,
//! ADR-0277's history) and installed on the store-level service bundle
//! (`Graph::configure_pattern_engine` in `graph/services`); the rungs
//! read it back through `Graph::pattern_engine`.

use std::str::FromStr;

use relate_core::{Error, Result};

/// Which pattern engine serves a clause (ADR-0519): a diagnostic-only
/// switch for the pattern-engine spike — never Cypher syntax, no
/// query-level spelling, and no stability promise. It dies with the old
/// engines it switches between.
///
/// Read once at `Database::open` from `RELATE_PATTERN_ENGINE`:
/// unset, blank, and `auto` are [`PatternEngine::Auto`] (today's routing
/// exactly); `nfa` forces the frontier family's rung (ADR-0478) onto
/// every frontier-compilable clause, bypassing that rung's cost gate —
/// every structural, answer-shape, allocation and budget proof stands,
/// and a decline still fails through; `walk` stands the traversal
/// kernels down (the `with_kernels_disabled` differential behavior,
/// env-reachable), so the batch scan-filter chain and the per-row walker
/// serve every clause.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PatternEngine {
    /// Today's routing exactly: every rung's own proofs and cost gates
    /// decide, unchanged.
    #[default]
    Auto,
    /// Force the frontier family's rung for every frontier-compilable
    /// clause (the cost gate `frontier_match::frontier_engages` is
    /// bypassed); the batch/chain-kernel rungs stand down for the
    /// clauses the frontier takes — they sit below it on both operator
    /// ladders and decline the quantified shapes structurally.
    Nfa,
    /// Stand the traversal kernels down: the chain/hub-star kernels
    /// (ADR-0473) and the frontier family (ADR-0478) decline every
    /// clause, and the batch scan-filter chain plus the per-row walker
    /// serve — the differential suites' `with_kernels_disabled`
    /// behavior, reachable from a real process.
    Walk,
}

impl PatternEngine {
    /// Every rung, in the vocabulary's order — the parser's iteration
    /// source, so the accepted names derive from [`Self::as_str`] instead
    /// of a second hand-maintained match. Crate-private: the only readers
    /// are this module's `from_str` and `expected_names`, so the table is
    /// not part of the re-exported type's surface.
    pub(crate) const ALL: [PatternEngine; 3] =
        [PatternEngine::Auto, PatternEngine::Nfa, PatternEngine::Walk];

    /// The rung's stable wire name — the same three values the env value
    /// parses (`auto`, `nfa`, `walk`), single-sourced here for the plan
    /// args.
    pub const fn as_str(self) -> &'static str {
        match self {
            PatternEngine::Auto => "auto",
            PatternEngine::Nfa => "nfa",
            PatternEngine::Walk => "walk",
        }
    }

    /// The authored rejection's expected-names clause, built from
    /// [`Self::ALL`] so a renamed rung reaches the message too.
    fn expected_names() -> String {
        let names = Self::ALL.map(Self::as_str);
        let (last, rest) = names.split_last().expect("three pattern engines");
        format!("{}, or {last}", rest.join(", "))
    }
}

impl FromStr for PatternEngine {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let lower = s.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|engine| engine.as_str() == lower)
            .ok_or_else(|| {
                Error::semantic(format!(
                    "invalid pattern engine {lower:?}; expected {}",
                    Self::expected_names()
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The value vocabulary, one parse each: the three valid values
    /// (case-insensitively, the `DurabilityMode` grammar) and the
    /// unknown value's authored rejection naming all three.
    #[test]
    fn parses_the_three_values_and_rejects_the_rest() {
        assert_eq!(
            "auto".parse::<PatternEngine>().unwrap(),
            PatternEngine::Auto
        );
        assert_eq!("nfa".parse::<PatternEngine>().unwrap(), PatternEngine::Nfa);
        assert_eq!(
            "walk".parse::<PatternEngine>().unwrap(),
            PatternEngine::Walk
        );
        assert_eq!(
            " NFA ".parse::<PatternEngine>().unwrap(),
            PatternEngine::Nfa,
            "trimmed, case-insensitive"
        );
        let err = "dfs".parse::<PatternEngine>().unwrap_err();
        let display = err.to_string();
        assert!(display.contains("invalid pattern engine"), "{display}");
        assert!(display.contains("expected auto, nfa, or walk"), "{display}");
    }
}
