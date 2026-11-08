//! CIP-228 pattern parsing: the `date(value, pattern)` family and the
//! `duration(value, pattern)` two-argument form.
//!
//! Instants follow the Java `DateTimeFormatter` pattern alphabet Neo4j
//! uses (a letter outside that alphabet is a pattern error; a letter the
//! target kind has no use for is parsed and then ignored, CIP-228's
//! "subset of characters" rule). Durations follow Neo4j's own
//! `DurationValue.parsePattern`: only the duration alphabet is a field
//! token, every other character is literal text, and a field may not
//! repeat.
//!
//! Both grammars are compiled to a token list and matched sequentially
//! against the input, so a pattern that does not consume the whole input —
//! the input ending early, a literal that does not match, jumbled fields —
//! is one typed mismatch rather than a per-arm special case.
//!
//! This module root owns the primitives both grammars share (the error
//! spellings, [`read_number`], [`read_literal`], the duplicate-field
//! rule); `duration_pattern` is the other grammar. The instant grammar is
//! split by concern: `token` compiles a pattern, `fields` matches input
//! against it, `resolve` turns the collected fields into a value.

mod fields;
mod resolve;
mod token;

use std::collections::BTreeSet;

use chrono::Offset;
use chrono_tz::Tz;

use crate::{Error, Result};

use super::zone::{
    ambiguous_datetime_err, resolve_local, sub_offset, unknown_zone_err, LocalResolution,
};
use super::{range_err, Temporal};

use fields::{match_tokens, Fields};
use resolve::{resolve_date, resolve_time};
use token::tokenize;

/// The one spelling of a pattern/value failure. Neo4j reports pattern
/// parse and match failures as `InvalidArgumentValue`; the harness's GQL
/// tables and the CIP-228 suite both accept that class.
pub(super) fn invalid_value(detail: impl std::fmt::Display) -> Error {
    Error::ty(format!("InvalidArgumentValue: {detail}"))
}

pub(super) fn mismatch() -> Error {
    invalid_value("mismatched pattern")
}

/// Neo4j rejects a pattern that names the same field twice (CIP-228: "a
/// field may not occur multiple times"): two occurrences cannot both be
/// satisfied by one input position. One spelling for both grammars.
fn duplicate_field_err() -> Error {
    invalid_value("ambiguous pattern: duplicate field")
}

/// Record a field in a pattern's duplicate-detection set; a repeat is the
/// one duplicate-field error. Both tokenizers key it with their own field
/// vocabulary.
pub(super) fn note_field<K: Ord>(seen: &mut BTreeSet<K>, key: K) -> Result<()> {
    if seen.insert(key) {
        Ok(())
    } else {
        Err(duplicate_field_err())
    }
}

fn pattern_error(detail: impl std::fmt::Display) -> Error {
    invalid_value(format!("invalid pattern: {detail}"))
}

fn as_u32(value: i64) -> Result<u32> {
    u32::try_from(value).map_err(|_| range_err())
}

/// One numeric field read: the parsed value and the width of its digit run
/// (the fraction field pads by that width).
pub(super) struct Digits {
    pub(super) value: i64,
    pub(super) digits: usize,
}

/// Read up to `exact` digits (a run when `None`). With `sign`, a leading
/// `+`/`-` belongs to the number. `Ok(None)` is a shape mismatch; the
/// digits are ASCII and shape-checked, so a parse failure is overflow.
///
/// The position advances only on `Ok(Some)`: a mismatch (including a
/// consumed sign) or an overflow restores it, the same contract as
/// [`read_literal`]. Every caller propagates the failure immediately, so
/// nothing depended on the old advance-on-failure behaviour.
pub(super) fn read_number(
    input: &str,
    pos: &mut usize,
    exact: Option<usize>,
    sign: bool,
) -> Result<Option<Digits>> {
    let start = *pos;
    if sign && matches!(input.as_bytes().get(*pos), Some(b'+' | b'-')) {
        *pos += 1;
    }
    let digits_start = *pos;
    let limit = exact.unwrap_or(usize::MAX);
    while *pos < input.len()
        && *pos - digits_start < limit
        && input.as_bytes()[*pos].is_ascii_digit()
    {
        *pos += 1;
    }
    let digits = *pos - digits_start;
    let satisfied = match exact {
        Some(want) => digits == want,
        None => digits > 0,
    };
    if !satisfied {
        *pos = start;
        return Ok(None);
    }
    let Some(text) = input.get(start..*pos) else {
        *pos = start;
        return Ok(None);
    };
    let value = text.parse().map_err(|_| {
        *pos = start;
        range_err()
    })?;
    Ok(Some(Digits { value, digits }))
}

/// The nanosecond value of a fraction's digits: `digits` fraction digits
/// name that many tenths-powers of a second, so the value is right-padded
/// with zeros to nine places (`87` → 870_000_000 ns). The one spelling of
/// the pad rule for both pattern grammars.
pub(super) fn nanos_from_fraction(value: i64, digits: usize) -> Result<i64> {
    if digits >= 9 {
        return Ok(value);
    }
    let scale = 10i64
        .checked_pow((9 - digits) as u32)
        .ok_or_else(range_err)?;
    value.checked_mul(scale).ok_or_else(range_err)
}
/// Consume exactly `text` at `pos`; `None` (position unchanged) when the
/// input does not carry it. The one literal-consumption rule for both
/// pattern grammars.
pub(super) fn read_literal(input: &str, pos: &mut usize, text: &str) -> Option<()> {
    if !input.get(*pos..).is_some_and(|rest| rest.starts_with(text)) {
        return None;
    }
    *pos += text.len();
    Some(())
}
/// `date(value, pattern)` and the other instant kinds.
pub(super) fn parse_instant_pattern(kind: &str, value: &str, pattern: &str) -> Result<Temporal> {
    let tokens = tokenize(pattern)?;
    let mut fields = Fields::default();
    let end = match_tokens(&tokens, value, 0, &mut fields)?.ok_or_else(mismatch)?;
    if end != value.len() {
        return Err(mismatch());
    }
    match kind {
        "date" => Ok(Temporal::Date(resolve_date(&fields)?)),
        "localtime" => Ok(Temporal::LocalTime(resolve_time(&fields)?)),
        "time" => Ok(Temporal::Time {
            time: resolve_time(&fields)?,
            offset: fields.offset.unwrap_or(0),
        }),
        "localdatetime" => Ok(Temporal::LocalDateTime(
            resolve_date(&fields)?.and_time(resolve_time(&fields)?),
        )),
        "datetime" => {
            let date = resolve_date(&fields)?;
            let time = resolve_time(&fields)?;
            let local = date.and_time(time);
            if let Some(zone) = fields.zone.as_deref() {
                let tz: Tz = zone.parse().map_err(|_| unknown_zone_err(zone))?;
                let resolved = match resolve_local(tz, local) {
                    LocalResolution::Unique(dt) => dt,
                    // ADR-0178's overlap rule: the earlier occurrence.
                    LocalResolution::Ambiguous { earlier, .. } => earlier,
                    LocalResolution::None => return Err(ambiguous_datetime_err()),
                };
                return Ok(Temporal::DateTime {
                    utc: resolved.naive_utc(),
                    offset: resolved.offset().fix().local_minus_utc(),
                    zone: Some(zone.to_string()),
                });
            }
            if fields.has_time() {
                let offset = fields.offset.unwrap_or(0);
                return Ok(Temporal::DateTime {
                    utc: sub_offset(local, offset)?,
                    offset,
                    zone: fields.offset.is_none().then(|| "Z".into()),
                });
            }
            // A date-only pattern: a parsed offset does not apply (CIP-228
            // instant semantics), so the value is midnight UTC.
            Ok(Temporal::DateTime {
                utc: local,
                offset: 0,
                zone: Some("Z".into()),
            })
        }
        other => Err(invalid_value(format!("unknown temporal kind {other}"))),
    }
}
