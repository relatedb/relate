use std::collections::BTreeMap;
use std::panic::catch_unwind;

use super::clock::now_dt;
use super::construct::date_from_week;
use super::test_support::component_map;
use super::*;

#[test]
fn week_date() {
    let d = date_from_week(1816, 1, Weekday::Mon).unwrap();
    assert_eq!(d.format("%Y-%m-%d").to_string(), "1816-01-01");
}

#[test]
fn week_year_and_weekday_accessors() {
    let d = Temporal::Date(NaiveDate::from_ymd_opt(1984, 1, 1).unwrap());
    assert_eq!(d.component("weekYear"), Ok(Value::Int(1983)));
    assert_eq!(d.component("week"), Ok(Value::Int(52)));
    assert_eq!(d.component("weekDay"), Ok(Value::Int(7)));
    assert_eq!(d.component("dayOfWeek"), Ok(Value::Int(7)));
}

/// The direct constructor rejects `null` like its siblings; the executor's
/// `temporal_construct` short-circuits a null argument to `null` before any
/// constructor runs, so `date(null)` stays null at the Cypher level.
#[test]
fn date_null_rejects_like_its_siblings() {
    assert_eq!(
        construct_date(Some(&Value::Null)),
        Err(Error::syntax("Invalid call signature: date()"))
    );
    assert_eq!(
        construct_datetime(Some(&Value::Null)),
        Err(Error::syntax("Invalid call signature: datetime()"))
    );
}

/// chrono represents a leap second as second 59 with a fraction >= 1e9; the
/// truncation quantum must not wrap that out-of-day fraction to midnight.
#[test]
fn truncate_time_preserves_leap_second_encoding() {
    let leap = Temporal::Time {
        time: NaiveTime::from_hms_nano_opt(23, 59, 59, 1_234_567_890).unwrap(),
        offset: 0,
    };
    // Second-or-coarser units keep the represented second, no leap marker.
    let expected = |h, m, s| Temporal::Time {
        time: NaiveTime::from_hms_opt(h, m, s).unwrap(),
        offset: 0,
    };
    assert_eq!(
        truncate(ClockKind::Time, "second", &leap, None).unwrap(),
        expected(23, 59, 59)
    );
    assert_eq!(
        truncate(ClockKind::Time, "minute", &leap, None).unwrap(),
        expected(23, 59, 0)
    );
    assert_eq!(
        truncate(ClockKind::Time, "hour", &leap, None).unwrap(),
        expected(23, 0, 0)
    );
    assert_eq!(
        truncate(ClockKind::Time, "day", &leap, None).unwrap(),
        expected(0, 0, 0)
    );
    // Sub-second units truncate the fraction and keep the leap marker.
    assert_eq!(
        truncate(ClockKind::Time, "millisecond", &leap, None).unwrap(),
        Temporal::Time {
            time: NaiveTime::from_hms_nano_opt(23, 59, 59, 1_234_000_000).unwrap(),
            offset: 0,
        }
    );
    assert_eq!(
        truncate(ClockKind::Time, "nanosecond", &leap, None).unwrap(),
        leap
    );
}

#[test]
fn duration_component_table() {
    let d = construct_duration(Some(&Value::Map(
        [
            ("years".into(), Value::Int(1)),
            ("months".into(), Value::Int(4)),
            ("days".into(), Value::Int(10)),
            ("hours".into(), Value::Int(1)),
            ("minutes".into(), Value::Int(1)),
            ("seconds".into(), Value::Int(1)),
            ("nanoseconds".into(), Value::Int(111111111)),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>()
        .into(),
    )))
    .unwrap();
    assert_eq!(d.component("years"), Ok(Value::Int(1)));
    assert_eq!(d.component("quarters"), Ok(Value::Int(5)));
    assert_eq!(d.component("months"), Ok(Value::Int(16)));
    assert_eq!(d.component("weeks"), Ok(Value::Int(1)));
    assert_eq!(d.component("minutes"), Ok(Value::Int(61)));
    assert_eq!(d.component("seconds"), Ok(Value::Int(3661)));
    assert_eq!(d.component("quartersOfYear"), Ok(Value::Int(1)));
    assert_eq!(d.component("monthsOfQuarter"), Ok(Value::Int(1)));
    assert_eq!(d.component("monthsOfYear"), Ok(Value::Int(4)));
    assert_eq!(d.component("daysOfWeek"), Ok(Value::Int(3)));
    assert_eq!(d.component("millisecondsOfSecond"), Ok(Value::Int(111)));
    assert_eq!(
        d.component("nanosecondsOfSecond"),
        Ok(Value::Int(111111111))
    );
}

#[test]
fn datetime_timezone_and_epoch_accessors() {
    let d = construct_datetime(Some(&Value::Map(
        [
            ("year".into(), Value::Int(1984)),
            ("month".into(), Value::Int(11)),
            ("day".into(), Value::Int(11)),
            ("hour".into(), Value::Int(12)),
            ("minute".into(), Value::Int(31)),
            ("second".into(), Value::Int(14)),
            ("nanosecond".into(), Value::Int(645876123)),
            ("timezone".into(), Value::String("Europe/Stockholm".into())),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>()
        .into(),
    )))
    .unwrap();
    assert_eq!(
        d.component("timezone"),
        Ok(Value::String("Europe/Stockholm".into()))
    );
    assert_eq!(d.component("offset"), Ok(Value::String("+01:00".into())));
    assert_eq!(d.component("epochSeconds"), Ok(Value::Int(469020674)));
    assert_eq!(d.component("epochMillis"), Ok(Value::Int(469020674645)));
}

/// LABS-785: `datetime({datetime: x})` keeps `x`'s instant and offset even
/// when its wall clock is the ambiguous half of a fall-back overlap; an
/// explicit `timezone` converts that same instant, and changing a date field
/// re-resolves the new wall clock in the zone.
#[test]
fn datetime_from_a_datetime_source_keeps_its_offset() {
    let x = parse_iso("datetime", "2025-10-26T02:30:00+01:00[Europe/Stockholm]").unwrap();
    let source = || Value::Temporal(x.clone());
    assert_eq!(
        construct_datetime(Some(&component_map(&[("datetime", source())])))
            .unwrap()
            .iso_string(),
        "2025-10-26T02:30+01:00[Europe/Stockholm]"
    );
    assert_eq!(
        construct_datetime(Some(&component_map(&[
            ("datetime", source()),
            ("timezone", Value::String("UTC".into())),
        ])))
        .unwrap()
        .iso_string(),
        "2025-10-26T01:30Z"
    );
    // An override that moves the wall clock off the overlap resolves the
    // zone's own offset for that date.
    assert_eq!(
        construct_datetime(Some(&component_map(&[
            ("datetime", source()),
            ("year", Value::Int(2024)),
        ])))
        .unwrap()
        .iso_string(),
        "2024-10-26T02:30+02:00[Europe/Stockholm]"
    );
}

/// F-1-27: a `timezone` map field that is not a string is a typed error, not
/// a silent UTC default; the field's absence stays the documented UTC
/// default.
#[test]
fn datetime_map_timezone_must_be_a_string() {
    let fields = |timezone: Option<Value>| {
        let mut m: BTreeMap<String, Value> = [
            ("year".into(), Value::Int(1984)),
            ("month".into(), Value::Int(11)),
            ("day".into(), Value::Int(11)),
            ("hour".into(), Value::Int(12)),
        ]
        .into_iter()
        .collect();
        if let Some(timezone) = timezone {
            m.insert("timezone".into(), timezone);
        }
        Value::Map(m.into())
    };
    assert_eq!(
        construct_datetime(Some(&fields(Some(Value::Int(42))))),
        Err(Error::argument_error("timezone must be STRING"))
    );
    let d = construct_datetime(Some(&fields(None))).unwrap();
    // One zero-offset rule: both accessors spell a zero offset `Z`, like the
    // ISO suffix (verified against Neo4j 2026.09).
    assert_eq!(d.component("timezone"), Ok(Value::String("Z".into())));
    assert_eq!(d.component("offset"), Ok(Value::String("Z".into())));
}

#[test]
fn datetime_tostring_roundtrip_equals() {
    let d = construct_datetime(Some(&Value::Map(
        [
            ("year".into(), Value::Int(1984)),
            ("month".into(), Value::Int(10)),
            ("day".into(), Value::Int(11)),
            ("hour".into(), Value::Int(12)),
            ("minute".into(), Value::Int(31)),
            ("second".into(), Value::Int(14)),
            ("nanosecond".into(), Value::Int(645876123)),
            ("timezone".into(), Value::String("+01:00".into())),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>()
        .into(),
    )))
    .unwrap();
    assert_eq!(d.iso_string(), "1984-10-11T12:31:14.645876123+01:00");
    let parsed = parse_iso("datetime", &d.iso_string()).unwrap();
    assert_eq!(d.compare(&parsed), Some(std::cmp::Ordering::Equal));
    assert_eq!(
        format_pattern(&d, "yyyy-MM-dd'T'HH:mm:ss.SSSZ").unwrap(),
        "1984-10-11T12:31:14.645+0100"
    );
}

#[test]
fn parse_iso_year_month_and_compact_time_offset() {
    assert_eq!(
        parse_iso("date", "2015-07").unwrap().iso_string(),
        "2015-07-01"
    );
    assert_eq!(
        parse_iso("time", "2140-00:00").unwrap().iso_string(),
        "21:40Z"
    );
    // A year outside 0000..=9999 needs its sign, and a zoned datetime must
    // keep it through the local-part parse (a five-digit year is still
    // inside chrono's range).
    assert_eq!(
        parse_iso("datetime", "+10000-01-01T00:00Z")
            .unwrap()
            .iso_string(),
        "+10000-01-01T00:00Z"
    );
}

/// The value layer's two renderings: `full_seconds_string` (the
/// `toString` policy, ADR-0448) always emits the seconds field — Cypher
/// prints `toString(localtime('10:00'))` as `'10:00:00'`, never
/// `'10:00'` — while `iso_string` keeps the truncated whole-minute form.
#[test]
fn full_seconds_string_always_emits_the_seconds_field() {
    let cases = [
        ("localtime", "10:00", "10:00:00"),
        ("localtime", "10:00:30", "10:00:30"),
        ("localtime", "10:00:30.250", "10:00:30.25"),
        ("time", "10:00", "10:00:00Z"),
        ("time", "10:00+02:00", "10:00:00+02:00"),
        ("localdatetime", "2020-01-01T10:00", "2020-01-01T10:00:00"),
        ("datetime", "2020-01-01T10:00Z", "2020-01-01T10:00:00Z"),
        (
            "datetime",
            "2020-01-01T10:00+02:00",
            "2020-01-01T10:00:00+02:00",
        ),
    ];
    for (kind, input, expected) in cases {
        let value = parse_iso(kind, input).unwrap();
        assert_eq!(
            value.full_seconds_string(),
            expected,
            "for {kind}('{input}')"
        );
    }
    // The ISO rendering keeps its truncation of a whole minute.
    assert_eq!(
        parse_iso("localtime", "10:00").unwrap().iso_string(),
        "10:00"
    );
    assert_eq!(
        parse_iso("localdatetime", "2020-01-01T10:00")
            .unwrap()
            .iso_string(),
        "2020-01-01T10:00"
    );
}

#[test]
fn truncate_week_day_of_week() {
    let d = construct_date(Some(&Value::Map(
        [
            ("year".into(), Value::Int(1984)),
            ("month".into(), Value::Int(10)),
            ("day".into(), Value::Int(11)),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>()
        .into(),
    )))
    .unwrap();
    let extra = Value::Map(
        [("dayOfWeek".into(), Value::Int(2))]
            .into_iter()
            .collect::<BTreeMap<_, _>>()
            .into(),
    );
    let out = truncate(ClockKind::Date, "week", &d, Some(&extra)).unwrap();
    assert_eq!(out.iso_string(), "1984-10-09");
}

/// Every `parse_pattern` arm must return `Err` on malformed input instead of
/// panicking. Covers the four historically verified panic inputs plus one
/// malformed variant per remaining arm (memdb-merge/03-temporal-parse-safety).
#[test]
fn parse_pattern_never_panics_on_malformed_input() {
    let cases: &[(&str, &str, &str)] = &[
        // Verified panic inputs.
        ("datetime", "abc", "yyyy-MM-ddXXX"),
        ("date", "abc", "yyyy-MM-dd a"),
        ("datetime", "abc", "pppyy-MM-dd"),
        ("datetime", "abc", "yyyy, DDD 'day of year' c 'day-of-week'"),
        // Remaining arms, one malformed variant each.
        ("duration", "abc", "yyyy-MM-dd"),
        ("date", "2015-01-01", "JJ"),
        ("datetime", "2015-01", "yyyy-MM"),
        ("localtime", "abc def", "H G"),
        ("localdatetime", "abc", "yyyy-MM-dd VV"),
        ("date", "abc", "yyyy-MM-dd EEEE"),
        // Civil (out-of-range) date hits the former `unreachable!()`.
        ("date", "2015-17-21", "yyyy-MM-dd EEEE"),
        ("datetime", "abc", "EEEE, MMMM d, G uuuu"),
        ("datetime", "abc", "yyyy-MM-dd k:mm z"),
        ("datetime", "abc", "yyyy-MM-dd K:mm a O"),
        ("datetime", "abc", "yyyy LLL d, A m s"),
        ("datetime", "abc", "'Nano:' n 'of day:' N yyyy-MM-dd"),
        ("datetime", "abc", "yyyy-MM-dd hh:mm a VV B"),
        ("datetime", "abc", "YYYY-'W'ww-e"),
        ("datetime", "abc", "QQQQ yyyy 'day': D"),
        // Compact arms: wrong length, and a multi-byte value whose byte
        // length matches the guard but whose slices are not char boundaries.
        ("date", "201501a", "yyyyMMdd"),
        ("date", "201\u{e5}071", "yyyyMMdd"),
        ("localtime", "12345678", "HHmmssSSS"),
        ("localtime", "1234x", "HHmmss"),
        ("localdatetime", "201501011210", "yyyyMMddHHmmss"),
        ("datetime", "2015010112101x", "yyyyMMddHHmmssZ"),
        ("time", "99xx99+01", "HHmmssZ"),
        // parse_iso fallback paths reachable from parse_pattern.
        ("time", "\u{e5}", "HH"),
        ("date", "2015W5\u{e5}5", "yyyy"),
        ("date", "not-a-date", "yyyy"),
    ];
    for &(kind, value, pattern) in cases {
        let outcome = catch_unwind(|| parse_pattern(kind, value, pattern))
            .unwrap_or_else(|_| panic!("parse_pattern({kind:?}, {value:?}, {pattern:?}) panicked"));
        assert!(
            outcome.is_err(),
            "parse_pattern({kind:?}, {value:?}, {pattern:?}) must fail, got {outcome:?}"
        );
    }
}

/// `timestamp()` must read the pinned statement instant, not the wall clock:
/// it agrees with `datetime().epochMillis` and does not move while the guard
/// is held.
#[test]
fn now_epoch_millis_matches_pinned_datetime_and_stays_fixed() {
    let _clock = QueryClockGuard::new();
    let epoch_millis = now_dt()
        .component("epochMillis")
        .expect("datetime() exposes epochMillis");
    let Value::Int(expected) = epoch_millis else {
        panic!("epochMillis must be an INTEGER, got {epoch_millis:?}");
    };
    assert_eq!(now_epoch_millis(), expected);
    // Long enough for any wall-clock read to have crossed a millisecond.
    std::thread::sleep(std::time::Duration::from_millis(10));
    assert_eq!(now_epoch_millis(), expected);
}

/// LABS-261: `epochMillis`/`epochSeconds` map fields construct the temporal
/// value from the instant they name. Reference instant: 469020674645 ms =
/// 1984-11-11T11:31:14.645Z (the same one the accessor test pins, so
/// constructor and accessor round-trip each other).
#[test]
fn datetime_from_epoch_millis_and_epoch_seconds_map_fields() {
    let by_millis = construct_datetime(Some(&Value::Map(
        [("epochMillis".into(), Value::Int(469020674645))]
            .into_iter()
            .collect::<BTreeMap<_, _>>()
            .into(),
    )))
    .unwrap();
    // Compare on the ISO rendering: the constructor records the UTC zone
    // spelling explicitly, `parse_iso` leaves it unset, but both render "Z".
    assert_eq!(by_millis.iso_string(), "1984-11-11T11:31:14.645Z");
    assert_eq!(
        by_millis.component("epochMillis").unwrap(),
        Value::Int(469020674645)
    );

    let by_seconds = construct_datetime(Some(&Value::Map(
        [("epochSeconds".into(), Value::Int(469020674))]
            .into_iter()
            .collect::<BTreeMap<_, _>>()
            .into(),
    )))
    .unwrap();
    assert_eq!(by_seconds.iso_string(), "1984-11-11T11:31:14Z");
}

/// An explicit `timezone` converts the named instant into that zone.
#[test]
fn datetime_from_epoch_millis_with_timezone() {
    let dt = construct_datetime(Some(&Value::Map(
        [
            ("epochMillis".into(), Value::Int(469020674645)),
            ("timezone".into(), Value::String("Europe/Stockholm".into())),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>()
        .into(),
    )))
    .unwrap();
    assert_eq!(dt.component("offset"), Ok(Value::String("+01:00".into())));
    assert_eq!(dt.component("epochMillis"), Ok(Value::Int(469020674645)));
}

/// Only `datetime` has the epoch-map form: every other constructor rejects
/// an epoch field the way the reference engine does (`Not supported:
/// epochMillis`), instead of reading the same instant at a coarser kind.
#[test]
fn non_datetime_kinds_reject_epoch_millis() {
    let ms = Value::Map(
        [("epochMillis".into(), Value::Int(469020674645))]
            .into_iter()
            .collect::<BTreeMap<_, _>>()
            .into(),
    );
    for rejected in [
        construct_date(Some(&ms)),
        construct_localdatetime(Some(&ms)),
        construct_time(Some(&ms)),
        construct_localtime(Some(&ms)),
    ] {
        assert!(rejected.is_err(), "an epoch map is datetime-only");
    }
}

/// Epoch values outside chrono's timestamp range fail, not panic.
#[test]
fn epoch_fields_out_of_range_error() {
    let ms = Value::Map(
        [("epochMillis".into(), Value::Int(i64::MAX))]
            .into_iter()
            .collect::<BTreeMap<_, _>>()
            .into(),
    );
    assert!(construct_datetime(Some(&ms)).is_err());
    assert!(construct_date(Some(&ms)).is_err());
}

/// Non-integer epoch values are a typed error, not a silent default.
#[test]
fn epoch_fields_must_be_integers() {
    let ms = Value::Map(
        [("epochMillis".into(), Value::Float(469020674.645))]
            .into_iter()
            .collect::<BTreeMap<_, _>>()
            .into(),
    );
    assert!(construct_datetime(Some(&ms)).is_err());
}

// ADR-0325/0329: the typed helpers the Bolt structures rest on. The
// UTC-instant resolver's own cases live in `zone.rs` (ADR-0340).
#[test]
fn zoned_equality_degrades_not_panics_on_out_of_range_offset() {
    // The Bolt parameter decoders accept any i32 offset, so utc + offset can
    // leave chrono's range; equality (and every bucket derived from its
    // identity) degrades to the UTC wall clock instead of panicking.
    let extreme = |zone: Option<&str>| Temporal::DateTime {
        utc: chrono::NaiveDateTime::MAX,
        offset: 100_000,
        zone: zone.map(str::to_string),
    };
    let payload = extreme(None);
    let twin = payload.clone();
    assert!(payload.equals(&twin));
    assert_eq!(payload.datetime_eq_identity(), twin.datetime_eq_identity());
    // Distinct zones stay distinct even on the degraded local clock.
    assert!(!payload.equals(&extreme(Some("Europe/Stockholm"))));
    // Rendering the same payload takes the same stated UTC fallback: the
    // wall clock is the UTC value unchanged and the suffix is the supplied
    // offset (+27:46:40), so a regression that rendered another clock or
    // suffix without panicking still fails here.
    assert_eq!(
        payload.iso_string(),
        format!(
            "{}+27:46:40",
            chrono::NaiveDateTime::MAX.format("%Y-%m-%dT%H:%M:%S%.9f")
        )
    );
}

// Temporal `=` is offset/zone-sensitive and is not `compare() == Equal`.
#[test]
fn temporal_equality_is_offset_and_zone_sensitive() {
    use crate::temporal::ZoneId;
    use chrono::NaiveTime;

    let time = |h, m, offset| Temporal::Time {
        time: NaiveTime::from_hms_opt(h, m, 0).expect("valid time"),
        offset,
    };
    // Same instant, different offsets: unequal, and ordered on the offset.
    let a = time(12, 0, 3600);
    let b = time(11, 0, 0);
    assert!(!a.equals(&b));
    assert_eq!(a.compare(&b), Some(Ordering::Greater));
    // `Z` and `+00:00` are the same time.
    assert!(time(12, 0, 0).equals(&time(12, 0, 0)));

    // The two halves of a DST fall-back overlap: equal, yet ordered.
    let utc = chrono::NaiveDate::from_ymd_opt(2025, 10, 26)
        .expect("valid date")
        .and_hms_opt(0, 30, 0)
        .expect("valid time");
    let first = Temporal::DateTime {
        utc,
        offset: 7200,
        zone: Some("Europe/Stockholm".into()),
    };
    let second = Temporal::DateTime {
        utc: utc + chrono::Duration::hours(1),
        offset: 3600,
        zone: Some("Europe/Stockholm".into()),
    };
    assert!(first.equals(&second));
    assert_eq!(first.compare(&second), Some(Ordering::Less));
    // A named zone never equals the offset it denotes.
    let plain = Temporal::DateTime {
        utc,
        offset: 3600,
        zone: None,
    };
    assert!(!plain.equals(&second));
    // Durations are equality-only.
    let d = Temporal::Duration {
        months: 0,
        days: 1,
        nanos: 0,
    };
    assert!(d.equals(&d));
    assert_eq!(d.compare(&d), None);
    assert_eq!(ZoneId::of(Some("Z"), 0), ZoneId::Offset(0));
}

#[test]
fn chronological_same_instant_and_same_representation() {
    // The TemporalChronologicalCompare matrix rows that pin the two
    // relations: one instant under several offset/zone spellings, the DST
    // fall-back overlap (equal representation, different instants), and
    // Z == +00:00.
    let dt = |s| parse_iso("datetime", s).unwrap();
    let offset = dt("2020-01-01T12:00+01:00");
    let zulu = dt("2020-01-01T11:00Z");
    assert!(offset.same_instant(&zulu));
    assert!(!offset.equals(&zulu));
    let named = dt("2020-01-01T12:00[Europe/Stockholm]");
    assert!(offset.same_instant(&named));
    assert!(!offset.equals(&named));
    let early = dt("2020-10-25T02:30+02:00[Europe/Stockholm]");
    let late = dt("2020-10-25T02:30+01:00[Europe/Stockholm]");
    assert!(!early.same_instant(&late));
    assert!(early.equals(&late));
    let z = dt("2020-01-01T12:00Z");
    let plus = dt("2020-01-01T12:00+00:00");
    assert!(z.same_instant(&plus));
    assert!(z.equals(&plus));
    // Unlike kinds never reach the relations in the operator, and the
    // relations themselves stay false for them.
    let d = parse_iso("date", "2020-01-01").unwrap();
    assert!(!d.same_instant(&zulu));
    assert!(!d.equals(&zulu));
}

#[test]
fn chronological_instant_ordering_for_zoned_kinds() {
    let dt = |s| parse_iso("datetime", s).unwrap();
    // Equal instants under different offsets must order Equal -- the plain
    // `compare` tie-breaks by offset there, which the chronological
    // operator must not inherit.
    let offset = dt("2020-01-01T12:00+01:00");
    let zulu = dt("2020-01-01T11:00Z");
    assert_eq!(
        offset.instant_ordering(&zulu),
        Some(std::cmp::Ordering::Equal)
    );
    let t = |s| parse_iso("time", s).unwrap();
    // Zoned times order by instant across midnight, unwrapped.
    let past_midnight = t("23:30-02:00"); // 01:30 UTC the next day
    let early_utc = t("01:30Z");
    assert_eq!(
        past_midnight.instant_ordering(&early_utc),
        Some(std::cmp::Ordering::Greater)
    );
    assert!(!past_midnight.same_instant(&early_utc));
    let before_midnight = t("00:30+02:00"); // 22:30 UTC the previous day
    assert_eq!(
        before_midnight.instant_ordering(&early_utc),
        Some(std::cmp::Ordering::Less)
    );
    // Same clock reading, different day: a different instant (the
    // feature's "across midnight" row -- si false, before true).
    assert!(!before_midnight.same_instant(&t("22:30Z")));
    // Same local clock, different offsets: the smaller offset is the
    // earlier instant.
    assert_eq!(
        t("12:00+02:00").instant_ordering(&t("12:00+01:00")),
        Some(std::cmp::Ordering::Less)
    );
}

/// The padded-ISO fast value is the general construction's value, and
/// only ever answers for the exact padded form of a real calendar date:
/// for every case — real calendar dates, impossible padded dates (the
/// general policy's typed error), the other accepted forms (compact,
/// month-only, week and ordinal dates), signed and trimmed variants, and
/// garbage — a `Some` answer must equal what `construct_named` builds,
/// and anything else must be `None`. The scan fast path's correctness is
/// this mirror.
#[test]
fn padded_iso_date_value_mirrors_the_general_construction() {
    let cases = [
        "1990-01-01",
        "1985-06-15",
        "0000-01-01",
        "9999-12-31",
        "2000-02-29",
        // A padded form that is not a calendar date: neither the fast path
        // nor the general policy builds a value (the year is inside chrono's
        // range, so the calendar is checked).
        "1990-02-30",
        "2000-13-01",
        "1990-00-10",
        // Other accepted forms — never the padded fast path.
        "19900101",
        "1990-01",
        "1990",
        "1816-W01-1",
        "2016-366",
        "+1990-01-01",
        " 1990-01-01",
        "1990-01-01 ",
        "990-01-01",
        "19900-01-01",
        "1990-1-01",
        "1990-01-1",
        "not a date",
        "",
    ];
    for case in cases {
        let fast = padded_iso_date_value(case);
        let general = construct_named("date", &[Value::String(case.into())]);
        let general_ok = general.is_ok();
        match (&fast, general) {
            // The fast path answered: the general policy must agree on
            // the value, and must not have errored.
            (Some(value), Ok(general_value)) => assert_eq!(value, &general_value, "{case:?}"),
            (Some(_), Err(err)) => panic!("fast value for {case:?} but general errors: {err}"),
            // The fast path declined: nothing to check — the general
            // policy keeps whatever behavior it has for the shape.
            (None, _) => {}
        }
        // The exact-form check itself: a `Some` answer only for the
        // ten-byte padded shape that the general policy also builds — the
        // calendar validity comes from that policy, never a hand-listed
        // set of impossible dates.
        let is_form = case.len() == 10 && case.as_bytes()[4] == b'-' && case.as_bytes()[7] == b'-';
        assert_eq!(
            fast.is_some(),
            is_form && general_ok,
            "form classification {case:?}"
        );
    }
}

/// The CIP-228 pattern-form predicate and the construction policy read the
/// same vocabulary — the six base constructors and their six GQL alias
/// spellings, case-insensitively — and agree on all of it: every name the
/// dialect gate accepts constructs through `construct_named` (uppercase
/// variants included), and a non-constructor name does neither.
#[test]
fn accepts_pattern_covers_the_constructors_and_their_aliases() {
    let constructors: &[(&str, &str)] = &[
        ("date", "2024-06-27"),
        ("time", "14:30:45Z"),
        ("localtime", "14:30:45"),
        ("datetime", "2024-06-27T14:30:45Z"),
        ("localdatetime", "2024-06-27T14:30:45"),
        ("duration", "P1D"),
        ("local_time", "14:30:45"),
        ("local_datetime", "2024-06-27T14:30:45"),
        ("local_date_time", "2024-06-27T14:30:45"),
        ("zoned_time", "14:30:45+02:00"),
        ("zoned_datetime", "2024-06-27T14:30:45+02:00"),
        ("zoned_date_time", "2024-06-27T14:30:45+02:00"),
    ];
    for &(name, value) in constructors {
        for spelling in [name.to_string(), name.to_ascii_uppercase()] {
            assert!(accepts_pattern(&spelling), "{spelling}");
            assert!(
                construct_named(&spelling, &[Value::String(value.into())]).is_ok(),
                "{spelling} must construct through the same vocabulary"
            );
        }
    }
    for name in [
        "vector",
        "coalesce",
        "datetime_from_epoch",
        "date.truncate",
        "",
    ] {
        assert!(!accepts_pattern(name), "{name}");
        assert!(
            construct_named(name, &[Value::String("P1D".into())]).is_err(),
            "{name} is not a constructor"
        );
    }
}

/// A constructor call with more than the two grammar arguments is a typed
/// arity error instead of a construction over the first argument; the
/// zero-, one- and two-argument forms keep constructing.
#[test]
fn construct_named_rejects_more_than_two_arguments() {
    let value = Value::String("2024-06-27".into());
    let pattern = Value::String("yyyy-MM-dd".into());
    let extra = Value::String("extra".into());
    for name in ["date", "LOCAL_TIME", "zoned_datetime", "duration"] {
        let err = construct_named(name, &[value.clone(), pattern.clone(), extra.clone()])
            .expect_err("three arguments must be rejected");
        assert!(
            err.to_string().contains("InvalidNumberOfArguments"),
            "{name}: {err}"
        );
    }
    assert!(construct_named("date", &[]).is_ok(), "the clock form");
    assert!(construct_named("date", &[value.clone()]).is_ok());
    assert!(construct_named("date", &[value, pattern]).is_ok());
}

/// LABS-711: a temporal component map's fields are integer-typed. A present
/// non-integer (string/bool/list/integral float) is Neo4j's non-assignable
/// component error, never a silently absent field (`date({year: 2000,
/// month: 'x'})` used to answer 2000-01-01).
#[test]
fn temporal_map_non_integer_components_are_typed_errors() {
    let string = || Value::String("x".into());
    let list = || Value::List(vec![Value::Int(1), Value::Int(2)].into());
    assert!(construct_date(Some(&component_map(&[
        ("year", Value::Int(2000)),
        ("month", string()),
    ])))
    .is_err());
    assert!(construct_date(Some(&component_map(&[
        ("year", Value::Int(2000)),
        ("week", string()),
    ])))
    .is_err());
    assert!(construct_date(Some(&component_map(&[("year", Value::Float(2000.0))]))).is_err());
    assert!(construct_localtime(Some(&component_map(&[
        ("hour", Value::Int(12)),
        ("minute", list()),
    ])))
    .is_err());
    assert!(construct_datetime(Some(&component_map(&[
        ("year", Value::Int(2000)),
        ("month", Value::Int(1)),
        ("day", Value::Int(1)),
        ("hour", Value::Bool(true)),
    ])))
    .is_err());
    assert!(construct_localdatetime(Some(&component_map(&[
        ("year", Value::Int(2000)),
        ("month", Value::Int(1)),
        ("day", Value::Int(1)),
        ("nanosecond", Value::Bool(true)),
    ])))
    .is_err());
}

/// LABS-711: sub-second fields are range-checked per field, and the finer
/// fields' bounds narrow when a coarser sibling is present (`validNano`):
/// millisecond 0..999; microsecond 0..999 beside milliseconds else
/// 0..999_999; nanosecond 0..999 beside microseconds, 0..999_999 beside
/// milliseconds, else 0..999_999_999.
#[test]
fn temporal_map_subsecond_fields_are_range_checked() {
    let time = |entries: &[(&str, Value)]| {
        let mut all = vec![
            ("hour", Value::Int(1)),
            ("minute", Value::Int(1)),
            ("second", Value::Int(1)),
        ];
        all.extend_from_slice(entries);
        construct_time(Some(&component_map(&all)))
    };
    for entries in [
        vec![("millisecond", Value::Int(-1))],
        vec![("millisecond", Value::Int(1000))],
        vec![
            ("millisecond", Value::Int(0)),
            ("microsecond", Value::Int(1000)),
        ],
        vec![
            ("millisecond", Value::Int(0)),
            ("nanosecond", Value::Int(1_000_000)),
        ],
        vec![
            ("microsecond", Value::Int(0)),
            ("nanosecond", Value::Int(1000)),
        ],
        vec![("microsecond", Value::Int(1_000_000))],
        vec![("nanosecond", Value::Int(1_000_000_000))],
    ] {
        assert!(time(&entries).is_err(), "{entries:?}");
    }
    assert!(time(&[
        ("millisecond", Value::Int(999)),
        ("microsecond", Value::Int(999)),
        ("nanosecond", Value::Int(999)),
    ])
    .is_ok());
}

/// LABS-711 review: a quarter outside 1..=4 (including `quarter: 0`) reaches
/// the typed `invalid date` error instead of being clamped to January, and a
/// negative `dayOfQuarter` is rejected before the `u64` conversion can lose
/// its sign.
#[test]
fn temporal_map_quarter_components_are_validated() {
    assert!(construct_date(Some(&component_map(&[
        ("year", Value::Int(2020)),
        ("quarter", Value::Int(0)),
    ])))
    .is_err());
    assert!(construct_date(Some(&component_map(&[
        ("year", Value::Int(2020)),
        ("quarter", Value::Int(1)),
        ("dayOfQuarter", Value::Int(-1)),
    ])))
    .is_err());
}

/// LABS-711 review: the quarter probe and the day-of-quarter read are the
/// same validated fact, so a null-valued `dayOfQuarter` behaves exactly like
/// an absent one (the old `contains_key` probe forced the quarter's first
/// month while the read contributed nothing).
#[test]
fn temporal_map_null_day_of_quarter_does_not_shift_the_quarter() {
    let date = Value::Temporal(parse_iso("date", "2020-09-15").unwrap());
    let with_null = construct_date(Some(&component_map(&[
        ("date", date.clone()),
        ("quarter", Value::Int(3)),
        ("dayOfQuarter", Value::Null),
    ])))
    .unwrap();
    let absent = construct_date(Some(&component_map(&[
        ("date", date),
        ("quarter", Value::Int(3)),
    ])))
    .unwrap();
    assert_eq!(with_null, absent);
    assert_eq!(with_null.iso_string(), "2020-09-15");
}

/// LABS-711 review: the fraction-of-second width policy is shared — a run of
/// more than nine digits is a pattern error for a date/time `S` arm as well as
/// for a duration's `n`/`S` spellings, never a silent truncation.
#[test]
fn temporal_format_fraction_width_above_nine_is_an_error() {
    let date = parse_iso("datetime", "2020-09-15T12:34:56.123456789Z").unwrap();
    assert!(format_pattern(&date, "SSSSSSSSSS").is_err());
    assert!(format_pattern(&date, "SSSSSSSSS").is_ok());
    let duration = Temporal::Duration {
        months: 0,
        days: 0,
        nanos: 123_456_789,
    };
    assert!(format_pattern(&duration, "nnnnnnnnnn").is_err());
    assert!(format_pattern(&duration, "nnnnnnnnn").is_ok());
}

/// A `GMT`/`UTC` prefixed offset keeps its spelling as the value's time zone
/// while the arithmetic uses the numeric tail.
#[test]
fn gmt_prefixed_offsets_keep_their_spelling() {
    let dt = construct_datetime(Some(&component_map(&[
        ("year", Value::Int(2025)),
        ("timezone", Value::String("GMT+03:00".into())),
    ])))
    .unwrap();
    assert_eq!(
        dt.component("timezone").unwrap(),
        Value::String("GMT+03:00".into())
    );
    assert_eq!(
        dt.component("offset").unwrap(),
        Value::String("+03:00".into())
    );
    assert_eq!(dt.component("offsetMinutes").unwrap(), Value::Int(180));
    assert_eq!(dt.iso_string(), "2025-01-01T00:00+03:00");
}

/// A `datetime`/`localdatetime` needs a time-bearing source; a date (or a
/// bare time) cannot be widened into one.
#[test]
fn datetime_kinds_need_a_time_bearing_source() {
    let date = Value::Temporal(Temporal::Date(
        NaiveDate::from_ymd_opt(1984, 2, 11).unwrap(),
    ));
    let time = Value::Temporal(Temporal::LocalTime(
        NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
    ));
    assert!(construct_datetime(Some(&date)).is_err());
    assert!(construct_localdatetime(Some(&date)).is_err());
    assert!(construct_datetime(Some(&time)).is_err());
    assert!(construct_localdatetime(Some(&time)).is_err());
    assert!(construct_date(Some(&time)).is_err());
    let local = Value::Temporal(parse_iso("localdatetime", "1984-02-11T12:00").unwrap());
    assert!(construct_datetime(Some(&local)).is_ok());
}
