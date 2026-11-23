//! The duration-component arithmetic shared by the ISO numeral parser
//! ([`super::duration`]), the map constructor ([`super::construct`]), the
//! duration-pattern parser ([`super::duration_pattern`]) and duration
//! scaling ([`super::arith`]): a checked `i128` integer sum plus an `f64`
//! fractional remainder, folded into the stored triple by
//! [`DurationTiers::finish`] (Neo4j's `DurationValue.approximate` chain).
//!
//! The integer sum keeps components above 2^53 exact; the fold's `long`
//! casts truncate, dropping a sub-nanosecond remainder instead of rounding
//! it.

use crate::{Error, Result, Value};

use super::{
    f64_to_i128_range, f64_to_i64_range, range_err, AVG_NANOS_PER_MONTH, NANOS_PER_DAY,
    NANOS_PER_SECOND,
};

/// One duration field's number, keeping an integer exact: `as_number`
/// promotes an `Int` to `f64` (losing every digit above 2^53), which
/// `duration({seconds: 9223372036854775807})` must not do.
#[derive(Clone, Copy)]
pub(super) enum ExactNumber {
    Int(i128),
    Float(f64),
}

impl ExactNumber {
    /// The exact integer contribution, or `None` for a float field — the map
    /// constructor's all-integral split collects these.
    pub(super) fn as_int(&self) -> Option<i128> {
        match self {
            Self::Int(value) => Some(*value),
            Self::Float(_) => None,
        }
    }
}

/// One duration-map entry's number as an [`ExactNumber`]: a field present
/// but not numeric is the typed argument error the constructor uses —
/// never a silent zero contribution (`duration({seconds: '60'})` must not
/// answer `PT0S`).
pub(super) fn exact_number(key: &str, value: &Value) -> Result<ExactNumber> {
    match value {
        Value::Int(v) => Ok(ExactNumber::Int(i128::from(*v))),
        Value::Float(v) => Ok(ExactNumber::Float(*v)),
        _ => Err(Error::argument_error(format!(
            "duration field `{key}` must be a number"
        ))),
    }
}

/// A duration-component accumulator: an exact integer sum plus an `f64`
/// fractional remainder. [`DurationTiers::finish`] folds the tiers into the
/// stored triple.
#[derive(Default)]
pub(super) struct ExactAccumulator {
    whole: i128,
    frac: f64,
}

impl ExactAccumulator {
    /// Add one field's number under its unit `scale`.
    pub(super) fn add(&mut self, number: ExactNumber, scale: i128) -> Result<()> {
        match number {
            ExactNumber::Int(v) => {
                add_exact_nanos(&mut self.whole, v.checked_mul(scale).ok_or_else(range_err)?)
            }
            ExactNumber::Float(v) => self.add_float(v * scale as f64),
        }
    }

    /// Add one parsed numeral's whole part: `whole * scale` exactly. The
    /// grammar applies the numeral's sign before the call, so every add
    /// path here takes a signed value (the same convention as [`Self::add`]).
    pub(super) fn add_whole(&mut self, whole: i128, scale: i128) -> Result<()> {
        add_exact_nanos(
            &mut self.whole,
            whole.checked_mul(scale).ok_or_else(range_err)?,
        )
    }

    /// [`Self::add_whole`] for a pattern component, after the stored-width
    /// check: Neo4j's pattern parser scales each component in a `long`, so
    /// `whole * lane_unit` (the field's unit in its storage lane) must fit
    /// `i64` before the component is summed.
    pub(super) fn add_whole_in_lane(
        &mut self,
        whole: i64,
        scale: i128,
        lane_unit: i128,
    ) -> Result<()> {
        let width = i64::try_from(lane_unit).expect("every lane unit fits i64");
        whole.checked_mul(width).ok_or_else(range_err)?;
        self.add_whole(i128::from(whole), scale)
    }

    /// Add one parsed numeral component: [`Self::add_whole`] for the integer
    /// part, plus the fractional part `frac * scale` through
    /// [`Self::add_float`] — the one unit helper (hour/minute/second and the
    /// week/day tiers alike), with the `f64` scale derived from the integer.
    pub(super) fn add_unit(&mut self, whole: i128, frac: f64, scale: i128) -> Result<()> {
        self.add_whole(whole, scale)?;
        self.add_float(frac * scale as f64)
    }

    /// Add an already-scaled value, splitting it into whole units and a
    /// fractional remainder.
    pub(super) fn add_float(&mut self, value: f64) -> Result<()> {
        let whole_part = value.trunc();
        add_exact_nanos(&mut self.whole, f64_to_i128_range(whole_part)?)?;
        self.frac += value - whole_part;
        Ok(())
    }

    /// The accumulated whole units, range-checked to the `i64` the duration
    /// model stores.
    pub(super) fn whole_i64(&self) -> Result<i64> {
        i64::try_from(self.whole).map_err(|_| range_err())
    }
}

/// Checked addition into an `i128` component accumulator.
pub(super) fn add_exact_nanos(dst: &mut i128, delta: i128) -> Result<()> {
    *dst = dst.checked_add(delta).ok_or_else(range_err)?;
    Ok(())
}

/// The tiers a duration accumulates into. The ISO numeral parser
/// ([`super::duration`]), the map constructor ([`super::construct`]), the
/// duration-pattern parser ([`super::duration_pattern`]) and duration
/// scaling ([`super::arith`]) share one fold, [`Self::finish`], so the stored
/// triple is assembled one way. The nanosecond tier is two lanes, as in
/// Neo4j's `DurationValue`: `seconds` (hours, minutes and seconds) and
/// `sub_second` (milliseconds, microseconds and nanoseconds, the pattern's
/// fraction digits, and scaled stored nanoseconds). Both count in
/// nanoseconds.
#[derive(Default)]
pub(super) struct DurationTiers {
    pub(super) months: ExactAccumulator,
    pub(super) days: ExactAccumulator,
    pub(super) seconds: ExactAccumulator,
    pub(super) sub_second: ExactAccumulator,
}

/// Whole seconds from which the seconds lane no longer fits an `f64`
/// mantissa (2^53): below it the combined-nanosecond fold is exact for
/// every value the corpus pins, from here Neo4j's per-lane double
/// arithmetic is mirrored.
const LANE_MANTISSA_SECONDS: i128 = 1 << 53;

/// `2^63` as an `f64`: the value `i64::MAX` rounds to, which Java's
/// saturating `(long)` cast sends back to `i64::MAX`.
const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;

impl DurationTiers {
    /// The stored `(months, days, nanos)` triple, through Neo4j's
    /// `DurationValue.approximate` fold: the month fraction becomes
    /// nanoseconds and joins the day tier, the day fraction joins the
    /// nanosecond tier, and every tier is cast to `long` — truncated toward
    /// zero, never rounded. Pure-integer components take the exact lane
    /// (Neo4j's all-integral split), so counts above 2^53 keep every digit
    /// while a sub-nanosecond remainder is dropped
    /// (`duration({nanoseconds: 1.9})` is `PT0.000000001S`).
    ///
    /// A fractional duration whose seconds lane holds 2^53 seconds or more
    /// folds the lanes apart as Neo4j does (REL8-1164): the seconds lane is
    /// one `double` (ties-to-even, `2^63` saturating to `i64::MAX`) cast to
    /// `long` on its own, and the sub-second lane's nanoseconds join it
    /// only through the seconds remainder.
    pub(super) fn finish(self) -> Result<(i64, i64, i128)> {
        let fractional = self.months.frac != 0.0
            || self.days.frac != 0.0
            || self.seconds.frac != 0.0
            || self.sub_second.frac != 0.0;
        let whole_nanos = self
            .seconds
            .whole
            .checked_add(self.sub_second.whole)
            .ok_or_else(range_err)?;
        if !fractional {
            return Ok((
                self.months.whole_i64()?,
                self.days.whole_i64()?,
                whole_nanos,
            ));
        }
        let months = self.months.whole as f64 + self.months.frac;
        let months_whole = months.trunc();
        let month_nanos = AVG_NANOS_PER_MONTH * (months - months_whole);
        let days = self.days.whole as f64 + self.days.frac + month_nanos / NANOS_PER_DAY as f64;
        let days_whole = days.trunc();
        let day_nanos = NANOS_PER_DAY as f64 * (days - days_whole);
        let seconds = day_nanos / NANOS_PER_SECOND as f64;
        let total_nanos = if self.seconds.whole.abs() / NANOS_PER_SECOND >= LANE_MANTISSA_SECONDS {
            self.fold_lanes_apart(seconds)?
        } else {
            let seconds_whole = seconds.trunc();
            let second_nanos = NANOS_PER_SECOND as f64 * (seconds - seconds_whole);
            let nanos =
                whole_nanos as f64 + (self.seconds.frac + self.sub_second.frac) + second_nanos;
            let nanos_whole = f64_to_i128_range(nanos.trunc())?;
            f64_to_i128_range(seconds_whole)?
                .checked_mul(NANOS_PER_SECOND)
                .and_then(|total| total.checked_add(nanos_whole))
                .ok_or_else(range_err)?
        };
        Ok((
            f64_to_i64_range(months_whole)?,
            f64_to_i64_range(days_whole)?,
            total_nanos,
        ))
    }

    /// The total nanoseconds with the seconds lane folded as one `double`
    /// (`day_seconds` is the day fraction's seconds), the way Neo4j's
    /// `approximate` does: the lane's whole seconds are rounded to the
    /// nearest representable `double` before the `long` cast, and the
    /// sub-second lane's nanoseconds are cast on their own.
    fn fold_lanes_apart(&self, day_seconds: f64) -> Result<i128> {
        let lane_seconds = self.seconds.whole / NANOS_PER_SECOND;
        let lane_rest = self.seconds.whole % NANOS_PER_SECOND;
        let seconds = lane_seconds as f64
            + (lane_rest as f64 + self.seconds.frac) / NANOS_PER_SECOND as f64
            + day_seconds;
        let seconds_whole = seconds.trunc();
        let second_nanos = NANOS_PER_SECOND as f64 * (seconds - seconds_whole);
        let nanos = self.sub_second.whole as f64 + self.sub_second.frac + second_nanos;
        let seconds_whole = if seconds_whole == TWO_POW_63 {
            i128::from(i64::MAX)
        } else {
            f64_to_i128_range(seconds_whole)?
        };
        seconds_whole
            .checked_mul(NANOS_PER_SECOND)
            .and_then(|total| total.checked_add(f64_to_i128_range(nanos.trunc()).ok()?))
            .ok_or_else(range_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The month fraction becomes days, the day fraction becomes
    /// nanoseconds, and each tier truncates: `duration({months: 1.5})` is
    /// `P1M15DT5H14M33S` (live Neo4j 2026.10.1).
    #[test]
    fn float_components_fold_down_tiers() {
        let mut tiers = DurationTiers::default();
        tiers.months.add(ExactNumber::Float(1.5), 1).unwrap();
        let (months, days, nanos) = tiers.finish().unwrap();
        assert_eq!((months, days), (1, 15));
        assert_eq!(nanos, (5 * 3600 + 14 * 60 + 33) * 1_000_000_000);
    }

    /// The sub-nanosecond remainder is dropped, as Neo4j's `(long)` cast
    /// does — rounding it would answer `PT0.000000002S` for
    /// `duration({nanoseconds: 1.9})`.
    #[test]
    fn nanosecond_fold_truncates_like_neo4j_long_cast() {
        let mut tiers = DurationTiers::default();
        tiers.sub_second.add_float(1.9).unwrap();
        assert_eq!(tiers.finish().unwrap().2, 1);
    }

    #[test]
    fn exact_integers_above_f64_precision_survive() {
        let mut tiers = DurationTiers::default();
        tiers
            .seconds
            .add(ExactNumber::Int(i128::from(i64::MAX)), NANOS_PER_SECOND)
            .unwrap();
        assert_eq!(
            tiers.finish().unwrap().2,
            i128::from(i64::MAX) * 1_000_000_000
        );
    }
}
