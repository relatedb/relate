//! CIP-223 duration patterns: the `duration(value, pattern)` grammar.
//!
//! Only the duration alphabet is a field token; every other character
//! (letters included, as in Neo4j's `DurationValue.parsePattern`) is
//! literal text. A field may not occur twice, and the component arithmetic
//! runs in the stored `i64` widths with the shared duration range contract.

use std::collections::BTreeSet;

use crate::Result;

use super::duration_tokens::{self as tokens, Token, UnclosedEscape};
use super::exact::DurationTiers;
use super::field::PatternSymbol;
use super::pattern::{
    invalid_value, mismatch, nanos_from_fraction, note_field, read_literal, read_number,
};
use super::{DurationField, Temporal};

/// The duration pattern's token stream. Only the duration alphabet is a
/// field token; every other character (including letters) is literal text,
/// as in Neo4j's `DurationValue.parsePattern`. Run widths do not matter when
/// parsing.
fn tokenize_duration(pattern: &str) -> Result<Vec<Token>> {
    // An unpaired `'` is a malformed pattern, not a value mismatch
    // (Neo4j reports `pattern parsing failed`).
    let tokens = tokens::tokenize(pattern)
        .map_err(|UnclosedEscape| invalid_value("pattern parsing failed"))?;
    reject_duplicate_duration_fields(&tokens)?;
    Ok(tokens)
}

/// One pattern token's duplicate key. The fraction token (`n`/`S`) is its
/// own slot, not the nanosecond field: Neo4j's `FRACTION_TOKEN` and
/// `NANOSECONDS_TOKEN` (`N`) are distinct, so `n ... N` is a legal pattern
/// whose two contributions both land in the nanosecond tier.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum DuplicateSlot {
    Field(DurationField),
    Fraction,
}

fn reject_duplicate_duration_fields(tokens: &[Token]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for token in tokens {
        let slot = match token {
            Token::Symbol(PatternSymbol::Field(field), _) => DuplicateSlot::Field(*field),
            Token::Symbol(PatternSymbol::FractionDigits, _) => DuplicateSlot::Fraction,
            Token::Literal(_) => continue,
        };
        note_field(&mut seen, slot)?;
    }
    Ok(())
}

/// `duration(value, pattern)`: Neo4j's CIP-223 pattern grammar, with the
/// component arithmetic in the stored `i64` widths and the one shared
/// duration range contract.
pub(super) fn parse_duration_pattern(value: &str, pattern: &str) -> Result<Temporal> {
    let tokens = tokenize_duration(pattern)?;
    let mut tiers = DurationTiers::default();
    let mut pos = 0usize;
    for token in &tokens {
        match token {
            Token::Literal(text) => {
                if read_literal(value, &mut pos, text).is_none() {
                    return Err(mismatch());
                }
            }
            Token::Symbol(PatternSymbol::Field(field), _) => {
                let number = match read_number(value, &mut pos, None, true)? {
                    Some(number) => number,
                    None => return Err(mismatch()),
                };
                // Neo4j's pattern parser scales each component in a `long`:
                // the second-denominated time fields into a seconds count,
                // the sub-second fields into a nanosecond count. Keep that
                // width check; the calendar fields' final widths are then
                // checked by `finish` and the stored-range contract.
                let slot = field.slot(&mut tiers);
                slot.accumulator
                    .add_whole_in_lane(number.value, slot.scale, slot.lane_unit)?;
            }
            Token::Symbol(PatternSymbol::FractionDigits, _) => {
                let number = match read_number(value, &mut pos, None, false)? {
                    Some(number) => number,
                    None => return Err(mismatch()),
                };
                // Fraction digits name a fraction of a second: right-pad to
                // nine digits, never truncate (`0080090` → 8_009_000 ns).
                let nanos = nanos_from_fraction(number.value, number.digits)?;
                tiers.nanos.add_whole(i128::from(nanos), 1)?;
            }
        }
    }
    if pos != value.len() {
        return Err(mismatch());
    }
    let (months, days, nanos) = tiers.finish()?;
    // One assembly seam for every duration producer (main's shape, #612);
    // the pattern is exact text, so the strict stored-model bound applies.
    Temporal::duration_checked_exact(months, days, nanos)
}
