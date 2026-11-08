//! Token compilation for instant patterns: the Java pattern alphabet, the
//! compiled [`Token`] tree, and its nesting and duplicate-field bounds.

use std::collections::BTreeSet;

use crate::Result;

use super::{note_field, pattern_error};

// ---------------------------------------------------------------------------
// Instant patterns
// ---------------------------------------------------------------------------

/// One field the Java pattern alphabet names. Aliases share a kind
/// (`y`/`u` are both [`Self::Year`]), so duplicate detection and resolution
/// read one vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum FieldKind {
    Year,
    WeekYear,
    Month,
    Day,
    DayOfYear,
    Week,
    WeekOfMonth,
    Quarter,
    DayOfWeekNum,
    DayOfWeekName,
    AlignedDayOfWeek,
    ModifiedJulianDay,
    HourOfDay,
    ClockHour,
    HourOfAmPm,
    Minute,
    Second,
    Fraction,
    NanoOfSecond,
    MilliOfDay,
    NanoOfDay,
    AmPm,
    PeriodOfDay,
    Era,
    ZoneId,
    ZoneName,
    OffsetFractional,
    OffsetZ,
    OffsetLocalized,
}

impl FieldKind {
    /// The Java `DateTimeFormatter` pattern letters Neo4j accepts. A
    /// letter outside this table is an invalid pattern character.
    fn from_letter(letter: char) -> Option<Self> {
        Some(match letter {
            'G' => Self::Era,
            'y' | 'u' => Self::Year,
            'Y' => Self::WeekYear,
            'M' | 'L' => Self::Month,
            'd' => Self::Day,
            'D' => Self::DayOfYear,
            'w' => Self::Week,
            'W' => Self::WeekOfMonth,
            'Q' | 'q' => Self::Quarter,
            'e' | 'c' => Self::DayOfWeekNum,
            'E' => Self::DayOfWeekName,
            'F' => Self::AlignedDayOfWeek,
            'g' => Self::ModifiedJulianDay,
            'H' => Self::HourOfDay,
            'k' => Self::ClockHour,
            'h' | 'K' => Self::HourOfAmPm,
            'm' => Self::Minute,
            's' => Self::Second,
            'S' => Self::Fraction,
            'A' => Self::MilliOfDay,
            'n' => Self::NanoOfSecond,
            'N' => Self::NanoOfDay,
            'a' => Self::AmPm,
            'B' => Self::PeriodOfDay,
            'V' | 'v' => Self::ZoneId,
            'z' => Self::ZoneName,
            'X' | 'x' => Self::OffsetFractional,
            'Z' => Self::OffsetZ,
            'O' => Self::OffsetLocalized,
            _ => return None,
        })
    }
}

/// One compiled instant-pattern token.
#[derive(Debug, Clone)]
pub(super) enum Token {
    Field {
        kind: FieldKind,
        /// The spelling that produced the field (`y` vs `u`), which the
        /// era rule needs: only year-of-era (`y`) shifts for `BC`.
        letter: char,
        count: usize,
        /// The `p` pad modifier's width: the token consumes exactly this
        /// many characters, leading spaces included.
        pad: usize,
    },
    Literal(String),
    /// A `[...]` section: matched when it matches, otherwise skipped.
    Optional(Vec<Token>),
}

/// Maximum `[` section nesting accepted in a pattern, matching the query
/// parser's `MAX_NESTING_DEPTH` of 500 (that crate is not linkable from
/// here, so the number is restated). The compiled token tree is walked
/// recursively by [`reject_duplicate_fields`] and [`match_tokens`];
/// [`tokenize_section`] is the only constructor of [`Token::Optional`], so
/// refusing the pattern here bounds all three walks by construction. A
/// deeper pattern is a typed error instead of a stack overflow on the
/// connection thread.
const MAX_PATTERN_NESTING: usize = 500;

pub(super) fn tokenize(pattern: &str) -> Result<Vec<Token>> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut index = 0;
    let tokens = tokenize_section(&chars, &mut index, 0)?;
    reject_duplicate_fields(&tokens)?;
    Ok(tokens)
}

/// `depth` is the number of enclosing `[` sections; zero at the top level.
fn tokenize_section(chars: &[char], index: &mut usize, depth: usize) -> Result<Vec<Token>> {
    let in_optional = depth > 0;
    let mut tokens = Vec::new();
    let mut literal = String::new();
    let mut pad = 0usize;
    let flush = |literal: &mut String, tokens: &mut Vec<Token>| {
        if !literal.is_empty() {
            tokens.push(Token::Literal(std::mem::take(literal)));
        }
    };
    while *index < chars.len() {
        let c = chars[*index];
        match c {
            '\'' => {
                *index += 1;
                loop {
                    let Some(&q) = chars.get(*index) else {
                        return Err(pattern_error("unterminated quote"));
                    };
                    if q == '\'' {
                        if chars.get(*index + 1) == Some(&'\'') {
                            literal.push('\'');
                            *index += 2;
                            continue;
                        }
                        *index += 1;
                        break;
                    }
                    literal.push(q);
                    *index += 1;
                }
            }
            ']' if in_optional => {
                flush(&mut literal, &mut tokens);
                *index += 1;
                return Ok(tokens);
            }
            '[' => {
                if pad != 0 {
                    return Err(pattern_error("pad modifier has no field"));
                }
                if depth >= MAX_PATTERN_NESTING {
                    return Err(pattern_error("pattern nesting too deep"));
                }
                flush(&mut literal, &mut tokens);
                *index += 1;
                let inner = tokenize_section(chars, index, depth + 1)?;
                tokens.push(Token::Optional(inner));
            }
            ']' => return Err(pattern_error("unmatched ']'")),
            c if c.is_ascii_alphabetic() => {
                let start = *index;
                while chars.get(*index) == Some(&c) {
                    *index += 1;
                }
                let count = *index - start;
                if c == 'p' {
                    pad = count;
                    continue;
                }
                let kind = FieldKind::from_letter(c)
                    .ok_or_else(|| pattern_error(format!("unknown pattern letter '{c}'")))?;
                flush(&mut literal, &mut tokens);
                tokens.push(Token::Field {
                    kind,
                    letter: c,
                    count,
                    pad: std::mem::take(&mut pad),
                });
            }
            _ => {
                literal.push(c);
                *index += 1;
            }
        }
    }
    if in_optional {
        return Err(pattern_error("unterminated '['"));
    }
    if pad != 0 {
        return Err(pattern_error("pad modifier has no field"));
    }
    flush(&mut literal, &mut tokens);
    Ok(tokens)
}

fn reject_duplicate_fields(tokens: &[Token]) -> Result<()> {
    let mut seen = BTreeSet::new();
    fn walk(tokens: &[Token], seen: &mut BTreeSet<FieldKind>) -> Result<()> {
        for token in tokens {
            match token {
                Token::Field { kind, .. } => note_field(seen, *kind)?,
                Token::Optional(inner) => walk(inner, seen)?,
                Token::Literal(_) => {}
            }
        }
        Ok(())
    }
    walk(tokens, &mut seen)
}
