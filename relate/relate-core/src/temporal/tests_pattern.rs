//! CIP-228 pattern parsing: the instant pattern-character table, the
//! per-kind unsupported-character rejections, the mismatch/ambiguity
//! errors, and the duration grammar (fields, literals, escapes, overflow).

use super::*;

/// One row of `TemporalParseInstant.feature`'s "Parse a temporal from
/// every pattern character" outline, asserted through the canonical ISO
/// rendering (`iso_string`).
#[test]
fn instant_pattern_characters_parse_like_neo4j() {
    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "datetime",
            "2024-06-27T14:30:45.123+0200",
            "yyyy-MM-dd'T'HH:mm:ss.SSSZ",
            "2024-06-27T14:30:45.123+02:00",
        ),
        (
            "datetime",
            "Thursday, June 27, AD 2024",
            "EEEE, MMMM d, G uuuu",
            "2024-06-27T00:00Z",
        ),
        (
            "datetime",
            "2024, 179 day of year 5 day-of-week",
            "yyyy, DDD 'day of year' c 'day-of-week'",
            "2024-06-27T00:00Z",
        ),
        (
            "datetime",
            "2024-06-27 14:30 CEST",
            "yyyy-MM-dd k:mm z",
            "2024-06-27T14:30+02:00[Europe/Paris]",
        ),
        (
            "datetime",
            "2024-06-27 2:30 PM GMT+2",
            "yyyy-MM-dd K:mm a O",
            "2024-06-27T14:30+02:00",
        ),
        (
            "datetime",
            "2024 Jun 27, 52245123 30 45",
            "yyyy LLL d, A m s",
            "2024-06-27T14:30:45.123Z",
        ),
        (
            "datetime",
            "Nano: 123456789 of day: 52245123456789 2024-06-27",
            "'Nano:' n 'of day:' N yyyy-MM-dd",
            "2024-06-27T14:30:45.123456789Z",
        ),
        (
            "datetime",
            "2024-06-27 02:30 PM Europe/Paris in the afternoon",
            "yyyy-MM-dd hh:mm a VV B",
            "2024-06-27T14:30+02:00[Europe/Paris]",
        ),
        (
            "datetime",
            "2024-W26-5",
            "YYYY-'W'ww-e",
            "2024-06-27T00:00Z",
        ),
        (
            "datetime",
            "2nd quarter 2024 day: 179",
            "QQQQ yyyy 'day': D",
            "2024-06-27T00:00Z",
        ),
        ("datetime", " 24-06-27", "pppyy-MM-dd", "2024-06-27T00:00Z"),
        (
            "datetime",
            "2024-June-4-5",
            "yyyy-MMMM-F-e",
            "2024-06-27T00:00Z",
        ),
        ("datetime", "60488", "g", "2024-06-27T00:00Z"),
        (
            "localdatetime",
            "2024-06-27T14:30:45.123",
            "yyyy-MM-dd'T'HH:mm:ss.SSS",
            "2024-06-27T14:30:45.123",
        ),
        (
            "localdatetime",
            "Thursday, June 27, AD 2024",
            "EEEE, MMMM d, G uuuu",
            "2024-06-27T00:00",
        ),
        (
            "localdatetime",
            "2024, 179 day of year 5 day-of-week",
            "yyyy, DDD 'day of year' c 'day-of-week'",
            "2024-06-27T00:00",
        ),
        (
            "localdatetime",
            "2024-06-27 14:30",
            "yyyy-MM-dd k:mm",
            "2024-06-27T14:30",
        ),
        (
            "localdatetime",
            "2024-06-27 2:30 PM",
            "yyyy-MM-dd K:mm a",
            "2024-06-27T14:30",
        ),
        (
            "localdatetime",
            "2024 Jun 27, 52245123 30 45",
            "yyyy LLL d, A m s",
            "2024-06-27T14:30:45.123",
        ),
        (
            "localdatetime",
            "Nano: 123456789 of day: 52245123456789 2024-06-27",
            "'Nano:' n 'of day:' N yyyy-MM-dd",
            "2024-06-27T14:30:45.123456789",
        ),
        (
            "localdatetime",
            "2024-06-27 02:30 PM in the afternoon",
            "yyyy-MM-dd hh:mm a B",
            "2024-06-27T14:30",
        ),
        (
            "localdatetime",
            "2024-W26-5",
            "YYYY-'W'ww-e",
            "2024-06-27T00:00",
        ),
        (
            "localdatetime",
            "2nd quarter 2024 day: 179",
            "QQQQ yyyy 'day': D",
            "2024-06-27T00:00",
        ),
        (
            "localdatetime",
            " 24-06-27",
            "pppyy-MM-dd",
            "2024-06-27T00:00",
        ),
        (
            "localdatetime",
            "2024-June-4-5",
            "yyyy-MMMM-F-e",
            "2024-06-27T00:00",
        ),
        ("localdatetime", "60488", "g", "2024-06-27T00:00"),
        ("date", "2024-06-27", "yyyy-MM-dd", "2024-06-27"),
        (
            "date",
            "Thursday, June 27, AD 2024",
            "EEEE, MMMM d, G uuuu",
            "2024-06-27",
        ),
        (
            "date",
            "2024, 179 day of year 5 day-of-week",
            "yyyy, DDD 'day of year' c 'day-of-week'",
            "2024-06-27",
        ),
        ("date", "2024 Jun 27", "yyyy LLL d", "2024-06-27"),
        ("date", "2024-W26-5", "YYYY-'W'ww-e", "2024-06-27"),
        (
            "date",
            "2nd quarter 2024 day: 179",
            "QQQQ yyyy 'day': D",
            "2024-06-27",
        ),
        ("date", " 24-06-27", "pppyy-MM-dd", "2024-06-27"),
        ("date", "2024-June-4-5", "yyyy-MMMM-F-e", "2024-06-27"),
        ("date", "60488", "g", "2024-06-27"),
        (
            "time",
            "14:30:45.123+0200",
            "HH:mm:ss.SSSZ",
            "14:30:45.123+02:00",
        ),
        ("time", "14:30+02", "k:mmx", "14:30+02:00"),
        ("time", "14:30-0830", "k:mmX", "14:30-08:30"),
        ("time", "2:30 PM GMT+2", "K:mm a O", "14:30+02:00"),
        ("time", "52245123 30 45", "A m s", "14:30:45.123Z"),
        (
            "time",
            "Nano: 123456789 of day: 52245123456789",
            "'Nano:' n 'of day:' N",
            "14:30:45.123456789Z",
        ),
        (
            "time",
            "02:30 PM +02:00 in the afternoon",
            "hh:mm a VV B",
            "14:30+02:00",
        ),
        ("localtime", "14:30:45.123", "HH:mm:ss.SSS", "14:30:45.123"),
        ("localtime", "14:30", "k:mm", "14:30"),
        ("localtime", "2:30 PM", "K:mm a", "14:30"),
        ("localtime", "52245123 30 45", "A m s", "14:30:45.123"),
        (
            "localtime",
            "Nano: 123456789 of day: 52245123456789",
            "'Nano:' n 'of day:' N",
            "14:30:45.123456789",
        ),
        (
            "localtime",
            "02:30 PM in the afternoon",
            "hh:mm a B",
            "14:30",
        ),
    ];
    for &(kind, input, pattern, expected) in cases {
        let parsed = parse_pattern(kind, input, pattern)
            .unwrap_or_else(|e| panic!("{kind}('{input}', '{pattern}'): {e}"));
        assert_eq!(
            parsed.iso_string(),
            expected,
            "{kind}('{input}', '{pattern}')"
        );
    }
}

/// Compact numeric patterns with adjacent fields of a known width
/// (`HHmmssSSS`, `yyyyMMddHHmmss`), as the GQL function aliases use.
#[test]
fn instant_pattern_compact_numeric_fields() {
    assert_eq!(
        parse_pattern("localtime", "123456789", "HHmmssSSS")
            .unwrap()
            .iso_string(),
        "12:34:56.789"
    );
    assert_eq!(
        parse_pattern("localdatetime", "20200101120000", "yyyyMMddHHmmss")
            .unwrap()
            .iso_string(),
        "2020-01-01T12:00"
    );
    assert_eq!(
        parse_pattern("time", "123456+0100", "HHmmssZ")
            .unwrap()
            .iso_string(),
        "12:34:56+01:00"
    );
    assert_eq!(
        parse_pattern("datetime", "20200101120000+0100", "yyyyMMddHHmmssZ")
            .unwrap()
            .iso_string(),
        "2020-01-01T12:00+01:00"
    );
}

/// Each per-kind "unsupported pattern character" row is one letter against
/// a full-length input; every row is a mismatch because no field consumes
/// the whole input.
#[test]
fn unsupported_single_letter_patterns_are_mismatches() {
    const DATE: &str = "2024-06-27";
    const DATETIME: &str = "2024-06-27T14:30:45.123456789+02:00[Europe/Paris]";
    const LOCAL_DATETIME: &str = "2024-06-27T14:30:45.123";
    const TIME: &str = "14:30:45.123+02:00";
    const LOCAL_TIME: &str = "14:30:45.123";
    let cases: &[(&str, &str, &str)] = &[
        (
            "date",
            DATE,
            "A a B b C f H h I i J j K k l m N n O o P R r S s T t U v X x Z z",
        ),
        ("datetime", DATETIME, "b C f I i J j l o P R r T t U V"),
        (
            "localdatetime",
            LOCAL_DATETIME,
            "b C f I i J j l O o P R r T t U V v X x Z z",
        ),
        (
            "time",
            TIME,
            "b C c D d E e F f G g I i J j L l M o P Q q R r T t U u V v W w Y y z",
        ),
        (
            "localtime",
            LOCAL_TIME,
            "b C c D d E e F f G g I i J j L l M O o P Q q R r T t U u V v W w X x Y y Z z",
        ),
    ];
    for &(kind, input, letters) in cases {
        for letter in letters.split_whitespace() {
            assert!(
                parse_pattern(kind, input, letter).is_err(),
                "{kind}('{input}', '{letter}') must fail"
            );
        }
    }
}

/// Pattern/input mismatches, ambiguity, and trailing-input rejections.
#[test]
fn instant_pattern_mismatches_error() {
    let cases: &[(&str, &str, &str)] = &[
        // Input ends before the pattern does.
        (
            "datetime",
            "2024-06-27T14:30:45.123+0200 hello",
            "yyyy-MM-dd'T'HH:mm:ss.SSSZ",
        ),
        (
            "datetime",
            "2024-06-27T14:30:45.123+0200",
            "yyyy-MM-dd'T'HH:mm:ss.SSSZ 'hello'",
        ),
        (
            "localdatetime",
            "2024-06-27T14:30:45.123 hello",
            "yyyy-MM-dd'T'HH:mm:ss.SSS",
        ),
        ("date", "2024-06-27 hello", "yyyy-MM-dd"),
        ("time", "14:30:45.123+0200 hello", "HH:mm:ss.SSSZ"),
        ("localtime", "14:30:45.123 hello", "HH:mm:ss.SSS"),
        // A literal that does not match the input.
        ("date", "2024_06_27", "yyyy-MM-dd"),
        ("datetime", "2024-06-27 14.30", "yyyy-MM-dd HH:mm"),
        ("date", "2024-06-27 UTC", "yyyy-MM-dd 'CEST'"),
        ("localtime", "14.30", "HH:mm"),
        // Jumbled fields, duplicate fields, unknown letters, empty inputs.
        (
            "datetime",
            "2024-06-27T14:30:45.123+0200",
            "'T'HH:mm:ss.SSSZyyyy-MM-dd",
        ),
        ("date", "2024-2025 07 12", "yyyy-yyyy MM dd"),
        ("localtime", "17:16:30:12", "hh:hh:mm:ss"),
        ("date", "2024-06-27", "yyyy-MM-JJ"),
        ("date", "", "yyyy-MM-dd"),
        ("date", "2024-06-27", ""),
        // Incomplete dates.
        ("datetime", "2024-06", "yyyy-MM"),
        ("date", "2024", "yyyy"),
        // A 12-hour clock needs its AM/PM marker.
        ("localtime", "2:30", "K:mm"),
        ("localtime", "2:30", "h:mm"),
        // A conflicting day-of-week.
        ("date", "2024-06-27 Sunday", "yyyy-MM-dd EEEE"),
        ("date", "2024-06-27 3", "yyyy-MM-dd e"),
    ];
    for &(kind, input, pattern) in cases {
        assert!(
            parse_pattern(kind, input, pattern).is_err(),
            "{kind}('{input}', '{pattern}') must fail"
        );
    }
}

/// A numeric field whose digits do not fit `i64` is a value-range failure,
/// not a pattern mismatch (the digits are shape-valid).
#[test]
fn instant_pattern_numeric_overflow_is_a_range_error() {
    for (kind, input, pattern) in [
        ("date", "999999999999999999999999-01-01", "y-MM-dd"),
        ("localtime", "99999999999999999999", "H"),
        ("date", "99999999999999999999", "g"),
    ] {
        let error = parse_pattern(kind, input, pattern)
            .expect_err(&format!("{kind}('{input}', '{pattern}') must fail"));
        assert!(
            error.to_string().contains("out of range"),
            "{kind}('{input}', '{pattern}'): {error}"
        );
    }
    // A shape mismatch stays a mismatch.
    let error = parse_pattern("date", "abc", "yyyy-MM-dd").expect_err("mismatch");
    assert!(error.to_string().contains("mismatched pattern"), "{error}");
}

/// CIP-228's subset rule: a field the target kind does not use is parsed
/// and ignored, and a date-only datetime pattern ignores an offset.
#[test]
fn instant_pattern_parses_fields_the_kind_does_not_use() {
    assert_eq!(
        parse_pattern("datetime", "2024-06-27+02:00", "yyyy-MM-ddXXX")
            .unwrap()
            .iso_string(),
        "2024-06-27T00:00Z"
    );
    assert_eq!(
        parse_pattern("localtime", "5 AD", "H G")
            .unwrap()
            .iso_string(),
        "05:00"
    );
    assert_eq!(
        parse_pattern("localdatetime", "2024-06-27 Europe/Paris", "yyyy-MM-dd VV")
            .unwrap()
            .iso_string(),
        "2024-06-27T00:00"
    );
    assert_eq!(
        parse_pattern("date", "2024-06-27 PM", "yyyy-MM-dd a")
            .unwrap()
            .iso_string(),
        "2024-06-27"
    );
}

/// A `[...]` section matches when present and is skipped when absent.
#[test]
fn instant_pattern_optional_section() {
    let pattern = "yyyy-MM-dd'T'HH:mm:ss[XXX]";
    assert_eq!(
        parse_pattern("datetime", "2024-06-27T10:15:30+02:00", pattern)
            .unwrap()
            .iso_string(),
        "2024-06-27T10:15:30+02:00"
    );
    assert_eq!(
        parse_pattern("datetime", "2024-06-27T10:15:30", pattern)
            .unwrap()
            .iso_string(),
        "2024-06-27T10:15:30Z"
    );
}

/// ISO-string parsing checks the calendar for in-range years: an impossible
/// leap day is a typed error, while out-of-chrono-range years keep the civil
/// representation — with the same calendar check applied to their month and
/// day before that representation is chosen.
#[test]
fn iso_date_rejects_impossible_calendar_dates() {
    assert!(parse_iso("date", "1983-02-29").is_err());
    assert!(parse_iso("localdatetime", "1983-02-29T01:01:01").is_err());
    assert!(parse_iso("date", "1990-02-30").is_err());
    assert_eq!(
        parse_iso("date", "999999999-01-01").unwrap().iso_string(),
        "+999999999-01-01"
    );
    // Civil-range years get the same month/day check, not a free pass.
    for civil in [
        "999999999-13-45",
        "999999999-00-10",
        "999999999-02-30",
        "999999999-04-31",
        "-999999999-13-01",
        "-999999999-02-29",
    ] {
        assert!(parse_iso("date", civil).is_err(), "{civil}");
    }
    // The proleptic leap rule holds on both sides of year zero.
    assert!(parse_iso("date", "999999996-02-29").is_ok());
    assert!(parse_iso("date", "-999999996-02-29").is_ok());
    assert!(parse_iso("date", "-999999998-02-29").is_err());
}

// ---------------------------------------------------------------------------
// Durations
// ---------------------------------------------------------------------------

/// The CIP-228 duration field table and its variations.
#[test]
fn duration_pattern_fields_and_variations() {
    let cases: &[(&str, &str, &str)] = &[
        ("1 years 1 quarters 1 months 3 weeks 4 days 5 hours 6 minutes 7 seconds 8009010 nanos", "y 'years' q 'quarters' M 'months' w 'weeks' d 'days' h 'hours' m 'minutes' s 'seconds' N 'nanos'", "P1Y4M25DT5H6M7.00800901S"),
        ("5 quarters 1 months 3 weeks 4 days 5 hours 6 minutes 7 seconds 8009010 nanos", "q 'quarters' M 'months' w 'weeks' d 'days' h 'hours' m 'minutes' s 'seconds' N 'nanos'", "P1Y4M25DT5H6M7.00800901S"),
        ("16 months 3 weeks 4 days 5 hours 6 minutes 7 seconds 8009010 nanos", "M 'months' w 'weeks' d 'days' h 'hours' m 'minutes' s 'seconds' N 'nanos'", "P1Y4M25DT5H6M7.00800901S"),
        ("306 minutes 7 seconds 8009010 nanos", "m 'minutes' s 'seconds' N 'nanos'", "PT5H6M7.00800901S"),
        ("18367 seconds 8 millis", "s 'seconds' A 'millis'", "PT5H6M7.008S"),
        ("18367008 millis", "A 'millis'", "PT5H6M7.008S"),
        ("18367008009010 nanos", "N 'nanos'", "PT5H6M7.00800901S"),
        ("18367.0080090 seconds", "s.SSSSSSS 'seconds'", "PT5H6M7.008009S"),
        ("1-4-3", "y-M-d", "P1Y4M3D"),
        ("1 1 14", "y q M", "P2Y5M"),
        ("-1 1 14", "y q M", "P5M"),
        ("-1 1 145", "y q M", "P11Y4M"),
        ("1q14", "y'q'M", "P2Y2M"),
        ("1years1quarters1months", "y'years'q'quarters'M'months'", "P1Y4M"),
        ("years1years1quarters1months", "'years'y'years'q'quarters'M'months'", "P1Y4M"),
        (" 1 1 1 ", " y q M ", "P1Y4M"),
        ("1-4-3BbCcEeFfGgIiJjOoPpRrTtVvXxZz", "y-M-dBbCcEeFfGgIiJjOoPpRrTtVvXxZz", "P1Y4M3D"),
        ("1-4-00000000003", "y-M-ddddddddddd", "P1Y4M3D"),
        ("P1Y4M25DT5H6M7.00800901S", "'P'y'Y'M'M'd'DT'h'H'm'M's'.'S'S'", "P1Y4M25DT5H6M7.00800901S"),
        ("3𓀡0", "s'𓀡'h", "PT3S"),
        ("ab", "'ab'", "PT0S"),
        ("-56789", "s", "PT-15H-46M-29S"),
    ];
    for &(input, pattern, expected) in cases {
        let parsed = parse_pattern("duration", input, pattern)
            .unwrap_or_else(|e| panic!("duration('{input}', '{pattern}'): {e}"));
        assert_eq!(
            parsed.iso_string(),
            expected,
            "duration('{input}', '{pattern}')"
        );
    }
}

/// A fraction run beyond nine digits is a nanosecond count, not a
/// truncation and not a rejection: Neo4j parses the run as a `long` and
/// uses it as nanos (`duration('0.1234567890', 's.n')` → `PT1.23456789S`
/// on 2026.09-enterprise), so the pad rule applies only below nine digits.
#[test]
fn duration_pattern_fraction_beyond_nine_digits_is_a_nanosecond_count() {
    assert_eq!(
        parse_pattern("duration", "0.123456789", "s.n")
            .unwrap()
            .iso_string(),
        "PT0.123456789S"
    );
    assert_eq!(
        parse_pattern("duration", "0.1234567890", "s.n")
            .unwrap()
            .iso_string(),
        "PT1.23456789S"
    );
    assert_eq!(
        parse_pattern("duration", "0.084", "s.n")
            .unwrap()
            .iso_string(),
        "PT0.084S"
    );
}

/// The fraction token (`n`/`S`) and the nanosecond field (`N`) are distinct
/// fields: both may appear once each, and both contribute to the same tier.
#[test]
fn duration_pattern_fraction_and_nanoseconds_are_distinct_fields() {
    assert_eq!(
        parse_pattern("duration", "123 a 456 b", "n 'a' N 'b'")
            .unwrap()
            .iso_string(),
        "PT0.123000456S"
    );
    assert!(parse_pattern("duration", "1 a 2 b", "n 'a' n 'b'").is_err());
    assert!(parse_pattern("duration", "1 a 2 b", "N 'a' N 'b'").is_err());
}

/// The escaped-pattern table: `''` pairs inside an escape denote one
/// literal apostrophe; doubled quotes outside toggle the escape twice.
#[test]
fn duration_pattern_escapes() {
    let cases: &[(&str, &str, &str)] = &[
        ("1  0", "h '' m", "PT1H"),
        ("1 ' 0", "h '''' m", "PT1H"),
        ("1 '' 0", "h '''''' m", "PT1H"),
        ("1 ''' 0", "h '''''''' m", "PT1H"),
        ("1 a' 0", "h 'a''' m", "PT1H"),
        ("1 'a 0", "h '''a' m", "PT1H"),
        ("1 'a' 0", "h '''a''' m", "PT1H"),
        ("5 o'clock hours", "h 'o''clock hours'", "PT5H"),
    ];
    for &(input, pattern, expected) in cases {
        assert_eq!(
            parse_pattern("duration", input, pattern)
                .unwrap_or_else(|e| panic!("duration('{input}', '{pattern}'): {e}"))
                .iso_string(),
            expected,
            "duration('{input}', '{pattern}')"
        );
    }
    // An odd number of escapes is a pattern error.
    for pattern in ["h ''' m", "h ' m", "h ''''' m", "h ''''''' m"] {
        assert!(
            parse_pattern("duration", "1 ' 1", pattern).is_err(),
            "{pattern}"
        );
    }
}

/// Literal/input mismatches, duplicate fields, missing input, and trailing
/// input are one typed failure each.
#[test]
fn duration_pattern_mismatches_error() {
    let cases: &[(&str, &str)] = &[
        ("1", "'ab'H H"),
        ("1", "'a'H"),
        ("1", "''''H H"),
        ("1", "''''H"),
        ("1", "'ab'"),
        ("b1", "'a'H"),
        ("1a0", "h 'b' m"),
        ("ab", "'ab'H m"),
        ("1q14 5", "y q M"),
        ("1_4", "y-M"),
        ("1.2", "h:m"),
        ("1-", "y --"),
        ("1 years 1 quarters", "y 'years' q"),
        ("1 years 1", "y 'years' q 'quarters'"),
        ("1 years", "y 'years' q"),
        ("ab", "'ab'H"),
        (
            "1 years 1 years 1 quarters 1 months",
            "y 'years' y 'years' q 'quarters' M 'months'",
        ),
        (
            "5 hours 6 minutes 7 seconds 8009010 nanos 8009010 nanos",
            "h 'hours' m 'minutes' s 'seconds' N 'nanos' N 'nanos'",
        ),
        ("10 seconds 12 seconds", "s 'seconds' s 'seconds'"),
        // Neo4j rejects this spelling (the CIP-228 doc example's triple
        // quotes do not denote the literal it intends).
        ("5 o'clock hours", "h '''o''clock hours'''"),
    ];
    for &(input, pattern) in cases {
        assert!(
            parse_pattern("duration", input, pattern).is_err(),
            "duration('{input}', '{pattern}') must fail"
        );
    }
}

/// Neo4j's stored-duration range contract: every component scaling and the
/// final average-length check are `i64`-checked.
#[test]
fn duration_pattern_component_overflow_errors() {
    let cases: &[(&str, &str)] = &[
        ("9223372036854775807", "y"),
        ("9223372036854775807", "q"),
        ("9223372036854775807", "w"),
        ("9223372036854775807", "h"),
        ("9223372036854775807", "m"),
        ("9223372036854775807", "A"),
        ("1-9223372036854775807", "y-M"),
        ("-9223372036854775807", "y"),
        ("9223372036854775808", "y"),
        // The shared range contract (months * average month seconds).
        ("3507324295524", "M"),
        ("106751991167301", "d"),
        ("2562047788015216", "h"),
        ("153722867280912931", "m"),
    ];
    for &(input, pattern) in cases {
        assert!(
            parse_pattern("duration", input, pattern).is_err(),
            "duration('{input}', '{pattern}') must overflow"
        );
    }
}

/// `duration(string)` applies the same stored-range contract: the
/// out-of-range component table from `TemporalCreateFromString.feature`
/// errors, and the corresponding boundary spellings still parse.
#[test]
fn iso_duration_string_range_contract() {
    let rejected = [
        "P9223372036854775808Y",
        "P9223372036854775808M",
        "P9223372036854775808W",
        "P9223372036854775808D",
        "PT9223372036854775808H",
        "PT9223372036854775808M",
        "-P9223372036854775808M",
        "P-9223372036854775809Y",
        "P-9223372036854775809M",
        "P-9223372036854775809W",
        "P-9223372036854775809D",
        "PT-9223372036854775809H",
        "PT-9223372036854775809M",
        "PT-9223372036854775809S",
        "P768614336404564651Y",
        "P1317624576693539402W",
        "P3507324295524M",
        "P106751991167301D",
        "PT2562047788015216H",
        "PT2562047788015216H1.5M",
        "PT153722867280912931M",
        "P-9223372036854775808Y",
        "P-9223372036854775808M",
        "P-9223372036854775808W",
        "P-9223372036854775808D",
        "PT-9223372036854775808H",
        "PT-9223372036854775808M",
        "P106751991167300DT2562047788015215H",
        // Exactly `2^63` seconds is outside the stored `i64` seconds field;
        // only the f64 scaling path keeps that rounded image (LABS-785).
        "PT9223372036854775808S",
    ];
    for text in rejected {
        assert!(
            parse_iso("duration", text).is_err(),
            "duration('{text}') must overflow"
        );
    }
    let accepted = [
        "P14DT16H12M",
        "P5M1.5D",
        "P0.75M",
        "PT0.75M",
        "P2.5W",
        "P12Y5M14DT16H12M70S",
        "P2012-02-02T14:37:21.545",
        "PT9223372036854775807S",
        "P292277024626Y",
        "P3507324295523M",
        "P15250284452471W",
        "P106751991167300D",
        "PT2562047788015215H",
        "PT153722867280912930M",
        "P-292277024626Y",
        "P-3507324295523M",
        "P-15250284452471W",
        "P-106751991167300D",
        "PT-2562047788015215H",
        "PT-153722867280912930M",
        // `i64::MIN` seconds is representable and stays accepted.
        "PT-9223372036854775808S",
    ];
    for text in accepted {
        assert!(
            parse_iso("duration", text).is_ok(),
            "duration('{text}') must parse"
        );
    }
}

/// The accepted side of the range boundary: a duration whose average length
/// still fits the stored seconds.
#[test]
fn duration_pattern_accepts_the_range_boundary() {
    for (input, pattern) in [
        ("3507324295523", "M"),
        ("106751991167300", "d"),
        ("2562047788015215", "h"),
        ("153722867280912930", "m"),
    ] {
        assert!(
            parse_pattern("duration", input, pattern).is_ok(),
            "duration('{input}', '{pattern}')"
        );
    }
}

/// An mjd that fits `i64` but overflows chrono's duration bound is the
/// module's typed range error, never a `ChronoDur::days` panic; an ordinary
/// modified Julian day still resolves.
#[test]
fn modified_julian_day_out_of_chrono_bound_is_a_range_error() {
    let error = parse_pattern("date", "9223372036854775807", "g")
        .expect_err("i64::MAX modified Julian day must be rejected");
    assert!(error.to_string().contains("out of range"), "{error}");
    for (input, iso) in [("51544", "2000-01-01"), ("0", "1858-11-17")] {
        assert_eq!(
            parse_pattern("date", input, "g").unwrap().iso_string(),
            iso,
            "date('{input}', 'g')"
        );
    }
}

/// A pathological `[` nesting depth is a typed pattern error, not a stack
/// overflow. The tokenizer refuses the pattern at the depth cap; that is
/// what bounds the two later recursive walks over the token tree
/// (`reject_duplicate_fields`, `match_tokens`), which only ever see trees
/// the tokenizer built.
#[test]
fn nested_optional_sections_past_the_depth_cap_are_a_pattern_error() {
    let pattern = "[".repeat(10_000);
    let error = parse_pattern("date", "2024-06-27", &pattern)
        .expect_err("10_000 nested '[' must be refused");
    assert!(error.to_string().contains("nesting too deep"), "{error}");

    // A depth under the cap still compiles and matches.
    let shallow = format!("{}yyyy-MM-dd{}", "[".repeat(10), "]".repeat(10));
    assert_eq!(
        parse_pattern("date", "2024-06-27", &shallow)
            .unwrap()
            .iso_string(),
        "2024-06-27"
    );
}

/// `read_number` leaves the position unchanged unless it succeeds, the same
/// contract as `read_literal` (shape mismatch and sign rollback included).
#[test]
fn read_number_restores_position_on_failure() {
    use super::pattern::read_number;

    let mut pos = 0;
    assert!(read_number("abc", &mut pos, None, false).unwrap().is_none());
    assert_eq!(pos, 0, "no digits leaves the position unchanged");

    let mut pos = 0;
    assert!(read_number("+x", &mut pos, None, true).unwrap().is_none());
    assert_eq!(pos, 0, "a consumed sign is rolled back");

    let mut pos = 0;
    assert!(read_number("99", &mut pos, Some(5), false)
        .unwrap()
        .is_none());
    assert_eq!(pos, 0, "a short fixed-width run is rolled back");

    let mut pos = 0;
    assert!(read_number("99999999999999999999", &mut pos, None, false).is_err());
    assert_eq!(pos, 0, "overflow is rolled back");

    let mut pos = 0;
    let digits = read_number("123", &mut pos, None, false).unwrap().unwrap();
    assert_eq!((digits.value, digits.digits, pos), (123, 3, 3));
}

/// The month/weekday name tables are the formatter's own: full names and
/// the three-letter abbreviations parse case-insensitively, and nothing
/// else (a two-letter or near-miss spelling) matches.
#[test]
fn month_and_weekday_names_parse_case_insensitively_from_the_shared_tables() {
    for (input, pattern, want) in [
        ("JUNE 27", "MMMM d", "--06-27"),
        ("jun 27", "MMM d", "--06-27"),
        ("Dec 3", "MMM d", "--12-03"),
    ] {
        let parsed = parse_pattern("date", &format!("2024 {input}"), &format!("yyyy {pattern}"))
            .unwrap_or_else(|err| panic!("{input}: {err:?}"));
        assert!(parsed.iso_string().ends_with(&want[1..]), "{input}");
    }
    let monday = parse_pattern("date", "2024-06-24 mon", "yyyy-MM-dd EEE").unwrap();
    assert_eq!(monday.iso_string(), "2024-06-24");
    for (input, pattern) in [
        ("2024 Ju 27", "yyyy MMM d"),
        ("2024 Junee 27", "yyyy MMM d"),
        ("2024-06-24 Mo", "yyyy-MM-dd EEE"),
    ] {
        assert!(parse_pattern("date", input, pattern).is_err(), "{input}");
    }
}

/// A repeated field is the one duplicate-field error in both grammars.
#[test]
fn duplicate_fields_share_one_error_across_both_grammars() {
    let instant = parse_pattern("date", "2024 2025", "yyyy yyyy").unwrap_err();
    let duration = parse_pattern("duration", "1 2", "H H").unwrap_err();
    assert_eq!(format!("{instant:?}"), format!("{duration:?}"));
}
