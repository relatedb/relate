//! Temporal arithmetic: duration add/subtract and scaling, plus the civil-day
//! count pair. The difference family (`duration.between`) lives in
//! [`super::between`].
//!
//! Every computation that touches a user-controlled magnitude (duration
//! components, civil-date years, offsets) goes through checked arithmetic or
//! chrono's `try_*` / `checked_*` constructors and returns
//! `Err(Error::ty(..))` on out-of-range results. Nothing in this module may
//! panic on query input.

use chrono::{Datelike, Duration as ChronoDur, NaiveDate, NaiveDateTime, NaiveTime};

use crate::{Error, Result};

use super::exact::DurationTiers;
use super::zone::{add_offset, sub_offset};
use super::{nanos_to_time, range_err, time_to_nanos, Temporal, NANOS_PER_DAY};

/// Days from 1970-01-01 to `year-month-day` in the proleptic Gregorian
/// calendar (days-from-civil over i128, so civil years — i64, parsed
/// unbounded up to ±i64::MAX — cannot overflow). The one civil-day count:
/// civil-date arithmetic here and the Bolt Date struct (ADR-0325) use it.
pub fn civil_epoch_days(year: i64, month: u32, day: u32) -> i128 {
    let mut y = i128::from(year);
    let mut m = i128::from(month);
    if m <= 2 {
        y -= 1;
        m += 9;
    } else {
        m -= 3;
    }
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + i128::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The inverse of [`civil_epoch_days`]: the proleptic-Gregorian
/// `year-month-day` that is `days` days from 1970-01-01 (civil-from-days
/// over i128, matching the forward direction's unbounded civil years).
/// The one inverse: changelog date rendering and any other days→date
/// consumer consults it instead of re-deriving the algorithm.
pub fn civil_from_days(days: i128) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = era * 400 + yoe;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y } as i64, m, d)
}

/// `dt + d`, erroring instead of panicking when the result is out of chrono
/// range or the duration itself is out of `TimeDelta` bounds.
pub(super) fn checked_add_dur(dt: NaiveDateTime, d: Option<ChronoDur>) -> Result<NaiveDateTime> {
    let d = d.ok_or_else(range_err)?;
    dt.checked_add_signed(d).ok_or_else(range_err)
}

pub(super) fn add_months(dt: NaiveDateTime, months: i64) -> Result<NaiveDateTime> {
    // Total month index over the proleptic calendar, then one euclidean split.
    // `months` is user-controlled, so the sum is checked: an i64 overflow here
    // would otherwise reach the range check only through a wrap.
    let total = (i64::from(dt.year()) * 12 + (i64::from(dt.month()) - 1))
        .checked_add(months)
        .ok_or_else(range_err)?;
    let year = i32::try_from(total.div_euclid(12)).map_err(|_| range_err())?;
    let month = u32::try_from(total.rem_euclid(12) + 1).map_err(|_| range_err())?;
    let last = last_day(year, month);
    let day = dt.day().min(last);
    let date = NaiveDate::from_ymd_opt(year, month, day).ok_or_else(range_err)?;
    Ok(date.and_time(dt.time()))
}

fn last_day(year: i32, month: u32) -> u32 {
    NaiveDate::from_ymd_opt(year, month, 1)
        .and_then(|d| {
            let next = if month == 12 {
                year.checked_add(1)
                    .and_then(|y| NaiveDate::from_ymd_opt(y, 1, 1))
            } else {
                NaiveDate::from_ymd_opt(year, month + 1, 1)
            };
            next.map(|n| (n - d).num_days() as u32)
        })
        .unwrap_or(28)
}

/// Shift a time of day by `nanos`, wrapping within the day. `rem_euclid`
/// bounds the result to `[0, NANOS_PER_DAY)`, so it is a within-day nanosecond
/// count by construction and the `i64` conversion cannot fail.
fn shift_time_of_day(tm: NaiveTime, nanos: i128) -> NaiveTime {
    let within_day = (i128::from(time_to_nanos(tm)) + nanos).rem_euclid(NANOS_PER_DAY);
    nanos_to_time(i64::try_from(within_day).expect("rem_euclid bounds a within-day nanos count"))
}

/// The checked negation of a duration's three fields: the one spelling of
/// `-months / -days / -nanos`, shared by subtraction's additive twin and the
/// unary-minus entry point.
pub(super) fn negate_fields(months: i64, days: i64, nanos: i128) -> Result<(i64, i64, i128)> {
    Ok((
        months.checked_neg().ok_or_else(range_err)?,
        days.checked_neg().ok_or_else(range_err)?,
        nanos.checked_neg().ok_or_else(range_err)?,
    ))
}

/// The one duration-extraction boundary: the `(months, days, nanos)`
/// triple of a duration operand, with the family's single type error
/// ("expected duration") — negate/add/sub/scale all route through here,
/// so a fifth duration operation cannot grow a second spelling.
fn duration_fields(t: &Temporal) -> Result<(i64, i64, i128)> {
    let Temporal::Duration {
        months,
        days,
        nanos,
    } = t
    else {
        return Err(Error::ty("expected duration"));
    };
    Ok((*months, *days, *nanos))
}

/// The checked negation of a duration, through the one assembly seam:
/// negating a value at the inclusive bound can step one past it
/// (`-duration({seconds: 9223372036854775807, nanoseconds: 1000000001})`
/// floors one second further in `duration_checked`'s `div_euclid`), so the
/// negation is Neo4j's interval-field overflow, not a duration past the
/// bound. The unary-minus arm routes through here.
pub fn negate_duration(t: &Temporal) -> Result<Temporal> {
    let (months, days, nanos) = duration_fields(t)?;
    let (months, days, nanos) = negate_fields(months, days, nanos)?;
    // Exact arithmetic: the strict stored-model bound applies (negating
    // `i64::MIN` seconds reaches the unrepresentable `2^63`).
    Temporal::duration_checked_exact(months, days, nanos)
}

/// Apply a duration's three fields to a civil datetime — the
/// months→days→nanos tail the `LocalDateTime` arm runs directly and the
/// zoned `DateTime` arm runs between its offset shifts, so the two arms
/// cannot drift apart in order or checking.
fn add_months_days_nanos(
    dt: NaiveDateTime,
    months: i64,
    days: i64,
    nanos: i128,
) -> Result<NaiveDateTime> {
    let dt = add_months(dt, months)?;
    let dt = checked_add_dur(dt, ChronoDur::try_days(days))?;
    let nanos64 = i64::try_from(nanos).map_err(|_| range_err())?;
    checked_add_dur(dt, Some(ChronoDur::nanoseconds(nanos64)))
}

pub fn add_duration(t: &Temporal, d: &Temporal) -> Result<Temporal> {
    let (months, days, nanos) = duration_fields(d)?;
    match t {
        Temporal::Duration {
            months: m,
            days: da,
            nanos: n,
        } => {
            let months = m.checked_add(months).ok_or_else(range_err)?;
            let days = da.checked_add(days).ok_or_else(range_err)?;
            let nanos = n.checked_add(nanos).ok_or_else(range_err)?;
            // The one assembly seam applies the representability bound, so a
            // sum past it is the same overflow the constructors report. The
            // sum is exact, so the strict stored-model bound applies too.
            Temporal::duration_checked_exact(months, days, nanos)
        }
        Temporal::Date(date) => {
            // A date has no time of day, so the sub-day nanosecond tail is
            // dropped, not added: truncating toward zero keeps a negative
            // component smaller than one day from stealing an extra calendar
            // day. The i128 day count may exceed i64 for extreme durations;
            // error honestly instead of wrapping through `as`.
            let extra_days = i64::try_from(nanos / NANOS_PER_DAY).map_err(|_| range_err())?;
            let days_total = days.checked_add(extra_days).ok_or_else(range_err)?;
            let dt = add_months_days_nanos(date.and_time(NaiveTime::MIN), months, days_total, 0)?;
            Ok(Temporal::Date(dt.date()))
        }
        Temporal::LocalDateTime(dt) => Ok(Temporal::LocalDateTime(add_months_days_nanos(
            *dt, months, days, nanos,
        )?)),
        Temporal::DateTime { utc, offset, zone } => {
            let local = add_offset(*utc, *offset)?;
            let local = add_months_days_nanos(local, months, days, nanos)?;
            let utc = sub_offset(local, *offset)?;
            Ok(Temporal::DateTime {
                utc,
                offset: *offset,
                zone: zone.clone(),
            })
        }
        Temporal::LocalTime(tm) => Ok(Temporal::LocalTime(shift_time_of_day(*tm, nanos))),
        Temporal::Time { time, offset } => Ok(Temporal::Time {
            time: shift_time_of_day(*time, nanos),
            offset: *offset,
        }),
        Temporal::CivilDate { .. } | Temporal::CivilDateTime { .. } => {
            Err(Error::ty("civil date arithmetic out of range"))
        }
    }
}

pub fn sub_duration(t: &Temporal, d: &Temporal) -> Result<Temporal> {
    let (months, days, nanos) = {
        let (m, d, n) = duration_fields(d)?;
        negate_fields(m, d, n)?
    };
    add_duration(
        t,
        &Temporal::Duration {
            months,
            days,
            nanos,
        },
    )
}

/// Scale a duration by a number (openCypher `duration * n` / `duration / n`).
pub fn scale_duration(t: &Temporal, factor: f64) -> Result<Temporal> {
    let (months, days, nanos) = duration_fields(t)?;
    if !factor.is_finite() {
        return Err(Error::ty("duration scale factor"));
    }
    // The scaled tiers go through the one fold ([`DurationTiers::finish`]),
    // not a second hand-rolled months→days→nanos cascade — the same
    // approximate chain Neo4j's float `mul`/`div` uses.
    let mut tiers = DurationTiers::default();
    tiers.months.add_float(months as f64 * factor)?;
    tiers.days.add_float(days as f64 * factor)?;
    tiers.nanos.add_float(nanos as f64 * factor)?;
    let (months, days, nanos) = tiers.finish()?;
    Temporal::duration_checked(months, days, nanos)
}
