//! ISO-8601 duration parsing: the numeral form (`P1Y2M3DT4H5M6.7S`,
//! including the calendar/colon spellings) built through the exact
//! component arithmetic of [`super::exact`].

use crate::{Error, Result};

use super::arith::negate_fields;
use super::exact::{DurationTiers, ExactAccumulator};
use super::{
    range_err, DurationField, Temporal, MONTHS_PER_YEAR, NANOS_PER_HOUR, NANOS_PER_MINUTE,
    NANOS_PER_SECOND,
};

pub(super) fn parse_duration_iso(s: &str) -> Result<Temporal> {
    let raw = s.trim();
    let (neg, body) = match raw.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, raw.strip_prefix('+').unwrap_or(raw)),
    };
    // Neo4j's ISO duration pattern is case-insensitive (`P5d`, `p1dt2h`), so
    // the body folds once here: the designator, the time marker and the unit
    // letters are plain upper-case matches below and in the scanner, never a
    // per-letter case pair.
    let folded = body.to_ascii_uppercase();
    let Some(body) = folded.strip_prefix('P') else {
        return Err(Error::ty(format!("cannot parse duration '{s}'")));
    };
    // A present-but-empty time half is the dangling `T` of `PT`/`P1YT`, and
    // an entirely empty body (`P`) names no component either. Both used to
    // reach `parse_duration_part` as empty runs — whose guards only fire on a
    // unit letter — and silently produced a zero duration.
    // The ISO grammar is case-insensitive (Neo4j compiles it with
    // CASE_INSENSITIVE), so `PT1h` and `P1dt2h` name the same components as
    // their uppercase spellings.
    let (date, time) = match body.find('T') {
        Some(at) if at + 1 == body.len() => return Err(Error::ty("invalid duration time")),
        Some(at) => (&body[..at], Some(&body[at + 1..])),
        None => (body, None),
    };
    if date.is_empty() && time.is_none() {
        return Err(Error::ty("invalid duration component"));
    }
    let mut tiers = DurationTiers::default();
    if is_datetime_style_duration(date, time) {
        parse_duration_datetime_style(date, time, &mut tiers)?;
    } else {
        let mut fractional = false;
        parse_duration_part(date, Part::Date, &mut tiers, &mut fractional)?;
        if let Some(t) = time {
            parse_duration_part(t, Part::Time, &mut tiers, &mut fractional)?;
        }
    }
    let (mut months, mut days, mut nanos) = tiers.finish()?;
    if neg {
        (months, days, nanos) = negate_fields(months, days, nanos)?;
    }
    // The stored triple's range contract, shared with every duration
    // producer: Neo4j rejects a duration whose average length in seconds
    // does not fit the stored second count (`P3507324295524M`,
    // `PT2562047788015216H`). The ISO spelling is exact, so it also rejects
    // the `2^63`-second image rounding keeps alive for scaling
    // (`PT9223372036854775808S`).
    Temporal::duration_checked_exact(months, days, nanos)
}

/// Which half of the ISO spelling a component run belongs to — the tier
/// vocabulary [`parse_duration_part`] matches on.
#[derive(Clone, Copy)]
enum Part {
    Date,
    Time,
}

/// One scanned duration component: the numeral's sign-applied whole part, its
/// sign-applied `f64` fractional remainder, the unit letter it qualifies, and
/// whether the numeral carried a decimal mark (the ISO grammar's
/// decimal-only-in-the-last-component rule reads the spelling, not the
/// value, so `P1.0Y2.5M` is rejected while `P1Y2.5M` is legal).
struct Component {
    whole: i128,
    frac: f64,
    unit: char,
    decimal: bool,
}

/// The component scanner for one half of an ISO duration. It owns the
/// numeral accumulation, the one-sign-per-numeral rule, and the
/// empty/trailing/dangling validation, so [`parse_duration_part`] keeps only
/// the letter → field match and the accumulator call.
struct ComponentScanner<'a> {
    chars: std::str::Chars<'a>,
    num: String,
    sign: i64,
}

impl<'a> ComponentScanner<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            chars: s.chars(),
            num: String::new(),
            sign: 1,
        }
    }

    /// The next component in the run, or `None` at its end. A unit letter
    /// with no numeral, a repeated sign and a trailing numeral or dangling
    /// sign are typed errors, never silent zeroes or dropped components.
    fn next_component(&mut self) -> Result<Option<Component>> {
        for c in self.chars.by_ref() {
            if c == '-' && self.num.is_empty() {
                // One sign per numeral: a repeated `-` (`P--1Y`, `P1Y--2M`)
                // would otherwise re-assign the same value and silently accept
                // the malformed component.
                if self.sign == -1 {
                    return Err(Error::ty("invalid duration component"));
                }
                self.sign = -1;
                continue;
            }
            if c.is_ascii_digit() || c == '.' {
                self.num.push(c);
                continue;
            }
            // A unit letter with no numeral before it is not a component: an
            // empty run must not fabricate a zero (`PY`).
            if self.num.is_empty() {
                return Err(Error::ty("invalid duration component"));
            }
            let (whole, frac) = split_duration_numeral(&self.num)?;
            let decimal = self.num.contains('.');
            self.num.clear();
            // The grammar applies the numeral's sign once, here; the
            // accumulator then takes signed values only (the same convention
            // as `add`).
            let whole = whole
                .checked_mul(i128::from(self.sign))
                .ok_or_else(range_err)?;
            let frac = frac * self.sign as f64;
            self.sign = 1;
            return Ok(Some(Component {
                whole,
                frac,
                unit: c,
                decimal,
            }));
        }
        // Only a unit letter flushes a component. A trailing numeral with no
        // unit (`P12`, `PT5H12`) is not a component and must not be dropped,
        // and neither is a dangling sign (`PT-`, `P1Y-`): a sign only ever
        // qualifies a numeral that follows it, so `sign != 1` here means one
        // was never consumed.
        if !self.num.is_empty() || self.sign != 1 {
            return Err(Error::ty("invalid duration component"));
        }
        Ok(None)
    }
}

/// Parse one half's component run. `fractional` is the run-spanning flag:
/// the ISO grammar allows a decimal numeral only in the duration's final
/// component (`P1Y1.5M` is not a duration; `PT1H1.5M` is). Neo4j rejects the
/// former with a parse failure under 22007, so the whole spelling is a typed
/// `ArgumentError` here.
fn parse_duration_part(
    s: &str,
    part: Part,
    tiers: &mut DurationTiers,
    fractional: &mut bool,
) -> Result<()> {
    let mut scanner = ComponentScanner::new(s);
    while let Some(component) = scanner.next_component()? {
        if *fractional {
            return Err(Error::argument_error(
                "Text cannot be parsed to a Duration: a decimal is only allowed in the last component",
            ));
        }
        *fractional = component.decimal;
        // Integer parts stay exact (checked i128): a component above 2^53
        // must keep every digit, or `PT9223372036854775807S` renders
        // minutes/seconds off by one. Only the fraction goes through f64 —
        // it feeds the sub-day remainder, below the nanosecond resolution.
        // The ISO spelling's thin letter→field match; the field's tier and
        // scale are the one [`DurationField`] table's facts. The fraction
        // parks in the month tier and [`DurationTiers`]'s cascade owns the
        // month -> day carry, the same path the map constructor's
        // `years`/`months` fields take. The body is already upper-cased by
        // [`parse_duration_iso`], and the half (`Part`) still decides M:
        // months in the date half, minutes in the time one.
        let field = match (component.unit, part) {
            ('Y', Part::Date) => DurationField::Years,
            ('M', Part::Date) => DurationField::Months,
            ('W', Part::Date) => DurationField::Weeks,
            ('D', Part::Date) => DurationField::Days,
            ('H', Part::Time) => DurationField::Hours,
            ('M', Part::Time) => DurationField::Minutes,
            ('S', Part::Time) => DurationField::Seconds,
            _ => return Err(Error::ty("invalid duration component")),
        };
        let (accumulator, scale) = field.scaled(tiers);
        accumulator.add_unit(component.whole, component.frac, scale)?;
    }
    Ok(())
}

/// Split a duration component numeral (`"12"`, `"1.5"`, `".5"`, `"1."`)
/// into its exact integer part and its `f64` fractional remainder.
pub(super) fn split_duration_numeral(num: &str) -> Result<(i128, f64)> {
    let (whole, frac) = num.split_once('.').unwrap_or((num, ""));
    let whole = if whole.is_empty() {
        0
    } else {
        whole.parse::<i128>().map_err(|_| range_err())?
    };
    let frac = if frac.is_empty() {
        0.0
    } else {
        format!("0.{frac}")
            .parse::<f64>()
            .map_err(|_| Error::ty("invalid duration component"))?
    };
    Ok((whole, frac))
}

/// The shape test both datetime-style halves share: a run of digits, that
/// half's separator, and dots (the ISO decimal mark). A separator-bearing
/// string is non-empty by construction.
fn metadata_style(s: &str, sep: char) -> bool {
    s.contains(sep)
        && s.chars()
            .all(|c| c.is_ascii_digit() || c == sep || c == '.')
}

fn is_datetime_style_duration(date: &str, time: Option<&str>) -> bool {
    metadata_style(date, '-') || time.is_some_and(|t| metadata_style(t, ':'))
}

fn parse_duration_datetime_style(
    date: &str,
    time: Option<&str>,
    tiers: &mut DurationTiers,
) -> Result<()> {
    if !date.is_empty() {
        parse_duration_date(date, &mut tiers.months, &mut tiers.days)?;
    }
    if let Some(t) = time {
        let parts: Vec<&str> = t.split(':').collect();
        // Same exact-integer policy as [`parse_duration_part`]: the
        // colon-separated fields carry their whole part through i128
        // arithmetic, only the fraction through f64. The scales come from
        // the one constant table, never restated here.
        const TIME_FIELDS: [i128; 3] = [NANOS_PER_HOUR, NANOS_PER_MINUTE, NANOS_PER_SECOND];
        // A fourth field has no visit slot in the loop below, so without
        // this guard it would be silently dropped (`…T14:37:21:99`). A
        // shorter half stays legitimate.
        if parts.len() > TIME_FIELDS.len() {
            return Err(Error::ty("invalid duration time"));
        }
        for (index, scale) in TIME_FIELDS.iter().enumerate() {
            let Some(field) = parts.get(index) else {
                break;
            };
            if field.is_empty() {
                return Err(Error::ty("invalid duration time"));
            }
            let (whole, frac) =
                split_duration_numeral(field).map_err(|_| Error::ty("invalid duration time"))?;
            tiers.nanos.add_unit(whole, frac, *scale)?;
        }
    }
    Ok(())
}

/// The date half of the dash-separated datetime-style spelling
/// (`2012-02-02` = year-month-day), parsed through the same `Result`
/// discipline as the colon-separated time half: an unparseable field is a
/// typed error, never a silent drop that loses the day.
fn parse_duration_date(
    date: &str,
    months: &mut ExactAccumulator,
    days: &mut ExactAccumulator,
) -> Result<()> {
    let mut fields = date.split('-');
    if let Some(year) = fields.next() {
        months.add_whole(parse_duration_date_field(year)?, MONTHS_PER_YEAR)?;
    }
    if let Some(month) = fields.next() {
        months.add_whole(parse_duration_date_field(month)?, 1)?;
    }
    if let Some(day) = fields.next() {
        days.add_whole(parse_duration_date_field(day)?, 1)?;
    }
    // The date half is exactly three fields; a fourth (`P2012-02-02-04`)
    // has no slot and must not be dropped.
    if fields.next().is_some() {
        return Err(Error::ty("invalid duration date"));
    }
    Ok(())
}

fn parse_duration_date_field(field: &str) -> Result<i128> {
    field
        .parse::<i128>()
        .map_err(|_| Error::ty("invalid duration date"))
}
