//! The temporal component-accessor vocabulary and the one reader per
//! component.
//!
//! Neo4j rejects a component read on a kind that has no such component
//! (`date().hour`, `duration({days: 1}).year`, `date().epochSeconds`) with a
//! typed `TypeError`; the engine answered null. The readers below are the one
//! exposure source: a component is answered when the kind's part readers
//! produce it and rejected otherwise ([`read`]), so no second table can
//! admit a component a reader cannot produce.

use chrono::{Datelike, NaiveDate, NaiveTime, Timelike};

use crate::{Error, Result, Value};

use super::clock::ClockKind;
use super::zone::{add_offset, zone_kind, ZoneSpec};
use super::{
    format_zone_offset, lookup, split_seconds, Temporal, NANOS_PER_MICRO, NANOS_PER_MILLI,
    SECONDS_PER_HOUR, SECONDS_PER_MINUTE,
};

/// One calendar/time component an accessor can read. The variants are the
/// accessor vocabulary, not per-kind: [`component_value`] answers only the
/// kinds that carry the part, and its `None` is the typed rejection's cue.
/// Private to this module: the name table's inverse and the reader are its
/// only users.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Component {
    Year,
    Month,
    Day,
    OrdinalDay,
    Quarter,
    Week,
    WeekYear,
    DayOfWeek,
    DayOfQuarter,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
    Nanosecond,
    Timezone,
    Offset,
    OffsetSeconds,
    OffsetMinutes,
    EpochSeconds,
    EpochMillis,
}

impl Component {
    /// The accessor spellings, lower-case. `weekDay` is Neo4j's alias for
    /// `dayOfWeek`, so both names map to one variant. This is the only name
    /// table; [`Component::from_name`] is its inverse.
    const TABLE: &'static [(&'static str, Component)] = &[
        ("year", Component::Year),
        ("month", Component::Month),
        ("day", Component::Day),
        ("ordinalday", Component::OrdinalDay),
        ("quarter", Component::Quarter),
        ("week", Component::Week),
        ("weekyear", Component::WeekYear),
        ("dayofweek", Component::DayOfWeek),
        ("weekday", Component::DayOfWeek),
        ("dayofquarter", Component::DayOfQuarter),
        ("hour", Component::Hour),
        ("minute", Component::Minute),
        ("second", Component::Second),
        ("millisecond", Component::Millisecond),
        ("microsecond", Component::Microsecond),
        ("nanosecond", Component::Nanosecond),
        ("timezone", Component::Timezone),
        ("offset", Component::Offset),
        ("offsetseconds", Component::OffsetSeconds),
        ("offsetminutes", Component::OffsetMinutes),
        ("epochseconds", Component::EpochSeconds),
        ("epochmillis", Component::EpochMillis),
    ];

    fn from_name(name: &str) -> Option<Self> {
        lookup(Self::TABLE, name)
    }
}

/// One duration component. Neo4j exposes only the plural aggregate
/// spellings (`years`, not `year`); the singular calendar names are not
/// duration components at all.
#[derive(Clone, Copy)]
enum DurationComponent {
    Years,
    Quarters,
    Months,
    Weeks,
    Days,
    Hours,
    Minutes,
    Seconds,
    Millis,
    Micros,
    Nanos,
    MonthsOfYear,
    QuartersOfYear,
    MonthsOfQuarter,
    DaysOfWeek,
    MinutesOfHour,
    SecondsOfMinute,
    MillisOfSecond,
    MicrosOfSecond,
    NanosOfSecond,
}

impl DurationComponent {
    /// The duration accessor spellings, lower-case; the inverse is
    /// [`DurationComponent::from_name`].
    const TABLE: &'static [(&'static str, DurationComponent)] = &[
        ("years", DurationComponent::Years),
        ("quarters", DurationComponent::Quarters),
        ("months", DurationComponent::Months),
        ("weeks", DurationComponent::Weeks),
        ("days", DurationComponent::Days),
        ("hours", DurationComponent::Hours),
        ("minutes", DurationComponent::Minutes),
        ("seconds", DurationComponent::Seconds),
        ("milliseconds", DurationComponent::Millis),
        ("microseconds", DurationComponent::Micros),
        ("nanoseconds", DurationComponent::Nanos),
        ("monthsofyear", DurationComponent::MonthsOfYear),
        ("quartersofyear", DurationComponent::QuartersOfYear),
        ("monthsofquarter", DurationComponent::MonthsOfQuarter),
        ("daysofweek", DurationComponent::DaysOfWeek),
        ("minutesofhour", DurationComponent::MinutesOfHour),
        ("secondsofminute", DurationComponent::SecondsOfMinute),
        ("millisecondsofsecond", DurationComponent::MillisOfSecond),
        ("microsecondsofsecond", DurationComponent::MicrosOfSecond),
        ("nanosecondsofsecond", DurationComponent::NanosOfSecond),
    ];

    fn from_name(name: &str) -> Option<Self> {
        lookup(Self::TABLE, name)
    }
}

/// The clock kind a value reads as, or `None` for a duration and for the
/// proleptic civil forms (which carry no accessor vocabulary).
fn clock_kind(t: &Temporal) -> Option<ClockKind> {
    match t {
        Temporal::Date(_) => Some(ClockKind::Date),
        Temporal::LocalTime(_) => Some(ClockKind::LocalTime),
        Temporal::Time { .. } => Some(ClockKind::Time),
        Temporal::LocalDateTime(_) => Some(ClockKind::LocalDateTime),
        Temporal::DateTime { .. } => Some(ClockKind::DateTime),
        Temporal::CivilDate { .. }
        | Temporal::CivilDateTime { .. }
        | Temporal::CivilZoned(_)
        | Temporal::Duration { .. } => None,
    }
}

/// The component `key` of `t`, or the typed rejection the reference engine
/// raises for a component the value's kind does not carry. The reader's own
/// `None` *is* the exposure policy: there is no second table to disagree
/// with, and an unreadable stored value is the reader's typed range failure
/// rather than an internal error.
pub(super) fn read(t: &Temporal, key: &str) -> Result<Value> {
    let name = key.to_ascii_lowercase();
    if let Temporal::Duration {
        months,
        days,
        nanos,
    } = t
    {
        let component =
            DurationComponent::from_name(&name).ok_or_else(|| no_component("duration", key))?;
        return duration_component(*months, *days, *nanos, component);
    }
    let kind = clock_kind(t).ok_or_else(|| no_component("civil temporal", key))?;
    let component = Component::from_name(&name).ok_or_else(|| no_component(kind.name(), key))?;
    component_value(t, component)?.ok_or_else(|| no_component(kind.name(), key))
}

/// The one rejection spelling for an accessor a kind does not carry.
fn no_component(kind: &str, key: &str) -> Error {
    Error::ty(format!("no such component '{key}' on a {kind} value"))
}

/// The date part of a clock value: its local calendar date. `None` for a
/// kind without a date; an out-of-range `utc + offset` (reachable from a
/// decoded parameter) is the typed range failure, never a silent UTC
/// reading.
fn date_part(t: &Temporal) -> Result<Option<NaiveDate>> {
    Ok(match t {
        Temporal::Date(d) => Some(*d),
        Temporal::LocalDateTime(dt) => Some(dt.date()),
        Temporal::DateTime { utc, offset, .. } => Some(add_offset(*utc, *offset)?.date()),
        _ => None,
    })
}

/// The time-of-day part of a clock value. The error contract is
/// [`date_part`]'s.
fn time_part(t: &Temporal) -> Result<Option<NaiveTime>> {
    Ok(match t {
        Temporal::LocalTime(tm) | Temporal::Time { time: tm, .. } => Some(*tm),
        Temporal::LocalDateTime(dt) => Some(dt.time()),
        Temporal::DateTime { utc, offset, .. } => Some(add_offset(*utc, *offset)?.time()),
        _ => None,
    })
}

/// The UTC offset a zoned clock value carries; `None` for the zoneless
/// kinds.
fn offset_part(t: &Temporal) -> Option<i32> {
    match t {
        Temporal::Time { offset, .. } | Temporal::DateTime { offset, .. } => Some(*offset),
        _ => None,
    }
}

/// The `timezone` rendering: the stored zone spelling when the value names
/// one (an IANA region or a fixed `+HH:MM`/`GMT+HH:MM` spelling), `Z` for
/// UTC, and the offset-only spelling otherwise — the same zero-offset rule
/// the ISO suffix uses.
fn timezone_part(t: &Temporal) -> Option<String> {
    let offset = offset_part(t)?;
    if let Temporal::DateTime {
        zone: Some(zone), ..
    } = t
    {
        return Some(match zone_kind(zone) {
            ZoneSpec::Iana(name) => name.into(),
            // A numeric offset spelling renders through the one zero-offset
            // rule, so `.timezone` and `.offset` agree on `Z`; a
            // `GMT`/`UTC` prefixed spelling keeps its own text
            // (Neo4j-verified: `timezone: 'GMT'` answers `GMT` while
            // `.offset` answers `Z`).
            ZoneSpec::Fixed(_) => format_zone_offset(offset),
            ZoneSpec::Prefixed(name) => name.into(),
            ZoneSpec::Utc => "Z".into(),
            ZoneSpec::Other(_) => format_zone_offset(offset),
        });
    }
    Some(format_zone_offset(offset))
}

/// The one reader: the component's value for `t`, or `None` when the value's
/// kind carries no such part (the caller answers the typed rejection). An
/// unreadable stored value is the part reader's typed range failure.
fn component_value(t: &Temporal, component: Component) -> Result<Option<Value>> {
    // The calendar and clock components read one half each; a kind without
    // that half answers `None`.
    let date_int = |f: fn(NaiveDate) -> i64| -> Result<Option<Value>> {
        Ok(date_part(t)?.map(|d| Value::Int(f(d))))
    };
    let time_int = |f: fn(NaiveTime) -> i64| -> Result<Option<Value>> {
        Ok(time_part(t)?.map(|tm| Value::Int(f(tm))))
    };
    match component {
        Component::Year => date_int(|d| i64::from(d.year())),
        Component::Month => date_int(|d| i64::from(d.month())),
        Component::Day => date_int(|d| i64::from(d.day())),
        Component::OrdinalDay => date_int(|d| i64::from(d.ordinal())),
        Component::Quarter => date_int(|d| i64::from((d.month() - 1) / 3 + 1)),
        Component::Week => date_int(|d| i64::from(d.iso_week().week())),
        Component::WeekYear => date_int(|d| i64::from(d.iso_week().year())),
        Component::DayOfWeek => date_int(|d| i64::from(d.weekday().number_from_monday())),
        Component::DayOfQuarter => {
            let Some(d) = date_part(t)? else {
                return Ok(None);
            };
            let qstart = (d.month() - 1) / 3 * 3 + 1;
            // A quarter's first month is always a valid date; the guard keeps
            // the reader total instead of unwrapping the construction.
            let Some(start) = NaiveDate::from_ymd_opt(d.year(), qstart, 1) else {
                return Ok(None);
            };
            Ok(Some(Value::Int((d - start).num_days() + 1)))
        }
        Component::Hour => time_int(|tm| i64::from(tm.hour())),
        Component::Minute => time_int(|tm| i64::from(tm.minute())),
        Component::Second => time_int(|tm| i64::from(tm.second())),
        Component::Millisecond => time_int(|tm| i64::from(tm.nanosecond() / 1_000_000)),
        Component::Microsecond => time_int(|tm| i64::from(tm.nanosecond() / 1_000)),
        Component::Nanosecond => time_int(|tm| i64::from(tm.nanosecond())),
        Component::Timezone => Ok(timezone_part(t).map(|z| Value::String(z.into()))),
        Component::Offset => {
            Ok(offset_part(t).map(|o| Value::String(format_zone_offset(o).into())))
        }
        Component::OffsetSeconds => Ok(offset_part(t).map(|o| Value::Int(i64::from(o)))),
        Component::OffsetMinutes => Ok(offset_part(t).map(|o| Value::Int(i64::from(o / 60)))),
        Component::EpochSeconds => Ok(epoch_part(t).map(|(seconds, _)| Value::Int(seconds))),
        Component::EpochMillis => Ok(epoch_part(t).map(|(_, millis)| Value::Int(millis))),
    }
}

/// `(epoch seconds, epoch milliseconds)` of a zoned value's stored UTC
/// instant. The epoch components exist only on `DateTime`, whose instant is
/// its stored UTC half; every other kind answers `None`.
fn epoch_part(t: &Temporal) -> Option<(i64, i64)> {
    let Temporal::DateTime { utc, .. } = t else {
        return None;
    };
    let dt = utc.and_utc();
    let millis = dt.timestamp() * 1000 + i64::from(dt.timestamp_subsec_millis());
    Some((dt.timestamp(), millis))
}

fn duration_component(
    months: i64,
    days: i64,
    nanos: i128,
    component: DurationComponent,
) -> Result<Value> {
    // One decomposition, not two: the floored split of the stored i128
    // nanos into whole seconds and the sub-second remainder the
    // `…OfSecond` accessors read. Every arm below is expressed over it, so
    // the two rounding conventions cannot disagree again (the truncating
    // fields divide the whole-second count, exactly Neo4j's accessors).
    let (total_seconds, sub_nanos) = split_seconds(nanos);
    let total_minutes = total_seconds / SECONDS_PER_MINUTE;
    let hours = total_seconds / SECONDS_PER_HOUR;
    // Totals derived from the i128 nanosecond count can exceed i64 for
    // extreme durations; error honestly instead of truncating through `as`.
    let total = |v: i128| i64::try_from(v).map_err(|_| super::range_err());
    Ok(Value::Int(match component {
        DurationComponent::Years => months / 12,
        DurationComponent::Months => months,
        DurationComponent::MonthsOfYear => months.rem_euclid(12),
        DurationComponent::Quarters => months / 3,
        DurationComponent::QuartersOfYear => months.rem_euclid(12) / 3,
        DurationComponent::MonthsOfQuarter => months.rem_euclid(3),
        DurationComponent::Weeks => days / 7,
        DurationComponent::Days => days,
        DurationComponent::DaysOfWeek => days.rem_euclid(7),
        DurationComponent::Hours => total(hours)?,
        DurationComponent::Minutes => total(total_minutes)?,
        DurationComponent::MinutesOfHour => total(total_minutes % 60)?,
        DurationComponent::Seconds => total(total_seconds)?,
        DurationComponent::SecondsOfMinute => total(total_seconds % 60)?,
        DurationComponent::Nanos => total(nanos)?,
        // Neo4j's accessors floor a negative nanosecond count
        // (`DurationValue.milliseconds`), so the i64::MIN edge is reachable
        // only through `div_euclid`; truncating division lands one unit
        // short (-9223372036854775807 instead of i64::MIN).
        DurationComponent::Millis => total(nanos.div_euclid(NANOS_PER_MILLI))?,
        DurationComponent::Micros => total(nanos.div_euclid(NANOS_PER_MICRO))?,
        DurationComponent::MillisOfSecond => total(sub_nanos / NANOS_PER_MILLI)?,
        DurationComponent::MicrosOfSecond => total(sub_nanos / NANOS_PER_MICRO)?,
        DurationComponent::NanosOfSecond => total(sub_nanos)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::temporal::{
        construct_date, construct_datetime, construct_localdatetime, construct_localtime,
        construct_time,
    };

    fn sample(kind: ClockKind) -> Temporal {
        match kind {
            ClockKind::Date => construct_date(None).unwrap(),
            ClockKind::LocalTime => construct_localtime(None).unwrap(),
            ClockKind::Time => construct_time(None).unwrap(),
            ClockKind::LocalDateTime => construct_localdatetime(None).unwrap(),
            ClockKind::DateTime => construct_datetime(None).unwrap(),
        }
    }

    /// The readers are the one exposure table: each kind answers exactly the
    /// components it carries and rejects the rest with the typed `no such
    /// component` text — pinned by example, because the readers themselves
    /// now are the policy.
    #[test]
    fn accessors_read_exactly_the_parts_the_kind_carries() {
        let date = sample(ClockKind::Date);
        assert!(read(&date, "year").is_ok());
        assert!(read(&date, "dayOfQuarter").is_ok());
        assert!(read(&date, "epochSeconds").is_err());
        assert!(read(&date, "hour").is_err());
        assert!(read(&date, "offset").is_err());

        let localtime = sample(ClockKind::LocalTime);
        assert!(read(&localtime, "hour").is_ok());
        assert!(read(&localtime, "timezone").is_err());
        assert!(read(&localtime, "year").is_err());

        let time = sample(ClockKind::Time);
        assert!(read(&time, "hour").is_ok());
        assert!(read(&time, "offsetMinutes").is_ok());
        assert!(read(&time, "timezone").is_ok());
        assert!(read(&time, "year").is_err());
        assert!(read(&time, "epochMillis").is_err());

        let localdatetime = sample(ClockKind::LocalDateTime);
        assert!(read(&localdatetime, "year").is_ok());
        assert!(read(&localdatetime, "hour").is_ok());
        assert!(read(&localdatetime, "timezone").is_err());
        assert!(read(&localdatetime, "epochSeconds").is_err());

        let datetime = sample(ClockKind::DateTime);
        assert!(read(&datetime, "year").is_ok());
        assert!(read(&datetime, "timezone").is_ok());
        assert!(read(&datetime, "epochMillis").is_ok());

        let err = read(&date, "hour").unwrap_err();
        assert!(err.to_string().contains("no such component"), "{err}");
    }

    /// B8: one zero-offset rule — a stored zero offset renders `Z` through
    /// both the `timezone` and the `offset` accessor, the same spelling the
    /// ISO suffix uses, while a non-zero offset keeps its numeric form.
    #[test]
    fn a_zero_offset_renders_z_through_both_accessors() {
        let utc = crate::temporal::parse_iso("time", "12:00Z").unwrap();
        assert_eq!(read(&utc, "timezone").unwrap(), Value::String("Z".into()));
        assert_eq!(read(&utc, "offset").unwrap(), Value::String("Z".into()));
        let plus_one = crate::temporal::parse_iso("time", "12:00+01:00").unwrap();
        assert_eq!(
            read(&plus_one, "timezone").unwrap(),
            Value::String("+01:00".into())
        );
        assert_eq!(
            read(&plus_one, "offset").unwrap(),
            Value::String("+01:00".into())
        );
    }

    /// B7: a stored offset that pushes the wall clock past chrono's range is
    /// the typed range failure of the part reader — not an internal "no
    /// reader" error far from the cause.
    #[test]
    fn an_out_of_range_offset_is_a_typed_range_error() {
        let t = Temporal::DateTime {
            utc: NaiveDate::MAX.and_hms_opt(23, 59, 59).unwrap(),
            offset: 3600,
            zone: None,
        };
        let err = read(&t, "year").unwrap_err();
        assert!(err.to_string().contains("out of range"), "{err}");
    }

    /// The duration vocabulary is its own table: the singular calendar names
    /// are not duration components, the plural aggregate names are.
    #[test]
    fn duration_reads_only_its_own_spellings() {
        let d =
            crate::temporal::construct_duration(Some(&Value::String("P1Y2M3D".into()))).unwrap();
        assert_eq!(read(&d, "years").unwrap(), Value::Int(1));
        assert_eq!(read(&d, "months").unwrap(), Value::Int(14));
        assert!(read(&d, "year").is_err());
        assert!(read(&d, "weekOfMonth").is_err());
        assert!(read(&d, "epochSeconds").is_err());
    }
}
