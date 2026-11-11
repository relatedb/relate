//! String function helpers (character ranges, trim family, arg extraction,
//! normalization check).

use std::collections::HashSet;
use std::ops::Range;

use relate_core::{Error, ErrorKind, Result, Value};
use unicode_normalization::UnicodeNormalization;

pub(crate) fn string_arg(vals: &[Value], i: usize) -> Result<Option<&str>> {
    match vals.get(i) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        _ => Err(Error::ty("expected string argument")),
    }
}

/// The characters `r` of `s` (character indices, not bytes) as a slice of
/// `s`, found in one walk without copying. Both bounds clamp to the end of
/// the string, and an end before the start selects nothing.
pub(crate) fn char_range(s: &str, r: Range<usize>) -> &str {
    let byte = |s: &str, c: usize| s.char_indices().nth(c).map_or(s.len(), |(b, _)| b);
    let rest = &s[byte(s, r.start)..];
    &rest[..byte(rest, r.end.saturating_sub(r.start))]
}

/// The conservative byte-expansion factor for Unicode normalization
/// (`NFC`/`NFD`/`NFKC`/`NFKD`) and case mapping (`toLower`/`toUpper`):
/// compatibility decomposition and SpecialCasing can expand each input byte
/// into several output bytes (combining marks, ligature and case expansions),
/// and Unicode's data puts the worst observed expansion at about 12 output
/// bytes per input byte. Charging `len × 12` before the allocation is the
/// sound upper bound the ADR-0463 seam needs; the actual allocation is then
/// checked against the cap rather than the allocator.
pub(crate) const UNICODE_EXPANSION_BYTES_PER_INPUT_BYTE: u64 = 12;

/// Charge the conservative upper bound of a Unicode normalize/case-map
/// allocation before it is built.
pub(crate) fn charge_unicode_expansion(
    graph: &crate::overlay::ExecGraph<'_>,
    input: &str,
) -> Result<()> {
    graph.charge_expression(
        (input.len() as u64).saturating_mul(UNICODE_EXPANSION_BYTES_PER_INPUT_BYTE),
    )
}

/// [`str_unary`] for the Unicode-expanding transforms: charge the
/// conservative expansion bound from the arm that extracts the string, then
/// build once — no second probe of `vals.first()`.
pub(crate) fn str_unary_charged(
    graph: &crate::overlay::ExecGraph<'_>,
    vals: &[Value],
    f: impl Fn(&str) -> String,
) -> Result<Value> {
    Ok(match vals.first() {
        Some(Value::String(s)) => {
            charge_unicode_expansion(graph, s)?;
            Value::String(f(s).into())
        }
        Some(Value::Null) | None => Value::Null,
        _ => return Err(Error::ty("expected string")),
    })
}

#[derive(Clone, Copy)]
pub(crate) enum TrimMode {
    Both,
    Leading,
    Trailing,
}

pub(crate) fn java_whitespace(c: char) -> bool {
    match c {
        '\u{001C}'..='\u{001F}' => true,
        '\u{00A0}' | '\u{2007}' | '\u{202F}' | '\u{0085}' => false,
        _ => c.is_whitespace(),
    }
}

/// What a trim call matches: the Unicode whitespace set, or the explicit
/// characters the optional second argument names. An empty set contains no
/// character, so the empty-string trim returns its input unchanged without
/// a branch.
enum TrimChars {
    Whitespace,
    Set(HashSet<char>),
}

impl TrimChars {
    fn contains(&self, c: char) -> bool {
        match self {
            Self::Whitespace => java_whitespace(c),
            Self::Set(set) => set.contains(&c),
        }
    }
}

pub(crate) fn trim_function(vals: &[Value], mode: TrimMode, strict_single: bool) -> Result<Value> {
    let Some(input) = vals.first() else {
        return Err(Error::semantic("trim function requires an argument"));
    };
    if input.is_null() || vals.get(1).is_some_and(Value::is_null) {
        return Ok(Value::Null);
    }
    let Value::String(input) = input else {
        return Err(Error::invalid_argument_type("trim input must be a string"));
    };
    let chars = match vals.get(1) {
        None => TrimChars::Whitespace,
        Some(Value::String(chars)) => {
            if strict_single && chars.chars().count() != 1 {
                // Live Neo4j 2026.10.1: the ArgumentError class (ADR-0565).
                return Err(Error::argument_value_kind(
                    ErrorKind::InvalidArgumentValue,
                    "trim character string must contain one character",
                ));
            }
            TrimChars::Set(chars.chars().collect())
        }
        Some(_) => {
            return Err(Error::invalid_argument_type(
                "trim character string must be a string",
            ))
        }
    };
    let out = match mode {
        TrimMode::Both => input.trim_matches(|c| chars.contains(c)),
        TrimMode::Leading => input.trim_start_matches(|c| chars.contains(c)),
        TrimMode::Trailing => input.trim_end_matches(|c| chars.contains(c)),
    };
    Ok(Value::String(out.into()))
}

/// The comma spelling `trim(trimSpecification [, trimCharacterString],
/// input)` (the parser lowers it to `trim_spec`): the specification is an
/// ordinary expression, so the direction is chosen here at run time. Every
/// null argument yields null; a specification that is not the STRING
/// `'leading'`/`'trailing'`/`'both'` (case-insensitive) is a type error. The
/// trim itself reuses [`trim_function`], the direction-named entries' one
/// implementation, including their strict single-character rule.
pub(crate) fn trim_spec_function(vals: &[Value]) -> Result<Value> {
    let Some((spec, rest)) = vals.split_first() else {
        return Err(Error::semantic("trim_spec function requires arguments"));
    };
    if spec.is_null() || rest.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let Value::String(spec) = spec else {
        return Err(Error::invalid_argument_type(
            "trim specification must be a string",
        ));
    };
    let mode = match spec.to_ascii_uppercase().as_str() {
        "LEADING" => TrimMode::Leading,
        "TRAILING" => TrimMode::Trailing,
        "BOTH" => TrimMode::Both,
        _ => {
            return Err(Error::ty(format!(
                "InvalidArgumentValue: trim specification must be 'leading', 'trailing' \
                 or 'both', got '{spec}'"
            )))
        }
    };
    // The parser's 3-argument spelling is (specification, characters, input);
    // `trim_function` takes (input, characters).
    let args = match rest {
        [input] => vec![input.clone()],
        [chars, input] => vec![input.clone(), chars.clone()],
        _ => {
            return Err(Error::semantic(
                "trim_spec function requires two or three arguments",
            ))
        }
    };
    trim_function(&args, mode, true)
}

pub(crate) fn is_normalized(s: &str, form: &str) -> bool {
    let normalized: String = match form.to_ascii_uppercase().as_str() {
        "NFD" => s.nfd().collect(),
        "NFKC" => s.nfkc().collect(),
        "NFKD" => s.nfkd().collect(),
        _ => s.nfc().collect(),
    };
    normalized == s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_range_slices_by_character_and_clamps_at_the_end() {
        // Five characters of 1, 2, 4, 1 and 2 bytes.
        let s = "añ😀bç";
        assert_eq!(char_range(s, 0..5), s);
        assert_eq!(char_range(s, 1..3), "ñ😀");
        assert_eq!(char_range(s, 2..4), "😀b");
        assert_eq!(char_range(s, 4..5), "ç");
        assert_eq!(char_range(s, 3..usize::MAX), "bç");
        assert_eq!(char_range(s, 0..99), s);
        // Empty: zero width, start at or past the end, or end before start.
        assert_eq!(char_range(s, 0..0), "");
        assert_eq!(char_range(s, 5..9), "");
        assert_eq!(char_range(s, usize::MAX..usize::MAX), "");
        assert_eq!(char_range(s, Range { start: 3, end: 1 }), "");
        assert_eq!(char_range("", 0..1), "");
    }

    #[test]
    fn trim_spec_selects_the_direction_and_propagates_null() {
        let s = |v: &str| Value::String(v.into());
        let trim = |vals: Vec<Value>| trim_spec_function(&vals).unwrap();

        assert_eq!(
            trim(vec![s("LEADING"), s("x"), s("xxxhelloxxx")]),
            s("helloxxx")
        );
        assert_eq!(
            trim(vec![s("trailing"), s("x"), s("xxxhelloxxx")]),
            s("xxxhello")
        );
        assert_eq!(trim(vec![s("Both"), s("x"), s("xxxhelloxxx")]), s("hello"));
        // No trim character string: the whitespace default applies.
        assert_eq!(trim(vec![s("leading"), s("  hi  ")]), s("hi  "));
        // Null anywhere is null.
        assert_eq!(trim(vec![Value::Null, s("x"), s("hello")]), Value::Null);
        assert_eq!(
            trim(vec![s("leading"), Value::Null, s("hello")]),
            Value::Null
        );
        // An unknown specification is a type error, not a default.
        assert!(trim_spec_function(&[s("middle"), s("hello")]).is_err());
    }
}
