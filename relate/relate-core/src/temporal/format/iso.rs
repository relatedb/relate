//! The ISO-8601 rendering family: the one renderer behind
//! `Temporal::iso_string` / `full_seconds_string` and the offset and
//! duration spellings the pattern formatter and component accessors share.

use chrono::{NaiveDateTime, NaiveTime, Timelike};

use super::super::zone::{add_offset, zone_kind, ZoneSpec};
use super::super::Temporal;

/// The one ISO renderer both public forms delegate to: identical
/// except for the time-of-day's seconds-omission rule
/// (`format_time`'s `%H:%M` truncation for a whole minute), which
/// `full_seconds` disables.
pub(in crate::temporal) fn render(value: &Temporal, full_seconds: bool) -> String {
    match value {
        Temporal::Date(d) => d.format("%Y-%m-%d").to_string(),
        Temporal::CivilDate { year, month, day } => format_civil_date(*year, *month, *day),
        Temporal::LocalTime(t) => format_time(*t, full_seconds),
        Temporal::Time { time, offset } => {
            format!(
                "{}{}",
                format_time(*time, full_seconds),
                format_zone_suffix(*offset, None)
            )
        }
        Temporal::LocalDateTime(dt) => format_local_dt(*dt, full_seconds),
        Temporal::CivilDateTime {
            year,
            month,
            day,
            time,
        } => format!(
            "{}T{}",
            format_civil_date(*year, *month, *day),
            format_time(*time, full_seconds)
        ),
        Temporal::DateTime { utc, offset, zone } => {
            // A deliberate, stated fallback — not a silent one: the
            // Bolt parameter decoders accept any i32 offset, so a
            // crafted or extreme `utc + offset` can leave chrono's
            // range (constructors cannot, parameters can); rendering
            // has no error channel and must not panic on a decodable
            // value, so the UTC wall clock renders beside the
            // offset/zone suffix. (`local_of` is the identity
            // helper's stricter form; render does not use it.)
            let local = add_offset(*utc, *offset).unwrap_or(*utc);
            format!(
                "{}{}",
                format_local_dt(local, full_seconds),
                format_zone_suffix(*offset, zone.as_deref())
            )
        }
        Temporal::Duration {
            months,
            days,
            nanos,
        } => format_duration(*months, *days, *nanos),
    }
}

/// The signed, zero-padded civil-date form both civil variants render:
/// a negative year is zero-padded to nine digits behind the sign.
pub(in crate::temporal) fn format_civil_date(year: i64, month: u32, day: u32) -> String {
    if year < 0 {
        format!("-{year:09}-{month:02}-{day:02}", year = year.unsigned_abs())
    } else {
        format!("+{year}-{month:02}-{day:02}")
    }
}

/// The time-of-day renderer: `full_seconds` disables the whole-minute
/// truncation (`%H:%M`) — the one place the seconds-omission fact
/// lives, consulted by both of `render`'s modes (ADR-0448).
pub(in crate::temporal) fn format_time(t: NaiveTime, full_seconds: bool) -> String {
    let n = t.nanosecond();
    if n == 0 {
        if t.second() == 0 && !full_seconds {
            t.format("%H:%M").to_string()
        } else {
            t.format("%H:%M:%S").to_string()
        }
    } else {
        let s = format!("{n:09}");
        let trim = s.trim_end_matches('0');
        format!(
            "{:02}:{:02}:{:02}.{}",
            t.hour(),
            t.minute(),
            t.second(),
            trim
        )
    }
}

pub(in crate::temporal) fn format_local_dt(dt: NaiveDateTime, full_seconds: bool) -> String {
    format!(
        "{}T{}",
        dt.date().format("%Y-%m-%d"),
        format_time(dt.time(), full_seconds)
    )
}

pub(in crate::temporal) fn format_offset(secs: i32) -> String {
    let sign = if secs >= 0 { '+' } else { '-' };
    let abs = secs.unsigned_abs();
    let h = abs / 3600;
    let m = (abs % 3600) / 60;
    let s = abs % 60;
    if s != 0 {
        format!("{sign}{h:02}:{m:02}:{s:02}")
    } else if m == 0 {
        format!("{sign}{h:02}:00")
    } else {
        format!("{sign}{h:02}:{m:02}")
    }
}

pub(in crate::temporal) fn format_zone_suffix(offset: i32, zone: Option<&str>) -> String {
    // One classification, one rule: the IANA arm brackets its region name
    // over the offset-only spelling; `Z` is UTC; every offset spelling
    // renders through the one zero-offset rule.
    match zone.map(zone_kind) {
        Some(ZoneSpec::Iana(name)) => format!("{}[{name}]", format_zone_offset(offset)),
        Some(ZoneSpec::Utc) => "Z".into(),
        Some(ZoneSpec::Fixed(_) | ZoneSpec::Prefixed(_) | ZoneSpec::Other(_)) | None => {
            format_zone_offset(offset)
        }
    }
}

/// The offset-only spelling a zoneless clock value renders: `Z` at zero, the
/// numeric offset otherwise. The one zero-offset rule, shared by the ISO
/// suffix ([`format_zone_suffix`]) and the `timezone`/`offset` accessors so
/// one stored offset has one spelling.
pub(in crate::temporal) fn format_zone_offset(offset: i32) -> String {
    if offset == 0 {
        "Z".into()
    } else {
        format_offset(offset)
    }
}

pub(in crate::temporal) fn format_duration(months: i64, days: i64, nanos: i128) -> String {
    if months == 0 && days == 0 && nanos == 0 {
        return "PT0S".into();
    }
    let mut s = String::from("P");
    let y = months / 12;
    let mo = months % 12;
    if y != 0 {
        s.push_str(&format!("{y}Y"));
    }
    if mo != 0 {
        s.push_str(&format!("{mo}M"));
    }
    if days != 0 {
        s.push_str(&format!("{days}D"));
    }
    let neg_t = nanos < 0;
    let mut n = nanos.unsigned_abs();
    let hours = n / 3_600_000_000_000;
    n %= 3_600_000_000_000;
    let mins = n / 60_000_000_000;
    n %= 60_000_000_000;
    let secs = n / 1_000_000_000;
    let nano = n % 1_000_000_000;
    if hours != 0 || mins != 0 || secs != 0 || nano != 0 {
        s.push('T');
        let sign = if neg_t { "-" } else { "" };
        if hours != 0 {
            s.push_str(&format!("{sign}{hours}H"));
        }
        if mins != 0 {
            s.push_str(&format!("{sign}{mins}M"));
        }
        if secs != 0 || nano != 0 {
            if nano == 0 {
                s.push_str(&format!("{sign}{secs}S"));
            } else {
                let frac = format!("{nano:09}").trim_end_matches('0').to_string();
                s.push_str(&format!("{sign}{secs}.{frac}S"));
            }
        }
    }
    s
}
