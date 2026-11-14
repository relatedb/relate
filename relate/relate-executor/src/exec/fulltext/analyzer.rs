//! The fulltext analyzers: the one name table, the behaviours the engine
//! differentiates, and the index/query pair one scan crosses.

use relate_core::{bare_type_name, Error, Result, Value};

use super::fold::fold;
use super::stemmer::porter_stem;
use super::tokenize;

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
    /// `standard-folding`: the standard tokenizer plus ASCII folding.
    StandardFolding,
    English,
}

/// The one analyzer vocabulary: every fulltext analyzer name live Neo4j
/// admits (live-verified 2026-11-02 on neo4j:2026.09-enterprise) with the
/// behaviour it selects — `english` drops English stop words and
/// Porter-stems, `standard-folding` folds Latin diacritics, every other
/// admitted name shares the standard tokenizer. The name admission, the rejection sentence's list and
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
    ("standard-folding", Analyzer::StandardFolding),
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
    pub(super) fn analyze(self, text: &str) -> Vec<String> {
        let tokens = tokenize(text);
        match self {
            Self::Standard => tokens,
            Self::StandardFolding => tokens.iter().map(|token| fold(token)).collect(),
            Self::English => tokens
                .into_iter()
                .filter(|token| !ENGLISH_STOP_WORDS.contains(&token.as_str()))
                .map(|token| porter_stem(&token))
                .collect(),
        }
    }
}

impl Analyzer {
    /// Whether the analyzer ASCII-folds (stored tokens, plain query terms
    /// and, by ruling, wildcard terms).
    pub(crate) fn folds(self) -> bool {
        matches!(self, Self::StandardFolding)
    }

    /// The tokens a stored value contributes under the index's own analyzer:
    /// folded for a folding index, the plain tokenizer otherwise (stop words
    /// and stems stay a query-side shortcut).
    pub(super) fn stored_tokens(self, text: &str) -> Vec<String> {
        if self.folds() {
            self.analyze(text)
        } else {
            tokenize(text)
        }
    }

    /// The analyzer a query without `WITH ANALYZER` uses against an index
    /// configured with `self`: the index's folding carries over, its stemming
    /// does not (stored tokens are unstemmed).
    pub(crate) fn query_default(self) -> Self {
        if self.folds() {
            self
        } else {
            Self::Standard
        }
    }
}

/// The two analyzers one scan crosses: the index's (what its stored tokens
/// went through) and the query's (what the query's terms go through).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Analyzers {
    pub(super) index: Analyzer,
    pub(super) query: Analyzer,
}

impl Analyzers {
    /// `query_override` is the evaluated `WITH ANALYZER`; absent, the query
    /// follows the index ([`Analyzer::query_default`]).
    pub(crate) fn new(index: Analyzer, query_override: Option<Analyzer>) -> Self {
        Self {
            index,
            query: query_override.unwrap_or_else(|| index.query_default()),
        }
    }
}

/// The admitted analyzer names, rendered as the rejection sentence's list
/// in the one [`ANALYZERS`] table's order.
pub(super) fn analyzer_names() -> String {
    let names: Vec<&str> = ANALYZERS.iter().map(|(name, _)| *name).collect();
    let (last, rest) = names.split_last().expect("the analyzer table is non-empty");
    format!("{} or {last}", rest.join(", "))
}
