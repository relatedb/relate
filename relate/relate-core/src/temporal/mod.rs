//! OpenCypher temporal values (ADR-0021).

use std::fmt;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};

use crate::{Error, Value};

mod arith;
mod between;
mod clock;
mod component;
mod construct;
mod duration;
mod duration_pattern;
mod duration_tokens;
mod exact;
mod field;
mod field_rules;
mod format;
mod parse;
mod pattern;
mod relations;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_arith;
#[cfg(test)]
mod tests_between;
#[cfg(test)]
mod tests_civil_zoned;
#[cfg(test)]
mod tests_clock;
#[cfg(test)]
mod tests_construct;
#[cfg(test)]
mod tests_duration;
#[cfg(test)]
mod tests_fields;
#[cfg(test)]
mod tests_pattern;
#[cfg(test)]
mod tests_ranges;
#[cfg(test)]
mod tests_zone;
mod truncate;
mod zone;

use format::format_zone_offset;

pub use arith::{
    add_duration, civil_epoch_days, civil_epoch_seconds, civil_from_days, negate_duration,
    scale_duration, sub_duration,
};
pub use between::{duration_between, duration_between_unit, BetweenUnit};
pub use clock::{
    clock_access, clock_spelling, clock_spelling_rows, clock_spellings, construct_clock,
    now_epoch_millis, now_utc_dt, ClockKind, ClockMode, QueryClockGuard, TransactionClock,
};
pub use construct::{
    accepts_pattern, clock_kind, construct_date, construct_datetime, construct_duration,
    construct_localdatetime, construct_localtime, construct_named, construct_time, is_constructor,
    samples_clock,
};
pub use format::format_pattern;
pub use parse::{
    datetime_from_epoch, datetime_from_epoch_millis, padded_iso_date_value, parse_iso,
    parse_pattern,
};
pub use truncate::{truncate, truncate_now};
pub use zone::{zone_offset_at, zone_offset_at_utc, zone_region, ZoneError, MAX_OFFSET_SECONDS};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Temporal {
    Date(NaiveDate),
    /// Out-of-range proleptic date (TCK ±999999999).
    CivilDate {
        year: i64,
        month: u32,
        day: u32,
    },
    LocalTime(NaiveTime),
    Time {
        time: NaiveTime,
        offset: i32,
    },
    LocalDateTime(NaiveDateTime),
    CivilDateTime {
        year: i64,
        month: u32,
        day: u32,
        time: NaiveTime,
    },
    DateTime {
        utc: NaiveDateTime,
        offset: i32,
        zone: Option<String>,
    },
    /// Out-of-range zoned datetime (TCK ±999999999): the wall clock as
    /// proleptic civil fields — [`CivilZonedFields`], [`Temporal::CivilDateTime`]'s
    /// payload plus the offset and optional zone spelling
    /// [`Temporal::DateTime`] carries. Only an offset or `Z` zone spec
    /// constructs one (ADR-0683); the instant is the wall clock minus the
    /// offset, computed over the wide day arithmetic and never a chrono
    /// conversion. The payload is boxed: unboxed it is 56 bytes against
    /// the chrono `DateTime` variant's 48, and `Temporal` growing past 48
    /// grows every row's `Value` with it — the far-year form is rare
    /// enough to pay one allocation instead.
    CivilZoned(Box<CivilZonedFields>),
    Duration {
        months: i64,
        days: i64,
        nanos: i128,
    },
}

/// The boxed payload of [`Temporal::CivilZoned`] (ADR-0683): the wall
/// clock's proleptic civil fields plus the offset and optional zone
/// spelling — the fields a `parse_zoned_dt` far-year fallback fills and
/// every zoned relation reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CivilZonedFields {
    /// The wall clock's proleptic year, ±999999999.
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub time: NaiveTime,
    pub offset: i32,
    pub zone: Option<String>,
}

impl CivilZonedFields {
    /// The UTC instant in epoch nanoseconds — the wall clock minus the
    /// offset over the wide day arithmetic (the one civil derivation the
    /// relations' zoned view and the executor's group/index key read:
    /// `relations::ZonedView`). Never a chrono conversion.
    pub fn epoch_nanos(&self) -> i128 {
        civil_epoch_days(self.year, self.month, self.day) * NANOS_PER_DAY
            + i128::from(time_to_nanos(self.time))
            - i128::from(self.offset) * 1_000_000_000
    }
}

/// The six temporal type kinds, one per Cypher type name — the variant
/// grouping the type-name spelling and the ORDER BY sub-ranks both key
/// off, so it exists in one place. Declared in the ORDER BY rank order:
/// ZONED DATETIME < LOCAL DATETIME < DATE < ZONED TIME < LOCAL TIME <
/// DURATION (live Neo4j 2026.10.1, value-independent), so a kind's
/// discriminant is that rank's offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemporalKind {
    DateTime,
    LocalDateTime,
    Date,
    Time,
    LocalTime,
    Duration,
}

impl TemporalKind {
    /// The kind's lowercase wire name — the `Temporal::kind_name`
    /// spelling, owned by the kind so the variant partition has one
    /// owner.
    pub(crate) fn name(self) -> &'static str {
        match self {
            TemporalKind::DateTime => "datetime",
            TemporalKind::LocalDateTime => "localdatetime",
            TemporalKind::Date => "date",
            TemporalKind::Time => "time",
            TemporalKind::LocalTime => "localtime",
            TemporalKind::Duration => "duration",
        }
    }
}

impl Temporal {
    /// This value's type kind — the one spelling of the variant grouping
    /// (the civil out-of-range forms share their kind's name).
    pub fn kind(&self) -> TemporalKind {
        match self {
            Temporal::DateTime { .. } | Temporal::CivilZoned(_) => TemporalKind::DateTime,
            Temporal::LocalDateTime(_) | Temporal::CivilDateTime { .. } => {
                TemporalKind::LocalDateTime
            }
            Temporal::Date(_) | Temporal::CivilDate { .. } => TemporalKind::Date,
            Temporal::Time { .. } => TemporalKind::Time,
            Temporal::LocalTime(_) => TemporalKind::LocalTime,
            Temporal::Duration { .. } => TemporalKind::Duration,
        }
    }
}

/// The zoned facts of a `DateTime` in either representation (ADR-0683) —
/// the shape [`Temporal::zoned_parts`] publishes and the one the temporal
/// relations and the Bolt encoder read: the wide UTC instant in epoch
/// nanoseconds, the offset, and the already-classified IANA region (`None`
/// for a plain offset spelling). Named fields, so a consumer never has to
/// know a positional order to read the three facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZonedParts<'a> {
    /// The UTC instant in epoch nanoseconds — the value the ordering
    /// relations rank by and the executor's group/index key keys on. Wide
    /// `i128` throughout (never `timestamp_nanos`, an `i64` that
    /// overflows far inside chrono's range).
    pub instant: i128,
    pub offset: i32,
    /// The IANA region name, `None` for a plain offset spelling.
    pub region: Option<&'a str>,
}

/// The zone half of a `DateTime`'s equality identity (TCK TemporalCompare):
/// an IANA zone is identified by its name — so the two halves of a DST
/// fall-back overlap stay equal — and every other spelling (`Z`, `+00:00`,
/// `UTC`, …) by the UTC offset it denotes, so `Z` equals `+00:00` while a
/// named zone never equals the offset it happens to denote.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ZoneId {
    Named(String),
    Offset(i32),
}

impl ZoneId {
    /// The identity of a `DateTime`'s `zone` at `offset`.
    pub fn of(zone: Option<&str>, offset: i32) -> ZoneId {
        Self::of_region(zone_region(zone), offset)
    }

    /// The identity of an **already-classified** region at `offset` — the
    /// shape the zoned view yields (`relations::zoned_parts`), so a
    /// consumer holding the resolved region never re-runs the spelling
    /// classification. Crate-internal: [`ZoneId::of`] is the one public
    /// entry, so a caller holding a raw zone spelling cannot reach this
    /// form and mistake a spelling for a region.
    pub(crate) fn of_region(region: Option<&str>, offset: i32) -> ZoneId {
        match region {
            Some(name) => ZoneId::Named(name.to_string()),
            None => ZoneId::Offset(offset),
        }
    }
}

impl Temporal {
    /// The one place a duration's stored `(months, days, nanos)` triple is
    /// assembled, and the one place its representability bound lives: the
    /// sub-day part must fit Neo4j's `i64` seconds field (inclusive of
    /// `2^63`, the `f64` image of `i64::MAX` that the scaling path still
    /// casts back), and the duration's average length in seconds
    /// (`months * AVG_SECONDS_PER_MONTH + days * 86400 + seconds`) must fit
    /// an `i64` (`DurationValue.assertNoOverflow`). Every producer — the ISO
    /// numeral parser, the pattern parser, the map constructor, scaling and
    /// duration addition — builds through this seam, so one value is
    /// accepted or rejected regardless of how it was spelled; a value past
    /// the bound is Neo4j's interval-field overflow, never a silently larger
    /// duration. Neo4j's constructor enforces the same bound (verified
    /// against Neo4j 5: `duration({months: 4000000000000})` errors, while the
    /// same value via `* 1.0` errors identically).
    pub(crate) fn duration_checked(months: i64, days: i64, nanos: i128) -> Result<Temporal, Error> {
        let (seconds, _) = split_seconds(nanos);
        if !(-MAX_DURATION_SECONDS..=MAX_DURATION_SECONDS).contains(&seconds) {
            return Err(range_err());
        }
        // `seconds` can equal ±2^63, one past `i64`; the average-length sum
        // stays in i128 and the final `i64::try_from` rejects an out-of-range
        // total, so the clamp only keeps the sum's own arithmetic in range.
        let seconds = seconds.clamp(i128::from(i64::MIN), i128::from(i64::MAX));
        let average = i128::from(months) * AVG_SECONDS_PER_MONTH
            + i128::from(days) * (NANOS_PER_DAY / NANOS_PER_SECOND)
            + seconds;
        i64::try_from(average).map_err(|_| range_err())?;
        Ok(Temporal::Duration {
            months,
            days,
            nanos,
        })
    }

    /// [`Temporal::duration_checked`] with the strict stored-model bound for
    /// producers whose arithmetic is exact (the ISO and pattern parsers, the
    /// all-integer map constructor, negation and addition). Neo4j's duration
    /// stores its sub-day part as an `i64` seconds field, so exactly `2^63`
    /// seconds is not representable (`duration('PT9223372036854775808S')`
    /// overflows); only the `f64` scaling path produces that image by
    /// rounding `i64::MAX` and casts it back, which is why the shared seam
    /// accepts it.
    pub(crate) fn duration_checked_exact(
        months: i64,
        days: i64,
        nanos: i128,
    ) -> Result<Temporal, Error> {
        if nanos.div_euclid(NANOS_PER_SECOND) > i128::from(i64::MAX) {
            return Err(range_err());
        }
        Self::duration_checked(months, days, nanos)
    }

    pub fn kind_name(&self) -> &'static str {
        self.kind().name()
    }

    /// Whether the value carries a calendar date — the one statement of the
    /// capability partition, beside the type. The field policy
    /// (`field_rules::check_sources`), the truncation granularity rule and
    /// the part readers all read it, so no consumer keeps its own variant
    /// list.
    pub fn has_date(&self) -> bool {
        matches!(
            self.kind(),
            TemporalKind::Date | TemporalKind::LocalDateTime | TemporalKind::DateTime
        )
    }

    /// Whether the value carries a time of day (see [`Temporal::has_date`]).
    pub fn has_time(&self) -> bool {
        matches!(
            self.kind(),
            TemporalKind::LocalTime
                | TemporalKind::Time
                | TemporalKind::LocalDateTime
                | TemporalKind::DateTime
        )
    }

    /// Whether the value carries both halves (the `datetime` source field).
    pub fn has_date_and_time(&self) -> bool {
        self.has_date() && self.has_time()
    }

    pub fn iso_string(&self) -> String {
        format::render(self, false)
    }

    /// The full-precision ISO rendering: [`Temporal::iso_string`] with
    /// the time-of-day's seconds always present. The value layer's
    /// temporal rendering policy for every string-producing surface —
    /// `toString` (top-level, nested in a map or accepted list, or
    /// through `toStringOrNull`) and the interpolated-string scalar
    /// path — one rendering for one value, however it was built or
    /// wherever it is embedded (ADR-0448).
    pub fn full_seconds_string(&self) -> String {
        format::render(self, true)
    }

    /// One component accessor (`d.year`, `t.offsetMinutes`, `dur.months`).
    /// The reference engine rejects a component the value's kind does not
    /// carry, so [`component::read`] is the whole policy: the reader answers
    /// a component its kind carries or the read is a typed error.
    pub fn component(&self, key: &str) -> Result<Value, Error> {
        component::read(self, key)
    }
}

impl fmt::Display for Temporal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.iso_string())
    }
}

fn time_to_nanos(t: NaiveTime) -> i64 {
    t.num_seconds_from_midnight() as i64 * 1_000_000_000 + t.nanosecond() as i64
}

/// Nanoseconds of the UTC clock time a `Time` denotes. Deliberately *not*
/// wrapped at midnight: an offset can push the wall clock out of the day, and
/// the ordering contract is this signed value plus the offset. The lowest
/// shared home for its two consumers — the relations (`temporal/relations.rs`)
/// and `between`'s unit arithmetic, which shares the signed-nanos convention.
pub(super) fn utc_time_nanos(t: NaiveTime, offset: i32) -> i64 {
    time_to_nanos(t) - i64::from(offset) * 1_000_000_000
}

/// The wall clock of a zoned datetime as its normalized `(year, month,
/// day, time)` parts (ADR-0683): the relations construct it from either
/// representation — a chrono wall clock through its calendar fields, a
/// civil one from its own — so equality is the tuple's (see
/// `relations::ZonedView`). The type lives here, at the crate's temporal
/// root, because it is the public signature type of
/// [`Temporal::datetime_eq_identity`]. Named for its shape:
/// `zone::WallClock` owns the resolver concept.
pub type WallClockParts = (i64, u32, u32, NaiveTime);

/// The one floored decomposition of a stored nanosecond count: whole
/// seconds and the non-negative sub-second remainder. The split floors, so
/// a negative sub-second duration lands on seconds = -1 with a
/// non-negative remainder — Neo4j's own stored model — and every reader of
/// the seconds/sub-second pair shares the one rounding convention.
fn split_seconds(nanos: i128) -> (i128, i128) {
    (
        nanos.div_euclid(NANOS_PER_SECOND),
        nanos.rem_euclid(NANOS_PER_SECOND),
    )
}

/// `n` is always a within-day nanosecond count (rem_euclid at the call sites),
/// so the conversion is infallible in practice; fall back instead of unwrap.
fn nanos_to_time(n: i64) -> NaiveTime {
    let secs = (n / 1_000_000_000) as u32;
    let nano = (n % 1_000_000_000) as u32;
    NaiveTime::from_num_seconds_from_midnight_opt(secs, nano).unwrap_or(NaiveTime::MIN)
}

/// Error returned when a temporal computation overflows the representable
/// range (chrono dates ±262143 years, i64 duration months/days, i128 nanos).
/// The honest contract for out-of-range results is an error, never a panic.
pub(super) fn range_err() -> Error {
    Error::ty("temporal value out of range")
}

/// The one spelling of a duration arithmetic overflow: Neo4j's 22N28
/// (`ArithmeticException`) text, naming the operation that overflowed
/// (`duration()` for construction, the field for pattern formatting).
pub(super) fn duration_overflow_err(operation: &str) -> Error {
    Error::ty(format!(
        "the result of the operation '{operation}' has caused an overflow"
    ))
}

/// Narrow one i128 lane total to the i64 a duration component stores.
/// Components that overflow individually but cancel stay representable because
/// the lanes are summed before this check (Neo4j's exact wide arithmetic).
pub(super) fn checked_lane(value: i128, operation: &str) -> crate::Result<i64> {
    i64::try_from(value).map_err(|_| duration_overflow_err(operation))
}

/// The `Temporal` payload a field map holds under `k`, or `None` when the
/// key is absent or non-temporal. Shared by the constructors and the zone
/// resolution (`construct.rs`, `zone.rs`); the scalar extraction is the
/// canonical [`Value::as_temporal`]. The field *policy* never reads a source
/// field through this — a present non-temporal value is a typed rejection
/// there (`field_rules`), not an absent field.
pub(super) fn map_temporal<'a>(
    map: &'a std::collections::BTreeMap<String, Value>,
    k: &str,
) -> Option<&'a Temporal> {
    map.get(k).and_then(Value::as_temporal)
}

/// The value a `(spelling, value)` table names, or `None` — the one keyed
/// table lookup behind every `from_name` ([`clock::ClockKind`],
/// [`component::Component`], its duration sibling), so no table is walked by
/// a hand-rolled copy.
pub(super) fn lookup<T: Copy>(table: &[(&str, T)], name: &str) -> Option<T> {
    table
        .iter()
        .find(|(spelling, _)| *spelling == name)
        .map(|(_, value)| *value)
}

/// f64 → i128 with a range error; 1e38 is comfortably below i128::MAX ≈ 1.7e38.
pub(super) fn f64_to_i128_range(v: f64) -> Result<i128, Error> {
    if v.is_finite() && v.abs() < 1.0e38 {
        Ok(v as i128)
    } else {
        Err(range_err())
    }
}

/// f64 → i64 with a range error — Neo4j's `DurationValue.safeDoubleToLong`
/// seam: the stored month and day fields are `i64`, so an out-of-range
/// fraction fold is a typed range error, never a saturating cast.
pub(super) fn f64_to_i64_range(v: f64) -> Result<i64, Error> {
    i64::try_from(f64_to_i128_range(v)?).map_err(|_| range_err())
}

fn weekday_from_i(i: i64) -> Weekday {
    // `i.rem_euclid(7)` is total (unlike `(i - 1).rem_euclid(7)`, which
    // overflows at i64::MIN) and selects the same weekday for every input:
    // 1..=6 -> Mon..Sat, 0 -> Sun.
    match i.rem_euclid(7) {
        1 => Weekday::Mon,
        2 => Weekday::Tue,
        3 => Weekday::Wed,
        4 => Weekday::Thu,
        5 => Weekday::Fri,
        6 => Weekday::Sat,
        _ => Weekday::Sun,
    }
}

/// The duration field vocabulary — fields, tiers, unit scales and the
/// average-month/overflow facts — lives in [`field`]; these names are the
/// facade the sibling modules read through `super::`.
use field::{
    DurationField, AVG_NANOS_PER_MONTH, AVG_SECONDS_PER_MONTH, MAX_DURATION_SECONDS,
    MONTHS_PER_QUARTER, MONTHS_PER_YEAR, NANOS_PER_DAY, NANOS_PER_HOUR, NANOS_PER_MICRO,
    NANOS_PER_MILLI, NANOS_PER_MINUTE, NANOS_PER_SECOND, SECONDS_PER_HOUR, SECONDS_PER_MINUTE,
};
