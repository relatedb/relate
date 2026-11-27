//! The fulltext analyzers: the one name table, the behaviours the engine
//! differentiates, and the index/query pair one scan crosses.

use relate_core::{bare_type_name, Error, Result, Value};

use super::fold::fold;
use super::stemmer::porter_stem;

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
    /// `cjk`: overlapping bigrams over Han, Hiragana and Katakana runs.
    Cjk,
}

/// The one analyzer vocabulary: every fulltext analyzer name live Neo4j
/// admits (live-verified 2026-11-02 on neo4j:2026.09-enterprise) with the
/// behaviour it selects and the description live's
/// `db.index.fulltext.listAvailableAnalyzers` reports for it (live-verified
/// 2026-11-16 on neo4j:2026.10-enterprise) — `english` drops English stop
/// words and Porter-stems, `standard-folding` folds Latin diacritics, `cjk`
/// bigrams its CJK runs, every other admitted name shares the standard
/// tokenizer. The name admission,
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
        Analyzer::Cjk,
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
    /// analyzer-name table. A NULL value is no override — live 2026.10.1
    /// probed a NULL analyzer on a default index, where the hits are the
    /// same either way — so the query follows the index's own analyzer
    /// (ADR-0636). A STRING must name a live-admitted analyzer (live
    /// 22N27), and every other type is live's 22NB1 string type mismatch.
    pub(crate) fn from_value(value: &Value) -> Result<Option<Self>> {
        match value {
            Value::Null => Ok(None),
            Value::String(name) => Self::from_name(name).map(Some).ok_or_else(|| {
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
}

/// The kind of one tokenizer segment — the runs [`tokenize`] and
/// [`cjk_bigrams`] classify a text into.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// A run of ordinary word characters (alphanumerics and `_`).
    Word,
    /// A run of Katakana characters.
    Katakana,
    /// A run of Han ideographs and Hiragana characters — the scripts written
    /// without spaces between words.
    Ideographic,
}

impl Kind {
    /// The run kind `c` continues, or `None` where `c` is a separator.
    fn of(c: char) -> Option<Self> {
        if is_han(c) || is_hiragana(c) {
            Some(Self::Ideographic)
        } else if is_katakana(c) {
            Some(Self::Katakana)
        } else if c.is_alphanumeric() || c == '_' {
            Some(Self::Word)
        } else {
            None
        }
    }
}

/// A Han ideograph: the blocks the reference tokenizer emits one character
/// at a time, since Han text is written without spaces between words.
fn is_han(c: char) -> bool {
    matches!(
        c as u32,
        0x3400..=0x4DBF        // Extension A
        | 0x4E00..=0x9FFF      // Unified Ideographs
        | 0xF900..=0xFAFF      // Compatibility Ideographs
        | 0x20000..=0x2A6DF    // Extension B
        | 0x2A700..=0x2EBEF    // Extensions C–F
        | 0x2EBF0..=0x2EE5F    // Extension I
        | 0x2F800..=0x2FA1F    // Compatibility Ideographs Supplement
        | 0x30000..=0x3134F    // Extension G
        | 0x31350..=0x323AF    // Extension H
    )
}

/// Hiragana: one token per character, like Han.
fn is_hiragana(c: char) -> bool {
    matches!(c as u32, 0x3041..=0x3096 | 0x309D..=0x309F)
}

/// Katakana: a run of these stays one token (UAX #29's Katakana rule). The
/// range includes the prolonged sound mark (U+30FC) and the iteration
/// marks, so `データベース` stays one run.
fn is_katakana(c: char) -> bool {
    matches!(
        c as u32,
        0x30A1..=0x30FA | 0x30FC..=0x30FF | 0x31F0..=0x31FF | 0x32D0..=0x32FE
    )
}

/// A combining kana sound mark (U+3099 voiced, U+309A semi-voiced): it stays
/// with the kana before it, so decomposed `か` + U+3099 reads as `が`.
fn is_kana_mark(c: char) -> bool {
    matches!(c as u32, 0x3099 | 0x309A)
}

/// Whether `c` attaches to the open run instead of ending it: the one
/// decomposed-kana rule, a combining sound mark after an Ideographic or
/// Katakana run. [`clusters`] reads the same [`is_kana_mark`] for the
/// characters inside such a run.
fn attaches_to_run(c: char, open: Option<Kind>) -> bool {
    is_kana_mark(c) && matches!(open, Some(Kind::Ideographic | Kind::Katakana))
}

/// The characters of an Ideographic or Katakana run, each with any trailing
/// combining kana sound marks attached. A run is never empty and never
/// starts with a sound mark (see [`attaches_to_run`]).
fn clusters(run: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (at, c) in run.char_indices().skip(1) {
        if !is_kana_mark(c) {
            out.push(&run[start..at]);
            start = at;
        }
    }
    out.push(&run[start..]);
    out
}

/// Walk `text`, lowercased, as its tokenizer segments: the runs of word
/// characters, Katakana, and Han/Hiragana; every other character separates.
fn for_each_segment(text: &str, mut each: impl FnMut(Kind, &str)) {
    let lower = text.to_lowercase();
    let mut open: Option<Kind> = None;
    let mut start = 0;
    for (at, c) in lower.char_indices() {
        let kind = Kind::of(c);
        if kind == open || (kind.is_none() && attaches_to_run(c, open)) {
            continue;
        }
        if let Some(previous) = open {
            each(previous, &lower[start..at]);
        }
        open = kind;
        start = at;
    }
    if let Some(previous) = open {
        each(previous, &lower[start..]);
    }
}

/// Lowercase and split the way the reference tokenizer does. Latin text
/// splits on any run of non-alphanumeric characters except `_` — live-fitted:
/// the default analyzer keeps stop words and underscores (`foo_bar` is one
/// token; `foo` alone matches nothing) and splits hyphenated words
/// (`wish-granting` is two tokens), which is what the length statistics of
/// the calibrated scores require. The scripts written without spaces between
/// words split as UAX #29 has it: one token per Han ideograph and per
/// Hiragana character, a whole Katakana run as one token — so `中文` is
/// searchable inside `中文搜索引擎`.
pub(super) fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for_each_segment(text, |kind, run| match kind {
        Kind::Word | Kind::Katakana => tokens.push(run.to_string()),
        Kind::Ideographic => tokens.extend(clusters(run).into_iter().map(String::from)),
    });
    tokens
}

/// Whether `text` holds at least one term [`tokenize`] would emit — the
/// lexical question the suffix grammar asks, without building the terms.
pub(super) fn has_token(text: &str) -> bool {
    let mut found = false;
    for_each_segment(text, |_, _| found = true);
    found
}

/// The `cjk` analyzer's terms: the overlapping character bigrams of every run
/// of Han, Hiragana or Katakana characters, and [`tokenize`]'s terms
/// elsewhere. A one-character run contributes no bigram — the reference
/// analyzer's output is the same, so a lone ideograph is not searchable
/// under `cjk`.
fn cjk_bigrams(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for_each_segment(text, |kind, run| match kind {
        Kind::Word => tokens.push(run.to_string()),
        Kind::Katakana | Kind::Ideographic => {
            tokens.extend(clusters(run).windows(2).map(|pair| pair.concat()));
        }
    });
    tokens
}

impl Analyzer {
    /// The analyzer's tokens: `standard` is the tokenizer alone (the
    /// default `standard-no-stop-words` index analyzer), `english` drops
    /// English stop words and Porter-stems what remains, `standard-folding`
    /// folds, `cjk` bigrams the CJK runs. Both sides of one scan run this
    /// one analysis (ADR-0636): the index's stored tokens at corpus build,
    /// and the query's terms when no `WITH ANALYZER` overrides — the
    /// stored/query distinction lives in the [`Analyzers`] pair alone.
    pub(super) fn analyze(self, text: &str) -> Vec<String> {
        match self {
            Self::Cjk => cjk_bigrams(text),
            Self::Standard => tokenize(text),
            Self::StandardFolding => tokenize(text).iter().map(|token| fold(token)).collect(),
            Self::English => tokenize(text)
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

    /// The stop-word set this analyzer filters: `english`'s Lucene default
    /// set, empty for the analyzers that filter none. The one home for the
    /// variant→set mapping, read by [`Self::analyze`] and by the
    /// `listAvailableAnalyzers` catalogue.
    pub(crate) fn stop_words(self) -> &'static [&'static str] {
        match self {
            Self::English => &ENGLISH_STOP_WORDS,
            Self::Standard | Self::StandardFolding | Self::Cjk => &[],
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
    /// is analyzed by the index's own analyzer, exactly as its stored
    /// tokens were (ADR-0636).
    pub(crate) fn new(index: Analyzer, query_override: Option<Analyzer>) -> Self {
        Self {
            index,
            query: query_override.unwrap_or(index),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_splits_hyphens_and_lowercases() {
        assert_eq!(
            tokenize("A wish-granting GENIE."),
            ["a", "wish", "granting", "genie"]
        );
        // Live 2026.10.1: `foo_bar` is one token, and `foo` alone does not
        // match a document holding it.
        assert_eq!(tokenize("foo_bar baz"), ["foo_bar", "baz"]);
    }

    #[test]
    fn decomposed_kana_tokenizes_like_composed() {
        // か + U+3099 is が; the mark is not a separator and not a term.
        let decomposed = "か\u{3099}き\u{309A}";
        assert_eq!(tokenize(decomposed), ["か\u{3099}", "き\u{309A}"]);
        assert_eq!(tokenize("ハ\u{309A}ン"), ["ハ\u{309A}ン"]);
        assert_eq!(
            cjk_bigrams("あか\u{3099}い"),
            ["あか\u{3099}", "か\u{3099}い"]
        );
        // The decomposed form emits the same number of terms as the composed
        // one, each the composed term's base kana plus its sound mark.
        let composed = tokenize("がぎ");
        let bases: Vec<String> = tokenize(decomposed)
            .iter()
            .map(|term| term.chars().take(1).collect())
            .collect();
        assert_eq!(bases, ["か", "き"]);
        assert_eq!(composed, ["が", "ぎ"]);
        // A mark after a non-kana character is still a separator.
        assert_eq!(tokenize("a\u{3099}b"), ["a", "b"]);
    }

    #[test]
    fn tokenizer_splits_the_scripts_written_without_spaces() {
        // One token per Han ideograph and per Hiragana character, a whole
        // Katakana run as one token (live's UAX #29 shape) — the prolonged
        // sound mark included, so `データベース` is one run.
        assert_eq!(
            tokenize("中文搜索引擎"),
            ["中", "文", "搜", "索", "引", "擎"]
        );
        assert_eq!(
            tokenize("日本語のテキスト"),
            ["日", "本", "語", "の", "テキスト"]
        );
        assert_eq!(tokenize("graph データベース"), ["graph", "データベース"]);
        // The prolonged sound mark (U+30FC) belongs to the run, not a
        // separator: `データベース` is one token, never `デ` + `タベ` + `ス`.
        assert_eq!(tokenize("データベース"), ["データベース"]);
        assert_eq!(tokenize("テ\u{30FC}"), ["テ\u{30FC}"]);
        // Space-separated Korean stays one token per word.
        assert_eq!(tokenize("한국어 검색"), ["한국어", "검색"]);
        // The cjk analyzer bigrams each run and yields nothing for a
        // one-character run.
        assert_eq!(
            cjk_bigrams("中文搜索引擎"),
            ["中文", "文搜", "搜索", "索引", "引擎"]
        );
        assert_eq!(
            cjk_bigrams("graph データベース"),
            ["graph", "デー", "ータ", "タベ", "ベー", "ース"]
        );
        assert_eq!(cjk_bigrams("图"), Vec::<String>::new());
        assert_eq!(cjk_bigrams("한국어"), ["한국어"]);
    }

    #[test]
    fn a_folding_analyzer_folds_diacritics_and_ligatures() {
        assert_eq!(
            Analyzer::StandardFolding.analyze("Eifr\u{e9}m Stra\u{df}e"),
            ["eifrem", "strasse"]
        );
    }

    /// ADR-0636: without `WITH ANALYZER` the query runs the index's own
    /// analyzer; an override replaces the query side alone.
    #[test]
    fn the_query_without_an_override_follows_the_index_analyzer() {
        assert_eq!(
            Analyzers::new(Analyzer::StandardFolding, None).query,
            Analyzer::StandardFolding
        );
        assert_eq!(
            Analyzers::new(Analyzer::English, None).query,
            Analyzer::English
        );
        let overridden = Analyzers::new(Analyzer::StandardFolding, Some(Analyzer::English));
        assert_eq!(
            (overridden.index, overridden.query),
            (Analyzer::StandardFolding, Analyzer::English)
        );
    }

    #[test]
    fn analyzers_differ_on_stop_words_and_stemming() {
        assert_eq!(
            Analyzer::Standard.analyze("The princesses"),
            ["the", "princesses"]
        );
        assert_eq!(Analyzer::English.analyze("The princesses"), ["princess"]);
    }

    #[test]
    fn the_index_analyzer_reads_its_stored_config() {
        use std::collections::BTreeMap;
        let config = |analyzer: Value| {
            BTreeMap::from([(
                "indexConfig".to_string(),
                Value::Map(std::sync::Arc::new(BTreeMap::from([(
                    crate::exec::ddl_options::KEY_FULLTEXT_ANALYZER.to_string(),
                    analyzer,
                )]))),
            )])
        };
        assert_eq!(
            crate::exec::ddl_options::index_analyzer(&BTreeMap::new(), "i").unwrap(),
            Analyzer::Standard
        );
        assert_eq!(
            crate::exec::ddl_options::index_analyzer(
                &config(Value::String("standard-folding".into())),
                "i"
            )
            .unwrap(),
            Analyzer::StandardFolding
        );
        assert!(crate::exec::ddl_options::index_analyzer(
            &config(Value::String("nope".into())),
            "i"
        )
        .is_err());
        assert!(crate::exec::ddl_options::index_analyzer(&config(Value::Int(1)), "i").is_err());
    }

    #[test]
    fn analyzer_values_are_validated_like_live() {
        // Live 2026.10.1: a NULL analyzer is no override — probed on a
        // default index, where the hits are the same either way; the query
        // follows the index's analyzer (ADR-0636). An unknown name is
        // 22N27, a non-STRING value is 22NB1.
        assert_eq!(Analyzer::from_value(&Value::Null).unwrap(), None);
        assert_eq!(
            Analyzer::from_value(&Value::String("ENGLISH".into())).unwrap(),
            Some(Analyzer::English)
        );
        assert_eq!(
            Analyzer::from_value(&Value::String("not-a-real-analyzer".into())).unwrap_err(),
            Error::argument_error(format!(
                "Invalid input 'not-a-real-analyzer' for analyzer name. Expected to be {}.",
                analyzer_names()
            ))
        );
        assert_eq!(
            Analyzer::from_value(&Value::Int(42)).unwrap_err(),
            Error::ty("Type mismatch: expected to be STRING but was 'INTEGER'.")
        );
    }
}
