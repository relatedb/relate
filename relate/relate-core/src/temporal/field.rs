//! The duration field vocabulary: the unit a duration symbol or map field
//! names, the accumulator tier it lands in, and its unit scale. One fact
//! base — [`DurationField::tier_and_scale`] — states the tier and scale of
//! every field; the ISO numeral parser, the CIP-223 pattern parser and
//! formatter, and the map constructor keep only their own spelling over
//! those facts.

use super::exact;

/// The duration unit scales, spelled once. Every site that needs them (the
/// ISO numeral parser, the map constructor, the truncation quanta and the
/// CIP-223 formatter) reads these; a site needing the `f64` view casts the
/// same constant instead of restating the literal.
pub(super) const MONTHS_PER_YEAR: i128 = 12;
pub(super) const MONTHS_PER_QUARTER: i128 = 3;
pub(super) const DAYS_PER_WEEK: i128 = 7;
pub(super) const NANOS_PER_DAY: i128 = 24 * NANOS_PER_HOUR;
pub(super) const NANOS_PER_HOUR: i128 = 3_600_000_000_000;
pub(super) const NANOS_PER_MINUTE: i128 = 60_000_000_000;
pub(super) const NANOS_PER_SECOND: i128 = 1_000_000_000;
pub(super) const NANOS_PER_MILLI: i128 = 1_000_000;
pub(super) const NANOS_PER_MICRO: i128 = 1_000;
/// Whole-second views of the minute/hour scales, derived from the nanosecond
/// constants (the map constructor's and the formatter's lane arithmetic work
/// in seconds and milliseconds).
pub(super) const SECONDS_PER_MINUTE: i128 = NANOS_PER_MINUTE / NANOS_PER_SECOND;
pub(super) const SECONDS_PER_HOUR: i128 = NANOS_PER_HOUR / NANOS_PER_SECOND;

/// Neo4j's average month length in nanoseconds, the constant its
/// `DurationValue.approximate` fold turns a fractional month into
/// nanoseconds with (`2.629746E15d`) — [`AVG_SECONDS_PER_MONTH`] in
/// nanoseconds, a relation a test pins.
pub(super) const AVG_NANOS_PER_MONTH: f64 = AVG_SECONDS_PER_MONTH as f64 * NANOS_PER_SECOND as f64;

/// Neo4j's average month length in seconds, the factor its duration overflow
/// guard multiplies the month field by (`DurationValue.assertNoOverflow` via
/// `TemporalUtil.AVG_SECONDS_PER_MONTH`). Declared once — [`AVG_NANOS_PER_MONTH`]
/// is its nanosecond image, a relation a test pins — so the fold and the
/// guard cannot drift apart.
pub(super) const AVG_SECONDS_PER_MONTH: i128 = 2_629_746;

/// The largest `seconds` magnitude a duration accepts. The sub-day part is a
/// pair of `i64` `seconds` and `[0, 10^9)` `nanos` fields in Neo4j, but the
/// scaling path rounds its product through `f64` before the cast, and `2^63`
/// (the `f64` image of `i64::MAX`) still casts back to `i64::MAX` — so the
/// accepted bound is inclusive of `2^63`.
pub(super) const MAX_DURATION_SECONDS: i128 = 1 << 63;

/// One duration component field: the unit a duration symbol or map field
/// names, independent of the tier it accumulates into (years and months are
/// distinct fields in the same tier). The declaration order is the
/// formatter's cascade order — largest unit first — and it groups the
/// fields by tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum DurationField {
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
}

/// The accumulator tier a duration field lands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DurationTier {
    Months,
    Days,
    Nanos,
}

/// The storage lane a duration field sums in under Neo4j's lane arithmetic:
/// the nanosecond tier splits at whole seconds, its time-of-day fields
/// counting in seconds and its sub-second fields in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DurationLane {
    Months,
    Days,
    Seconds,
    SubSecond,
}

impl DurationLane {
    /// The size of the lane index space (`lane as usize`), derived from the
    /// last declared lane so it cannot drift from the enum.
    pub(super) const COUNT: usize = Self::SubSecond as usize + 1;
}

/// A duration field's landing slot (see [`DurationField::slot`]): the
/// accumulator its components sum into and the unit scale of that sum.
pub(super) struct FieldSlot<'a> {
    pub(super) accumulator: &'a mut exact::ExactAccumulator,
    pub(super) scale: i128,
}

/// One resolved CIP-223 duration-pattern spelling: either a duration field
/// (rendered as that field's cascade share) or the sub-second fraction
/// digits. The pattern alphabet resolves each letter to this once, so the
/// tokenizer, the token's field projection and the renderer never re-decide
/// what a letter means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum PatternSymbol {
    Field(DurationField),
    /// Sub-second fraction digits (`n`, `S`) — not a field's cascade share.
    FractionDigits,
}

impl PatternSymbol {
    /// The duration field this symbol names. Fraction digits are the
    /// nanosecond field by definition, so a token carries the field without
    /// a second spelling match.
    pub(super) fn field(self) -> DurationField {
        match self {
            Self::Field(field) => field,
            Self::FractionDigits => DurationField::Nanos,
        }
    }
}

impl DurationField {
    /// Every field in declaration order (largest unit first, grouped by
    /// tier) — the formatter's cascade order and the vocabulary's iteration
    /// order. `tier_and_scale` states each field's tier; this list only
    /// states the order, and the two are checked against each other by the
    /// field-table tests.
    pub(super) const CASCADE: [Self; 11] = [
        Self::Years,
        Self::Quarters,
        Self::Months,
        Self::Weeks,
        Self::Days,
        Self::Hours,
        Self::Minutes,
        Self::Seconds,
        Self::Millis,
        Self::Micros,
        Self::Nanos,
    ];

    /// The duration-map spelling of this field — the inverse of
    /// [`Self::from_map_key`] over the map-admitted vocabulary (every
    /// field but `Quarters`, which the map rejects and the pattern and
    /// accessor keep, REL8-1063) — so a checked-lane overflow can name
    /// its field without a per-call-site string.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Years => "years",
            Self::Quarters => "quarters",
            Self::Months => "months",
            Self::Weeks => "weeks",
            Self::Days => "days",
            Self::Hours => "hours",
            Self::Minutes => "minutes",
            Self::Seconds => "seconds",
            Self::Millis => "milliseconds",
            Self::Micros => "microseconds",
            Self::Nanos => "nanoseconds",
        }
    }

    /// The field's `(accumulator tier, unit scale)` — the one duration table,
    /// exhaustive so the compiler enforces a row per variant.
    pub(super) fn tier_and_scale(self) -> (DurationTier, i128) {
        match self {
            Self::Years => (DurationTier::Months, MONTHS_PER_YEAR),
            Self::Quarters => (DurationTier::Months, MONTHS_PER_QUARTER),
            Self::Months => (DurationTier::Months, 1),
            Self::Weeks => (DurationTier::Days, DAYS_PER_WEEK),
            Self::Days => (DurationTier::Days, 1),
            Self::Hours => (DurationTier::Nanos, NANOS_PER_HOUR),
            Self::Minutes => (DurationTier::Nanos, NANOS_PER_MINUTE),
            Self::Seconds => (DurationTier::Nanos, NANOS_PER_SECOND),
            Self::Millis => (DurationTier::Nanos, NANOS_PER_MILLI),
            Self::Micros => (DurationTier::Nanos, NANOS_PER_MICRO),
            Self::Nanos => (DurationTier::Nanos, 1),
        }
    }

    /// The field's `(storage lane, unit in that lane)`, derived from
    /// [`Self::tier_and_scale`]: the one place the nanosecond tier's
    /// seconds/sub-second split lives. The unit is also the width a
    /// pattern component is checked against in `i64`.
    pub(super) fn lane_and_unit(self) -> (DurationLane, i128) {
        let (tier, scale) = self.tier_and_scale();
        match tier {
            DurationTier::Months => (DurationLane::Months, scale),
            DurationTier::Days => (DurationLane::Days, scale),
            DurationTier::Nanos if scale >= NANOS_PER_SECOND => {
                (DurationLane::Seconds, scale / NANOS_PER_SECOND)
            }
            DurationTier::Nanos => (DurationLane::SubSecond, scale),
        }
    }

    /// Where a component of this field lands: its accumulator in `tiers`
    /// and its unit scale, from one read of the field table.
    pub(super) fn slot(self, tiers: &mut exact::DurationTiers) -> FieldSlot<'_> {
        let (tier, scale) = self.tier_and_scale();
        let accumulator = match tier {
            DurationTier::Months => &mut tiers.months,
            DurationTier::Days => &mut tiers.days,
            DurationTier::Nanos => &mut tiers.nanos,
        };
        FieldSlot { accumulator, scale }
    }

    /// The CIP-223 pattern spelling of this field — the one letter table the
    /// pattern parser and formatter share. `None` for a symbol outside the
    /// duration-pattern alphabet.
    pub(super) fn pattern_symbol(c: char) -> Option<PatternSymbol> {
        Some(match c {
            'y' | 'Y' | 'u' => PatternSymbol::Field(Self::Years),
            'q' | 'Q' => PatternSymbol::Field(Self::Quarters),
            'M' | 'L' => PatternSymbol::Field(Self::Months),
            'w' | 'W' => PatternSymbol::Field(Self::Weeks),
            'd' | 'D' => PatternSymbol::Field(Self::Days),
            'h' | 'H' | 'k' | 'K' => PatternSymbol::Field(Self::Hours),
            'm' => PatternSymbol::Field(Self::Minutes),
            's' => PatternSymbol::Field(Self::Seconds),
            'A' => PatternSymbol::Field(Self::Millis),
            'n' | 'S' => PatternSymbol::FractionDigits,
            'N' => PatternSymbol::Field(Self::Nanos),
            _ => return None,
        })
    }

    /// The duration-map field spelling of this field — the map-key sibling
    /// of [`Self::pattern_symbol`]. `None` for a key that does not name
    /// a duration field, so the map constructor reads one spelling table.
    /// Derived from [`Self::name`], its inverse, minus `Quarters`: Neo4j's
    /// duration map rejects `quarters` with `Unknown field: quarters`
    /// (22G0I, REL8-1063) while the `q`/`Q` pattern spelling, the
    /// `d.quarters` accessor and the CIP-223 formatter keep the field —
    /// so the map-key vocabulary is the cascade filtered, never a second
    /// name table.
    pub(super) fn from_map_key(key: &str) -> Option<Self> {
        Self::CASCADE
            .into_iter()
            .filter(|field| !matches!(field, Self::Quarters))
            .find(|field| field.name() == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The nanosecond factor is the seconds factor in nanoseconds: the fold's
    /// month length and the overflow guard's month factor are one fact.
    #[test]
    fn average_month_nanos_matches_the_second_factor() {
        assert_eq!(AVG_SECONDS_PER_MONTH, 3_652_425 * 86_400 / (10_000 * 12));
        assert_eq!(AVG_NANOS_PER_MONTH, 2.629_746e15);
    }

    /// The map-key spelling derives from the cascade: every field but
    /// `Quarters` resolves from its own name, `quarters` resolves to
    /// nothing with the other non-field keys (REL8-1063) while its
    /// `q`/`Q` pattern spelling still resolves, and the cascade order
    /// keeps each tier contiguous — one test, one derivation, no
    /// hand-maintained key list.
    #[test]
    fn map_admits_every_cascade_field_but_quarters() {
        for field in DurationField::CASCADE {
            assert_eq!(
                DurationField::from_map_key(field.name()),
                (field != DurationField::Quarters).then_some(field),
                "the map admits every cascade field but quarters"
            );
        }
        assert_eq!(DurationField::from_map_key("quarters"), None);
        assert_eq!(DurationField::from_map_key("fortnights"), None);
        assert!(matches!(
            DurationField::pattern_symbol('q'),
            Some(PatternSymbol::Field(DurationField::Quarters))
        ));
        let mut tiers: Vec<DurationTier> = DurationField::CASCADE
            .iter()
            .map(|field| field.tier_and_scale().0)
            .collect();
        tiers.dedup();
        assert_eq!(
            tiers,
            vec![
                DurationTier::Months,
                DurationTier::Days,
                DurationTier::Nanos
            ],
            "the cascade keeps each tier contiguous"
        );
    }
}
