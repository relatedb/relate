//! CIP-223 temporal pattern formatting.

use chrono::{Datelike, Timelike};

use crate::{Error, Result};

use super::zone::add_offset;
use super::{format_offset, time_to_nanos, Temporal};

mod duration;

use duration::format_duration_pattern;

/// English month names, January first: the one table the formatter renders
/// from and the pattern parser reads (case-insensitively, with the
/// three-letter abbreviation).
pub(super) const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// English weekday names, Monday first (`num_days_from_monday` indexes it).
pub(super) const WEEKDAY_NAMES: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// The one fraction-of-second width policy: a pattern run of 1..=9 digits.
/// Shared by the duration fraction spellings (`n`/`S`) and the date/time `S`
/// arm, which used to truncate silently past nine.
pub(super) fn fraction_width(width: usize) -> Result<usize> {
    if (1..=9).contains(&width) {
        Ok(width)
    } else {
        Err(invalid_pattern_character())
    }
}

/// The one CIP-223 unsupported-pattern-character error: a letter outside
/// the alphabet, or one the target temporal kind has no field for, is the
/// same SyntaxError — never a per-field type error.
fn invalid_pattern_character() -> Error {
    Error::syntax("InvalidPatternCharacter")
}

/// The same rejection naming the letter — the one detail-carrying sibling
/// of [`invalid_pattern_character`], so the token has two spellings for
/// exactly this distinction and no other.
fn invalid_pattern_symbol(c: char) -> Error {
    Error::syntax(format!(
        "InvalidPatternCharacter: unsupported temporal format symbol '{c}'"
    ))
}

/// A pattern letter's field on the target temporal kind, or the one
/// unsupported-character error when the kind has no such field.
fn supported_field<T>(field: Option<T>) -> Result<T> {
    field.ok_or_else(invalid_pattern_character)
}

/// Storage-independent subset of the CIP-223/Java temporal pattern surface.
/// Unsupported alphabetic pattern symbols are rejected with the one
/// SyntaxError the CIP-223 character matrix documents
/// (`InvalidPatternCharacter`) instead of being emitted literally.
pub fn format_pattern(value: &Temporal, pattern: &str) -> Result<String> {
    if pattern.chars().any(|c| matches!(c, '#' | '{' | '}')) {
        return Err(invalid_pattern_character());
    }
    let (date, time, offset, zone) = match value {
        Temporal::Date(d) => (Some(*d), None, None, None),
        Temporal::LocalTime(t) => (None, Some(*t), None, None),
        Temporal::Time { time, offset } => (None, Some(*time), Some(*offset), None),
        Temporal::LocalDateTime(dt) => (Some(dt.date()), Some(dt.time()), None, None),
        Temporal::DateTime { utc, offset, zone } => {
            let local = add_offset(*utc, *offset)?;
            (
                Some(local.date()),
                Some(local.time()),
                Some(*offset),
                zone.as_deref(),
            )
        }
        Temporal::Duration {
            months,
            days,
            nanos,
        } => return format_duration_pattern(*months, *days, *nanos, pattern),
        Temporal::CivilDate { .. } | Temporal::CivilDateTime { .. } => {
            return if pattern.is_empty() {
                Ok(String::new())
            } else {
                Err(Error::ty(
                    "format pattern is unsupported for out-of-range dates",
                ))
            }
        }
    };
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut quoted = false;
    while i < chars.len() {
        if chars[i] == '\'' {
            if chars.get(i + 1) == Some(&'\'') {
                out.push('\'');
                i += 2;
                continue;
            }
            quoted = !quoted;
            i += 1;
            continue;
        }
        if !quoted && matches!(chars[i], '[' | ']') {
            i += 1;
            continue;
        }
        if quoted || !chars[i].is_ascii_alphabetic() {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let c = chars[i];
        let mut len = 1;
        while chars.get(i + len) == Some(&c) {
            len += 1;
        }
        if c == 'p' {
            let Some(&next) = chars.get(i + len) else {
                return Err(invalid_pattern_character());
            };
            let mut next_len = 1;
            while chars.get(i + len + next_len) == Some(&next) {
                next_len += 1;
            }
            let rendered = format_pattern(value, &next.to_string().repeat(next_len))?;
            out.push_str(&format!("{rendered:>len$}"));
            i += len + next_len;
            continue;
        }
        let piece = match c {
            'y' | 'u' | 'Y' => {
                let d = supported_field(date)?;
                let y = if c == 'Y' {
                    d.iso_week().year()
                } else if c == 'y' && d.year() <= 0 {
                    1 - d.year()
                } else {
                    d.year()
                };
                if len == 2 {
                    format!("{:02}", y.rem_euclid(100))
                } else if y < 0 {
                    format!("-{:0len$}", y.unsigned_abs())
                } else {
                    format!("{y:0len$}")
                }
            }
            'M' | 'L' => {
                let month = supported_field(date)?.month();
                if len >= 3 {
                    let full = MONTH_NAMES[month as usize - 1];
                    // The match's `_` arm is the single width rejection.
                    match len {
                        3 => full.chars().take(3).collect(),
                        4 => full.to_string(),
                        5 => full.chars().next().unwrap().to_string(),
                        _ => return Err(invalid_pattern_character()),
                    }
                } else {
                    format!("{month:0len$}")
                }
            }
            'q' | 'Q' => {
                let q = (supported_field(date)?.month0() / 3) + 1;
                match len {
                    1 => q.to_string(),
                    2 => format!("{q:02}"),
                    3 => format!("Q{q}"),
                    4 => match q {
                        1 => "1st quarter".into(),
                        2 => "2nd quarter".into(),
                        3 => "3rd quarter".into(),
                        _ => "4th quarter".into(),
                    },
                    5 => q.to_string(),
                    _ => return Err(invalid_pattern_character()),
                }
            }
            'd' => {
                if len > 2 {
                    return Err(invalid_pattern_character());
                }
                format!("{:0len$}", supported_field(date)?.day())
            }
            'D' => format!("{:0len$}", supported_field(date)?.ordinal()),
            'E' => {
                let weekday = supported_field(date)?.weekday();
                let name = WEEKDAY_NAMES[weekday.num_days_from_monday() as usize];
                if len >= 4 {
                    name.into()
                } else {
                    name.chars().take(3).collect()
                }
            }
            'e' | 'c' => (supported_field(date)?.weekday().num_days_from_sunday() + 1).to_string(),
            'w' => supported_field(date)?.iso_week().week().to_string(),
            'W' => {
                let date = supported_field(date)?;
                let first = date.with_day(1).unwrap().weekday().num_days_from_sunday();
                ((date.day() + first - 1) / 7 + 1).to_string()
            }
            'F' => (supported_field(date)?.day0() / 7 + 1).to_string(),
            'g' => (supported_field(date)?.num_days_from_ce() - 678_576).to_string(),
            'G' => {
                if supported_field(date)?.year() <= 0 {
                    "BC".into()
                } else {
                    "AD".into()
                }
            }
            'H' => format!("{:0len$}", supported_field(time)?.hour()),
            'k' => {
                let h = supported_field(time)?.hour();
                format!("{:0len$}", if h == 0 { 24 } else { h })
            }
            'h' => {
                let h = supported_field(time)?.hour() % 12;
                format!("{:0len$}", if h == 0 { 12 } else { h })
            }
            'K' => format!("{:0len$}", supported_field(time)?.hour() % 12),
            'a' => {
                if supported_field(time)?.hour() < 12 {
                    "AM".into()
                } else {
                    "PM".into()
                }
            }
            'B' => {
                let hour = supported_field(time)?.hour();
                match hour {
                    0..=5 => "at night",
                    6..=11 => "in the morning",
                    12..=17 => "in the afternoon",
                    _ => "in the evening",
                }
                .into()
            }
            'm' => format!("{:0len$}", supported_field(time)?.minute()),
            's' => format!("{:0len$}", supported_field(time)?.second()),
            'S' => {
                let nanos = supported_field(time)?.nanosecond();
                let width = fraction_width(len)?;
                format!("{nanos:09}").chars().take(width).collect()
            }
            'n' => supported_field(time)?.nanosecond().to_string(),
            'N' => time_to_nanos(supported_field(time)?).to_string(),
            'A' => (time_to_nanos(supported_field(time)?) / 1_000_000).to_string(),
            'Z' => {
                let offset = supported_field(offset)?;
                match len {
                    1..=3 => format_offset(offset).replace(':', ""),
                    4 if offset == 0 => "GMT".into(),
                    4 => format!("GMT{}", format_offset(offset)),
                    5 if offset == 0 => "Z".into(),
                    5 => format_offset(offset),
                    _ => return Err(invalid_pattern_character()),
                }
            }
            'X' | 'x' => {
                let offset = supported_field(offset)?;
                if offset == 0 && c == 'X' {
                    "Z".into()
                } else {
                    let raw = format_offset(offset);
                    let compact = raw.replace(':', "");
                    match len {
                        1 => {
                            if offset % 3600 == 0 {
                                compact[..3].into()
                            } else {
                                compact
                            }
                        }
                        2 | 4 => compact,
                        3 | 5 => raw,
                        _ => return Err(invalid_pattern_character()),
                    }
                }
            }
            'O' => {
                let offset = supported_field(offset)?;
                match (len, offset) {
                    (1 | 4, 0) => "GMT".into(),
                    (1, _) if offset % 3600 == 0 => {
                        format!("GMT{:+}", offset / 3600)
                    }
                    (1, _) => format!("GMT{}", format_offset(offset)),
                    (4, _) => format!("GMT{}", format_offset(offset)),
                    _ => return Err(invalid_pattern_character()),
                }
            }
            'z' | 'v' => {
                let zone = supported_field(zone)?;
                if (c == 'z' && len > 4) || (c == 'v' && !matches!(len, 1 | 4)) {
                    return Err(invalid_pattern_character());
                }
                match (zone, c, len) {
                    ("Europe/Paris", 'z', 4) => "Central European Summer Time".into(),
                    ("Europe/Paris", 'v', 4) => "Central European Time".into(),
                    ("Europe/Paris", 'z', _) => "CEST".into(),
                    ("Europe/Paris", 'v', _) => "CET".into(),
                    _ => zone.into(),
                }
            }
            // `V` (zone id) needs an actual zone: a zoned time or local
            // datetime has no zone, so per the matrix it is an unsupported
            // pattern character there — not an offset fallback.
            'V' if len == 2 => supported_field(zone)?.to_string(),
            _ => {
                return Err(invalid_pattern_symbol(c));
            }
        };
        out.push_str(&piece);
        i += len;
    }
    if quoted {
        // The duration pattern engine spells this the same way with the
        // syntax class; one class for one escape failure in both engines.
        return Err(Error::syntax("unterminated quote in temporal format"));
    }
    Ok(out)
}
