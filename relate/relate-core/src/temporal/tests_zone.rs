//! Zoned-datetime tests: IANA offset resolution, DST gaps and overlaps, and
//! the offset/zone agreement rules.

use std::collections::BTreeMap;

use super::test_support::naive_utc;
use super::*;

/// The exported resolver's three outcomes (ADR-0178). A unique wall time
/// resolves; a DST gap and an unknown zone name are the two typed errors; a
/// DST overlap takes the earliest (pre-transition) occurrence.
#[test]
fn zone_offset_at_resolves_unique_wall_time() {
    let local = NaiveDate::from_ymd_opt(1970, 1, 1)
        .unwrap()
        .and_hms_opt(2, 15, 0)
        .unwrap();
    assert_eq!(zone_offset_at("Europe/Paris", local), Ok(3600));
}

#[test]
fn zone_offset_at_unknown_zone_is_typed_error() {
    let local = NaiveDate::from_ymd_opt(2021, 3, 28)
        .unwrap()
        .and_hms_opt(2, 30, 0)
        .unwrap();
    assert_eq!(
        zone_offset_at("Not/AZone", local),
        Err(ZoneError::UnknownZone)
    );
}

#[test]
fn zone_offset_at_dst_gap_is_typed_error() {
    let local = NaiveDate::from_ymd_opt(2021, 3, 28)
        .unwrap()
        .and_hms_opt(2, 30, 0)
        .unwrap();
    assert_eq!(
        zone_offset_at("Europe/Stockholm", local),
        Err(ZoneError::NonexistentWallTime)
    );
}

#[test]
fn zone_offset_at_dst_overlap_takes_earliest_offset() {
    let local = NaiveDate::from_ymd_opt(2021, 10, 31)
        .unwrap()
        .and_hms_opt(2, 30, 0)
        .unwrap();
    assert_eq!(zone_offset_at("Europe/Stockholm", local), Ok(7200));
}

/// The construction path resolves a DST gap forward and keeps the unknown
/// zone name a typed error (ADR-0178's gap rule refined by LABS-785).
#[test]
fn construct_datetime_iana_gap_shifts_and_unknown_zone_errors() {
    let fields = |timezone: &str| {
        Value::Map(
            [
                ("year".into(), Value::Int(2021)),
                ("month".into(), Value::Int(3)),
                ("day".into(), Value::Int(28)),
                ("hour".into(), Value::Int(2)),
                ("minute".into(), Value::Int(30)),
                ("timezone".into(), Value::String(timezone.into())),
            ]
            .into_iter()
            .collect::<BTreeMap<_, _>>()
            .into(),
        )
    };
    assert_eq!(
        construct_datetime(Some(&fields("Europe/Stockholm")))
            .unwrap()
            .iso_string(),
        "2021-03-28T03:30+02:00[Europe/Stockholm]"
    );
    assert_eq!(
        construct_datetime(Some(&fields("Not/AZone"))),
        Err(Error::ty("unknown timezone 'Not/AZone'"))
    );
}

/// F-1-32: a `time` truncate's `timezone` is resolved at the statement's
/// pinned UTC instant, read as a UTC instant (`IanaAt::Utc`), not as a UTC
/// wall clock re-read in the zone. 2021-10-31T01:30Z is transition-adjacent
/// for Stockholm: the UTC reading is +01:00, while reading the same wall
/// clock locally would land before the fall-back and take +02:00.
#[test]
fn time_truncate_resolves_timezone_at_the_pinned_utc_instant() {
    let _clock = QueryClockGuard::pinned_at(naive_utc(2021, 10, 31, 1, 30, 0), 0);
    let extra = Value::Map(
        [("timezone".into(), Value::String("Europe/Stockholm".into()))]
            .into_iter()
            .collect::<BTreeMap<_, _>>()
            .into(),
    );
    let value = Temporal::Time {
        time: NaiveTime::from_hms_opt(12, 31, 14).unwrap(),
        offset: 0,
    };
    let out = truncate(ClockKind::Time, "hour", &value, Some(&extra)).unwrap();
    assert_eq!(out.iso_string(), "12:00+01:00");
}

#[test]
fn zone_region_classifies_iana_names_only() {
    assert_eq!(
        zone_region(Some("Europe/Stockholm")),
        Some("Europe/Stockholm")
    );
    for offset_only in [None, Some("Z"), Some("+01:00"), Some("UTC")] {
        assert_eq!(zone_region(offset_only), None, "{offset_only:?}");
    }
}

/// An explicit offset beside a bracketed zone disambiguates a DST fall-back
/// overlap, so the two halves of the repeated wall clock are both
/// representable — equal under `Temporal::equals`, ordered by
/// `Temporal::compare` (TCK TemporalCompare).
#[test]
fn zoned_literal_offset_disambiguates_a_dst_overlap() {
    let first = parse_iso("datetime", "2020-10-25T02:30+02:00[Europe/Stockholm]").unwrap();
    let second = parse_iso("datetime", "2020-10-25T02:30+01:00[Europe/Stockholm]").unwrap();
    // The offset beside the zone is preserved exactly as the literal
    // supplied it; the whole-minute wall clock keeps the ISO truncation,
    // while the `toString` rendering carries the seconds (ADR-0448).
    assert_eq!(
        first.iso_string(),
        "2020-10-25T02:30+02:00[Europe/Stockholm]"
    );
    assert_eq!(
        second.iso_string(),
        "2020-10-25T02:30+01:00[Europe/Stockholm]"
    );
    assert_eq!(
        first.full_seconds_string(),
        "2020-10-25T02:30:00+02:00[Europe/Stockholm]"
    );
    assert!(first.equals(&second));
    assert_eq!(first.compare(&second), Some(Ordering::Less));
    // A still-ambiguous wall clock (no offset) stays an error, while a DST
    // gap shifts the clock forward (Neo4j's `ZonedDateTime` rule).
    assert_eq!(
        parse_iso("datetime", "2020-10-25T02:30[Europe/Stockholm]"),
        Err(Error::ty("ambiguous datetime"))
    );
    let gap = parse_iso("datetime", "2021-03-28T02:30[Europe/Stockholm]").unwrap();
    assert_eq!(gap.iso_string(), "2021-03-28T03:30+02:00[Europe/Stockholm]");
    // An explicit offset cannot name a wall clock the zone never produces.
    assert_eq!(
        parse_iso("datetime", "2021-03-28T02:30+01:00[Europe/Stockholm]"),
        Err(Error::ty("ambiguous datetime"))
    );
}

/// A bracketed named zone must agree with a supplied offset: the zone's own
/// offset is authoritative, so a contradicting offset is rejected instead of
/// being silently replaced.
#[test]
fn named_zone_offset_must_match_the_zone() {
    for invalid in [
        "1984-07-07T12:34+03:00[Europe/Stockholm]",
        "2012-11-04T00:59:59-06:00[America/Chicago]",
        "2021-03-28T00:59:59+01:00[Europe/London]",
        "2019-10-30T07:54:02.129790999+01:00[UTC]",
    ] {
        assert!(parse_iso("datetime", invalid).is_err(), "{invalid}");
    }
    assert!(parse_iso("datetime", "1984-07-07T12:34+02:00[Europe/Stockholm]").is_ok());
    assert!(parse_iso("datetime", "2019-10-30T07:54:02+00:00[UTC]").is_ok());
}

/// A zone offset is bounded at ±18:00, and a `time` literal can never carry a
/// bracketed zone name (it has no date to resolve it against).
#[test]
fn offsets_are_bounded_and_time_literals_reject_named_zones() {
    assert!(parse_iso("datetime", "2018-04-01T12:45:45+45").is_err());
    assert!(parse_iso("datetime", "2018-04-01T12:45:45+18:00").is_ok());
    assert!(parse_iso("datetime", "2018-04-01T12:45:45+18:01").is_err());
    assert!(parse_iso("time", "07:54:02.129790999+00:00[UTC]").is_err());
    assert!(parse_iso("time", "12:34:56[Europe/Stockholm]").is_err());
}
