//! Instant-pattern matching: the collected [`Fields`], the token matcher,
//! and the per-field input readers.

use chrono::Weekday;
use chrono_tz::Tz;

use crate::Result;

use super::super::format::{MONTH_NAMES, WEEKDAY_NAMES};
use super::super::range_err;
use super::super::zone::{parse_offset, zone_from_short_name};
use super::token::{FieldKind, Token};
use super::{as_u32, nanos_from_fraction, read_literal, read_number, Digits};

/// The era a pattern's `G` field named; a year-of-era under `Bc` is
/// shifted by one (44 BC is proleptic year -43).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Era {
    Ad,
    Bc,
}

/// The field values an instant pattern collected, before the target kind
/// decides which of them it uses (CIP-228: a character outside the kind's
/// checkmarks is parsed but ignored).
#[derive(Default, Clone)]
pub(super) struct Fields {
    pub(super) year: Option<i64>,
    pub(super) week_year: Option<i64>,
    pub(super) month: Option<u32>,
    pub(super) day: Option<u32>,
    pub(super) day_of_year: Option<u32>,
    pub(super) week: Option<u32>,
    pub(super) quarter: Option<u32>,
    pub(super) day_of_week: Option<u32>,
    pub(super) day_of_week_name: Option<Weekday>,
    pub(super) aligned_day_of_week: Option<u32>,
    pub(super) modified_julian: Option<i64>,
    pub(super) hour_of_day: Option<u32>,
    pub(super) clock_hour: Option<u32>,
    pub(super) hour_of_am_pm: Option<u32>,
    pub(super) minute: Option<u32>,
    pub(super) second: Option<u32>,
    pub(super) nano: Option<u32>,
    pub(super) milli_of_day: Option<i64>,
    pub(super) nano_of_day: Option<i64>,
    pub(super) am_pm: Option<bool>,
    /// The `y` spelling is year-of-era; `u` is the proleptic year.
    pub(super) year_of_era: bool,
    /// The parsed era, when the pattern named one: `BC`/`BCE` shifts a
    /// year-of-era by one, an absent era reads as `AD`.
    pub(super) era: Option<Era>,
    pub(super) offset: Option<i32>,
    pub(super) zone: Option<Tz>,
}

impl Fields {
    /// The proleptic year: a `BC`/`BCE` era shifts a year-of-era (`y`) by
    /// one (44 BC is year -43); a proleptic `u` year is already absolute.
    pub(super) fn proleptic_year(&self) -> Option<i64> {
        self.year.map(|year| {
            if self.year_of_era && self.era == Some(Era::Bc) {
                1 - year
            } else {
                year
            }
        })
    }

    /// Whether the pattern named a time-of-day field — derived from the
    /// collected fields rather than tracked beside them.
    pub(super) fn has_time(&self) -> bool {
        self.hour_of_day.is_some()
            || self.clock_hour.is_some()
            || self.hour_of_am_pm.is_some()
            || self.minute.is_some()
            || self.second.is_some()
            || self.nano.is_some()
            || self.milli_of_day.is_some()
            || self.nano_of_day.is_some()
    }
}

/// `Ok(None)` is a shape mismatch (the input does not fit the pattern);
/// `Err` is a value the grammar parsed but cannot represent (a component
/// that overflows `i64`), which must not read as a mismatch.
pub(super) fn match_tokens(
    tokens: &[Token],
    input: &str,
    pos: usize,
    fields: &mut Fields,
) -> Result<Option<usize>> {
    let mut pos = pos;
    for token in tokens {
        match token {
            Token::Literal(text) => {
                if read_literal(input, &mut pos, text).is_none() {
                    return Ok(None);
                }
            }
            Token::Optional(inner) => {
                let mut trial = fields.clone();
                if let Some(end) = match_tokens(inner, input, pos, &mut trial)? {
                    *fields = trial;
                    pos = end;
                }
            }
            Token::Field {
                kind,
                letter,
                count,
                pad,
            } => {
                let Some(end) = read_field(*kind, *letter, *count, *pad, input, pos, fields)?
                else {
                    return Ok(None);
                };
                pos = end;
            }
        }
    }
    Ok(Some(pos))
}

/// Read a numeric field into a `u32` slot: `Ok(false)` is a shape mismatch.
fn take_u32(slot: &mut Option<u32>, input: &str, pos: &mut usize, count: usize) -> Result<bool> {
    let Some(raw) = read_number(input, pos, field_width(count), false)? else {
        return Ok(false);
    };
    *slot = Some(as_u32(raw.value)?);
    Ok(true)
}

/// Read a numeric field into an `i64` slot, with an optional leading sign.
fn take_i64(
    slot: &mut Option<i64>,
    input: &str,
    pos: &mut usize,
    count: usize,
    sign: bool,
) -> Result<bool> {
    let Some(raw) = read_number(input, pos, field_width(count), sign)? else {
        return Ok(false);
    };
    *slot = Some(raw.value);
    Ok(true)
}

/// Read one field; `Ok(None)` is a shape mismatch. Every arm evaluates to
/// whether it matched, and the position is only reported on a match.
fn read_field(
    kind: FieldKind,
    letter: char,
    count: usize,
    pad: usize,
    input: &str,
    mut pos: usize,
    fields: &mut Fields,
) -> Result<Option<usize>> {
    let p = &mut pos;
    let matched = match kind {
        FieldKind::Year => {
            let raw = if pad == 0 {
                read_number(input, p, field_width(count), false)?
            } else {
                read_padded_number(input, p, pad)?
            };
            match raw {
                Some(raw) => {
                    fields.year_of_era = letter == 'y';
                    fields.year = Some(if count == 2 {
                        2000 + raw.value.rem_euclid(100)
                    } else {
                        raw.value
                    });
                    true
                }
                None => false,
            }
        }
        FieldKind::WeekYear => take_i64(&mut fields.week_year, input, p, count, false)?,
        FieldKind::Month if count >= 3 => {
            fields.month = read_alpha(input, p).and_then(month_from_name);
            fields.month.is_some()
        }
        FieldKind::Month => take_u32(&mut fields.month, input, p, count)?,
        FieldKind::Day => take_u32(&mut fields.day, input, p, count)?,
        FieldKind::DayOfYear => take_u32(&mut fields.day_of_year, input, p, count)?,
        FieldKind::Week => take_u32(&mut fields.week, input, p, count)?,
        // Defined but does not contribute to the kinds this engine
        // constructs; still consumes its input.
        FieldKind::WeekOfMonth => read_number(input, p, field_width(count), false)?.is_some(),
        FieldKind::Quarter if count >= 4 => {
            fields.quarter = read_quarter(input, p);
            fields.quarter.is_some()
        }
        FieldKind::Quarter => take_u32(&mut fields.quarter, input, p, count)?,
        FieldKind::DayOfWeekNum => take_u32(&mut fields.day_of_week, input, p, count)?,
        FieldKind::DayOfWeekName => {
            fields.day_of_week_name = read_alpha(input, p).and_then(weekday_from_name);
            fields.day_of_week_name.is_some()
        }
        FieldKind::AlignedDayOfWeek => take_u32(&mut fields.aligned_day_of_week, input, p, count)?,
        FieldKind::ModifiedJulianDay => {
            take_i64(&mut fields.modified_julian, input, p, count, true)?
        }
        FieldKind::HourOfDay => take_u32(&mut fields.hour_of_day, input, p, count)?,
        FieldKind::ClockHour => take_u32(&mut fields.clock_hour, input, p, count)?,
        FieldKind::HourOfAmPm => take_u32(&mut fields.hour_of_am_pm, input, p, count)?,
        FieldKind::Minute => take_u32(&mut fields.minute, input, p, count)?,
        FieldKind::Second => take_u32(&mut fields.second, input, p, count)?,
        FieldKind::Fraction => {
            let width = count.min(9);
            match read_number(input, p, Some(width), false)? {
                Some(raw) => {
                    fields.nano = Some(as_u32(nanos_from_fraction(raw.value, raw.digits)?)?);
                    true
                }
                None => false,
            }
        }
        FieldKind::NanoOfSecond => take_u32(&mut fields.nano, input, p, count)?,
        FieldKind::MilliOfDay => take_i64(&mut fields.milli_of_day, input, p, count, true)?,
        FieldKind::NanoOfDay => take_i64(&mut fields.nano_of_day, input, p, count, true)?,
        FieldKind::AmPm => {
            fields.am_pm = read_alpha(input, p).and_then(|marker| {
                match marker.to_ascii_uppercase().as_str() {
                    "AM" => Some(false),
                    "PM" => Some(true),
                    _ => None,
                }
            });
            fields.am_pm.is_some()
        }
        FieldKind::PeriodOfDay => {
            // A day-period phrase is informational; it consumes the rest of
            // the input and contributes nothing to the value.
            *p = input.len();
            true
        }
        FieldKind::Era => {
            let era = read_alpha(input, p).and_then(era_from_name);
            if era.is_some() {
                fields.era = era;
            }
            era.is_some()
        }
        FieldKind::ZoneId => read_zone_id(input, p, fields),
        FieldKind::ZoneName => read_zone_name(input, p, fields),
        FieldKind::OffsetFractional | FieldKind::OffsetZ => {
            fields.offset = read_offset(input, p);
            fields.offset.is_some()
        }
        FieldKind::OffsetLocalized => read_localized_offset(input, p, fields),
    };
    Ok(matched.then_some(pos))
}

/// `VV` is a zone id: an IANA region, or a `ZoneOffset` id such as
/// `+02:00` (java.time's `ZoneId` admits both). Anything else does not
/// match this field.
fn read_zone_id(input: &str, pos: &mut usize, fields: &mut Fields) -> bool {
    let Some(name) = read_non_space(input, pos) else {
        return false;
    };
    if name.starts_with(['+', '-']) {
        let Ok(offset) = parse_offset(name) else {
            return false;
        };
        fields.offset = Some(offset);
        true
    } else {
        accept_zone(name, fields)
    }
}

/// Record `name` as the pattern's zone when it is an IANA id; `false`
/// (a shape mismatch) otherwise. The one zone-id acceptance rule for `VV`
/// and `z`.
fn accept_zone(name: &str, fields: &mut Fields) -> bool {
    let Ok(zone) = name.parse::<Tz>() else {
        return false;
    };
    fields.zone = Some(zone);
    true
}

/// `z`: a short zone name, or a full IANA id.
fn read_zone_name(input: &str, pos: &mut usize, fields: &mut Fields) -> bool {
    let Some(name) = read_non_space(input, pos) else {
        return false;
    };
    accept_zone(zone_from_short_name(name).unwrap_or(name), fields)
}

/// `O` is the localized offset spelling: a `GMT`/`UTC` prefix and an
/// optional numeric offset (`GMT+2`, `UTC-08:00`).
fn read_localized_offset(input: &str, pos: &mut usize, fields: &mut Fields) -> bool {
    let Some(raw) = read_non_space(input, pos) else {
        return false;
    };
    let Some(rest) = raw.strip_prefix("GMT").or_else(|| raw.strip_prefix("UTC")) else {
        return false;
    };
    let offset = if rest.is_empty() {
        0
    } else {
        let Ok(offset) = parse_offset(rest) else {
            return false;
        };
        offset
    };
    fields.offset = Some(offset);
    true
}

/// The input width a field token consumes: a repeated letter run (`MM`,
/// `yyyy`) is fixed-width; a single letter consumes the whole digit run.
fn field_width(count: usize) -> Option<usize> {
    (count > 1).then_some(count)
}

/// A `p`-padded field: exactly `width` characters, leading spaces ignored.
fn read_padded_number(input: &str, pos: &mut usize, width: usize) -> Result<Option<Digits>> {
    let Some(text) = input.get(*pos..*pos + width) else {
        return Ok(None);
    };
    let trimmed = text.trim_start();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(None);
    }
    let value = trimmed.parse().map_err(|_| range_err())?;
    *pos += width;
    Ok(Some(Digits {
        value,
        digits: trimmed.len(),
    }))
}

/// Take the longest run of characters `take` accepts; `None` when the run
/// is empty.
fn read_while<'a>(input: &'a str, pos: &mut usize, take: impl Fn(char) -> bool) -> Option<&'a str> {
    let start = *pos;
    while let Some(c) = input[*pos..].chars().next() {
        if !take(c) {
            break;
        }
        *pos += c.len_utf8();
    }
    (*pos > start).then(|| &input[start..*pos])
}

fn read_alpha<'a>(input: &'a str, pos: &mut usize) -> Option<&'a str> {
    read_while(input, pos, char::is_alphabetic)
}

fn read_non_space<'a>(input: &'a str, pos: &mut usize) -> Option<&'a str> {
    read_while(input, pos, |c| !c.is_whitespace())
}

/// `QQQQ`: a quarter ordinal plus the literal word `quarter`, e.g.
/// `2nd quarter`. The ordinal suffix is not inspected; the number is.
fn read_quarter(input: &str, pos: &mut usize) -> Option<u32> {
    let quarter: u32 = read_while(input, pos, |c| c.is_ascii_digit())?
        .parse()
        .ok()?;
    // The ordinal suffix is not inspected: it is consumed and dropped.
    let _ = read_alpha(input, pos);
    read_while(input, pos, |c| c == ' ')?;
    let word = input.get(*pos..)?.get(..7)?;
    if !word.eq_ignore_ascii_case("quarter") {
        return None;
    }
    *pos += 7;
    Some(quarter)
}

/// A `X`/`x`/`Z` zone offset: `Z` for UTC, otherwise a signed `HH`,
/// `HHMM`, `HH:MM` (optionally with seconds) run.
fn read_offset(input: &str, pos: &mut usize) -> Option<i32> {
    if matches!(input.as_bytes().get(*pos), Some(b'Z' | b'z')) {
        *pos += 1;
        return Some(0);
    }
    let start = *pos;
    if matches!(input.as_bytes().get(*pos), Some(b'+' | b'-')) {
        *pos += 1;
    }
    read_while(input, pos, |c| c.is_ascii_digit() || c == ':')?;
    parse_offset(input.get(start..*pos)?).ok()
}

/// Index of `name` in a full-name table, case-insensitively and accepting
/// the three-letter abbreviation (Java's `MMM`/`EEE` short forms) — the one
/// spelling of the name rule both tables read.
fn match_name(name: &str, table: &[&str]) -> Option<usize> {
    table.iter().position(|full| {
        full.eq_ignore_ascii_case(name) || (name.len() == 3 && full[..3].eq_ignore_ascii_case(name))
    })
}

fn era_from_name(name: &str) -> Option<Era> {
    match name.to_ascii_uppercase().as_str() {
        "AD" | "CE" => Some(Era::Ad),
        "BC" | "BCE" => Some(Era::Bc),
        _ => None,
    }
}

fn month_from_name(name: &str) -> Option<u32> {
    match_name(name, &MONTH_NAMES).map(|index| index as u32 + 1)
}

fn weekday_from_name(name: &str) -> Option<Weekday> {
    let index = match_name(name, &WEEKDAY_NAMES)?;
    Weekday::try_from(u8::try_from(index).ok()?).ok()
}
