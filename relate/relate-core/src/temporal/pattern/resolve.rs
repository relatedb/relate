//! Instant-pattern resolution: the collected [`Fields`] to a date or a
//! time of day, with CIP-228's cross-field rules.

use chrono::{Datelike, Duration as ChronoDur, NaiveDate, NaiveTime, Offset, Weekday};

use crate::Result;

use super::super::clock::zoned;
use super::super::construct::date_from_week;
use super::super::zone::{ambiguous_datetime_err, resolve_local, sub_offset, ResolvedZone};
use super::super::Temporal;
use super::super::{
    range_err, NANOS_PER_DAY, NANOS_PER_HOUR, NANOS_PER_MILLI, NANOS_PER_MINUTE, NANOS_PER_SECOND,
};
use super::fields::Fields;
use super::{as_u32, invalid_value};

/// The localized day-of-week (`e`/`c`): Sunday is 1, Saturday 7 — the
/// number-to-weekday mapping `format.rs` renders with
/// `num_days_from_sunday() + 1`.
pub(super) fn weekday_from_localized(value: u32) -> Result<Weekday> {
    Ok(match value {
        1 => Weekday::Sun,
        2 => Weekday::Mon,
        3 => Weekday::Tue,
        4 => Weekday::Wed,
        5 => Weekday::Thu,
        6 => Weekday::Fri,
        7 => Weekday::Sat,
        _ => return Err(invalid_value("day-of-week out of range")),
    })
}
/// The one year narrowing: a pattern year (`i64`) to chrono's `i32`, a
/// range error beyond it.
fn year_i32(year: i64) -> Result<i32> {
    i32::try_from(year).map_err(|_| range_err())
}

/// The one civil-date constructor: narrows the year through [`year_i32`]
/// and maps an impossible date to `invalid date`.
fn ymd(year: i64, month: u32, day: u32) -> Result<NaiveDate> {
    let year = year_i32(year)?;
    NaiveDate::from_ymd_opt(year, month, day).ok_or_else(|| invalid_value("invalid date"))
}

pub(super) fn resolve_date(fields: &Fields) -> Result<NaiveDate> {
    if let Some(mjd) = fields.modified_julian {
        let epoch = NaiveDate::from_ymd_opt(1858, 11, 17).ok_or_else(range_err)?;
        // `ChronoDur::days` panics past chrono's duration bound; an mjd that
        // parses as `i64` is query input, so it must be a typed range error.
        let days = ChronoDur::try_days(mjd).ok_or_else(range_err)?;
        return epoch.checked_add_signed(days).ok_or_else(range_err);
    }
    if let (Some(year), Some(week), Some(day)) = (fields.week_year, fields.week, fields.day_of_week)
    {
        let year = year_i32(year)?;
        return date_from_week(year, week, weekday_from_localized(day)?);
    }
    let year = fields.proleptic_year();
    if let (Some(year), Some(ordinal)) = (year, fields.day_of_year) {
        let date = NaiveDate::from_yo_opt(year_i32(year)?, ordinal)
            .ok_or_else(|| invalid_value("invalid date"))?;
        check_day_of_week(fields, date)?;
        return Ok(date);
    }
    if let (Some(year), Some(month), Some(aligned), Some(day)) = (
        year,
        fields.month,
        fields.aligned_day_of_week,
        fields.day_of_week,
    ) {
        let first = ymd(year, month, 1)?;
        let weekday = weekday_from_localized(day)?;
        let offset = i64::from(weekday.num_days_from_monday())
            - i64::from(first.weekday().num_days_from_monday());
        let day_of_month = 1 + offset.rem_euclid(7) + (i64::from(aligned) - 1) * 7;
        let day_of_month = u32::try_from(day_of_month).map_err(|_| range_err())?;
        return ymd(year, month, day_of_month);
    }
    if let (Some(year), Some(month), Some(day)) = (year, fields.month, fields.day) {
        let date = ymd(year, month, day)?;
        check_day_of_week(fields, date)?;
        return Ok(date);
    }
    if let (Some(year), Some(quarter)) = (year, fields.quarter) {
        if !(1..=4).contains(&quarter) {
            return Err(invalid_value("quarter out of range"));
        }
        return ymd(year, (quarter - 1) * 3 + 1, 1);
    }
    Err(invalid_value("incomplete date"))
}

fn check_day_of_week(fields: &Fields, date: NaiveDate) -> Result<()> {
    if let Some(day) = fields.day_of_week {
        if weekday_from_localized(day)? != date.weekday() {
            return Err(invalid_value("conflicting day of week"));
        }
    }
    if let Some(weekday) = fields.day_of_week_name {
        if weekday != date.weekday() {
            return Err(invalid_value("conflicting day of week"));
        }
    }
    Ok(())
}

/// The one range guard for a time component: `value` within `0..=max`, or
/// the `<what> out of range` error.
fn bounded(value: u32, max: u32, what: &str) -> Result<u32> {
    if value > max {
        return Err(invalid_value(format!("{what} out of range")));
    }
    Ok(value)
}

pub(super) fn resolve_time(fields: &Fields) -> Result<NaiveTime> {
    let mut hour: i64 = 0;
    let mut minute: i64 = 0;
    let mut second: i64 = 0;
    let mut nano: i64 = 0;
    // The two day-relative spellings name one nanosecond-of-day total, so
    // it is decomposed once; each spelling keeps its own name in the range
    // error. The unit constants are `i128` (the duration arithmetic's
    // width) and fit `i64` here: a day is 8.6e13 ns against 9.2e18.
    let of_day = if let Some(total) = fields.nano_of_day {
        if !(0..NANOS_PER_DAY).contains(&i128::from(total)) {
            return Err(invalid_value("nano-of-day out of range"));
        }
        Some(i128::from(total))
    } else if let Some(millis) = fields.milli_of_day {
        if !(0..NANOS_PER_DAY / NANOS_PER_MILLI).contains(&i128::from(millis)) {
            return Err(invalid_value("milli-of-day out of range"));
        }
        Some(i128::from(millis) * NANOS_PER_MILLI)
    } else {
        None
    };
    if let Some(total) = of_day {
        let total = i64::try_from(total).map_err(|_| range_err())?;
        let (hour_nanos, minute_nanos, second_nanos) = (
            NANOS_PER_HOUR as i64,
            NANOS_PER_MINUTE as i64,
            NANOS_PER_SECOND as i64,
        );
        hour = total / hour_nanos;
        minute = total % hour_nanos / minute_nanos;
        second = total % minute_nanos / second_nanos;
        nano = total % second_nanos;
    }
    if let Some(value) = fields.hour_of_day {
        hour = i64::from(bounded(value, 23, "hour")?);
    }
    if let Some(value) = fields.clock_hour {
        hour = i64::from(bounded(if value == 24 { 0 } else { value }, 23, "hour")?);
    }
    if let Some(value) = fields.hour_of_am_pm {
        let value = bounded(value, 12, "hour")?;
        // A 12-hour clock needs its AM/PM marker: Java's resolver cannot
        // build a time from hour-of-am-pm alone, and the mismatch is the
        // pattern's, not a silent AM.
        let pm = fields
            .am_pm
            .ok_or_else(|| invalid_value("hour-of-am-pm without AM/PM marker"))?;
        hour = i64::from(value % 12) + if pm { 12 } else { 0 };
    }
    if let Some(value) = fields.minute {
        minute = i64::from(bounded(value, 59, "minute")?);
    }
    if let Some(value) = fields.second {
        second = i64::from(bounded(value, 59, "second")?);
    }
    if let Some(value) = fields.nano {
        nano = i64::from(bounded(value, 999_999_999, "nanosecond")?);
    }
    NaiveTime::from_hms_nano_opt(
        as_u32(hour)?,
        as_u32(minute)?,
        as_u32(second)?,
        as_u32(nano)?,
    )
    .ok_or_else(|| invalid_value("invalid time"))
}

/// `datetime(value, pattern)`: a parsed zone resolves the wall clock under
/// ADR-0178's overlap rule; otherwise the offset (or none, meaning `Z`)
/// places it, and a date-only pattern is midnight UTC.
pub(super) fn resolve_datetime(fields: &Fields) -> Result<Temporal> {
    let date = resolve_date(fields)?;
    let time = resolve_time(fields)?;
    let local = date.and_time(time);
    if let Some(tz) = fields.zone {
        let resolved = resolve_local(tz, local)
            .earliest()
            .ok_or_else(ambiguous_datetime_err)?;
        return Ok(zoned(
            resolved.naive_utc(),
            ResolvedZone {
                offset: resolved.offset().fix().local_minus_utc(),
                zone: Some(tz.name().to_string()),
            },
        ));
    }
    // A date-only pattern: a parsed offset does not apply (CIP-228
    // instant semantics), so the value is midnight UTC.
    let offset = fields.offset.filter(|_| fields.has_time());
    Ok(zoned(
        sub_offset(local, offset.unwrap_or(0))?,
        ResolvedZone {
            offset: offset.unwrap_or(0),
            zone: offset.is_none().then(|| "Z".into()),
        },
    ))
}
