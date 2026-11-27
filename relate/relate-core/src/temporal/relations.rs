//! The temporal relations: `=` ([`Temporal::equals`]), the three-valued
//! [`Temporal::compare`], the chronological [`Temporal::same_instant`] /
//! [`Temporal::instant_ordering`], the zoned identities
//! ([`Temporal::datetime_eq_identity`], [`Temporal::zoned_instant_identity`])
//! and the total order `ORDER BY` reads ([`Temporal::total_cmp`]) —
//! extracted from `mod.rs` with the helpers only they use.
//!
//! Every kind-partitioned relation reads its key from [`kind_key`], the
//! **one exhaustive match** from a value to its kind's key: each Cypher
//! kind's two internal representations (a chrono date and a proleptic
//! civil one, and the zoned pair of ADR-0683) share one key, and the
//! zoned key carries the whole [`ZonedView`] the ordering, the `=`
//! identity and the executor's group key all derive from — so a
//! representation added to any kind is a compile error at that one match,
//! never a silently unkeyed value. The kind-level predicates
//! ([`Temporal::same_instant`]) ask [`Temporal::kind`] instead, the one
//! vocabulary for "same Cypher kind".

use std::cmp::Ordering;

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime};

use super::zone::{add_offset, zone_region};
use super::{
    utc_time_nanos, CivilZonedFields, Temporal, TemporalKind, WallClockParts, ZoneId, ZonedParts,
};

/// The one view over the zoned partition (ADR-0683) — the facts the
/// zoned relations read, matched out of either representation by
/// [`kind_key`]: the UTC instant (wide `i128` epoch nanoseconds), the
/// offset, the IANA region (`None` for a plain offset spelling) and the
/// matched-out source, from which the local wall clock
/// ([`ZonedView::wall`]) is derived on demand — the ordering relations
/// and the group key never read it, so their comparisons skip the offset
/// add and the calendar decomposition the `=` identity alone needs.
struct ZonedView<'a> {
    /// The published three facts — instant, offset, region — in their one
    /// definition ([`ZonedParts`]); the ordering reads them directly and
    /// [`Temporal::zoned_parts`] hands them to the Bolt encoder.
    parts: ZonedParts<'a>,
    /// The matched-out representation — the memo of the one partition
    /// match, so [`ZonedView::wall`] reads the matched representation's
    /// own fields, never a re-derived policy.
    source: ZonedSource<'a>,
}

/// The representation a [`ZonedView`] was matched out of.
enum ZonedSource<'a> {
    /// A chrono-range `DateTime`'s stored UTC half (the view carries its
    /// offset).
    Chrono { utc: NaiveDateTime },
    /// The far-year civil form's own fields.
    Civil(&'a CivilZonedFields),
}

impl ZonedView<'_> {
    /// The local wall clock as its normalized `(year, month, day, time)`
    /// parts — the `=` identity's clock half ([`WallClockParts`]),
    /// derived on demand from the matched source: only the `=` identity
    /// reads it.
    fn wall(&self) -> WallClockParts {
        match self.source {
            ZonedSource::Chrono { utc } => local_datetime_parts(local_of(utc, self.parts.offset)),
            ZonedSource::Civil(fields) => (fields.year, fields.month, fields.day, fields.time),
        }
    }

    /// The two components [`Temporal::equals`] compares for a zoned
    /// datetime: its local wall clock and its [`ZoneId`] — the `=`
    /// identity only, finer than the instant so the two halves of a DST
    /// fall-back overlap stay equal (LABS-785 keys the index and `MERGE`
    /// on [`ZonedView::instant`] plus the zone instead).
    fn eq_identity(&self) -> (WallClockParts, ZoneId) {
        (
            self.wall(),
            ZoneId::of_region(self.parts.region, self.parts.offset),
        )
    }
}

/// The zoned pair's `=` relation, and the one its [`KindKey`] variant
/// compares with: the view's own identity — wall clock plus [`ZoneId`] —
/// never a derived field-by-field comparison, so the two halves of a DST
/// fall-back overlap are `=`-equal though their instants differ.
impl PartialEq for ZonedView<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.eq_identity() == other.eq_identity()
    }
}

/// One representation's ordering/equality key, per kind — the payloads
/// [`kind_key`] produces and every key-level relation matches on. Each
/// kind's two internal representations share one variant, so DATE's
/// chrono and civil forms order and compare alike.
#[derive(PartialEq)]
enum KindKey<'a> {
    /// The DATE kind: the proleptic calendar triple.
    Date((i64, u32, u32)),
    /// The LOCAL TIME kind.
    LocalTime(NaiveTime),
    /// The LOCAL DATETIME kind: the wall clock parts ([`WallClockParts`]).
    LocalDateTime(WallClockParts),
    /// The TIME kind: the signed UTC nanos-of-day and the offset — named,
    /// so the arm that reads only the instant
    /// ([`Temporal::instant_ordering`]) says which half it means.
    Time { utc_nanos: i64, offset: i32 },
    /// The DATETIME kind: the one zoned view.
    Zoned(ZonedView<'a>),
    /// The DURATION kind: the stored `(months, days, nanos)` triple,
    /// which `=` compares as-is and [`Temporal::total_cmp`] ranks.
    Duration((i64, i64, i128)),
}

/// The one exhaustive match from a value to its kind's key — the
/// relations' dispatch and their single extension point: a new
/// representation of any kind fails to compile here, never silently
/// answering an unkeyed value.
fn kind_key(t: &Temporal) -> KindKey<'_> {
    match t {
        Temporal::Date(d) => KindKey::Date(date_parts(*d)),
        Temporal::CivilDate { year, month, day } => KindKey::Date((*year, *month, *day)),
        Temporal::LocalTime(time) => KindKey::LocalTime(*time),
        Temporal::LocalDateTime(dt) => KindKey::LocalDateTime(local_datetime_parts(*dt)),
        Temporal::CivilDateTime {
            year,
            month,
            day,
            time,
        } => KindKey::LocalDateTime((*year, *month, *day, *time)),
        Temporal::Time { time, offset } => KindKey::Time {
            utc_nanos: utc_time_nanos(*time, *offset),
            offset: *offset,
        },
        // The zoned pair (ADR-0683): one view, from either
        // representation — the chrono instant through its seconds pair,
        // the civil one through the wide day arithmetic.
        Temporal::DateTime { utc, offset, zone } => KindKey::Zoned(ZonedView {
            parts: ZonedParts {
                instant: utc_epoch_nanos(*utc),
                offset: *offset,
                region: zone_region(zone.as_deref()),
            },
            source: ZonedSource::Chrono { utc: *utc },
        }),
        Temporal::CivilZoned(fields) => KindKey::Zoned(ZonedView {
            parts: ZonedParts {
                instant: fields.epoch_nanos(),
                offset: fields.offset,
                region: zone_region(fields.zone.as_deref()),
            },
            source: ZonedSource::Civil(fields),
        }),
        Temporal::Duration {
            months,
            days,
            nanos,
        } => KindKey::Duration((*months, *days, *nanos)),
    }
}

/// The three-valued ordering of two **already-derived** kind keys — the
/// one key-level comparison sink, so a relation that has built its keys
/// ([`Temporal::instant_ordering`], [`Temporal::total_cmp`]) never derives
/// them a second time through [`Temporal::compare`].
fn compare_keys(a: KindKey<'_>, b: KindKey<'_>) -> Option<Ordering> {
    match (a, b) {
        (KindKey::Date(a), KindKey::Date(b)) => Some(a.cmp(&b)),
        (KindKey::LocalTime(a), KindKey::LocalTime(b)) => Some(a.cmp(&b)),
        (KindKey::LocalDateTime(a), KindKey::LocalDateTime(b)) => Some(a.cmp(&b)),
        // The UTC nanos first, then the offset.
        (
            KindKey::Time {
                utc_nanos: a,
                offset: oa,
            },
            KindKey::Time {
                utc_nanos: b,
                offset: ob,
            },
        ) => Some(a.cmp(&b).then_with(|| oa.cmp(&ob))),
        // A zoned pair — both representations — orders by the one instant
        // both representations share (ADR-0683), then offset, then zone.
        (KindKey::Zoned(a), KindKey::Zoned(b)) => Some(
            a.parts
                .instant
                .cmp(&b.parts.instant)
                .then_with(|| a.parts.offset.cmp(&b.parts.offset))
                // A named (IANA) zone sorts after the plain offset it
                // equals, by name: `None < Some(_)`.
                .then_with(|| a.parts.region.cmp(&b.parts.region)),
        ),
        // Unlike kinds, and durations (not orderable at all — only `=`
        // is defined for them), answer the null result.
        _ => None,
    }
}

/// The wide epoch nanoseconds of a chrono UTC datetime: through the
/// seconds pair, never `timestamp_nanos` (an `i64` that overflows far
/// inside chrono's range). The chrono half of the one instant derivation
/// — [`CivilZonedFields::epoch_nanos`] is the civil half.
fn utc_epoch_nanos(utc: NaiveDateTime) -> i128 {
    let instant = utc.and_utc();
    i128::from(instant.timestamp()) * 1_000_000_000 + i128::from(instant.timestamp_subsec_nanos())
}

impl Temporal {
    /// Cypher `=` between two values of the same temporal kind (`false` for
    /// unlike kinds). The relation is deliberately *not* the ordering one:
    ///
    /// * `Time` — local time **and** offset, so `12:00+01:00` and `11:00Z`
    ///   (the same instant) are different times, while `12:00Z` and
    ///   `12:00+00:00` are the same.
    /// * `DateTime` — local date-time **and** zone identity, so the two
    ///   halves of a DST fall-back overlap (`02:30+02:00[Europe/Stockholm]`
    ///   and `02:30+01:00[Europe/Stockholm]`) are equal even though they are
    ///   different instants, while `[Europe/Stockholm]` never equals
    ///   `+01:00` (`ZoneId`).
    pub fn equals(&self, other: &Temporal) -> bool {
        // The key's own `=` — per kind the key *is* the `=` identity, and
        // the zoned variant's [`PartialEq`] reads the wall clock plus zone
        // (so the two halves of a DST fall-back overlap stay equal across
        // the representations they parse into, ADR-0683). Unlike kinds are
        // different variants, hence unequal.
        kind_key(self) == kind_key(other)
    }

    /// The two components [`Self::equals`] compares for a zoned datetime —
    /// either representation, the civil far-year form included (ADR-0683):
    /// its local wall clock and its [`ZoneId`]. `None` for every other
    /// temporal kind. This is the `=` identity only; the property index,
    /// uniqueness probe and `MERGE` key the finer equivalence identity
    /// (UTC instant plus zone, [`Self::zoned_instant_identity`]), so the
    /// two halves of a DST fall-back overlap are equal yet separate index
    /// entries (LABS-785).
    pub fn datetime_eq_identity(&self) -> Option<(WallClockParts, ZoneId)> {
        self.zoned_view().map(|view| view.eq_identity())
    }

    /// The zoned datetime's identity for the property index, uniqueness
    /// probe and `MERGE` key (LABS-785): the UTC instant plus [`ZoneId`] —
    /// finer than [`Self::datetime_eq_identity`]'s wall clock and zone, so
    /// the two halves of a DST fall-back overlap are `=`-equal yet separate
    /// entries. `None` for every non-zoned kind. The executor's group key
    /// reads this one derivation, so a representation added to the zoned
    /// partition can never fork the equivalence.
    pub fn zoned_instant_identity(&self) -> Option<(i128, ZoneId)> {
        self.zoned_parts()
            .map(|parts| (parts.instant, ZoneId::of_region(parts.region, parts.offset)))
    }

    /// The zoned facts every consumer reads (ADR-0683): the wide UTC
    /// instant in epoch nanoseconds, the offset, and the already-classified
    /// IANA region (`None` for a plain offset spelling). `None` for every
    /// non-zoned kind. The relations derive from the one [`kind_key`], and
    /// so does the Bolt encoder, which needs exactly these three — one
    /// derivation, never a per-consumer reassembly of the instant.
    pub fn zoned_parts(&self) -> Option<ZonedParts<'_>> {
        self.zoned_view().map(|view| view.parts)
    }

    /// The one "is this zoned — hand me its view" extraction the three
    /// zoned accessors project from, so the `Some` set and its `None`
    /// contract have one home (the same `KindKey::Zoned` arm every
    /// relation reads).
    fn zoned_view(&self) -> Option<ZonedView<'_>> {
        match kind_key(self) {
            KindKey::Zoned(view) => Some(view),
            _ => None,
        }
    }

    /// Three-valued ordering for `<`, `>`, `<=`, `>=` (`None` is the null
    /// result of comparing incomparable values). The dispatch is the
    /// [`kind_key`] pair — the one key-level spelling of the variant
    /// grouping (`Temporal::kind` stays the kind-level discriminant) — so
    /// each kind's two representations compare through one key:
    ///
    /// * `Date` — the proleptic calendar triple, so DATE's chrono and
    ///   civil forms share one order.
    /// * `LocalDateTime` — the wall clock parts, likewise.
    /// * `Time` — UTC nanos-of-day (not wrapped at midnight, so it can be
    ///   negative), then the offset, so two times at the same instant
    ///   still order.
    /// * `DateTime` — the [`ZonedView`]'s instant (nanos included), then
    ///   offset, then a plain offset before a named zone, then the zone
    ///   name.
    /// * `Duration` — not orderable at all; only `=` is defined for it.
    ///
    /// Unlike kinds answer `None` (the null result).
    pub fn compare(&self, other: &Temporal) -> Option<Ordering> {
        compare_keys(kind_key(self), kind_key(other))
    }

    /// `IS SAME INSTANT` (Cypher 25 chronological comparison): the instant
    /// for the two instant-bearing kinds — `DateTime` (either
    /// representation) and `Time` — is [`Self::instant_ordering`]'s
    /// equality, one derivation for both; [`Self::equals`] for the
    /// non-zoned kinds, whose clock already *is* the instant. Unlike kinds
    /// are `false` here too; the operator's mixed-kind null/false policy
    /// stays with the caller, this relation only answers for a comparable
    /// pair.
    pub fn same_instant(&self, other: &Temporal) -> bool {
        match (self.kind(), other.kind()) {
            (TemporalKind::DateTime, TemporalKind::DateTime)
            | (TemporalKind::Time, TemporalKind::Time) => {
                self.instant_ordering(other) == Some(Ordering::Equal)
            }
            _ => self.equals(other),
        }
    }

    /// `IS BEFORE`/`IS AFTER` ordering (Cypher 25): the instant alone for
    /// the two instant-bearing kinds — [`Self::compare`] also ranks equal
    /// instants by offset and then zone, which a chronological operator
    /// must not — and [`Self::compare`] for every other kind (durations
    /// stay unordered: `None`).
    pub fn instant_ordering(&self, other: &Temporal) -> Option<Ordering> {
        match (kind_key(self), kind_key(other)) {
            // A zoned pair — both representations — orders by the wide
            // instant (ADR-0683).
            (KindKey::Zoned(a), KindKey::Zoned(b)) => Some(a.parts.instant.cmp(&b.parts.instant)),
            // The UTC nanos alone: the offset must not break a tie.
            (KindKey::Time { utc_nanos: a, .. }, KindKey::Time { utc_nanos: b, .. }) => {
                Some(a.cmp(&b))
            }
            // The keys are already built — the one comparison sink reads
            // them, never a second derivation through `compare`.
            (a, b) => compare_keys(a, b),
        }
    }

    /// Total order for `ORDER BY`, `min` and `max`: [`Self::compare`] where it
    /// is defined, the duration triple for durations (which equality alone
    /// cannot rank), and `Equal` across unlike kinds — the callers keep
    /// unlike temporals distinct by the value type rank.
    pub(crate) fn total_cmp(&self, other: &Temporal) -> Ordering {
        match (kind_key(self), kind_key(other)) {
            (KindKey::Duration(a), KindKey::Duration(b)) => a.cmp(&b),
            (a, b) => compare_keys(a, b).unwrap_or(Ordering::Equal),
        }
    }
}

/// The calendar ordering key of a representable date — the same triple a
/// [`Temporal::CivilDate`] carries, so DATE's two representations share one
/// order.
fn date_parts(d: NaiveDate) -> (i64, u32, u32) {
    (i64::from(d.year()), d.month(), d.day())
}

/// The ordering key of a representable local date-time, in the shape a
/// [`Temporal::CivilDateTime`] carries.
fn local_datetime_parts(dt: NaiveDateTime) -> WallClockParts {
    let (year, month, day) = date_parts(dt.date());
    (year, month, day, dt.time())
}

/// The wall clock a zoned instant reads at (`utc + offset`). Constructors
/// keep the sum inside chrono's range, but the Bolt parameter decoders
/// accept any i32 offset (the render path's comment at the `DateTime` arm
/// documents this input class), and equality bucketing has no error
/// channel — so an out-of-range sum degrades to the UTC wall clock, the
/// same stated fallback render uses, instead of panicking. Deterministic
/// per payload, so `a = b` still implies one bucket.
fn local_of(utc: NaiveDateTime, offset: i32) -> NaiveDateTime {
    add_offset(utc, offset).unwrap_or(utc)
}
