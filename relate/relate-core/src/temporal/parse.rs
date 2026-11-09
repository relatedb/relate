//! Parsing of temporal values: ISO-8601 forms, CIP-228 patterns, and durations.

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Weekday};

use crate::{Error, Result, Value};

use super::clock::ClockKind;
use super::construct::date_from_week;
use super::duration::parse_duration_iso;
use super::duration_pattern::parse_duration_pattern;
use super::pattern::{invalid_value, parse_instant_pattern};
use super::zone::{
    ambiguous_datetime_err, parse_offset, resolve_wall_forward, sub_offset, wall_clock, zone_kind,
    WallClock, ZoneSpec,
};
use super::{range_err, weekday_from_i, Temporal};

pub fn parse_iso(kind: &str, s: &str) -> Result<Temporal> {
    match kind {
        "date" => parse_date_str(s),
        // One arm and one parser per time kind: a `localtime` literal never
        // carries an offset, and its parser says so (`cannot parse time`),
        // rather than the kind falling out of a shared guard into the
        // "unknown temporal kind" arm.
        "localtime" => parse_naive_time(s).map(Temporal::LocalTime),
        "time" => parse_time_str(s),
        "localdatetime" => parse_localdatetime_str(s),
        "datetime" => parse_zoned_dt(s),
        "duration" => parse_duration_iso(s),
        _ => Err(Error::ty(format!("unknown temporal kind {kind}"))),
    }
}

/// Whether a time literal carries an offset marker: a `+`/`Z` suffix, or a
/// `-` past the first byte (a `-` in the first position is a sign the time
/// grammar never has, so it is left to the parser's own rejection).
fn time_has_offset(s: &str) -> bool {
    s.contains('+') || s.contains('Z') || s.get(1..).unwrap_or("").contains('-')
}

/// A `time` literal: the zone-less spelling builds the offset-zero wall
/// clock, a carrying one goes through [`parse_zoned_time`]. A `time` literal
/// has no date, so a bracketed zone name is not assignable to it; the
/// reference engine rejects the literal rather than dropping the zone.
fn parse_time_str(s: &str) -> Result<Temporal> {
    if s.contains('[') {
        return Err(Error::argument_error(
            "Using a named time zone is not valid for a time without a date.",
        ));
    }
    if time_has_offset(s) {
        return parse_zoned_time(s);
    }
    parse_naive_time(s).map(|time| Temporal::Time { time, offset: 0 })
}

pub fn parse_pattern(kind: &str, value: &str, pattern: &str) -> Result<Temporal> {
    if value.is_empty() || pattern.is_empty() {
        return Err(invalid_value("empty temporal value or pattern"));
    }
    if kind == "duration" {
        return parse_duration_pattern(value, pattern);
    }
    let kind = ClockKind::from_name(kind)
        .ok_or_else(|| invalid_value(format!("unknown temporal kind {kind}")))?;
    parse_instant_pattern(kind, value, pattern)
}

fn parse_civil_ymd(s: &str) -> Option<(i64, u32, u32)> {
    let t = s.trim();
    let (sign, rest) = if let Some(rest) = t.strip_prefix('+') {
        (1i64, rest)
    } else if let Some(rest) = t.strip_prefix('-') {
        if rest.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            (-1i64, rest)
        } else {
            (1i64, t)
        }
    } else {
        (1i64, t)
    };
    let mut parts = rest.split('-');
    let year = parts.next()?.parse::<i64>().ok()? * sign;
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;
    Some((year, month, day))
}

fn parse_date_str(s: &str) -> Result<Temporal> {
    if let Some((year, month, day)) = parse_civil_ymd(s) {
        // One calendar check per branch: inside chrono's range
        // `NaiveDate::from_ymd_opt` is the calendar, and outside it
        // `is_civil_date` is (the check chrono cannot make for an i64 year).
        // The split only chooses the representation.
        if (i64::from(NaiveDate::MIN.year())..=i64::from(NaiveDate::MAX.year())).contains(&year) {
            let date =
                NaiveDate::from_ymd_opt(i32::try_from(year).map_err(|_| range_err())?, month, day)
                    .ok_or_else(|| Error::ty(format!("invalid date '{s}'")))?;
            return Ok(Temporal::Date(date));
        }
        if !is_civil_date(year, month, day) {
            return Err(Error::ty(format!("invalid date '{s}'")));
        }
        return Ok(Temporal::CivilDate { year, month, day });
    }
    let t = s.trim().trim_start_matches('+');
    parse_date_flexible(t)
        .map(Temporal::Date)
        .ok_or_else(|| Error::ty(format!("cannot parse date '{s}'")))
}

/// Whether `(year, month, day)` names a proleptic-Gregorian calendar date
/// for any `i64` year — the check chrono cannot make outside its own range.
fn is_civil_date(year: i64, month: u32, day: u32) -> bool {
    days_in_month(year, month).is_some_and(|length| (1..=length).contains(&day))
}

/// The proleptic-Gregorian length of `month` in `year`, or `None` when
/// `month` is not a month: February follows the 4/100/400 leap rule.
/// Divisibility tests read the same for negative years, so the calendar
/// stays proleptic on both sides of year zero.
fn days_in_month(year: i64, month: u32) -> Option<u32> {
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => return None,
    })
}

/// `date(s)` for the exact padded-ISO calendar form `YYYY-MM-DD` —
/// ASCII digits in the year, month and day slots, dashes in the fixed
/// separators — as [`parse_date_str`] builds it, without the
/// split-and-parse walk. `None` for any other shape or for an impossible
/// calendar date: the caller then constructs through the general policy
/// (and reports its typed error). The per-row scan path reads stored
/// padded-ISO birthdays this way; the mirrored reading is pinned by a
/// differential test against `parse_date_str`.
pub fn padded_iso_date_value(s: &str) -> Option<Value> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let digits = |slot: &[u8]| slot.iter().all(u8::is_ascii_digit);
    if !(digits(&b[..4]) && digits(&b[5..7]) && digits(&b[8..])) {
        return None;
    }
    let year: i32 = s[..4].parse().ok()?;
    let month: u32 = s[5..7].parse().ok()?;
    let day: u32 = s[8..].parse().ok()?;
    NaiveDate::from_ymd_opt(year, month, day).map(|d| Value::Temporal(Temporal::Date(d)))
}

fn parse_date_flexible(s: &str) -> Option<NaiveDate> {
    let t = s.trim().trim_start_matches('+');
    if let Ok(d) = NaiveDate::parse_from_str(t, "%Y-%m-%d") {
        return Some(d);
    }
    if let Ok(d) = NaiveDate::parse_from_str(t, "%Y%m%d") {
        return Some(d);
    }
    if t.len() == 7 && t.as_bytes().get(4) == Some(&b'-') {
        if let (Ok(y), Ok(m)) = (t[..4].parse::<i32>(), t[5..].parse::<u32>()) {
            return NaiveDate::from_ymd_opt(y, m, 1);
        }
    }
    if t.len() == 6 && t.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(d) = NaiveDate::parse_from_str(&format!("{t}01"), "%Y%m%d") {
            return Some(d);
        }
    }
    if t.len() == 4 && t.chars().all(|c| c.is_ascii_digit()) {
        return NaiveDate::from_ymd_opt(t.parse().ok()?, 1, 1);
    }
    if let Some(d) = parse_week_date(t) {
        return Some(d);
    }
    parse_ordinal_date(t)
}

fn parse_week_date(s: &str) -> Option<NaiveDate> {
    let compact = s.contains('W') && !s.contains('-');
    let rest = s.split_once('W')?.1;
    let year: i32 = s.split(['W', '-']).next()?.parse().ok()?;
    if compact {
        if rest.len() >= 3 {
            let week: u32 = rest.get(..2)?.parse().ok()?;
            let dow = rest.get(2..)?.parse::<u32>().unwrap_or(1);
            return date_from_week(year, week, weekday_from_i(i64::from(dow))).ok();
        }
        let week: u32 = rest.parse().ok()?;
        return date_from_week(year, week, Weekday::Mon).ok();
    }
    let parts: Vec<&str> = rest.split('-').collect();
    let week: u32 = parts.first()?.parse().ok()?;
    let dow = parts
        .get(1)
        .and_then(|p| p.parse::<i64>().ok())
        .unwrap_or(1);
    date_from_week(year, week, weekday_from_i(dow)).ok()
}

fn parse_ordinal_date(s: &str) -> Option<NaiveDate> {
    if let Some((y, d)) = s.split_once('-') {
        if y.len() == 4 && d.len() == 3 && d.chars().all(|c| c.is_ascii_digit()) {
            return NaiveDate::from_yo_opt(y.parse().ok()?, d.parse().ok()?);
        }
    }
    if s.len() == 7 && s.chars().all(|c| c.is_ascii_digit()) {
        return NaiveDate::from_yo_opt(s[..4].parse().ok()?, s[4..].parse().ok()?);
    }
    None
}

fn parse_localdatetime_str(s: &str) -> Result<Temporal> {
    let (body, _) = split_offset(s);
    if let Ok(dt) = parse_naive_dt(body) {
        return Ok(Temporal::LocalDateTime(dt));
    }
    let (date_s, time_s) = match body.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (body, None),
    };
    if let Temporal::CivilDate { year, month, day } = parse_date_str(date_s)? {
        let time = match time_s {
            Some(t) if !t.is_empty() => parse_naive_time(t).unwrap_or(NaiveTime::MIN),
            _ => NaiveTime::MIN,
        };
        return Ok(Temporal::CivilDateTime {
            year,
            month,
            day,
            time,
        });
    }
    Err(Error::ty(format!("cannot parse localdatetime '{s}'")))
}

fn parse_naive_time(s: &str) -> Result<NaiveTime> {
    for fmt in ["%H:%M:%S%.f", "%H:%M:%S", "%H:%M", "%H%M%S", "%H%M"] {
        if let Ok(t) = NaiveTime::parse_from_str(s, fmt) {
            return Ok(t);
        }
    }
    if let Some((hms, frac)) = s.split_once('.') {
        if hms.len() == 6 && hms.chars().all(|c| c.is_ascii_digit()) {
            let h: u32 = hms[..2].parse().unwrap_or(0);
            let m: u32 = hms[2..4].parse().unwrap_or(0);
            let sec: u32 = hms[4..].parse().unwrap_or(0);
            let mut nano = 0u32;
            let mut place = 100_000_000u32;
            for c in frac.chars().filter(|c| c.is_ascii_digit()) {
                nano += (c as u32 - b'0' as u32) * place;
                place /= 10;
                if place == 0 {
                    break;
                }
            }
            if let Some(t) = NaiveTime::from_hms_nano_opt(h, m, sec, nano) {
                return Ok(t);
            }
        }
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        let padded = match s.len() {
            1 | 2 => format!("{s:0>2}0000"),
            4 => format!("{s}00"),
            6 => s.to_string(),
            _ => String::new(),
        };
        if padded.len() == 6 {
            if let Ok(t) = NaiveTime::parse_from_str(&padded, "%H%M%S") {
                return Ok(t);
            }
        }
    }
    Err(Error::ty(format!("cannot parse time '{s}'")))
}

pub fn datetime_from_epoch(seconds: i64, nanos: i64) -> Result<Temporal> {
    let utc = chrono::DateTime::from_timestamp(seconds, nanos.clamp(0, 999_999_999) as u32)
        .ok_or_else(|| Error::ty("epoch out of range"))?
        .naive_utc();
    Ok(Temporal::DateTime {
        utc,
        offset: 0,
        zone: Some("Z".into()),
    })
}

pub fn datetime_from_epoch_millis(ms: i64) -> Result<Temporal> {
    datetime_from_epoch(ms.div_euclid(1000), (ms.rem_euclid(1000)) * 1_000_000)
}

fn parse_naive_dt(s: &str) -> Result<NaiveDateTime> {
    let normalized = s.replace(' ', "T");
    let (body, _) = split_offset(&normalized);
    // chrono's `%Y` needs the sign for a year outside `0000..=9999`
    // (`+275760-09-13T00:00`, the reference engine's maximum datetime), so
    // try the text as written before the unsigned spelling the shorter
    // fast paths expect. Trimming first would turn a signed six-digit year
    // into an unparseable unsigned one.
    let unsigned = body.trim_start_matches('+');
    for candidate in [body, unsigned] {
        for fmt in [
            "%Y-%m-%dT%H:%M:%S%.f",
            "%Y-%m-%dT%H:%M:%S",
            "%Y-%m-%dT%H:%M",
            "%Y-%m-%dT%H",
            "%Y-%m-%d",
        ] {
            if let Ok(dt) = NaiveDateTime::parse_from_str(candidate, fmt) {
                return Ok(dt);
            }
            if fmt == "%Y-%m-%d" {
                if let Ok(d) = NaiveDate::parse_from_str(candidate, fmt) {
                    return Ok(d.and_time(NaiveTime::MIN));
                }
            }
        }
    }
    let (date_s, time_s) = match unsigned.split_once('T') {
        Some((d, t)) => (d, t),
        None => {
            return parse_date_flexible(unsigned)
                .map(|d| d.and_time(NaiveTime::MIN))
                .ok_or_else(|| Error::ty(format!("cannot parse localdatetime '{s}'")));
        }
    };
    let date = parse_date_flexible(date_s)
        .ok_or_else(|| Error::ty(format!("cannot parse localdatetime '{s}'")))?;
    let time = if time_s.is_empty() {
        NaiveTime::MIN
    } else {
        parse_naive_time(time_s)?
    };
    Ok(date.and_time(time))
}

fn split_offset(s: &str) -> (&str, Option<&str>) {
    if let Some(i) = s.find('Z') {
        return (&s[..i], Some("Z"));
    }
    if let Some(i) = s.rfind('+') {
        if i > 0 {
            return (&s[..i], Some(&s[i..]));
        }
    }
    if let Some(i) = s.rfind('-') {
        if i > 0 && s.as_bytes()[i - 1].is_ascii_digit() {
            if s[..i].contains('T') || s[..i].contains(':') {
                return (&s[..i], Some(&s[i..]));
            }
            let body = &s[..i];
            let off = &s[i + 1..];
            let compact_time = body.chars().all(|c| c.is_ascii_digit() || c == '.');
            let offset_ok = off.contains(':')
                && off.chars().all(|c| c.is_ascii_digit() || c == ':')
                || off.chars().all(|c| c.is_ascii_digit()) && matches!(off.len(), 2 | 4);
            if compact_time && offset_ok {
                return (&s[..i], Some(&s[i..]));
            }
        }
    }
    (s, None)
}

/// A `time` literal's offset. The bracketed-zone rejection lives in
/// [`parse_time_str`], which every `time` literal passes through first, so
/// there is no second copy here.
fn parse_zoned_time(s: &str) -> Result<Temporal> {
    let (body, off) = split_offset(s);
    let time = parse_naive_time(body)?;
    let offset = off.map(parse_offset).transpose()?.unwrap_or(0);
    Ok(Temporal::Time { time, offset })
}

/// The one spelling of a named zone contradicted by a supplied offset.
fn zone_offset_mismatch(zone: &str) -> Error {
    Error::ty(format!("Timezone and offset do not match: {zone}"))
}

/// A supplied offset must name the zone's own offset at that wall clock; a
/// contradicting offset is rejected, never silently replaced by the zone's.
/// The one check every non-overlap zone arm shares.
fn check_matching_offset(zone: &str, want: Option<i32>, actual: i32) -> Result<()> {
    if want.is_some_and(|want| want != actual) {
        return Err(zone_offset_mismatch(zone));
    }
    Ok(())
}

fn parse_zoned_dt(s: &str) -> Result<Temporal> {
    let (body, zone_part) = if let Some(i) = s.find('[') {
        (&s[..i], Some(s[i + 1..].trim_end_matches(']')))
    } else {
        let (b, o) = split_offset(s);
        (b, o)
    };
    // An offset written beside a bracketed zone disambiguates the wall clock
    // inside a DST overlap: `datetime('2020-10-25T02:30+01:00[Europe/Stockholm]')`
    // is the post-transition 02:30, not an error.
    let (body, body_offset) = split_offset(body);
    let local = parse_naive_dt(body)?;
    if let Some(z) = zone_part {
        let want = body_offset.map(parse_offset).transpose()?;
        // Every arm answers the same three facts — the (possibly
        // gap-shifted) wall clock, its offset, and the zone spelling to
        // keep — and the value is built once.
        let (local, offset, zone) = match zone_kind(z) {
            ZoneSpec::Iana(_) | ZoneSpec::Other(_) => {
                let (local, offset) = match wall_clock(z, local)? {
                    // A supplied offset must name the zone's own offset at
                    // that wall clock; a contradicting offset is rejected,
                    // never silently replaced by the zone's offset.
                    WallClock::Unique { offset } => {
                        check_matching_offset(z, want, offset)?;
                        (local, offset)
                    }
                    WallClock::Ambiguous { earlier, later } => {
                        let offset = match want {
                            Some(want) if want == earlier => earlier,
                            Some(want) if want == later => later,
                            _ => return Err(ambiguous_datetime_err()),
                        };
                        (local, offset)
                    }
                    // An explicit offset cannot name a wall clock that never
                    // occurs; without one, Neo4j shifts the clock forward by
                    // the gap (ADR-0178's gap rule refined).
                    WallClock::Gap { .. } if want.is_some() => return Err(ambiguous_datetime_err()),
                    WallClock::Gap { .. } => resolve_wall_forward(z, local, None)?,
                };
                (local, offset, Some(z.to_string()))
            }
            ZoneSpec::Utc => {
                check_matching_offset(z, want, 0)?;
                (local, 0, Some("Z".into()))
            }
            // The classifier already split the two spellings: a numeric
            // offset stays offset-only, a `GMT`/`UTC` prefixed one keeps its
            // spelling as the value's zone.
            ZoneSpec::Fixed(_) => {
                let offset = parse_offset(z)?;
                check_matching_offset(z, want, offset)?;
                (local, offset, None)
            }
            ZoneSpec::Prefixed(_) => {
                let offset = parse_offset(z)?;
                check_matching_offset(z, want, offset)?;
                (local, offset, Some(z.to_string()))
            }
        };
        return Ok(Temporal::DateTime {
            utc: sub_offset(local, offset)?,
            offset,
            zone,
        });
    }
    Ok(Temporal::DateTime {
        utc: local,
        offset: 0,
        zone: Some("Z".into()),
    })
}
