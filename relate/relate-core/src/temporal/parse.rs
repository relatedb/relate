//! Parsing of temporal values: ISO-8601 forms, CIP-228 patterns, and durations.

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Weekday};

use crate::{Error, Result, Value};

use super::clock::ClockKind;
use super::construct::date_from_week;
use super::duration::parse_duration_iso;
use super::duration_pattern::parse_duration_pattern;
use super::field::first_month_of_quarter;
use super::pattern::{invalid_value, parse_instant_pattern};
use super::zone::{
    parse_offset, resolve_wall_forward, sub_offset, zone_kind, zone_offset_mismatch, OverlapPolicy,
    WrittenZone,
};
use super::{decimal_comma_as_dot, range_err, weekday_from_i, CivilZonedFields, Temporal};

pub fn parse_iso(kind: &str, s: &str) -> Result<Temporal> {
    // The one literal-entry trim: the offset scans and the form parsers
    // all read the trimmed text (the leaf parsers' own trims are
    // idempotent).
    let s = s.trim();
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

/// Whether a `-` split is a compact time's offset rather than part of its
/// fraction: the body is a digits-and-separator run under
/// [`decimal_comma_as_dot`], and the tail is a non-empty offset by the one
/// grammar [`zone::parse_offset`] owns — so the offset spelling set has
/// exactly one owner, in prose as well as in code.
fn compact_time_offset(body: &str, off: &str) -> bool {
    // The offset half reads the one grammar `zone::parse_offset` owns —
    // a second offset spelling here could disagree with the parser that
    // consumes the split (an empty tail is not an offset: the grammar
    // reads it as zero, this predicate must not).
    decimal_comma_as_dot(body)
        .chars()
        .all(|c| c.is_ascii_digit() || c == '.')
        && !off.is_empty()
        && parse_offset(off).is_ok()
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

/// Whether `year` is inside chrono's `NaiveDate` range — the one statement
/// of the chrono/civil representation split, consulted by the date parse,
/// the far-year body parse, and the named-zone rejection's bounds.
fn in_chrono_year_range(year: i64) -> bool {
    (i64::from(NaiveDate::MIN.year())..=i64::from(NaiveDate::MAX.year())).contains(&year)
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

/// The ISO quarter date form `YYYYQn` — the first day of that quarter,
/// as a built date (the fall-through family's contract, like
/// [`parse_week_date`] beside it). Live-verified contract (2026.10.1): a
/// leading `+` is optional (the caller strips it), the year is exactly
/// four unsigned digits, `Q` is uppercase and the quarter 1-4 — lowercase
/// `q`, short years and `-` signs are rejections, never quarter
/// spellings. The month is [`first_month_of_quarter`], the field
/// module's one derivation of the quarter→month relation.
fn quarter_date(t: &str) -> Option<NaiveDate> {
    let b: &[u8; 6] = t.as_bytes().try_into().ok()?;
    match *b {
        // The guard proves the four year digits, so the year reads
        // straight off them — no second (infallible) parse to keep in
        // step with the pattern.
        [a, b_, c, d, b'Q', q @ b'1'..=b'4'] if [a, b_, c, d].iter().all(u8::is_ascii_digit) => {
            let year = i32::from(a - b'0') * 1000
                + i32::from(b_ - b'0') * 100
                + i32::from(c - b'0') * 10
                + i32::from(d - b'0');
            let month = first_month_of_quarter(u32::from(q - b'0'))
                .expect("the byte pattern bounds the quarter to 1..=4");
            NaiveDate::from_ymd_opt(year, month, 1)
        }
        _ => None,
    }
}

fn parse_date_str(s: &str) -> Result<Temporal> {
    if let Some((year, month, day)) = parse_civil_ymd(s) {
        // One calendar check per branch: inside chrono's range
        // `NaiveDate::from_ymd_opt` is the calendar, and outside it
        // `is_civil_date` is (the check chrono cannot make for an i64 year).
        // The split only chooses the representation.
        if in_chrono_year_range(year) {
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

/// The date-part spellings `parse_date_flexible` accepts, in try order:
/// the quarter form, the dashed and compact ISO calendars, the year-month,
/// compact year-month and year-alone completions, then the week and
/// ordinal forms. One entry per spelling — adding one is a row, not a
/// branch in the chain.
const DATE_FORMS: &[fn(&str) -> Option<NaiveDate>] = &[
    quarter_date,
    |t| NaiveDate::parse_from_str(t, "%Y-%m-%d").ok(),
    |t| NaiveDate::parse_from_str(t, "%Y%m%d").ok(),
    |t| {
        (t.len() == 7 && t.as_bytes().get(4) == Some(&b'-'))
            .then(|| NaiveDate::from_ymd_opt(t[..4].parse().ok()?, t[5..].parse().ok()?, 1))
            .flatten()
    },
    |t| {
        (t.len() == 6 && t.chars().all(|c| c.is_ascii_digit()))
            .then(|| NaiveDate::parse_from_str(&format!("{t}01"), "%Y%m%d").ok())
            .flatten()
    },
    |t| {
        (t.len() == 4 && t.chars().all(|c| c.is_ascii_digit()))
            .then(|| NaiveDate::from_ymd_opt(t.parse().ok()?, 1, 1))
            .flatten()
    },
    parse_week_date,
    parse_ordinal_date,
];

fn parse_date_flexible(s: &str) -> Option<NaiveDate> {
    let t = s.trim().trim_start_matches('+');
    DATE_FORMS.iter().find_map(|form| form(t))
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
    if let Some((year, month, day, time)) = parse_civil_body(body) {
        return Ok(Temporal::CivilDateTime {
            year,
            month,
            day,
            time,
        });
    }
    Err(Error::ty(format!("cannot parse localdatetime '{s}'")))
}

fn parse_naive_time(input: &str) -> Result<NaiveTime> {
    // The decimal-separator policy at the time-of-day funnel; the parsed
    // text carries the normalized separator while a rejection quotes the
    // caller's own spelling. The literal-entry trim is [`parse_iso`]'s —
    // every caller here already carries trimmed text.
    let s = decimal_comma_as_dot(input);
    for fmt in ["%H:%M:%S%.f", "%H:%M:%S", "%H:%M", "%H%M%S", "%H%M"] {
        if let Ok(t) = NaiveTime::parse_from_str(&s, fmt) {
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
    Err(Error::ty(format!("cannot parse time '{input}'")))
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
    // Only `T` separates the date from the time (ISO 8601, as Neo4j reads
    // it): a space-separated text is a rejection, never a silent rewrite.
    let (body, _) = split_offset(s);
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
        ] {
            if let Ok(dt) = NaiveDateTime::parse_from_str(candidate, fmt) {
                return Ok(dt);
            }
        }
        if let Ok(d) = NaiveDate::parse_from_str(candidate, "%Y-%m-%d") {
            return Ok(d.and_time(NaiveTime::MIN));
        }
    }
    // A text with no `T` is a date alone; an empty time part is midnight.
    let (date_s, time_s) = unsigned.split_once('T').unwrap_or((unsigned, ""));
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
            if compact_time_offset(&s[..i], &s[i + 1..]) {
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
    // The chrono parse stays first so its own rejections — a malformed
    // time, an unparsable date — answer exactly as before; the civil
    // fallback (ADR-0683) only claims a body whose year is outside
    // chrono's range, and only an offset or `Z` zone spec can complete it.
    let local = match parse_naive_dt(body) {
        Ok(dt) => dt,
        Err(chrono_err) => {
            return match parse_civil_body(body) {
                Some(wall) => civil_zoned_dt(s, zone_part, body_offset, wall),
                None => Err(chrono_err),
            }
        }
    };
    if let Some(z) = zone_part {
        let want = body_offset.map(parse_offset).transpose()?;
        // Every arm answers the same three facts — the (possibly
        // gap-shifted) wall clock, its offset, and the zone spelling to
        // keep — and the value is built once.
        let spec = zone_kind(z);
        let (local, offset, zone) = match spec.written() {
            // A supplied offset must name the zone's own occurrence of
            // the wall clock (never silently replaced), an overlap needs
            // one, and a gap shifts forward only when none was written.
            None => {
                let policy = want.map_or(OverlapPolicy::Reject, OverlapPolicy::Require);
                let (local, offset) = resolve_wall_forward(z, local, policy)?;
                (local, offset, Some(z.to_string()))
            }
            Some(written) => {
                let (offset, zone) = resolve_written_zone(written, z, want)?;
                (local, offset, zone)
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

/// The far-year civil body of a zoned literal (ADR-0683): a wall clock
/// whose year is outside chrono's range, or `None` — an in-range body
/// never reaches the caller (the chrono parse took it), and a malformed
/// one stays with the chrono parse's own error.
/// The far-year civil wall clock one `Y-M-D` body names — the one body
/// parse the localdatetime and zoned-literal paths share (ADR-0683):
/// `None` unless the year is outside chrono's range (an in-range body
/// reached a civil fallback only because the chrono parse rejected it,
/// and its rejection stands), an absent or empty time part is midnight,
/// and a present malformed time declines — never a midnight substitution.
fn parse_civil_body(body: &str) -> Option<(i64, u32, u32, NaiveTime)> {
    // A date-alone body is midnight, the chrono parse's own rule.
    let (date_s, time_s) = body.split_once('T').unwrap_or((body, ""));
    let (year, month, day) = parse_civil_ymd(date_s)?;
    if in_chrono_year_range(year) {
        return None;
    }
    // The calendar check `parse_date_str` applies to its civil branch: an
    // impossible far-year date declines, so the chrono parse's `invalid
    // date` rejection stands instead of a stored impossible value.
    if !is_civil_date(year, month, day) {
        return None;
    }
    let time = if time_s.is_empty() {
        NaiveTime::MIN
    } else {
        parse_naive_time(time_s).ok()?
    };
    Some((year, month, day, time))
}

/// Complete a far-year zoned datetime (ADR-0683): the civil wall clock
/// plus an offset or `Z` zone spec — the two spellings whose completion
/// never runs a zone-calendar resolution. A named zone at a far year keeps
/// the rejection; the resolver is chrono-bound there.
fn civil_zoned_dt(
    s: &str,
    zone_part: Option<&str>,
    body_offset: Option<&str>,
    wall: (i64, u32, u32, NaiveTime),
) -> Result<Temporal> {
    let (year, month, day, time) = wall;
    let want = body_offset.map(parse_offset).transpose()?;
    let (offset, zone) = match zone_part {
        // No offset and no bracket: the UTC reading, as the chrono path.
        None => (0, Some("Z".to_string())),
        Some(z) => match zone_kind(z).written() {
            Some(written) => resolve_written_zone(written, z, want)?,
            None => {
                return Err(Error::ty(format!(
                    "cannot parse datetime '{s}': a named time zone needs a year within \
                     {}..={}",
                    NaiveDate::MIN.year(),
                    NaiveDate::MAX.year()
                )))
            }
        },
    };
    Ok(Temporal::CivilZoned(Box::new(CivilZonedFields {
        year,
        month,
        day,
        time,
        offset,
        zone,
    })))
}

/// The `(offset, zone spelling)` a written-offset zone spec completes to
/// — the one policy the chrono path and the far-year civil fallback
/// share: `Z` and a numeric offset stay offset-only, a `GMT`/`UTC`
/// prefixed one keeps its spelling as the value's zone, and a supplied
/// offset must match the zone's own (never silently replaced).
fn resolve_written_zone(
    written: WrittenZone,
    z: &str,
    want: Option<i32>,
) -> Result<(i32, Option<String>)> {
    match written {
        WrittenZone::Utc => {
            check_matching_offset(z, want, 0)?;
            Ok((0, Some("Z".to_string())))
        }
        WrittenZone::Fixed | WrittenZone::Prefixed => {
            let offset = parse_offset(z)?;
            check_matching_offset(z, want, offset)?;
            let zone = matches!(written, WrittenZone::Prefixed).then(|| z.to_string());
            Ok((offset, zone))
        }
    }
}
