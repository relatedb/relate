//! ASCII folding for the `standard-folding` analyzer: the Latin-script
//! diacritic and ligature folding of Lucene's `ASCIIFoldingFilter`, which is
//! what Neo4j's `standard-folding` applies at index and query time.
//!
//! Calibrated against Neo4j 2026.09-enterprise: Latin letters lose their
//! diacritics (`é` -> `e`), ligatures and letter variants expand (`ß` ->
//! `ss`, `æ` -> `ae`, `œ` -> `oe`, `ø` -> `o`, `ł` -> `l`, `þ` -> `th`,
//! `ﬁ` -> `fi`), and full-width forms become ASCII. Greek and Cyrillic are
//! left alone (`ά` stays distinct from `α`).

use unicode_normalization::UnicodeNormalization;

/// Letters that carry no decomposition but fold to ASCII, lowercase forms
/// only (the tokenizer lowercases before folding).
const SPECIAL: [(char, &str); 12] = [
    ('ß', "ss"),
    ('æ', "ae"),
    ('œ', "oe"),
    ('ø', "o"),
    ('ł', "l"),
    ('đ', "d"),
    ('ð', "d"),
    ('þ', "th"),
    ('ı', "i"),
    ('ŋ', "n"),
    ('ħ', "h"),
    ('ŧ', "t"),
];

/// Fold one already-lowercased token.
pub(super) fn fold(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    for c in token.chars() {
        if c.is_ascii() {
            out.push(c);
        } else if unicode_normalization::char::is_combining_mark(c) {
            // A standalone mark (decomposed input, or the dot Rust's
            // lowercasing leaves after `İ`) is dropped: a deliberate
            // superset of Lucene, which would keep it and miss `e` + acute.
        } else if let Some((_, ascii)) = SPECIAL.iter().find(|(from, _)| *from == c) {
            out.push_str(ascii);
        } else {
            // Compatibility decomposition handles diacritics, ligatures and
            // full-width forms; the result counts only when it is plain
            // ASCII once combining marks are dropped, so non-Latin scripts
            // pass through unchanged.
            let base: String = std::iter::once(c)
                .nfkd()
                .filter(|part| !unicode_normalization::char::is_combining_mark(*part))
                .collect();
            if base.is_ascii() && !base.is_empty() {
                out.push_str(&base);
            } else {
                out.push(c);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::fold;

    #[test]
    fn latin_diacritics_ligatures_and_variants_fold() {
        for (from, to) in [
            ("eifrém", "eifrem"),
            ("müller", "muller"),
            ("zoë", "zoe"),
            ("naïve", "naive"),
            ("æther", "aether"),
            ("straße", "strasse"),
            ("œuvre", "oeuvre"),
            ("łódź", "lodz"),
            ("ångström", "angstrom"),
            ("ﬁsh", "fish"),
            ("ａｂｃ", "abc"),
            ("ǆ", "dz"),
            ("ĳssel", "ijssel"),
            ("ø", "o"),
            ("þ", "th"),
            ("ı", "i"),
            ("plain_ascii", "plain_ascii"),
        ] {
            assert_eq!(fold(from), to, "{from}");
        }
        // Lowercasing `İ` leaves `i` plus a combining dot; decomposed
        // input (`e` + acute accent) folds like its precomposed form.
        assert_eq!(fold(&"İ".to_lowercase()), "i");
        assert_eq!(fold("e\u{301}"), "e");
    }

    #[test]
    fn non_latin_scripts_are_untouched() {
        for token in ["άλφα", "ελλάς", "йод", "日本語"] {
            assert_eq!(fold(token), token);
        }
    }
}
