//! Duration parsing: the ISO-8601 grammar, the datetime-style spelling's
//! error discipline, and exact integer component construction (LABS-712).
//! Split from `tests.rs` so the family stays one file as it grows.

use super::duration::parse_duration_iso;
use super::test_support::component_map;
use super::*;

#[test]
fn duration_iso_roundtrip_hours() {
    let t = parse_duration_iso("PT22H").unwrap();
    assert_eq!(t.iso_string(), "PT22H");
}

/// Neo4j's ISO duration pattern is `CASE_INSENSITIVE`, so the designator,
/// the time marker and every unit letter parse in either case (`P5d` is the
/// spelling the TemporalArithmetic setup query uses).
#[test]
fn duration_iso_units_and_designator_are_case_insensitive() {
    assert_eq!(parse_duration_iso("P5d").unwrap().iso_string(), "P5D");
    assert_eq!(
        parse_duration_iso("p1y2m3dt4h5m6s").unwrap().iso_string(),
        "P1Y2M3DT4H5M6S"
    );
    assert_eq!(
        parse_duration_iso("p2012-02-02t14:37:21.545")
            .unwrap()
            .iso_string(),
        "P2012Y2M2DT14H37M21.545S"
    );
}

#[test]
fn parse_fractional_and_datetime_style_duration() {
    assert_eq!(
        parse_duration_iso("P5M1.5D").unwrap().iso_string(),
        "P5M1DT12H"
    );
    assert_eq!(
        parse_duration_iso("P2.5W").unwrap().iso_string(),
        "P17DT12H"
    );
    assert_eq!(
        parse_duration_iso("P2012-02-02T14:37:21.545")
            .unwrap()
            .iso_string(),
        "P2012Y2M2DT14H37M21.545S"
    );
    assert_eq!(
        parse_duration_iso("P12Y5M-14DT16H").unwrap().iso_string(),
        "P12Y5M-14DT16H"
    );
}

/// LABS-712: a fractional final month lands in the month tier, and the one
/// cascade carries the accumulated whole months to days (the old inline
/// conversion poured the fraction into the day tier separately and answered
/// `P36DT12H34M55.2S`). A decimal in a non-final component is a parse
/// failure, as the reference engine rejects it.
#[test]
fn duration_fractional_months_accumulate_before_the_carry() {
    let split = parse_duration_iso("P1.2M").unwrap();
    // The 1ns shortfall is Neo4j's own `long` cast of the month fraction
    // (live 2026.10.1 answers the same `.199999999S`).
    assert_eq!(split.iso_string(), "P1M6DT2H5M49.199999999S");
    assert!(parse_duration_iso("P0.6M0.6M").is_err());
    assert!(parse_duration_iso("P1.5Y1M").is_err());
    assert!(parse_duration_iso("P1Y1M1DT1.5H1M").is_err());
    assert!(parse_duration_iso("P1Y1M1DT1H1.5M1S").is_err());
    assert!(parse_duration_iso("P1Y1M1DT1H1.5M").is_ok());
}

/// The nanosecond tier truncates its sub-nanosecond remainder, as Neo4j's
/// `(long)` cast does; rounding answered one nanosecond high
/// (`P10M24DT8H23M16.800000002S` for `P0.9Y`; live Neo4j 2026.10.1 answers
/// `…001S`) and turned a sub-nanosecond map field into a whole nanosecond.
/// The canonical `duration_map` helper plus its iso text — no second
/// map constructor (the closure delegates to the one this file's other
/// map tests use).
fn map_text(entries: &[(&str, Value)]) -> String {
    duration_map(entries).unwrap().iso_string()
}

#[test]
fn duration_fraction_truncates_at_the_nanosecond_tier() {
    for (spelling, expected) in [
        ("P0.9Y", "P10M24DT8H23M16.800000001S"),
        ("P0.3Y", "P3M18DT6H17M27.599999998S"),
        ("P0.9M", "P27DT9H26M11.399999999S"),
        ("P-0.9Y", "P-10M-24DT-8H-23M-16.800000001S"),
    ] {
        assert_eq!(
            parse_duration_iso(spelling).unwrap().iso_string(),
            expected,
            "{spelling}"
        );
    }
    assert_eq!(
        map_text(&[("nanoseconds", Value::Float(1.9))]),
        "PT0.000000001S"
    );
    assert_eq!(map_text(&[("microseconds", Value::Float(0.0005))]), "PT0S");
    assert_eq!(map_text(&[("seconds", Value::Float(1.0000000005))]), "PT1S");
    assert_eq!(
        map_text(&[("months", Value::Float(0.9))]),
        "P27DT9H26M11.399999999S"
    );
}

/// LABS-712: a duration map field that is present but not a number is an
/// error, never a silent zero contribution (the old `Option` fold answered
/// `PT0S` for `duration({seconds: '60'})`).
#[test]
fn duration_map_non_numeric_field_errors() {
    let map = |value: Value| construct_duration(Some(&component_map(&[("seconds", value)])));
    assert!(matches!(
        map(Value::String("60".into())),
        Err(Error::ArgumentError(_))
    ));
    assert_eq!(map(Value::Int(60)).unwrap().iso_string(), "PT1M");
}

/// LABS-712: the datetime-style spelling parses its date half through the
/// same `Result` discipline as its time half, so an unparseable date field
/// is an error instead of being dropped (`P1DT12:00` used to answer `PT12H`,
/// `P1Y2MT03:04:05` `PT3H4M5S`).
#[test]
fn duration_datetime_style_unparseable_date_field_errors() {
    for invalid in ["P1DT12:00", "P1Y2MT03:04:05"] {
        assert!(parse_duration_iso(invalid).is_err(), "{invalid}");
    }
}

/// LABS-712: duration components above 2^53 must be built with exact integer
/// arithmetic — the old f64 path dropped low digits, rendering
/// `PT9223372036854775807S` with 30M8S instead of the true 30M7S.
#[test]
fn duration_construction_keeps_integer_components_exact() {
    assert_eq!(
        parse_duration_iso("PT9223372036854775807S")
            .unwrap()
            .iso_string(),
        "PT2562047788015215H30M7S"
    );
    assert_eq!(
        parse_duration_iso("PT2562047788015215H")
            .unwrap()
            .iso_string(),
        "PT2562047788015215H"
    );
    // The map constructor takes the same exact path: an Int field above 2^53
    // must not be promoted through f64.
    let from_map = duration_map(&[("seconds", Value::Int(i64::MAX))]).unwrap();
    assert_eq!(from_map.iso_string(), "PT2562047788015215H30M7S");
}

/// LABS-712: the CIP-228 pattern path keeps an integer component above
/// 2^53 exact instead of rounding through `f64`.
#[test]
fn duration_pattern_keeps_integer_components_exact() {
    assert_eq!(
        parse_pattern("duration", "9007199254740993", "s").unwrap(),
        Temporal::Duration {
            months: 0,
            days: 0,
            nanos: 9_007_199_254_740_993_000_000_000,
        }
    );
    // The same exact integer in a month-sized unit is past Neo4j's duration
    // representability bound (the one `Temporal::duration_checked` applies to
    // every producer), so the pattern path reports the overflow instead of
    // returning a duration no other spelling could build.
    assert_eq!(
        parse_pattern("duration", "9007199254740993", "y"),
        Err(Error::ty("temporal value out of range"))
    );
}

/// LABS-712: trailing garbage is not a component and must be rejected,
/// never silently dropped (`P12` answered `PT0S`, `PT5H12` answered
/// `PT5H`) — neither a numeral with no unit letter nor a dangling sign
/// (`PT-`/`P1Y-` answered `PT0S`/`P1Y`).
#[test]
fn duration_trailing_garbage_errors() {
    for invalid in ["P12", "PT5H12", "PT-", "P1Y-"] {
        assert!(parse_duration_iso(invalid).is_err(), "{invalid}");
    }
}

/// LABS-712: a component carries at most one sign. The assignment was
/// idempotent, so a repeated `-` (`P--1Y`, `P1Y--2M`) parsed as a signed
/// component instead of being rejected.
#[test]
fn duration_repeated_component_sign_errors() {
    for invalid in ["P--1Y", "P1Y--2M", "PT--1S"] {
        assert!(parse_duration_iso(invalid).is_err(), "{invalid}");
    }
    assert_eq!(
        parse_duration_iso("P-1Y-2M").unwrap().iso_string(),
        "P-1Y-2M"
    );
}

/// LABS-712: an empty numeral run is rejected instead of fabricating a zero
/// component (`PY` answered `PT0S`, `P.T` likewise), while the legitimate
/// empty-whole / empty-fraction decimal forms still parse.
#[test]
fn duration_empty_numeral_errors_but_decimal_forms_parse() {
    for invalid in ["PY", "P.T"] {
        assert!(parse_duration_iso(invalid).is_err(), "{invalid}");
    }
    assert_eq!(parse_duration_iso("PT.5S").unwrap().iso_string(), "PT0.5S");
    assert_eq!(parse_duration_iso("PT1.S").unwrap().iso_string(), "PT1S");
}

/// LABS-712: an empty body or a dangling `T` names no component — `P`, `PT`
/// and `P1YT` used to parse as a zero duration (or as the date half alone).
#[test]
fn duration_empty_body_or_dangling_time_marker_errors() {
    for invalid in ["P", "PT", "P1YT"] {
        assert!(parse_duration_iso(invalid).is_err(), "{invalid}");
    }
    assert_eq!(parse_duration_iso("PT0S").unwrap().iso_string(), "PT0S");
}

/// LABS-712: the datetime-style date half is exactly three fields; a fourth
/// (`P2012-02-02-04`) used to be dropped silently.
#[test]
fn duration_datetime_style_extra_date_field_errors() {
    assert!(parse_duration_iso("P2012-02-02-04").is_err());
}

/// LABS-712: the datetime-style time half visits at most three fields; a
/// fourth (`P2012-02-02T14:37:21:99`) used to lose the `99` silently, while
/// a short half (`T14`) stays legitimate.
#[test]
fn duration_datetime_style_extra_time_field_errors() {
    assert!(parse_duration_iso("P2012-02-02T14:37:21:99").is_err());
    assert_eq!(
        parse_duration_iso("P2012-02-02T14").unwrap().iso_string(),
        "P2012Y2M2DT14H"
    );
}

/// The sub-second pattern spellings resolve once in the vocabulary: `n` and
/// `S` render fraction-of-second digits, while `N` renders the nanosecond
/// field's cascade share (nano-of-day). This pins all three letters so the
/// alphabet, the token and the renderer cannot drift apart again.
#[test]
fn duration_pattern_fraction_letters_render_their_own_spellings() {
    let value = Temporal::Duration {
        months: 0,
        days: 0,
        nanos: 1_234_567_890,
    };
    assert_eq!(format_pattern(&value, "s").unwrap(), "1");
    assert_eq!(format_pattern(&value, "S").unwrap(), "2");
    assert_eq!(format_pattern(&value, "SSS").unwrap(), "234");
    assert_eq!(format_pattern(&value, "n").unwrap(), "2");
    assert_eq!(format_pattern(&value, "nnnnnnnnn").unwrap(), "234567890");
    assert_eq!(format_pattern(&value, "N").unwrap(), "1234567890");
}

/// LABS-711: the ISO duration grammar is case-insensitive (Neo4j compiles it
/// with `CASE_INSENSITIVE`), so `PT1h` and `P1dt2h` name the same components
/// as their uppercase spellings. The duration-pattern features call
/// `format(duration('PT1h'), …)`, which used to fail at parse time.
#[test]
fn duration_iso_units_are_case_insensitive() {
    for (text, expected) in [
        ("PT1h", "PT1H"),
        ("pt1h", "PT1H"),
        ("P1y2m3dt4h5m6s", "P1Y2M3DT4H5M6S"),
        ("P1Y2MT3H4M5S", "P1Y2MT3H4M5S"),
    ] {
        assert_eq!(
            parse_duration_iso(text).unwrap().iso_string(),
            expected,
            "{text}"
        );
    }
}

fn duration_map(entries: &[(&str, Value)]) -> crate::Result<Temporal> {
    construct_duration(Some(&component_map(entries)))
}

/// LABS-711: null-valued duration fields are ignored (Neo4j filters them
/// before constructing), and a map with no non-null field names no unit
/// (22N30) — `duration({})` and `duration({hours: null})` used to answer
/// `PT0S`.
#[test]
fn duration_map_ignores_nulls_and_requires_a_unit() {
    assert_eq!(
        map_text(&[("days", Value::Int(14)), ("hours", Value::Null)]),
        "P14D"
    );
    assert!(duration_map(&[]).is_err());
    assert!(duration_map(&[("hours", Value::Null)]).is_err());
}

/// LABS-711: the map constructor's integer lanes are Neo4j's checked lanes:
/// a component whose scaled value leaves i64 (milliseconds/nanoseconds), a
/// month total beyond the average-month model, or a total that only
/// overflows after summing errors 22N28. Components that individually
/// overflow but cancel stay representable.
#[test]
fn duration_map_overflow_matches_neo4j_lanes() {
    for entries in [
        vec![("years", Value::Int(292_277_024_627))],
        vec![("hours", Value::Int(i64::MAX))],
        vec![("minutes", Value::Int(i64::MAX))],
        vec![("milliseconds", Value::Int(i64::MAX))],
        vec![("microseconds", Value::Int(i64::MAX))],
        vec![("hours", Value::Int(-i64::MAX))],
        vec![
            ("hours", Value::Int(153_722_867_280_912_930)),
            ("minutes", Value::Int(7)),
        ],
    ] {
        assert!(duration_map(&entries).is_err(), "{entries:?}");
    }
    assert_eq!(
        duration_map(&[("years", Value::Int(292_277_024_626))])
            .unwrap()
            .iso_string(),
        "P292277024626Y"
    );
    for (entries, expected) in [
        (
            vec![
                ("years", Value::Int(768_614_336_404_564_651)),
                ("months", Value::Int(-9_223_372_036_854_775_807)),
            ],
            "P5M",
        ),
        (
            vec![
                ("weeks", Value::Int(1_317_624_576_693_539_402)),
                ("days", Value::Int(i64::MIN)),
            ],
            "P6D",
        ),
        (
            vec![
                ("hours", Value::Int(2_562_047_788_015_216)),
                ("minutes", Value::Int(-153_722_867_280_912_930)),
                ("seconds", Value::Int(-9)),
            ],
            "PT29M51S",
        ),
        (
            vec![
                ("milliseconds", Value::Int(9_223_372_036_855)),
                ("microseconds", Value::Int(-9_223_372_036_855_000)),
            ],
            "PT0S",
        ),
    ] {
        assert_eq!(map_text(&entries), expected);
    }
}

/// LABS-711: the map constructor must not bypass the stored-range bound. Both
/// the month total whose average length leaves `i64` and a nanosecond edge
/// that carries the seconds lane past `2^63` are the *same* typed
/// interval-field overflow the ISO spelling reports, because both producers
/// assemble through [`Temporal::duration_checked`].
#[test]
fn duration_map_bound_matches_the_iso_spelling() {
    // 3_507_324_295_524 months fits `i64` but is ~9.223372036857e18 average
    // seconds, one step past `i64::MAX`.
    let months = 3_507_324_295_524i64;
    let map = duration_map(&[("months", Value::Int(months))]);
    assert_eq!(
        map,
        Err(Error::ty("temporal value out of range")),
        "months: {months}"
    );
    assert_eq!(map, parse_iso("duration", "P3507324295524M"));

    // `i64::MAX` seconds plus two seconds of nanoseconds is 2^63 + 1
    // seconds: the nanosecond field fits, the seconds lane does not.
    let map = duration_map(&[
        ("seconds", Value::Int(i64::MAX)),
        ("nanoseconds", Value::Int(2_000_000_000)),
    ]);
    assert_eq!(
        map,
        Err(Error::ty("temporal value out of range")),
        "nanosecond edge"
    );
    assert_eq!(map, parse_iso("duration", "PT9223372036854775809S"));
}

/// LABS-711: the integral map path sums the same lanes the float path and
/// the ISO parser do — `quarters` rides the months lane, so the integral and
/// float spellings construct identically (the integral path once dropped it).
#[test]
fn duration_map_quarters_sum_into_the_months_lane() {
    assert_eq!(
        duration_map(&[("quarters", Value::Int(2))]),
        parse_iso("duration", "P6M")
    );
    assert_eq!(
        duration_map(&[("quarters", Value::Float(2.0))]),
        parse_iso("duration", "P6M")
    );
    // Mixed with the months lane's other fields.
    assert_eq!(
        duration_map(&[
            ("years", Value::Int(1)),
            ("quarters", Value::Int(1)),
            ("months", Value::Int(2))
        ]),
        parse_iso("duration", "P1Y5M")
    );
}

/// LABS-711: a pattern run is a contiguous stretch of one *token type*, not
/// one letter — `ML` is one two-wide month run and `Yuy` one three-wide year
/// run (Neo4j's `DurationToken` counters).
#[test]
fn duration_pattern_runs_group_by_token_type() {
    let value = Temporal::Duration {
        months: 16,
        days: 3,
        nanos: 0,
    };
    assert_eq!(
        format_pattern(&value, "ML-Yuy-M-dDDddddd").unwrap(),
        "04-001-4-00000003"
    );
}

/// The parser groups runs by letter: mixed letters naming one field (`yY`)
/// or the fraction (`nS`) are duplicate fields, not one wider run — only the
/// formatter merges them.
#[test]
fn duration_pattern_parse_rejects_mixed_letter_duplicate_fields() {
    for (value, pattern) in [("1", "yY"), ("12", "nS"), ("1", "dD")] {
        assert!(
            parse_pattern("duration", value, pattern).is_err(),
            "{pattern}"
        );
    }
    assert!(parse_pattern("duration", "11", "yy").is_ok());
}

/// LABS-711: duration-pattern escaping follows Neo4j's state machine — a
/// single quote opens, a lone quote closes (emitting nothing), and a doubled
/// quote inside emits one quote and stays escaped. An unclosed escape is a
/// pattern-parsing error.
#[test]
fn duration_pattern_escapes_match_neo4j() {
    let value = parse_duration_iso("PT1H").unwrap();
    for (pattern, expected) in [
        ("h '' m", "1  0"),
        ("h '''' m", "1 ' 0"),
        ("h '''''' m", "1 '' 0"),
        ("h '''''''' m", "1 ''' 0"),
        ("h 'a''' m", "1 a' 0"),
        ("h '''a' m", "1 'a 0"),
        ("h '''a''' m", "1 'a' 0"),
        ("h 'h'' with ' m", "1 h' with  0"),
    ] {
        assert_eq!(
            format_pattern(&value, pattern).unwrap(),
            expected,
            "{pattern}"
        );
    }
    for unterminated in ["h ' m", "h ''' m", "h ''''' m", "h ''''''' m"] {
        assert!(
            format_pattern(&value, unterminated).is_err(),
            "{unterminated}"
        );
    }
}

/// LABS-711: a duration whose seconds lane cannot scale into milliseconds or
/// nanoseconds is a 22N28 overflow while formatting. The total-nanoseconds
/// lane is computed unconditionally (Neo4j), so even an `s` pattern fails.
#[test]
fn duration_pattern_scaled_lane_overflow_errors() {
    let value = parse_duration_iso("PT9223372036854775807S").unwrap();
    for pattern in ["A", "N", "s", ""] {
        assert!(format_pattern(&value, pattern).is_err(), "{pattern}");
    }
}

/// LABS-785: the millisecond/microsecond accessors floor a negative
/// nanosecond count, exactly at the `i64::MIN` edge. Truncating division
/// lands one unit short (-9223372036854775807) and, one nanosecond further
/// down, hides the overflow the floor produces.
#[test]
fn negative_duration_millisecond_accessors_floor_at_the_i64_edge() {
    let boundary = duration_map(&[
        ("seconds", Value::Int(-9_223_372_036_854_776)),
        ("nanoseconds", Value::Int(192_999_999)),
    ])
    .unwrap();
    assert_eq!(boundary.component("milliseconds"), Ok(Value::Int(i64::MIN)));
    let boundary = duration_map(&[
        ("seconds", Value::Int(-9_223_372_036_855)),
        ("nanoseconds", Value::Int(224_192_999)),
    ])
    .unwrap();
    assert_eq!(boundary.component("microseconds"), Ok(Value::Int(i64::MIN)));
    // One nanosecond further down floors past `i64::MIN` and overflows.
    let overflow = duration_map(&[
        ("seconds", Value::Int(-9_223_372_036_854_776)),
        ("nanoseconds", Value::Int(191_999_999)),
    ])
    .unwrap();
    assert!(overflow.component("milliseconds").is_err());
    let overflow = duration_map(&[
        ("seconds", Value::Int(-9_223_372_036_855)),
        ("nanoseconds", Value::Int(224_191_999)),
    ])
    .unwrap();
    assert!(overflow.component("microseconds").is_err());
}

/// LABS-785: exactly `2^63` seconds is outside Neo4j's stored `i64` seconds
/// field, so the exact producers reject it. The f64 scaling path keeps
/// accepting the rounded image of `i64::MAX` (`* 1.0`).
#[test]
fn exact_producers_reject_the_two_pow_63_seconds_image() {
    assert!(parse_duration_iso("PT9223372036854775808S").is_err());
    assert!(duration_map(&[
        ("seconds", Value::Int(i64::MAX)),
        ("nanoseconds", Value::Int(1_000_000_000)),
    ])
    .is_err());
    // The negative edge `i64::MIN` seconds is representable.
    let min = parse_duration_iso("PT-9223372036854775808S").unwrap();
    assert_eq!(min.iso_string(), "PT-2562047788015215H-30M-8S");
    // Scaling `i64::MAX` seconds by `1.0` rounds through f64 to `2^63` and
    // must stay accepted (Neo4j's `(long) (double)` cast): the inclusive
    // seam keeps the scaling path representable.
    let max = parse_duration_iso("PT9223372036854775807S").unwrap();
    assert!(scale_duration(&max, 1.0).is_ok());
}

/// The duration accessors decompose the stored nanos once — the Euclidean
/// whole-second split plus its non-negative remainder — so a negative
/// sub-second duration answers one consistent set. Neo4j-verified:
/// `duration({nanoseconds: -1})` is seconds `-1`, secondsOfMinute `-1`,
/// nanosecondsOfSecond `999999999`, while `duration('PT-1.5S')` is seconds
/// `-2`, secondsOfMinute `-2`, nanosecondsOfSecond `500000000`.
#[test]
fn negative_duration_components_use_one_decomposition() {
    let sub_second = Temporal::Duration {
        months: 0,
        days: 0,
        nanos: -1,
    };
    assert_eq!(
        sub_second.component("seconds").unwrap(),
        crate::Value::Int(-1)
    );
    assert_eq!(
        sub_second.component("secondsOfMinute").unwrap(),
        crate::Value::Int(-1)
    );
    assert_eq!(
        sub_second.component("nanosecondsOfSecond").unwrap(),
        crate::Value::Int(999_999_999)
    );
    assert_eq!(
        sub_second.component("milliseconds").unwrap(),
        crate::Value::Int(-1)
    );
    assert_eq!(
        sub_second.component("minutesOfHour").unwrap(),
        crate::Value::Int(0)
    );

    let one_and_a_half = Temporal::Duration {
        months: 0,
        days: 0,
        nanos: -1_500_000_000,
    };
    assert_eq!(
        one_and_a_half.component("seconds").unwrap(),
        crate::Value::Int(-2)
    );
    assert_eq!(
        one_and_a_half.component("secondsOfMinute").unwrap(),
        crate::Value::Int(-2)
    );
    assert_eq!(
        one_and_a_half.component("nanosecondsOfSecond").unwrap(),
        crate::Value::Int(500_000_000)
    );
}
