//! The `vector()` coordinate-string text layer: the bracketed coordinate
//! list's tokenizer and classifier, its numeric parser, and the CIP-216
//! dimension domain. Split from `value.rs` so the value model keeps the
//! `VectorCoordKind` type and its narrowing, and this text handling lives
//! beside its only consumers.

use crate::value::VectorCoordKind;

impl VectorCoordKind {
    /// The bracketed coordinate-list spelling the `vector()` string
    /// constructor accepts (`"[1, 2, 3]"`), token by token: `None` when the
    /// text lacks its brackets. The one tokenizer the executor's construction
    /// and the front end's compile-time check share; each layers its own
    /// policy on the classification (the executor rejects every non-number,
    /// the front end only [`VectorCoordToken::Malformed`], leaving
    /// [`VectorCoordToken::NonNumeric`] to the runtime type error).
    pub fn coordinate_tokens(text: &str) -> Option<Vec<VectorCoordToken>> {
        let body = text.trim().strip_prefix('[')?.strip_suffix(']')?;
        if body.trim().is_empty() {
            return Some(Vec::new());
        }
        Some(body.split(',').map(VectorCoordToken::classify).collect())
    }

    /// Parse the bracketed coordinate-list spelling (`"[1, 2, 3]"`) to its
    /// numbers: `None` when the text lacks its brackets or any token is not
    /// a number.
    pub fn parse_coordinates(text: &str) -> Option<Vec<f64>> {
        Self::coordinate_tokens(text)?
            .into_iter()
            .map(|token| match token {
                VectorCoordToken::Number(value) => Some(value),
                VectorCoordToken::NonNumeric | VectorCoordToken::Malformed => None,
            })
            .collect()
    }
}

/// One token of a bracketed `vector()` coordinate string, as the shared
/// tokenizer reads it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VectorCoordToken {
    /// A number (`1`, `-2.5`, `1_000`).
    Number(f64),
    /// A valid Cypher value spelling that is not a number — an identifier
    /// word, a quoted string, a boolean/null keyword. A runtime type
    /// mismatch, not an input error.
    NonNumeric,
    /// Neither a number nor a valid value spelling (`6 005`) — a
    /// compile-time input error.
    Malformed,
}

impl VectorCoordToken {
    fn classify(token: &str) -> Self {
        let token = token.trim();
        if let Ok(value) = token.replace('_', "").parse::<f64>() {
            return VectorCoordToken::Number(value);
        }
        let quoted = token.starts_with('\'') || token.starts_with('"');
        let word = token
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if quoted || word {
            VectorCoordToken::NonNumeric
        } else {
            VectorCoordToken::Malformed
        }
    }
}

/// The CIP-216 vector dimension domain — the one bound the parser's type
/// annotation and the front end's `vector()` construction check consult.
pub const VECTOR_DIMENSION_RANGE: std::ops::RangeInclusive<i64> = 1..=4096;

/// The authored rendering of [`VECTOR_DIMENSION_RANGE`], derived from its
/// endpoints so moving the bound moves the message with it.
pub fn dimension_range_text() -> String {
    format!(
        "{} to {}",
        VECTOR_DIMENSION_RANGE.start(),
        VECTOR_DIMENSION_RANGE.end()
    )
}
