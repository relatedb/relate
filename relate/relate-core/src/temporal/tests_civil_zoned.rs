//! ADR-0683's far-year zoned datetimes: the parse fallback, the instant
//! ordering and equality across both zoned representations, the instant
//! relations, the ISO rendering, and the boundaries — an in-range zoned
//! literal keeps the chrono path and a named zone at a far year stays
//! rejected.

use std::cmp::Ordering;

use chrono::{Datelike, NaiveDate, Timelike};

/// ADR-0683: the boxed far-year payload exists so `Temporal` — and every
/// row's `Value` with it — does not grow past the chrono `DateTime`
/// variant's 48-byte footprint. The memory-calibration tests
/// (`relate::tests::processing_limits`, `memory`) charge this size; a
/// change here is a change there, so it fails loudly instead.
#[test]
fn the_enum_stays_at_the_datetime_variant_footprint() {
    assert_eq!(std::mem::size_of::<Temporal>(), 48);
}

use super::parse::parse_iso;
use super::Temporal;

fn zoned(query: &str) -> Temporal {
    parse_iso("datetime", query).unwrap_or_else(|e| panic!("`{query}` should parse: {e}"))
}

#[test]
fn far_year_offsets_parse_as_civil_zoned() {
    // The TCK sentinels: the three far-year spellings the corpus stores
    // and compares.
    let min = zoned("-999999999-01-01T05:00+18:00");
    let Temporal::CivilZoned(min_fields) = &min else {
        panic!("the far-year fallback builds a CivilZoned value");
    };
    assert_eq!(
        (
            min_fields.year,
            min_fields.month,
            min_fields.day,
            min_fields.offset
        ),
        (-999_999_999, 1, 1, 64_800)
    );
    assert!(min_fields.zone.is_none());
    let max = zoned("+999999999-12-31T23:59:59.999999999-18:00");
    let Temporal::CivilZoned(max_fields) = &max else {
        panic!("the far-year fallback builds a CivilZoned value");
    };
    assert_eq!(
        (
            max_fields.year,
            max_fields.month,
            max_fields.day,
            max_fields.offset
        ),
        (999_999_999, 12, 31, -64_800)
    );
    assert_eq!(max_fields.time.nanosecond(), 999_999_999);
    // `Z` keeps its spelling as the zone (the chrono path's rule).
    let utc_far = zoned("+275760-09-13T00:00Z");
    let Temporal::CivilZoned(utc_fields) = &utc_far else {
        panic!("the far-year fallback builds a CivilZoned value");
    };
    assert_eq!(
        (
            utc_fields.year,
            utc_fields.month,
            utc_fields.day,
            utc_fields.offset
        ),
        (275_760, 9, 13, 0)
    );
    assert_eq!(utc_fields.zone.as_deref(), Some("Z"));
    // No offset and no bracket reads as UTC, as the chrono path does.
    let bare = zoned("-999999999-01-01T05:00");
    let Temporal::CivilZoned(bare_fields) = &bare else {
        panic!("the far-year fallback builds a CivilZoned value");
    };
    assert_eq!(bare_fields.offset, 0);
}

#[test]
fn a_malformed_time_at_a_far_year_declines_not_midnight() {
    // The fallback's contract: a far-year body with a malformed time part
    // declines, so the chrono parse's own error answers — never a silent
    // midnight substitution.
    let err = parse_iso("datetime", "-999999999-01-01T99:99+18:00")
        .expect_err("a malformed time is never a far-year value");
    // The chrono parse's own rejection for this shape — the same text it
    // raised before the far-year fallback existed.
    assert!(
        err.to_string().contains("cannot parse localdatetime"),
        "{err}"
    );
}

#[test]
fn in_range_zoned_literals_keep_the_chrono_path() {
    // The DST-overlap corpus shape still builds the chrono `DateTime`.
    assert!(matches!(
        zoned("2020-10-25T02:30+01:00[Europe/Stockholm]"),
        Temporal::DateTime { .. }
    ));
    assert!(matches!(
        zoned("2020-01-01T00:00Z"),
        Temporal::DateTime { .. }
    ));
}

#[test]
fn an_impossible_far_year_date_declines_not_stores() {
    // The calendar check the date parse applies: a far-year body naming an
    // impossible date declines, so the chrono parse's `invalid date`
    // rejection stands instead of a stored impossible value.
    for body in ["-999999999-02-31T00:00+18:00", "-999999999-13-45T00:00"] {
        assert!(
            parse_iso("datetime", body).is_err(),
            "`{body}` must not construct"
        );
        assert!(
            parse_iso("localdatetime", body).is_err(),
            "`{body}` must not construct as a localdatetime either"
        );
    }
}

#[test]
fn a_far_year_date_alone_is_midnight() {
    // The date-alone body keeps the chrono parse's midnight rule at far
    // years too.
    let v = zoned("-999999999-01-01");
    let Temporal::CivilZoned(fields) = &v else {
        panic!("the far-year fallback builds a CivilZoned value");
    };
    assert_eq!(fields.time, chrono::NaiveTime::MIN);
    assert!(parse_iso("localdatetime", "-999999999-01-01").is_ok());
}

#[test]
fn named_zone_at_a_far_year_is_rejected() {
    let err = parse_iso("datetime", "-999999999-01-01T05:00[Europe/Stockholm]")
        .expect_err("a named zone needs the chrono resolver");
    assert!(err.to_string().contains("named time zone"), "{err}");
}

#[test]
fn contradicting_offset_at_a_far_year_is_rejected() {
    // The bracket's fixed offset must match the one written beside the
    // body — the same rejection the chrono path raises.
    let err = parse_iso("datetime", "-999999999-01-01T05:00+18:00[+02:00]")
        .expect_err("a contradicting offset is never silently replaced");
    assert!(err.to_string().contains("do not match"), "{err}");
}

#[test]
fn ordering_spans_the_representations() {
    let far_min = zoned("-999999999-01-01T05:00+18:00");
    let far_max = zoned("+999999999-12-31T23:59:59.999999999-18:00");
    let near = zoned("2020-01-01T00:00Z");
    assert_eq!(far_min.compare(&near), Some(Ordering::Less));
    assert_eq!(near.compare(&far_min), Some(Ordering::Greater));
    assert_eq!(far_max.compare(&near), Some(Ordering::Greater));
    assert_eq!(far_min.compare(&far_max), Some(Ordering::Less));
    // A far-year civil value orders against an in-range chrono one in
    // both directions.
    let civil = zoned("-999999999-01-01T03:00+02:00");
    let chrono = zoned("2020-01-01T01:00Z");
    assert_eq!(civil.compare(&chrono), Some(Ordering::Less));
    assert_eq!(chrono.compare(&civil), Some(Ordering::Greater));
    // Same representation, same instant, different offsets: the offset
    // tiebreak answers, the smaller offset first (`Z` before `+02:00`).
    let a = zoned("-999999999-01-01T03:00+02:00");
    let b = zoned("-999999999-01-01T01:00Z");
    assert_eq!(a.compare(&b), Some(Ordering::Greater));
    assert_eq!(b.compare(&a), Some(Ordering::Less));
}

#[test]
fn equals_compares_the_wall_clock_and_zone_identity() {
    // Equal wall clock, both zones `Z`: equal across representations.
    let civil = zoned("2020-01-01T00:00Z");
    let chrono = zoned("2020-01-01T00:00+00:00");
    assert!(civil.equals(&chrono));
    assert!(chrono.equals(&civil));
    // The same instant at different wall clocks is not equal.
    let shifted = zoned("2020-01-01T02:00+02:00");
    assert!(!civil.equals(&shifted));
    // A far-year pair of the same spelling is equal to itself.
    let far = zoned("-999999999-01-01T05:00+18:00");
    assert!(far.equals(&zoned("-999999999-01-01T05:00+18:00")));
}

#[test]
fn same_instant_and_instant_ordering_across_representations() {
    // `2020-01-01T00:00Z` and the far year -999999999-01-01T03:00+02:00
    // are far apart; the relations must order them across the
    // representations.
    let far = zoned("-999999999-01-01T03:00+02:00");
    let near = zoned("2020-01-01T00:00Z");
    assert_eq!(far.instant_ordering(&near), Some(Ordering::Less));
    assert!(!far.same_instant(&near));
    // The civil pair at one instant, two offsets: same instant, and the
    // chronological ordering is the instant alone (no offset tiebreak).
    let a = zoned("-999999999-01-01T03:00+02:00");
    let b = zoned("-999999999-01-01T01:00Z");
    assert!(a.same_instant(&b));
    assert_eq!(a.instant_ordering(&b), Some(Ordering::Equal));
}

#[test]
fn renders_the_iso_form_with_offset_and_zone() {
    assert_eq!(
        zoned("-999999999-01-01T05:00+18:00").iso_string(),
        "-999999999-01-01T05:00+18:00"
    );
    assert_eq!(
        zoned("+999999999-12-31T23:59:59.999999999-18:00").iso_string(),
        "+999999999-12-31T23:59:59.999999999-18:00"
    );
    assert_eq!(
        zoned("+275760-09-13T00:00Z").iso_string(),
        "+275760-09-13T00:00Z"
    );
    // The full-seconds policy keeps the seconds the value carries.
    assert_eq!(
        zoned("+275760-09-13T00:00Z").full_seconds_string(),
        "+275760-09-13T00:00:00Z"
    );
}

#[test]
fn far_year_zoned_arithmetic_and_between_are_typed_rejections() {
    // The civil forms' recorded policy (tests_ranges): arithmetic and the
    // between stepper answer errors or no-readings, never a panic.
    let far = zoned("-999999999-01-01T05:00+18:00");
    let day = super::parse_iso("duration", "P1D").expect("duration");
    assert!(super::add_duration(&far, &day).is_err());
}

#[test]
fn the_constructor_readers_answer_for_the_civil_form() {
    // `datetime(<zoned>)` keeps a far-year source verbatim and
    // `time(<zoned>)` reads its wall clock and offset — the parity the
    // chrono twins have (the ADR's identity-surfaces bullet).
    let far = zoned("-999999999-01-01T05:00+18:00");
    let kept = super::construct_datetime(Some(&crate::Value::Temporal(far.clone())))
        .expect("a zoned source is kept verbatim");
    assert!(kept.equals(&far));
    let read = super::construct_time(Some(&crate::Value::Temporal(far)))
        .expect("the wall clock and offset read");
    assert!(matches!(read, Temporal::Time { offset: 64_800, .. }));
}

#[test]
fn far_year_zoned_reads_as_the_datetime_kind() {
    let far = zoned("-999999999-01-01T05:00+18:00");
    assert_eq!(far.kind_name(), "datetime");
    assert!(far.has_date() && far.has_time() && far.has_date_and_time());
    // The civil accessor policy: no component vocabulary.
    assert!(far.component("year").is_err());
}

/// The chrono wall-clock bounds the ADR names in its named-zone
/// rejection — the same bounds `NaiveDate` carries, stated once here so
/// the test cannot drift from the message it checks.
#[test]
fn named_zone_rejection_names_the_chrono_bounds() {
    let err = parse_iso("datetime", "+999999999-01-01T05:00[Europe/Stockholm]")
        .expect_err("named zone at a far year");
    let min = NaiveDate::MIN.year();
    let max = NaiveDate::MAX.year();
    assert!(
        err.to_string()
            .contains(&format!("a year within {min}..={max}")),
        "{err}"
    );
}
