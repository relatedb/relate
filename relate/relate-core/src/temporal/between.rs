//! Temporal difference (`duration.between` and its per-unit form): classify
//! the operand pair once into a comparison domain, then shape the difference
//! per unit. Split from [`super::arith`] so the additive/scaling family and
//! the between family each own one job.
//!
//! Every computation over a user-controlled magnitude goes through checked
//! arithmetic or chrono's `try_*` / `checked_*` constructors and returns
//! `Err(Error::ty(..))` on out-of-range results. Nothing in this module may
//! panic on query input.

use chrono::{Datelike, Duration as ChronoDur, NaiveDateTime, NaiveTime};

use crate::{Error, Result};

use super::arith::{add_months, checked_add_dur};
use super::zone::{add_offset, sub_offset, zone_offset_at, zone_region};
use super::{civil_epoch_days, range_err, time_to_nanos, utc_time_nanos, Temporal, NANOS_PER_DAY};

fn utc_in_zone(local: NaiveDateTime, zone: &str) -> Option<NaiveDateTime> {
    // The single-or-earliest rule is the exported resolver (ADR-0178); its
    // gap/unknown-zone outcomes collapse to this path's `None`, and the
    // checked subtraction keeps an out-of-range `local - offset` a `None`.
    let offset = zone_offset_at(zone, local).ok()?;
    sub_offset(local, offset).ok()
}

/// `t` read at the zone the pair classified (`zone`), anchored to `ctx`'s
/// wall date when `t` is a bare time of day.
fn as_utc_instant(t: &Temporal, ctx: &Temporal, zone: &str) -> Option<NaiveDateTime> {
    match t {
        Temporal::DateTime { utc, .. } => Some(*utc),
        Temporal::Date(d) => utc_in_zone(d.and_time(NaiveTime::MIN), zone),
        Temporal::LocalDateTime(dt) => utc_in_zone(*dt, zone),
        Temporal::LocalTime(tm) => utc_in_zone(wall_dt(ctx)?.date().and_time(*tm), zone),
        Temporal::Time { time, offset } => {
            let local = wall_dt(ctx)?.date().and_time(*time);
            // checked_neg: a user-parsed offset can be exactly i32::MIN, and
            // that reading stays a typed no-reading error (pinned by
            // `temporal_arith_out_of_range_errors_not_panics`).
            add_offset(local, offset.checked_neg()?).ok()
        }
        Temporal::Duration { .. }
        | Temporal::CivilDate { .. }
        | Temporal::CivilDateTime { .. }
        | Temporal::CivilZoned(_) => None,
    }
}

fn wall_dt(t: &Temporal) -> Option<NaiveDateTime> {
    match t {
        Temporal::Date(d) => Some(d.and_time(NaiveTime::MIN)),
        Temporal::LocalDateTime(dt) => Some(*dt),
        Temporal::DateTime { utc, offset, .. } => add_offset(*utc, *offset).ok(),
        // The civil forms carry no chrono wall clock for the stepper.
        Temporal::LocalTime(_)
        | Temporal::Time { .. }
        | Temporal::CivilDate { .. }
        | Temporal::CivilDateTime { .. }
        | Temporal::CivilZoned(_)
        | Temporal::Duration { .. } => None,
    }
}

fn utc_dt(t: &Temporal) -> Option<NaiveDateTime> {
    match t {
        Temporal::DateTime { utc, .. } => Some(*utc),
        other => wall_dt(other),
    }
}

fn time_nanos_of(t: &Temporal, as_utc: bool) -> Option<i64> {
    match t {
        Temporal::LocalTime(tm) => Some(time_to_nanos(*tm)),
        Temporal::Time { time, offset } => Some(if as_utc {
            utc_time_nanos(*time, *offset)
        } else {
            time_to_nanos(*time)
        }),
        Temporal::Date(_) => Some(0),
        Temporal::LocalDateTime(dt) => Some(time_to_nanos(dt.time())),
        Temporal::DateTime { utc, offset, .. } => {
            if as_utc {
                Some(time_to_nanos(utc.time()))
            } else {
                let local = add_offset(*utc, *offset).ok()?;
                Some(time_to_nanos(local.time()))
            }
        }
        Temporal::Duration { .. } => None,
        Temporal::CivilDate { .. } => Some(0),
        Temporal::CivilDateTime { time, .. } => Some(time_to_nanos(*time)),
        // A far-year zoned datetime carries no chrono wall clock to read
        // the nanos from (ADR-0683); the between stepper treats it as the
        // no-reading the other civil forms give the zoned conversions.
        // The far-year zoned form has no reading for the between stepper:
        // its zoned conversions are chrono-bound (ADR-0683), so it declines
        // whole, the same no-reading the other civil forms give the zoned
        // paths.
        Temporal::CivilZoned(_) => None,
    }
}

/// `t` overshot `db` when stepping away from `da`: the tentative result
/// landed on the far side of `db` in the direction of travel.
fn overshot(da: NaiveDateTime, db: NaiveDateTime, t: NaiveDateTime) -> bool {
    if db >= da {
        t > db
    } else {
        t < db
    }
}

/// The exact i128 nanosecond difference between two in-chrono-range
/// datetimes, read as whole calendar days plus within-day nanos: two chrono
/// datetimes can differ by more than i64 nanoseconds (±262143 years
/// ≈ ±8.8e21 ns), where `TimeDelta::num_nanoseconds` returns `None`.
fn nanos_between(da: NaiveDateTime, db: NaiveDateTime) -> i128 {
    i128::from((db.date() - da.date()).num_days()) * NANOS_PER_DAY
        + i128::from(time_to_nanos(db.time()))
        - i128::from(time_to_nanos(da.time()))
}

/// One difference in the producer's own terms: the calendar decomposition
/// (`months`, then `days`, then sub-day `nanos`) that `Full` and `Months`
/// read, and the whole-span `span_days` / `span_nanos` that `Days` and
/// `Seconds` read. Each producer computes every field once; only
/// [`shaped_duration`] selects, so no producer pre-projects for a unit.
struct BetweenFields {
    months: i64,
    /// Producer-bounded: a within-month adjusted day count
    /// ([`calendar_between`]) or a day-of-month difference ([`civil_fields`]),
    /// so it fits `i64` by construction. The unbounded magnitude is
    /// `span_days`, range-checked only when the `Days` unit keeps it.
    days: i64,
    nanos: i128,
    span_days: i128,
    span_nanos: i128,
}

impl BetweenFields {
    /// A difference that is entirely sub-day nanoseconds (the time-of-day
    /// domain): no months, no whole days.
    fn from_nanos(nanos: i128) -> Self {
        Self {
            months: 0,
            days: 0,
            nanos,
            span_days: 0,
            span_nanos: nanos,
        }
    }
}

/// Shape a [`BetweenFields`] per `unit`: `Full` keeps the calendar
/// decomposition, `Months` the month count, `Days` the whole-span day count,
/// `Seconds` the whole-span nanosecond count. The one place the per-unit
/// `Duration` shaping lives; the assembly goes through
/// [`Temporal::duration_checked`], so a between result cannot exceed the
/// duration representability bound either.
fn shaped_duration(fields: BetweenFields, unit: BetweenUnit) -> Result<Temporal> {
    // `span_days` is the unbounded magnitude (a civil span over unbounded
    // i64 years), range-checked against the day bound only when the `Days`
    // unit keeps it.
    let (months, days, nanos) = match unit {
        BetweenUnit::Full => (fields.months, fields.days, fields.nanos),
        BetweenUnit::Months => (fields.months, 0, 0),
        BetweenUnit::Days => (
            0,
            i64::try_from(fields.span_days).map_err(|_| range_err())?,
            0,
        ),
        BetweenUnit::Seconds => (0, 0, fields.span_nanos),
    };
    Temporal::duration_checked(months, days, nanos)
}

/// The calendar difference between two in-chrono-range datetimes: months and
/// days adjusted so the remainder is under one month and one day
/// respectively, the sub-day remainder, and the whole-span day and
/// nanosecond counts.
fn calendar_between(da: NaiveDateTime, db: NaiveDateTime) -> Result<BetweenFields> {
    // Chrono-range years are i32-bounded, so this difference cannot overflow.
    let months =
        i64::from(db.year() - da.year()) * 12 + i64::from(db.month()) - i64::from(da.month());
    let step = if db >= da { 1 } else { -1 };
    let mut adj_months = months;
    let tentative = add_months(da, adj_months)?;
    if overshot(da, db, tentative) {
        adj_months -= step;
    }
    let after_m = add_months(da, adj_months)?;
    let days = (db.date() - after_m.date()).num_days();
    let mut adj_days = days;
    let after_d = checked_add_dur(after_m, ChronoDur::try_days(adj_days))?;
    if overshot(da, db, after_d) {
        adj_days -= step;
    }
    let after_d = checked_add_dur(after_m, ChronoDur::try_days(adj_days))?;
    // `after_d` is within a day of `db`.
    Ok(BetweenFields {
        months: adj_months,
        days: adj_days,
        nanos: nanos_between(after_d, db),
        span_days: i128::from((db - da).num_days()),
        span_nanos: nanos_between(da, db),
    })
}

pub fn duration_between(a: &Temporal, b: &Temporal) -> Result<Temporal> {
    duration_between_unit(a, b, BetweenUnit::Full)
}

#[derive(Clone, Copy)]
pub enum BetweenUnit {
    Full,
    Months,
    Days,
    Seconds,
}

/// A civil value's parts: the unbounded i64 year, month, day, and the
/// within-day nanosecond count, named so call sites cannot swap fields.
#[derive(Clone, Copy)]
struct CivilParts {
    year: i64,
    month: u32,
    day: u32,
    nanos: i64,
}

fn civil_parts(t: &Temporal) -> Option<CivilParts> {
    match t {
        Temporal::CivilDate { year, month, day } => Some(CivilParts {
            year: *year,
            month: *month,
            day: *day,
            nanos: 0,
        }),
        Temporal::CivilDateTime {
            year,
            month,
            day,
            time,
        } => Some(CivilParts {
            year: *year,
            month: *month,
            day: *day,
            nanos: time_to_nanos(*time),
        }),
        Temporal::Date(d) => Some(CivilParts {
            year: i64::from(d.year()),
            month: d.month(),
            day: d.day(),
            nanos: 0,
        }),
        Temporal::LocalDateTime(dt) => Some(CivilParts {
            year: i64::from(dt.year()),
            month: dt.month(),
            day: dt.day(),
            nanos: time_to_nanos(dt.time()),
        }),
        _ => None,
    }
}

/// Whether `t` is a genuinely civil value (unbounded i64 years).
fn is_civil(t: &Temporal) -> bool {
    matches!(
        t,
        Temporal::CivilDate { .. } | Temporal::CivilDateTime { .. }
    )
}

/// The civil parts of both operands when they are date-like and at least one
/// is a genuinely civil value (unbounded i64 years); `None` leaves the pair
/// to the chrono/zone domains below.
fn civil_parts_pair(a: &Temporal, b: &Temporal) -> Option<(CivilParts, CivilParts)> {
    if !is_civil(a) && !is_civil(b) {
        return None;
    }
    Some((civil_parts(a)?, civil_parts(b)?))
}

/// The civil difference in the same shape [`calendar_between`] produces: the
/// day-of-month difference is the `Full` day field (civil values carry no
/// month-relative day breakdown), and the whole-span day count feeds `Days`
/// and `Seconds`.
fn civil_fields(a: CivilParts, b: CivilParts) -> Result<BetweenFields> {
    // Civil years are unbounded i64; compute in i128 and error when the
    // result does not fit the Duration fields.
    let months =
        (i128::from(b.year) - i128::from(a.year)) * 12 + i128::from(b.month) - i128::from(a.month);
    let months = i64::try_from(months).map_err(|_| range_err())?;
    let span_days =
        civil_epoch_days(b.year, b.month, b.day) - civil_epoch_days(a.year, a.month, a.day);
    let nanos = i128::from(b.nanos) - i128::from(a.nanos);
    Ok(BetweenFields {
        months,
        days: i64::from(b.day) - i64::from(a.day),
        nanos,
        span_days,
        // Fits i128 for any i64 year (~5.8e35 ns at ±i64::MAX years).
        span_nanos: span_days * NANOS_PER_DAY + nanos,
    })
}

/// The exact civil difference shaped per `unit`.
fn civil_duration(a: CivilParts, b: CivilParts, unit: BetweenUnit) -> Result<Temporal> {
    shaped_duration(civil_fields(a, b)?, unit)
}

/// The one classification of the operand pair that selects the comparison
/// domain (F-1-3): every predicate is evaluated exactly once here, and
/// `duration_between_unit` matches on the outcome instead of re-deriving it.
enum PairDomain<'a> {
    /// Both operands are date-like and at least one is a genuinely civil
    /// value (unbounded i64 years): the exact i128 difference.
    Civil { a: CivilParts, b: CivilParts },
    /// At least one operand is a time of day: compare within-day nanos, read
    /// at UTC when both operands are zoned (`as_utc`). `both_times` marks a
    /// pair with no date at all, whose month- and day-sized units are
    /// undefined.
    TimeOnly { as_utc: bool, both_times: bool },
    /// Compare at `zone`, the pair's first IANA region, anchored to `ctx`'s
    /// wall date for a bare time of day.
    Zoned { zone: &'a str, ctx: &'a Temporal },
    /// Both operands are zoned without an IANA region: compare UTC instants.
    Instant,
    /// Neither operand is zoned: compare naive wall clocks.
    Wall,
}

/// An operand's comparison shape, classified once per operand so the pair
/// match never re-probes the value.
enum OperandShape<'a> {
    /// A time of day; `zoned` distinguishes `Temporal::Time` (an
    /// offset-carrying time) from `Temporal::LocalTime`.
    Time { zoned: bool },
    /// A `Temporal::DateTime`; `zone` is the IANA region it names, if any.
    DateTime { zone: Option<&'a str> },
    /// A date, local datetime, civil value or duration.
    Other,
}

fn operand_shape(t: &Temporal) -> OperandShape<'_> {
    match t {
        Temporal::LocalTime(_) => OperandShape::Time { zoned: false },
        Temporal::Time { .. } => OperandShape::Time { zoned: true },
        Temporal::DateTime { zone, .. } => OperandShape::DateTime {
            zone: zone_region(zone.as_deref()),
        },
        Temporal::Date(_)
        | Temporal::CivilDate { .. }
        | Temporal::LocalDateTime(_)
        | Temporal::CivilDateTime { .. }
        | Temporal::CivilZoned(_)
        | Temporal::Duration { .. } => OperandShape::Other,
    }
}

fn pair_domain<'a>(a: &'a Temporal, b: &'a Temporal) -> PairDomain<'a> {
    if let Some((a_parts, b_parts)) = civil_parts_pair(a, b) {
        return PairDomain::Civil {
            a: a_parts,
            b: b_parts,
        };
    }
    use OperandShape::*;
    match (operand_shape(a), operand_shape(b)) {
        // Both time-of-day: within-day nanos, read at UTC when both zoned.
        (Time { zoned: za }, Time { zoned: zb }) => PairDomain::TimeOnly {
            as_utc: za && zb,
            both_times: true,
        },
        // A time of day against a non-IANA datetime: the nanos are read at
        // UTC when the time side is `Temporal::Time` (the datetime side is
        // always zoned).
        (Time { zoned }, DateTime { zone: None }) | (DateTime { zone: None }, Time { zoned }) => {
            PairDomain::TimeOnly {
                as_utc: zoned,
                both_times: false,
            }
        }
        // A time of day against a date-like value: never at UTC.
        (Time { .. }, Other) | (Other, Time { .. }) => PairDomain::TimeOnly {
            as_utc: false,
            both_times: false,
        },
        // An IANA-zoned datetime reads the pair at its zone; the first
        // operand wins when both name one.
        (DateTime { zone: Some(zone) }, _) => PairDomain::Zoned { zone, ctx: a },
        (_, DateTime { zone: Some(zone) }) => PairDomain::Zoned { zone, ctx: b },
        // Both sides zoned without an IANA region: compare UTC instants.
        (DateTime { zone: None }, DateTime { zone: None }) => PairDomain::Instant,
        // The remaining pairs hold at most one zoned side: wall clocks.
        (DateTime { zone: None }, Other) | (Other, DateTime { zone: None }) | (Other, Other) => {
            PairDomain::Wall
        }
    }
}

/// Both operands mapped by `f`, or the `duration.between` type error when a
/// value has no reading in the pair's domain. The one map-or-error step: the
/// datetime resolvers and the time-of-day arm share it.
fn map_pair<T>(a: &Temporal, b: &Temporal, f: impl Fn(&Temporal) -> Option<T>) -> Result<(T, T)> {
    let map_one = |t: &Temporal| f(t).ok_or_else(between_err);
    Ok((map_one(a)?, map_one(b)?))
}

/// Error returned when an operand has no reading in the pair's comparison
/// domain (`duration.between`'s typed no-reading signal).
fn between_err() -> Error {
    Error::ty("duration.between")
}

/// Shape the difference between the pair's two resolved datetimes per `unit`.
fn calendar_duration(da: NaiveDateTime, db: NaiveDateTime, unit: BetweenUnit) -> Result<Temporal> {
    shaped_duration(calendar_between(da, db)?, unit)
}

pub fn duration_between_unit(a: &Temporal, b: &Temporal, unit: BetweenUnit) -> Result<Temporal> {
    match pair_domain(a, b) {
        PairDomain::Civil {
            a: a_parts,
            b: b_parts,
        } => civil_duration(a_parts, b_parts, unit),
        PairDomain::TimeOnly { as_utc, both_times } => {
            let (a_nanos, b_nanos) = map_pair(a, b, |t| time_nanos_of(t, as_utc))?;
            // Two times have no date at all, so a month- or day-sized unit
            // is undefined between them; a time against a dated value keeps
            // the zero-month/zero-day reading.
            if both_times && matches!(unit, BetweenUnit::Months | BetweenUnit::Days) {
                return Err(Error::ty("Unsupported unit for a pair of times"));
            }
            shaped_duration(
                BetweenFields::from_nanos(i128::from(b_nanos) - i128::from(a_nanos)),
                unit,
            )
        }
        PairDomain::Zoned { zone, ctx } => {
            let (da, db) = map_pair(a, b, |t| as_utc_instant(t, ctx, zone))?;
            calendar_duration(da, db, unit)
        }
        PairDomain::Instant => {
            let (da, db) = map_pair(a, b, utc_dt)?;
            calendar_duration(da, db, unit)
        }
        PairDomain::Wall => {
            let (da, db) = map_pair(a, b, wall_dt)?;
            calendar_duration(da, db, unit)
        }
    }
}
