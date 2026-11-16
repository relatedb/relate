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
/// behaviour it selects and the description live's
/// `db.index.fulltext.listAvailableAnalyzers` reports for it (live-verified
/// 2026-11-16 on neo4j:2026.10-enterprise) — `english` drops English stop
/// words and Porter-stems, `standard-folding` folds Latin diacritics, every
/// other admitted name shares the standard tokenizer. The name admission,
/// the rejection sentence's list, `exec::ddl_options`' `fulltext.analyzer`
/// config validation and the `listAvailableAnalyzers` rows all read these
/// rows; the name list, the name→behaviour and the name→description
/// mappings are never split across modules again.
const ANALYZERS: [(&str, Analyzer, &str); 45] = [
    (
        "arabic",
        Analyzer::Standard,
        "Arabic analyzer with light stemming, as specified by \"Light Stemming for Arabic Information Retrieval\".",
    ),
    ("armenian", Analyzer::Standard, "Armenian analyzer with stemming and stop word filtering."),
    ("basque", Analyzer::Standard, "Basque analyzer with stemming and stop word filtering."),
    (
        "brazilian",
        Analyzer::Standard,
        "Brazilian Portuguese analyzer with stemming and stop word filtering.",
    ),
    (
        "bulgarian",
        Analyzer::Standard,
        "Bulgarian analyzer with light stemming, as specified by \"Searching Strategies for the Bulgarian Language\", and stop word filtering.",
    ),
    ("catalan", Analyzer::Standard, "Catalan analyzer with stemming and stop word filtering."),
    (
        "cjk",
        Analyzer::Standard,
        "CJK - Chinese/Japanese/Korean - analyzer. Terms are normalised and case-folded. Produces bi-grams, and filters out stop words.",
    ),
    (
        "classic",
        Analyzer::Standard,
        "Classic Lucene analyzer. Similar to 'standard', but with worse unicode support.",
    ),
    ("czech", Analyzer::Standard, "Czech analyzer with stemming and stop word filtering."),
    ("danish", Analyzer::Standard, "Danish analyzer with stemming and stop word filtering."),
    ("dutch", Analyzer::Standard, "Dutch analyzer with stemming and stop word filtering."),
    (
        "email",
        Analyzer::Standard,
        "Tokenizes into sequences of alpha-numeric, numeric, URL, email, southeast asian terms, and into terms of individual ideographic and hiragana characters. English stop words are filtered out.",
    ),
    ("english", Analyzer::English, "English analyzer with stemming and stop word filtering."),
    ("finnish", Analyzer::Standard, "Finnish analyzer with stemming and stop word filtering."),
    ("french", Analyzer::Standard, "French analyzer with stemming and stop word filtering."),
    ("galician", Analyzer::Standard, "Galician analyzer with stemming and stop word filtering."),
    ("german", Analyzer::Standard, "German analyzer with stemming and stop word filtering."),
    ("greek", Analyzer::Standard, "Greek analyzer with stemming and stop word filtering."),
    (
        "hindi",
        Analyzer::Standard,
        "Hindi analyzer with stemming, normalization, and stop word filtering.",
    ),
    ("hungarian", Analyzer::Standard, "Hungarian analyzer with stemming and stop word filtering."),
    ("indonesian", Analyzer::Standard, "Indonesian analyzer with stemming and stop word filtering."),
    ("irish", Analyzer::Standard, "Irish analyzer with stemming and stop word filtering."),
    ("italian", Analyzer::Standard, "Italian analyzer with stemming and stop word filtering."),
    (
        "keyword",
        Analyzer::Standard,
        "Keyword analyzer \"tokenizes\" the text as a single term. Useful for zip-codes, ids, etc. Situations where complete and exact matches are desired.",
    ),
    ("latvian", Analyzer::Standard, "Latvian analyzer with stemming and stop word filtering."),
    ("lithuanian", Analyzer::Standard, "Lithuanian analyzer with stemming and stop word filtering."),
    ("norwegian", Analyzer::Standard, "Norwegian analyzer with stemming and stop word filtering."),
    (
        "persian",
        Analyzer::Standard,
        "Persian analyzer. Tokenizes with zero-width non-joiner characters in addition to whitespace. Persian-specific variants, such as the farsi 'yeh' and 'keheh', are standardized. Simple stemming is accomplished via stop words.",
    ),
    ("portuguese", Analyzer::Standard, "Portuguese analyzer with stemming and stop word filtering."),
    ("romanian", Analyzer::Standard, "Romanian analyzer with stemming and stop word filtering."),
    ("russian", Analyzer::Standard, "Russian analyzer with stemming and stop word filtering."),
    (
        "simple",
        Analyzer::Standard,
        "A simple analyzer that tokenizes at non-letter boundaries. No stemming or filtering. Works okay for most European languages, but is terrible for languages where words are not separated by spaces, such as many Asian languages.",
    ),
    ("sorani", Analyzer::Standard, "Sorani Kurdish analyzer with stemming and stop word filtering."),
    ("spanish", Analyzer::Standard, "Spanish analyzer with stemming and stop word filtering."),
    (
        "standard",
        Analyzer::Standard,
        "The standard analyzer. Tokenizes on non-letter and filters out English stop words and punctuation. Does no stemming, but takes care to keep likely product names, URLs and email addresses as single terms.",
    ),
    (
        "standard-folding",
        Analyzer::StandardFolding,
        "Analyzer that uses ASCIIFoldingFilter to remove accents (diacritics). Otherwise behaves as standard english analyzer. Note! This Analyser may have unexpected behaviour, such as tokenizing, for all non ASCII numbers and symbols.",
    ),
    (
        "standard-no-stop-words",
        Analyzer::Standard,
        "The default analyzer. Similar to the 'standard' analyzer, but filters no stop words. Tokenizes on non-letter boundaries filters out punctuation. Does no stemming, but takes care to keep likely product names, URLs and email addresses as single terms.",
    ),
    (
        "stop",
        Analyzer::Standard,
        "Stop analyzer tokenizes at non-letter characters, and filters out English stop words. This differs from the 'classic' and 'standard' analyzers in that it makes no effort to recognize special terms, like likely product names, URLs or email addresses.",
    ),
    ("swedish", Analyzer::Standard, "Swedish analyzer with stemming and stop word filtering."),
    (
        "thai",
        Analyzer::Standard,
        "Thai analyzer with stop word filtering. It relies on the Java built-in localization support for the Thai locale in order to break apart and tokenize words, which might not be available depending on Java version and JRE vendor.",
    ),
    ("turkish", Analyzer::Standard, "Turkish analyzer with stemming and stop word filtering."),
    (
        "unicode_whitespace",
        Analyzer::Standard,
        "Breaks text into terms by characters that have the unicode WHITESPACE property.",
    ),
    (
        "url",
        Analyzer::Standard,
        "Tokenizes into sequences of alpha-numeric, numeric, URL, email, southeast asian terms, and into terms of individual ideographic and hiragana characters. English stop words are filtered out.",
    ),
    (
        "url_or_email",
        Analyzer::Standard,
        "Tokenizes into sequences of alpha-numeric, numeric, URL, email, southeast asian terms, and into terms of individual ideographic and hiragana characters. English stop words are filtered out.",
    ),
    (
        "whitespace",
        Analyzer::Standard,
        "Breaks text into terms by characters that are considered \"Java whitespace\".",
    ),
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
            .find(|(admitted, _, _)| *admitted == name.as_str())
            .map(|(_, behavior, _)| *behavior)
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
                .filter(|token| !self.stop_words().contains(&token.as_str()))
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
        self.query_default().analyze(text)
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

    /// The stop-word set this analyzer filters: `english`'s Lucene default
    /// set, empty for the analyzers that filter none. The one home for the
    /// variant→set mapping, read by [`Self::analyze`] and by the
    /// `listAvailableAnalyzers` catalogue.
    pub(crate) fn stop_words(self) -> &'static [&'static str] {
        match self {
            Self::English => &ENGLISH_STOP_WORDS,
            Self::Standard | Self::StandardFolding => &[],
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
    let names: Vec<&str> = ANALYZERS.iter().map(|(name, _, _)| *name).collect();
    let (last, rest) = names.split_last().expect("the analyzer table is non-empty");
    format!("{} or {last}", rest.join(", "))
}

/// One row of the `db.index.fulltext.listAvailableAnalyzers` catalogue.
pub(crate) struct AnalyzerCatalogEntry {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    /// The set this engine's analyzer applies — never a per-language Lucene
    /// list the engine does not use.
    pub(crate) stopwords: &'static [&'static str],
}

/// The `db.index.fulltext.listAvailableAnalyzers` catalogue: one row per
/// admitted name in the table's order.
pub(crate) fn analyzer_catalog() -> impl Iterator<Item = AnalyzerCatalogEntry> {
    ANALYZERS
        .iter()
        .map(|(name, behavior, description)| AnalyzerCatalogEntry {
            name,
            description,
            stopwords: behavior.stop_words(),
        })
}
