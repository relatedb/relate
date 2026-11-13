//! Construction of temporal values from components and maps. The
//! zone-resolution cluster lives in [`super::zone`] (ADR-0340), the
//! truncation cluster in [`super::truncate`], and the CIP-223 pattern
//! formatter in [`super::format`].

use chrono::{
    Datelike, Days, Duration as ChronoDur, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday,
};

use crate::{bare_type_name, Error, Result, Value};

use super::clock::now_value;
use super::clock::{self, ClockKind};
use super::exact::{exact_number, DurationTiers, ExactNumber};
use super::field::DurationLane;
use super::field_rules::{
    self, DateForm, EpochField, Fields, Sources, DAY_OF_QUARTER_KEYS, DAY_OF_WEEK_KEYS,
    EPOCH_MILLIS_KEYS, EPOCH_SECONDS_KEYS, ORDINAL_DAY_KEYS,
};
use super::parse::{parse_iso, parse_pattern};
use super::zone::{add_offset, apply_timezone, offset_from_map, IanaAt, ResolvedZone};
use super::{
    checked_lane, weekday_from_i, DurationField, Temporal, MONTHS_PER_QUARTER, NANOS_PER_SECOND,
};

/// The integer a temporal component map holds under `k`, or `None` when the
/// field is absent or null. A present non-integer is Neo4j's 22G06
/// non-assignable component (an `ArgumentError`): an integer-typed component
/// map never reads a float/string/bool/list value as an absent field. Null is
/// ignored, as Neo4j filters null-valued fields before constructing.
pub(super) fn map_int(map: &Fields, k: &str) -> Result<Option<i64>> {
    match map.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Int(v)) => Ok(Some(*v)),
        Some(other) => Err(Error::argument_error(format!(
            "cannot assign '{k}' of a {}",
            bare_type_name(other)
        ))),
    }
}

/// [`map_int`] narrowed to the bounded integer a component stores (`u32` for
/// a date/time field, `i32` for a calendar year). The one home of the
/// conversion and its typed out-of-range error.
pub(super) fn map_int_bounded<T>(map: &Fields, k: &str, msg: &str) -> Result<Option<T>>
where
    T: TryFrom<i64>,
{
    map_int(map, k)?
        .map(|value| T::try_from(value).map_err(|_| Error::argument_error(msg)))
        .transpose()
}

/// The first present spelling of a multi-spelling field (`dayOfWeek`/
/// `dayofweek`), validated through [`map_int`]. The first spelling keeps the
/// documented precedence when both are present.
pub(super) fn map_int_alias(map: &Fields, keys: &[&str]) -> Result<Option<i64>> {
    for key in keys {
        if let Some(value) = map_int(map, key)? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

/// `(epoch seconds, subsecond nanos)` named by the epoch field `field` —
/// the field [`field_rules::validate_epoch`] selected, read through its own
/// key family, so the validator and the reader cannot disagree about which
/// field names the instant.
fn epoch_from_map(m: &Fields, field: EpochField) -> Result<(i64, u32)> {
    // The validator proved one spelling carries a non-null value; the alias
    // reader keeps the first-spelling precedence and the 22G06 rejection for
    // a non-integer.
    let value = map_int_alias(m, field.keys())?
        .ok_or_else(|| Error::argument_error(format!("{} must be an integer", field.name())))?;
    Ok(field.split(value))
}

/// The UTC naive datetime the selected epoch field names. Epoch fields are
/// instants on the UTC timeline by definition, so the conversion itself
/// needs no zone.
fn epoch_utc_dt(m: &Fields, field: EpochField) -> Result<NaiveDateTime> {
    let (secs, nanos) = epoch_from_map(m, field)?;
    // `split` answers nanos already in `0..=999_999_999`, so no clamp or
    // unchecked cast is needed here.
    chrono::DateTime::from_timestamp(secs, nanos)
        .map(|d| d.naive_utc())
        .ok_or_else(|| Error::argument_error("epoch out of range"))
}

pub(super) fn date_from_parts(year: i32, month: u32, day: u32) -> Result<NaiveDate> {
    NaiveDate::from_ymd_opt(year, month, day).ok_or_else(|| Error::argument_error("invalid date"))
}

pub(super) fn date_from_week(year: i32, week: u32, dow: Weekday) -> Result<NaiveDate> {
    // chrono's `from_isoywd_opt` computes `year ± 1` internally *before* its
    // range guard, so i32-extreme years (reachable here from week-date
    // strings, e.g. '2147483647W526') panic inside chrono. Restrict to
    // chrono's range ±1: the internal `year ± 1` then stays within i32 and
    // any genuinely out-of-range result still returns `None`.
    if !(-262_144..=262_144).contains(&year) {
        return Err(Error::argument_error("invalid week date"));
    }
    NaiveDate::from_isoywd_opt(year, week, dow)
        .ok_or_else(|| Error::argument_error("invalid week date"))
}

/// Whether `name` is a temporal constructor spelling [`construct_named`]
/// builds values from — a clock kind ([`ClockKind::from_name`], which reads
/// the canonical kind table) or `duration`, the one non-clock constructor.
/// The one predicate the front-end's dialect gate and literal folding and
/// the executor's scan-predicate specialization consult, so none can
/// recognize a name the shared policy would not construct. The alias
/// spellings (`local_time`, …) canonicalize inside; the clock-access
/// spellings are not constructors of arguments at all.
pub fn is_constructor(name: &str) -> bool {
    let canon = canonical(name);
    canon == "duration" || ClockKind::from_name(&canon).is_some()
}

/// The one name canonicalization both readers share: every spelling is
/// case-folded and mapped to the base constructor it stands for.
/// [`is_constructor`] recognizes only the base and alias spellings, so every name
/// comparison goes through here first — [`accepts_pattern`] and
/// [`construct_named`] cannot disagree on a spelling.
fn canonical(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "local_time" => "localtime".to_string(),
        "local_datetime" | "local_date_time" => "localdatetime".to_string(),
        "zoned_time" => "time".to_string(),
        "zoned_datetime" | "zoned_date_time" => "datetime".to_string(),
        other => other.to_string(),
    }
}

/// Whether `name` accepts the CIP-228 `(value, pattern)` form — every
/// constructor does, so this is [`is_constructor`], the one vocabulary
/// predicate.
pub fn accepts_pattern(name: &str) -> bool {
    is_constructor(name)
}

/// The three shapes a temporal constructor's map argument can take.
enum MapForm {
    /// An epoch field (`epochMillis`/`epochSeconds`) names an instant on the
    /// UTC timeline.
    Epoch,
    /// The map's only constructor-relevant entry is a `timezone`: the
    /// clock-reading form.
    Clock,
    /// Component fields — the field constructors' map.
    Fields,
}

/// The one map-shape classifier, consulted once per constructor call. Its
/// positive rules are the readers' own vocabulary — the epoch fields, the
/// `timezone` selector, and (by exclusion) the component fields — so no
/// second field list can drift from them. A map carrying an epoch field
/// alongside `timezone` stays the epoch form; null-valued entries are
/// ignored, as every field reader ignores them, so a null epoch spelling
/// neither selects the epoch form nor hides the component fields beside it.
/// A null `timezone` keeps the clock form so it is rejected as a timezone
/// rather than read as an absent field.
fn map_form(m: &Fields) -> MapForm {
    if field_rules::present_alias(m, EPOCH_MILLIS_KEYS)
        || field_rules::present_alias(m, EPOCH_SECONDS_KEYS)
    {
        return MapForm::Epoch;
    }
    if m.contains_key("timezone")
        && m.iter()
            .all(|(key, value)| key == "timezone" || value.is_null())
    {
        return MapForm::Clock;
    }
    MapForm::Fields
}

/// Whether a constructor argument is the clock-reading map form. The
/// literal fold consults this: a clock reading samples the clock, so folding
/// it would bake a lowering-time instant into the plan.
pub fn samples_clock(arg: &Value) -> bool {
    matches!(arg, Value::Map(m) if matches!(map_form(m), MapForm::Clock))
}

/// The map-argument policy for a clock-capable kind: an epoch field names
/// an instant, a timezone-only map is the clock reading, every other map is
/// the component-field map.
fn construct_map(kind: ClockKind, m: &Fields) -> Result<Temporal> {
    match map_form(m) {
        MapForm::Epoch => epoch_map(kind, m),
        MapForm::Clock => clock::clock_from_map(kind, m),
        MapForm::Fields => {
            let validated = field_rules::validate(kind, m)?;
            field_map(kind, m, &validated)
        }
    }
}

/// The epoch map form: the instant the epoch field names, then every other
/// component field applied over its wall clock in the resolved zone. The
/// component-field policy is the one that reads those overrides, so the
/// validator's "component fields and `timezone` stay legal overrides"
/// contract and the reader cannot diverge (Neo4j-verified:
/// `datetime({epochSeconds: 0, year: 2020})` is `2020-01-01T00:00Z`, and
/// `{epochMillis: 0, minute: 5, timezone: '+02:00'}` is
/// `1970-01-01T02:05+02:00`).
fn epoch_map(kind: ClockKind, m: &Fields) -> Result<Temporal> {
    let field = field_rules::validate_epoch(kind, m)?;
    let utc = epoch_utc_dt(m, field)?;
    let resolved = offset_from_map(m, IanaAt::Utc(utc))?;
    // The instant's wall clock in the resolved zone is this map's source
    // date/time; the epoch spellings are replaced by it and every other key
    // (component fields, `timezone`) is carried through unchanged.
    let mut component_map: Fields = m
        .iter()
        .filter(|(key, _)| !field_rules::is_epoch_field(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    component_map.insert(
        "datetime".into(),
        Value::Temporal(clock::zoned(utc, resolved)),
    );
    let validated = field_rules::validate(kind, &component_map)?;
    field_map(kind, &component_map, &validated)
}

/// The component-field map constructor for `kind` — the map path for maps
/// [`map_form`] classified as [`MapForm::Fields`] (and the epoch form, once
/// its instant is the map's source), after [`field_rules::validate`] admitted
/// the field set and resolved its form and sources.
fn field_map(
    kind: ClockKind,
    m: &Fields,
    validated: &field_rules::Validated<'_>,
) -> Result<Temporal> {
    let form = validated.form;
    let sources = &validated.sources;
    Ok(match kind {
        ClockKind::Date => Temporal::Date(date_from_map(m, form, sources.date)?),
        ClockKind::LocalTime => Temporal::LocalTime(time_from_map(m, sources)?),
        ClockKind::Time => {
            let time = time_from_map(m, sources)?;
            // A `time` map has no date fields (`accepts` rejects them), so
            // the date comes from a zoned `time` source or the epoch default.
            let date = match sources.time {
                Some(Temporal::DateTime { utc, offset, .. }) => add_offset(*utc, *offset)?.date(),
                _ => date_from_parts(1970, 1, 1)?,
            };
            let (utc, resolved) = apply_timezone(date.and_time(time), m)?;
            time_wall_clock(utc, resolved.offset)?
        }
        ClockKind::LocalDateTime => Temporal::LocalDateTime(
            date_from_map(m, form, sources.date)?.and_time(time_from_map(m, sources)?),
        ),
        ClockKind::DateTime => {
            let local = date_from_map(m, form, sources.date)?.and_time(time_from_map(m, sources)?);
            let (utc, resolved) = apply_timezone(local, m)?;
            clock::zoned(utc, resolved)
        }
    })
}

/// The one constructor entry: the clock reading (`None`), the value
/// construction (`Some`), or the map policy — the five kinds differ only in
/// the vocabulary the kind supplies.
fn construct(kind: ClockKind, arg: Option<&Value>) -> Result<Temporal> {
    match arg {
        None => Ok(now_value(kind)),
        Some(Value::String(value)) => parse_iso(kind.name(), value),
        // A duration is not a temporal source any clock constructor accepts:
        // Neo4j reports it as an invalid call signature (42001), not as a
        // value-conversion failure.
        Some(Value::Temporal(Temporal::Duration { .. })) => Err(Error::syntax(format!(
            "Invalid call signature: {}() does not accept a duration",
            kind.name()
        ))),
        Some(Value::Temporal(value)) => temporal_of(kind, value),
        Some(Value::Map(m)) => construct_map(kind, m),
        Some(_) => Err(Error::syntax(format!(
            "Invalid call signature: {}()",
            kind.name()
        ))),
    }
}

/// The value conversion each kind applies to an existing temporal (`date(x)`,
/// `time(x)`, …). `datetime` keeps a zoned source verbatim; `time` keeps the
/// source's offset where it carries one. A source without the part a kind
/// needs is a typed rejection (`datetime(date(…))`, `date(localtime(…))`).
fn temporal_of(kind: ClockKind, value: &Temporal) -> Result<Temporal> {
    Ok(match kind {
        ClockKind::Date => Temporal::Date(date_of(value)?),
        ClockKind::Time => {
            let time = time_of(value).ok_or_else(|| Error::ty("cannot convert to time"))?;
            let offset = match value {
                Temporal::Time { offset, .. } | Temporal::DateTime { offset, .. } => *offset,
                _ => 0,
            };
            Temporal::Time { time, offset }
        }
        ClockKind::LocalTime => time_of(value)
            .map(Temporal::LocalTime)
            .ok_or_else(|| Error::ty("cannot convert to localtime"))?,
        // Every dated-and-timed source reads as its local wall clock; a
        // source without both halves is the typed rejection.
        ClockKind::LocalDateTime => match value {
            Temporal::LocalDateTime(_)
            | Temporal::CivilDateTime { .. }
            | Temporal::DateTime { .. } => Temporal::LocalDateTime(local_dt_of(value)?),
            _ => return Err(Error::ty("cannot convert to localdatetime")),
        },
        ClockKind::DateTime => match value {
            Temporal::DateTime { .. } => value.clone(),
            Temporal::LocalDateTime(_) | Temporal::CivilDateTime { .. } => {
                clock::zoned(local_dt_of(value)?, ResolvedZone::utc())
            }
            _ => return Err(Error::ty("cannot convert to datetime")),
        },
    })
}

/// A constructor call with more than the two arguments the grammar
/// defines. The message follows the executor's `invalid_arity` shape
/// (`InvalidNumberOfArguments: <name>() requires <range>`); it is spelled
/// here because core cannot call that helper (the dependency points the
/// other way).
fn too_many_arguments(name: &str, got: usize) -> Error {
    Error::syntax(format!(
        "InvalidNumberOfArguments: {name}() requires zero to two arguments, got {got}"
    ))
}

/// The one name-keyed temporal construction policy: the spelling
/// canonicalization (`local_time` → `localtime`, …), the two-argument
/// `value, pattern` form, and the base constructors — shared by the
/// executor's builtin dispatch, the scan-predicate specialization, and
/// the front-end's literal folding, so no two of them can disagree on
/// what `date("…")` means. Callers passing zero arguments get the clock
/// reading (`QueryClockGuard` pins it per statement).
pub fn construct_named(name: &str, vals: &[Value]) -> Result<Value> {
    let canon = canonical(name);
    match vals {
        // The clock reading: the base constructor with no argument.
        [] => base_value(&canon, None),
        [Value::Null] => Ok(Value::Null),
        [value] => base_value(&canon, Some(value)),
        // CIP-228 `(value, pattern)`: a null on either side constructs null.
        [value, pattern] => match (value, pattern) {
            (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
            (Value::String(value), Value::String(pattern)) => {
                Ok(Value::Temporal(parse_pattern(&canon, value, pattern)?))
            }
            (Value::String(_), _) => Err(Error::invalid_argument_type("temporal pattern")),
            _ => Err(Error::invalid_argument_type("temporal value")),
        },
        // Every other arity is the typed arity rejection, never a silent
        // construction from the first argument.
        _ => Err(too_many_arguments(name, vals.len())),
    }
}

/// The zero/one-argument construction: a clock kind through the one
/// [`construct`] entry, `duration` through its own ISO/map constructor.
fn base_value(canon: &str, arg: Option<&Value>) -> Result<Value> {
    match ClockKind::from_name(canon) {
        Some(kind) => construct(kind, arg).map(Value::Temporal),
        None if canon == "duration" => construct_duration(arg).map(Value::Temporal),
        None => Err(Error::ty("unknown temporal constructor")),
    }
}

pub fn construct_date(arg: Option<&Value>) -> Result<Temporal> {
    construct(ClockKind::Date, arg)
}

/// The calendar date a value reads as. A civil datetime's date is outside
/// chrono's range by construction (that is why it is civil), so the reader
/// answers the authored unreadable-source rejection rather than a generic
/// conversion failure.
pub(super) fn date_of(t: &Temporal) -> Result<NaiveDate> {
    match t {
        Temporal::Date(d) => Ok(*d),
        Temporal::CivilDate { year, month, day } => {
            // `as i32` would wrap extreme civil years into chrono range;
            // convert honestly so only representable years can succeed.
            let year =
                i32::try_from(*year).map_err(|_| Error::ty("civil date out of chrono range"))?;
            NaiveDate::from_ymd_opt(year, *month, *day)
                .ok_or_else(|| Error::ty("civil date out of chrono range"))
        }
        Temporal::LocalDateTime(dt) => Ok(dt.date()),
        Temporal::CivilDateTime { .. } => Err(field_rules::cannot_get_temporal("date", t)),
        Temporal::DateTime { utc, offset, .. } => Ok(add_offset(*utc, *offset)?.date()),
        _ => Err(Error::ty("cannot convert to date")),
    }
}

/// The date a component map names, reading the fields of the form
/// [`field_rules::validate`] resolved — so presence is decided once, and a
/// null-valued form field cannot select a form here that the validator
/// ignored (or vice versa). `source` is the validated `date`/`datetime`
/// source temporal, supplying every form's defaults.
fn date_from_map(
    m: &Fields,
    form: Option<DateForm>,
    source: Option<&Temporal>,
) -> Result<NaiveDate> {
    let source = source.map(date_of).transpose()?;
    match form {
        Some(DateForm::Week) => week_from_map(m, source),
        Some(DateForm::Ordinal) => ordinal_from_map(m, source),
        Some(DateForm::Quarter) => quarter_from_map(m, source),
        Some(DateForm::Calendar) | None => calendar_from_map(m, source),
    }
}

/// The `year` field, else the source date's calendar year, else zero (the
/// documented default of an unset year).
fn year_from_map(m: &Fields, source: Option<NaiveDate>) -> Result<i32> {
    Ok(map_int_bounded(m, "year", "invalid date")?
        .or_else(|| source.map(|d| d.year()))
        .unwrap_or(0))
}

/// The calendar form (`month`/`day` over the source date).
fn calendar_from_map(m: &Fields, source: Option<NaiveDate>) -> Result<NaiveDate> {
    let year = year_from_map(m, source)?;
    let month = map_int_bounded(m, "month", "invalid date")?
        .or_else(|| source.map(|d| d.month()))
        .unwrap_or(1);
    let day = map_int_bounded(m, "day", "invalid date")?
        .or_else(|| source.map(|d| d.day()))
        .unwrap_or(1);
    date_from_parts(year, month, day)
}

/// The week form: the ISO week-year and weekday the source date falls in,
/// overridden by the map's own `year`/`week`/`dayOfWeek`.
fn week_from_map(m: &Fields, source: Option<NaiveDate>) -> Result<NaiveDate> {
    let (default_year, default_week, default_dow) = match source {
        Some(d) => (
            d.iso_week().year(),
            d.iso_week().week(),
            i64::from(d.weekday().number_from_monday()),
        ),
        None => (0, 1, 1),
    };
    let year = map_int_bounded(m, "year", "invalid date")?.unwrap_or(default_year);
    let week = map_int_bounded(m, "week", "invalid date")?.unwrap_or(default_week);
    let dow = map_int_alias(m, DAY_OF_WEEK_KEYS)?.unwrap_or(default_dow);
    date_from_week(year, week, weekday_from_i(dow))
}

/// The ordinal form: `year` plus `ordinalDay`.
fn ordinal_from_map(m: &Fields, source: Option<NaiveDate>) -> Result<NaiveDate> {
    let year = year_from_map(m, source)?;
    // The ordinal form is selected by `ordinalDay` itself, so the non-null
    // integer the validator admitted is what is read here.
    let ordinal = map_int_alias(m, ORDINAL_DAY_KEYS)?
        .expect("the ordinal form implies a non-null ordinalDay");
    let ordinal = u32::try_from(ordinal).map_err(|_| Error::argument_error("invalid date"))?;
    NaiveDate::from_yo_opt(year, ordinal).ok_or_else(|| Error::argument_error("invalid date"))
}

/// The quarter form: an optional `quarter` (defaulting to the source date's
/// month, or January) and an optional `dayOfQuarter`.
fn quarter_from_map(m: &Fields, source: Option<NaiveDate>) -> Result<NaiveDate> {
    let year = year_from_map(m, source)?;
    let default_month = source.map(|d| d.month()).unwrap_or(1);
    let quarter = map_int(m, "quarter")?.unwrap_or_else(|| i64::from((default_month - 1) / 3 + 1));
    // Quarter-of-year math on a user-controlled i64: compute in i128 and
    // error on anything out of range instead of wrapping a u32. A quarter
    // outside 1..=4, or `quarter: 0` (whose first month is not a month),
    // reaches the typed `invalid date` error — never a silent clamp.
    let quarter_month = |moq: u32| -> Result<u32> {
        let month = (i128::from(quarter) - 1) * MONTHS_PER_QUARTER + i128::from(moq);
        u32::try_from(month).map_err(|_| Error::argument_error("invalid date"))
    };
    let Some(day_of_quarter) = map_int_alias(m, DAY_OF_QUARTER_KEYS)? else {
        // The month within the quarter keeps the source date's position, and
        // its day of month carries over unchanged.
        let moq = (default_month.saturating_sub(1) % 3) + 1;
        let day = source.map(|d| d.day()).unwrap_or(1);
        return date_from_parts(year, quarter_month(moq)?, day);
    };
    let dq = u32::try_from(day_of_quarter)
        .ok()
        .filter(|dq| *dq >= 1)
        .ok_or_else(|| Error::argument_error("invalid date"))?;
    let start = date_from_parts(year, quarter_month(1)?, 1)?;
    start
        .checked_add_days(Days::new(u64::from(dq - 1)))
        .ok_or_else(|| Error::argument_error("invalid date"))
}

fn time_from_map(m: &Fields, sources: &Sources<'_>) -> Result<NaiveTime> {
    // An embedded `time`/`datetime` value supplies the base clock; a map
    // without one starts at midnight.
    let base = sources.time.and_then(time_of).unwrap_or(NaiveTime::MIN);
    time_from_map_with_base(base, m)
}

/// The time of day a component map names, overridden field by field on top
/// of `base`. One override block shared by construction (base midnight or an
/// embedded temporal) and truncation (base the truncated input's time).
pub(super) fn time_from_map_with_base(base: NaiveTime, m: &Fields) -> Result<NaiveTime> {
    let hour = map_int_bounded(m, "hour", "invalid time")?.unwrap_or(base.hour());
    let minute = map_int_bounded(m, "minute", "invalid time")?.unwrap_or(base.minute());
    let second = map_int_bounded(m, "second", "invalid time")?.unwrap_or(base.second());
    let nano = nano_with_subsecond_fields(base.nanosecond(), m)?;
    NaiveTime::from_hms_nano_opt(hour, minute, second, nano)
        .ok_or_else(|| Error::argument_error("invalid time"))
}

/// The upper bound (exclusive) each sub-second field accepts in the map
/// constructors. The finer fields' bounds narrow when a coarser sibling is
/// present: `millisecond` always stops at 1000, `microsecond` at 1000 when
/// milliseconds are supplied (else 1_000_000), and `nanosecond` at 1000 when
/// microseconds are supplied, else 1_000_000 when milliseconds are, else
/// 1_000_000_000. That is Neo4j's `validNano` rule, and the one place it is
/// stated.
fn subsecond_bound(coarser_present: bool, when_coarser: i64, otherwise: i64) -> i64 {
    if coarser_present {
        when_coarser
    } else {
        otherwise
    }
}

/// Sum the subsecond map fields (`millisecond`/`microsecond`/`nanosecond`)
/// into a nanosecond count, validating each field against its own range
/// ([`subsecond_bound`]). A negative or over-range field is Neo4j's 22N03
/// numeric-out-of-range error, never a wrapped contribution. The accumulation
/// stays in i128 because the three i64 fields are user-controlled.
fn nano_with_subsecond_fields(base: u32, m: &Fields) -> Result<u32> {
    let millisecond = map_int(m, "millisecond")?;
    let microsecond = map_int(m, "microsecond")?;
    let nanosecond = map_int(m, "nanosecond")?;
    let check = |field: &str, value: i64, bound: i64| -> Result<i64> {
        if (0..bound).contains(&value) {
            Ok(value)
        } else {
            Err(Error::argument_error(format!(
                "specified numeric value out of range: expected {field} in the range 0 to {} but found {value}",
                bound - 1
            )))
        }
    };
    let mut total = i128::from(base);
    if let Some(ms) = millisecond {
        total += i128::from(check("millisecond", ms, 1_000)?) * 1_000_000;
    }
    if let Some(us) = microsecond {
        let bound = subsecond_bound(millisecond.is_some(), 1_000, 1_000_000);
        total += i128::from(check("microsecond", us, bound)?) * 1_000;
    }
    if let Some(ns) = nanosecond {
        let millis_bound = subsecond_bound(millisecond.is_some(), 1_000_000, 1_000_000_000);
        let bound = subsecond_bound(microsecond.is_some(), 1_000, millis_bound);
        total += i128::from(check("nanosecond", ns, bound)?);
    }
    let nanos = u32::try_from(total).map_err(|_| Error::argument_error("invalid time"))?;
    // Chrono encodes a leap second as second 59 with a fraction >= 1e9 (up
    // to 1_999_999_999) — the encoding `truncate_time` and its test
    // deliberately preserve. The base can carry that encoding (an embedded
    // time or an extras map reading an existing value); the user fields
    // cannot reach 1e9 on their own (each component check bounds them and
    // the bounds tighten when a coarser field is present), so the full
    // representable range round-trips and only a genuinely over-range sum
    // errors.
    if nanos >= 2_000_000_000 {
        return Err(Error::argument_error("invalid time"));
    }
    Ok(nanos)
}

/// The time of day a value reads as, or `None` when it carries none. The
/// proleptic civil datetime answers its own wall clock, the same value
/// [`Temporal::has_time`] admits.
pub(super) fn time_of(t: &Temporal) -> Option<NaiveTime> {
    match t {
        Temporal::LocalTime(t) | Temporal::Time { time: t, .. } => Some(*t),
        Temporal::LocalDateTime(dt) => Some(dt.time()),
        Temporal::CivilDateTime { time, .. } => Some(*time),
        Temporal::DateTime { utc, offset, .. } => utc
            .checked_add_signed(ChronoDur::seconds(i64::from(*offset)))
            .map(|local| local.time()),
        _ => None,
    }
}

pub fn construct_localtime(arg: Option<&Value>) -> Result<Temporal> {
    construct(ClockKind::LocalTime, arg)
}

pub fn construct_time(arg: Option<&Value>) -> Result<Temporal> {
    construct(ClockKind::Time, arg)
}

/// The wall-clock `Temporal::Time` of the UTC instant `utc` read at
/// `offset` — the shared tail of `construct_time`'s epoch and field-map
/// paths (F-1-20).
fn time_wall_clock(utc: NaiveDateTime, offset: i32) -> Result<Temporal> {
    let wall = add_offset(utc, offset)?;
    Ok(Temporal::Time {
        time: wall.time(),
        offset,
    })
}

pub fn construct_localdatetime(arg: Option<&Value>) -> Result<Temporal> {
    construct(ClockKind::LocalDateTime, arg)
}

pub(super) fn local_dt_of(t: &Temporal) -> Result<NaiveDateTime> {
    match t {
        Temporal::LocalDateTime(dt) => Ok(*dt),
        Temporal::Date(d) => Ok(d.and_time(NaiveTime::MIN)),
        Temporal::DateTime { utc, offset, .. } => add_offset(*utc, *offset),
        _ => Err(Error::ty("cannot convert to localdatetime")),
    }
}

pub fn construct_datetime(arg: Option<&Value>) -> Result<Temporal> {
    construct(ClockKind::DateTime, arg)
}

pub fn construct_duration(arg: Option<&Value>) -> Result<Temporal> {
    match arg {
        Some(Value::String(s)) => parse_iso("duration", s),
        Some(Value::Temporal(t @ Temporal::Duration { .. })) => Ok(t.clone()),
        Some(Value::Map(m)) => duration_from_map(m),
        _ => Err(Error::syntax("Invalid call signature: duration()")),
    }
}

/// The duration a component map names. Every map key that names a duration
/// field contributes through the one field table (`DurationField::from_map_key`);
/// null fields are ignored, as Neo4j filters them before constructing, and a
/// map with no non-null field names no unit (22N30). Integer-only maps take
/// the exact i64 lane arithmetic; any float sends the whole map through the
/// approximate f64 cascade, matching Neo4j's all-integral-values split.
fn duration_from_map(m: &Fields) -> Result<Temporal> {
    let mut values: std::collections::BTreeMap<DurationField, ExactNumber> =
        std::collections::BTreeMap::new();
    for (key, value) in m.iter().filter(|(_, value)| !value.is_null()) {
        // A non-null field no duration unit names is Neo4j's
        // `Unknown field: foo`; a null one was filtered above, like every
        // other null field.
        let field = DurationField::from_map_key(key)
            .ok_or_else(|| Error::ty(format!("Unknown field: {key}")))?;
        values.insert(field, exact_number(key, value)?);
    }
    if values.is_empty() {
        return Err(field_rules::at_least_one_unit());
    }
    if values.values().all(ExactNumber::is_int) {
        return integral_duration(&values);
    }
    let mut tiers = DurationTiers::default();
    for (field, number) in &values {
        let (accumulator, scale) = field.scaled(&mut tiers);
        accumulator.add(*number, scale)?;
    }
    let (months, days, nanos) = tiers.finish()?;
    Temporal::duration_checked(months, days, nanos)
}

/// Build a duration from integer map fields through Neo4j's lane arithmetic:
/// years/months, weeks/days, hours/minutes/seconds and the three sub-second
/// fields each sum in one lane, then each lane is narrowed to i64 (22N28 on
/// overflow) and the assembled value goes through [`Temporal::duration_checked`],
/// the one representability seam every producer shares. Sub-second nanos fold
/// into the seconds lane for that bound; the stored value keeps the exact i128
/// nanosecond total.
fn integral_duration(
    values: &std::collections::BTreeMap<DurationField, ExactNumber>,
) -> Result<Temporal> {
    // Every lane is summed from the one field table
    // ([`DurationField::lane_and_unit`], walked in cascade order), so a
    // field added to the vocabulary cannot be dropped from a lane. The
    // all-integral guard makes every present field an Int; an absent field
    // contributes zero. The nanosecond tier splits at whole seconds: its
    // seconds lane counts in seconds, its sub-second lane in nanoseconds.
    let (mut months, mut days, mut seconds, mut subsecond) = (0i128, 0i128, 0i128, 0i128);
    for field in DurationField::CASCADE {
        let Some(number) = values.get(&field) else {
            continue;
        };
        let n = number
            .as_int()
            .expect("the all-integral guard makes every present field an Int");
        let (lane, unit) = field.lane_and_unit();
        match lane {
            DurationLane::Months => months += n * unit,
            DurationLane::Days => days += n * unit,
            DurationLane::Seconds => seconds += n * unit,
            DurationLane::SubSecond => subsecond += n * unit,
        }
    }
    let months = checked_lane(months, "duration()")?;
    let days = checked_lane(days, "duration()")?;
    let seconds = checked_lane(seconds, "duration()")?;
    let subsecond = checked_lane(subsecond, "duration()")?;
    Temporal::duration_checked_exact(
        months,
        days,
        i128::from(seconds) * NANOS_PER_SECOND + i128::from(subsecond),
    )
}
