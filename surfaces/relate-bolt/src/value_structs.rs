//! Bolt 4.4/5.4 temporal and spatial structures: their tags, shared with
//! the parameter decoders in `param_structs` (ADR-0177), and the
//! result-side encoders, the exact inverses of those decoders (temporal
//! ADR-0325, points ADR-0326, the `utc` patch ADR-0329, the 5.x forms
//! ADR-0484).

use chrono::{Datelike, NaiveDateTime, NaiveTime, Timelike};
use relate::Temporal;
use relate_core::temporal::{civil_epoch_days, zone_region};
use relate_core::PointValue;

use crate::dialect::Dialect;
use crate::packstream::Pack;

/// Date `[days]`.
pub(crate) const DATE: u8 = 0x44;
/// Time `[nanoseconds of day, tz_offset_seconds]`.
pub(crate) const TIME: u8 = 0x54;
/// LocalTime `[nanoseconds of day]`.
pub(crate) const LOCAL_TIME: u8 = 0x74;
/// Legacy DateTime `[local epoch seconds, nanoseconds, tz_offset_seconds]`
/// — the 4.4 form; legacy at 5.x, where the UTC pair below is
/// unconditional (ADR-0484 decision 7).
pub(crate) const DATE_TIME: u8 = 0x46;
/// Legacy DateTimeZoneId `[local epoch seconds, nanoseconds, tz_id]` — the
/// 4.4 form; legacy at 5.x (ADR-0484 decision 7).
pub(crate) const DATE_TIME_ZONE_ID: u8 = 0x66;
/// DateTimeUTC `[UTC epoch seconds, nanoseconds, tz_offset_seconds]`, under
/// the `utc` patch at 4.4 and unconditionally at 5.x (ADR-0484 decision 7).
pub(crate) const DATE_TIME_UTC: u8 = 0x49;
/// DateTimeZoneIdUTC `[UTC epoch seconds, nanoseconds, tz_id]`, under the
/// `utc` patch at 4.4 and unconditionally at 5.x (ADR-0484 decision 7).
pub(crate) const DATE_TIME_ZONE_ID_UTC: u8 = 0x69;
/// LocalDateTime `[epoch seconds, nanoseconds]`.
pub(crate) const LOCAL_DATE_TIME: u8 = 0x64;
/// Duration `[months, days, seconds, nanoseconds]`.
pub(crate) const DURATION: u8 = 0x45;
/// Point2D `[srid, x, y]`.
pub(crate) const POINT_2D: u8 = 0x58;
/// Point3D `[srid, x, y, z]`.
pub(crate) const POINT_3D: u8 = 0x59;

pub(crate) const NANOS_PER_SECOND: i64 = 1_000_000_000;
const SECONDS_PER_DAY: i128 = 86_400;

/// A temporal result as its Bolt structure (ADR-0325). Under the `utc`
/// patch or on a 5.x connection a DateTime uses `0x49`/`0x69`
/// (ADR-0329, ADR-0484 decision 7) — read from `dialect` through
/// `Dialect::utc_datetime_structures`, the one derivation, so the two
/// wire directions can never disagree. A component that does
/// not fit a PackStream Int fails the encode, naming the structure.
pub(crate) fn temporal_pack(t: &Temporal, dialect: Dialect) -> Result<Pack, String> {
    Ok(match t {
        Temporal::Date(d) => date_pack(d.year().into(), d.month(), d.day())?,
        Temporal::CivilDate { year, month, day } => date_pack(*year, *month, *day)?,
        Temporal::Time { time, offset } => Pack::Struct {
            tag: TIME,
            fields: vec![Pack::Int(nanos_of_day(*time)), Pack::Int((*offset).into())],
        },
        Temporal::LocalTime(time) => Pack::Struct {
            tag: LOCAL_TIME,
            fields: vec![Pack::Int(nanos_of_day(*time))],
        },
        Temporal::LocalDateTime(local) => {
            let (seconds, nanos) = epoch_parts(*local);
            Pack::Struct {
                tag: LOCAL_DATE_TIME,
                fields: vec![Pack::Int(seconds), Pack::Int(nanos)],
            }
        }
        Temporal::CivilDateTime {
            year,
            month,
            day,
            time,
        } => {
            let seconds = civil_epoch_days(*year, *month, *day) * SECONDS_PER_DAY
                + i128::from(time.num_seconds_from_midnight());
            Pack::Struct {
                tag: LOCAL_DATE_TIME,
                fields: vec![
                    Pack::Int(fit(LOCAL_DATE_TIME, seconds)?),
                    Pack::Int(time.nanosecond().into()),
                ],
            }
        }
        Temporal::DateTime { utc, offset, zone } => datetime_pack(*utc, *offset, zone, dialect)?,
        Temporal::Duration {
            months,
            days,
            nanos,
        } => {
            let per_second = i128::from(NANOS_PER_SECOND);
            Pack::Struct {
                tag: DURATION,
                fields: vec![
                    Pack::Int(*months),
                    Pack::Int(*days),
                    Pack::Int(fit(DURATION, nanos.div_euclid(per_second))?),
                    Pack::Int(fit(DURATION, nanos.rem_euclid(per_second))?),
                ],
            }
        }
    })
}

/// A point result as Point2D `0x58` or Point3D `0x59`, coordinates always
/// Floats (ADR-0326).
pub(crate) fn point_pack(p: &PointValue) -> Pack {
    let mut fields = vec![Pack::Int(p.srid.into()), Pack::Float(p.x), Pack::Float(p.y)];
    match p.z {
        None => Pack::Struct {
            tag: POINT_2D,
            fields,
        },
        Some(z) => {
            fields.push(Pack::Float(z));
            Pack::Struct {
                tag: POINT_3D,
                fields,
            }
        }
    }
}

/// The four DateTime structures: offset-only zones carry the offset, IANA
/// regions the zone id (the engine's `zone_region` decides); the legacy
/// forms carry the local wall clock, the UTC forms the UTC instant. The
/// UTC forms apply when the `utc` patch was negotiated or the connection
/// speaks 5.x (ADR-0329, ADR-0484 decision 7) — the codec's one predicate,
/// both axes. Only the tag depends on both axes — the exhaustive 4-arm
/// match derives it alone — while the seconds read the UTC axis once and
/// the `tz` field the region axis once.
fn datetime_pack(
    utc: NaiveDateTime,
    offset: i32,
    zone: &Option<String>,
    dialect: Dialect,
) -> Result<Pack, String> {
    let utc_structures = dialect.utc_datetime_structures();
    let region = zone_region(zone.as_deref());
    let (utc_seconds, nanos) = epoch_parts(utc);
    let tag = match (region, utc_structures) {
        (None, false) => DATE_TIME,
        (Some(_), false) => DATE_TIME_ZONE_ID,
        (None, true) => DATE_TIME_UTC,
        (Some(_), true) => DATE_TIME_ZONE_ID_UTC,
    };
    // The legacy structures carry the local wall clock read as UTC; the
    // UTC structures carry the UTC instant itself.
    let seconds = if utc_structures {
        utc_seconds
    } else {
        utc_seconds
            .checked_add(offset.into())
            .ok_or_else(|| out_of_range(tag))?
    };
    // Offset-only zones carry the offset, IANA regions the zone id.
    let tz = match region {
        Some(region) => Pack::String(region.into()),
        None => Pack::Int(offset.into()),
    };
    Ok(Pack::Struct {
        tag,
        fields: vec![Pack::Int(seconds), Pack::Int(nanos), tz],
    })
}

/// Date `0x44`: days since 1970-01-01 by the engine's one proleptic
/// Gregorian count, for chrono and out-of-range civil dates alike.
fn date_pack(year: i64, month: u32, day: u32) -> Result<Pack, String> {
    let days = civil_epoch_days(year, month, day);
    Ok(Pack::Struct {
        tag: DATE,
        fields: vec![Pack::Int(fit(DATE, days)?)],
    })
}

/// Epoch seconds (floored) and the non-negative sub-second nanoseconds.
fn epoch_parts(dt: NaiveDateTime) -> (i64, i64) {
    let instant = dt.and_utc();
    (instant.timestamp(), instant.timestamp_subsec_nanos().into())
}

fn nanos_of_day(t: NaiveTime) -> i64 {
    i64::from(t.num_seconds_from_midnight()) * NANOS_PER_SECOND + i64::from(t.nanosecond())
}

/// A component as a PackStream Int, or the out-of-range failure.
fn fit(tag: u8, value: i128) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| out_of_range(tag))
}

fn out_of_range(tag: u8) -> String {
    format!("temporal result does not fit Bolt structure 0x{tag:02X}: a component exceeds i64")
}

#[cfg(test)]
#[path = "value_structs_tests.rs"]
mod tests;
