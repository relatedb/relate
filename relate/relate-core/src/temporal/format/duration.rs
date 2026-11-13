//! The CIP-223 duration-pattern engine: tokenization (with Neo4j's escape
//! state machine), the per-tier unit cascade, and rendering.

use std::collections::BTreeSet;

use crate::{Error, Result};

use super::super::checked_lane;
use super::super::duration_tokens::{self as tokens, Token, UnclosedEscape};
use super::super::field::{DurationField, DurationTier, PatternSymbol, NANOS_PER_SECOND};
use super::fraction_width;

/// The pattern's token stream from the shared tokenizer, with each fraction
/// run width-checked here so the renderer never re-validates its slice.
fn tokenize(pattern: &str) -> Result<Vec<Token>> {
    let tokens = tokens::tokenize(pattern).map_err(|UnclosedEscape| {
        Error::syntax("pattern parsing failed: make sure that an even number of escapes are used")
    })?;
    let tokens = tokens::merge_symbol_runs(tokens);
    for token in &tokens {
        if let Token::Symbol(PatternSymbol::FractionDigits, width) = token {
            fraction_width(*width)?;
        }
    }
    Ok(tokens)
}

/// The rendered value of every duration unit, named densely so a field the
/// pattern does not name reads as zero by construction.
#[derive(Default)]
struct Units {
    years: i64,
    quarters: i64,
    months: i64,
    weeks: i64,
    days: i64,
    hours: i64,
    minutes: i64,
    seconds: i64,
    millis: i64,
    nanos: i64,
    /// Nanosecond-of-second, rendered by the fraction spellings (`n`/`S`).
    fraction: i64,
}

impl Units {
    fn set(&mut self, field: DurationField, value: i64) {
        match field {
            DurationField::Years => self.years = value,
            DurationField::Quarters => self.quarters = value,
            DurationField::Months => self.months = value,
            DurationField::Weeks => self.weeks = value,
            DurationField::Days => self.days = value,
            DurationField::Hours => self.hours = value,
            DurationField::Minutes => self.minutes = value,
            DurationField::Seconds => self.seconds = value,
            DurationField::Millis => self.millis = value,
            // No pattern spelling names microseconds, so the cascade never
            // sets it.
            DurationField::Micros => {}
            DurationField::Nanos => self.nanos = value,
        }
    }

    fn get(&self, field: DurationField) -> i64 {
        match field {
            DurationField::Years => self.years,
            DurationField::Quarters => self.quarters,
            DurationField::Months => self.months,
            DurationField::Weeks => self.weeks,
            DurationField::Days => self.days,
            DurationField::Hours => self.hours,
            DurationField::Minutes => self.minutes,
            DurationField::Seconds => self.seconds,
            DurationField::Millis => self.millis,
            // The alphabet has no microsecond spelling; nothing can request it.
            DurationField::Micros => 0,
            DurationField::Nanos => self.nanos,
        }
    }
}

/// One tier's cascade (largest unit first): every present level takes its
/// whole share of the running remainder, and the level's scale comes from the
/// one field table. A field the pattern does not name takes nothing, so the
/// next present field sees the whole remainder.
fn cascade(
    units: &mut Units,
    present: &BTreeSet<DurationField>,
    tier: DurationTier,
    total: i128,
) -> Result<()> {
    let mut remainder = total;
    for field in DurationField::CASCADE
        .into_iter()
        .filter(|field| field.tier_and_scale().0 == tier)
    {
        if !present.contains(&field) {
            continue;
        }
        let (_, scale) = field.tier_and_scale();
        units.set(field, checked_lane(remainder / scale, field.name())?);
        remainder %= scale;
    }
    Ok(())
}

/// Every duration unit's value, calculated once per pattern the way Neo4j
/// does: each tier cascades into the units the pattern names, and the
/// sub-second fraction is the nanosecond-of-second.
fn units(months: i64, days: i64, nanos: i128, present: &BTreeSet<DurationField>) -> Result<Units> {
    let mut units = Units {
        fraction: checked_lane(
            nanos.rem_euclid(NANOS_PER_SECOND),
            DurationField::Nanos.name(),
        )?,
        ..Units::default()
    };
    // Neo4j computes the total-nanoseconds lane unconditionally, so an
    // unrepresentable seconds lane is a 22N28 overflow for every pattern.
    let nanos_total = checked_lane(nanos, DurationField::Nanos.name())?;
    cascade(
        &mut units,
        present,
        DurationTier::Months,
        i128::from(months),
    )?;
    cascade(&mut units, present, DurationTier::Days, i128::from(days))?;
    cascade(
        &mut units,
        present,
        DurationTier::Nanos,
        i128::from(nanos_total),
    )?;
    Ok(units)
}

/// Left-zero padding that counts the sign as a character, matching Neo4j's
/// `padLeftZeros` (`-3` padded to four is `00-3`).
fn padded(value: i64, width: usize) -> String {
    let text = value.to_string();
    if text.len() < width {
        format!("{}{text}", "0".repeat(width - text.len()))
    } else {
        text
    }
}

/// Render a duration with a CIP-223 pattern: tokenize, calculate the named
/// units, then emit literals and padded unit runs.
pub(super) fn format_duration_pattern(
    months: i64,
    days: i64,
    nanos: i128,
    pattern: &str,
) -> Result<String> {
    let tokens = tokenize(pattern)?;
    let present: BTreeSet<DurationField> = tokens.iter().filter_map(Token::field).collect();
    let units = units(months, days, nanos, &present)?;
    let mut out = String::new();
    for token in tokens {
        match token {
            Token::Literal(text) => out.push_str(&text),
            Token::Symbol(PatternSymbol::Field(field), width) => {
                out.push_str(&padded(units.get(field), width));
            }
            Token::Symbol(PatternSymbol::FractionDigits, width) => {
                let digits = format!("{:09}", units.fraction);
                out.push_str(&digits[..width]);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_run_width_is_bounded() {
        assert!(fraction_width(1).is_ok());
        assert!(fraction_width(9).is_ok());
        assert!(fraction_width(0).is_err());
        assert!(fraction_width(10).is_err());
    }
}
