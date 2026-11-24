//! The named-constructor suite: `construct_named`'s arity and
//! `(value, pattern)` operand contract, and the component-map construction
//! tests. Split out of `tests.rs` by the file-size gate; the map *policy*
//! stays in `tests_fields.rs`.

use super::test_support::{component_map, EPOCH_REF_MILLIS, EPOCH_REF_SECONDS};
use super::*;

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
    assert!(construct_named("date", std::slice::from_ref(&value)).is_ok());
    assert!(construct_named("date", &[value, pattern]).is_ok());
}

/// The CIP-228 `(value, pattern)` operand faults split by side, as live
/// does (probed 2026-11-20 on 2026.10.1): a non-STRING value is the call's
/// own failure — the `ProcedureCallFailed` class, 42001 — while a
/// non-STRING pattern stays the runtime Type mismatch.
#[test]
fn pattern_form_operand_faults_split_by_side() {
    let err = construct_named(
        "duration",
        &[Value::Int(123), Value::String("s 'seconds'".into())],
    )
    .expect_err("a non-string value must fail the call");
    assert!(matches!(err, Error::ProcedureCall(_)), "{err}");
    let err = construct_named("date", &[Value::String("20150721".into()), Value::Int(42)])
        .expect_err("a non-string pattern must fail the call");
    assert!(matches!(err, Error::InvalidArgumentType(_)), "{err}");
}

/// The whole runtime `Invalid call signature` family reports the call's own
/// failure — live's `ProcedureCallFailed` class under 42001 (each shape
/// live-probed 2026-11-20 on 2026.10.1): the one-argument constructors'
/// non-temporal and duration operands, and `duration()`'s unconstructible
/// argument.
#[test]
fn call_signature_faults_report_the_procedure_class() {
    let duration = construct_named("duration", &[Value::String("P1D".into())])
        .expect("duration('P1D') must construct");
    let cases: [(&str, Vec<Value>); 4] = [
        ("duration", vec![Value::Int(123)]),
        ("date", vec![Value::Int(123)]),
        ("localdatetime", vec![Value::Int(123)]),
        ("date", vec![duration]),
    ];
    for (name, vals) in cases {
        let err = construct_named(name, &vals).expect_err("the call must fail");
        assert!(matches!(err, Error::ProcedureCall(_)), "{name}: {err}");
    }
}

/// The direct constructors reject `null` uniformly — every clock kind's one
/// shared `Invalid call signature` rejection, the family enumerated from the
/// canonical kind table with its spelling from `ClockKind::name`. The
/// executor's `temporal_construct` short-circuits a null argument to `null`
/// before any constructor runs, so `date(null)` stays null at the Cypher
/// level.
#[test]
fn every_clock_constructor_rejects_null() {
    for kind in ClockKind::all() {
        assert_eq!(
            super::construct::construct(kind, Some(&Value::Null)),
            Err(Error::procedure_call(format!(
                "Invalid call signature: {}()",
                kind.name()
            )))
        );
    }
}

/// LABS-711: a temporal component map's fields are integer-typed. A present
/// non-integer (string/bool/list/integral float) is Neo4j's non-assignable
/// component error — the `ArgumentError` class (22G06), naming the field —
/// never a silently absent field (`date({year: 2000, month: 'x'})` used to
/// answer 2000-01-01).
#[test]
fn temporal_map_non_integer_components_are_typed_errors() {
    let string = || Value::String("x".into());
    let list = || Value::List(vec![Value::Int(1), Value::Int(2)].into());
    let cases: Vec<(&str, Result<Temporal, Error>, &str)> = vec![
        (
            "date month",
            construct_date(Some(&component_map(&[
                ("year", Value::Int(2000)),
                ("month", string()),
            ]))),
            "cannot assign 'month' of a STRING",
        ),
        (
            "date week",
            construct_date(Some(&component_map(&[
                ("year", Value::Int(2000)),
                ("week", string()),
            ]))),
            "cannot assign 'week' of a STRING",
        ),
        (
            "date float year",
            construct_date(Some(&component_map(&[("year", Value::Float(2000.0))]))),
            "cannot assign 'year' of a FLOAT",
        ),
        (
            "localtime minute",
            construct_localtime(Some(&component_map(&[
                ("hour", Value::Int(12)),
                ("minute", list()),
            ]))),
            "cannot assign 'minute' of a LIST",
        ),
        (
            "datetime hour",
            construct_datetime(Some(&component_map(&[
                ("year", Value::Int(2000)),
                ("month", Value::Int(1)),
                ("day", Value::Int(1)),
                ("hour", Value::Bool(true)),
            ]))),
            "cannot assign 'hour' of a BOOLEAN",
        ),
        (
            "localdatetime nanosecond",
            construct_localdatetime(Some(&component_map(&[
                ("year", Value::Int(2000)),
                ("month", Value::Int(1)),
                ("day", Value::Int(1)),
                ("hour", Value::Int(12)),
                ("minute", Value::Int(0)),
                ("second", Value::Int(0)),
                ("nanosecond", Value::Bool(true)),
            ]))),
            "cannot assign 'nanosecond' of a BOOLEAN",
        ),
    ];
    for (case, outcome, expected) in cases {
        let err = outcome.expect_err("a non-integer component must be rejected");
        assert!(
            matches!(err, Error::ArgumentError(_)),
            "{case}: the class must be ArgumentError (22G06), got {err:?}"
        );
        assert!(err.to_string().contains(expected), "{case}: {err}");
    }
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

/// A `datetime`/`localdatetime` needs a time-bearing source; a date (or a
/// bare time) cannot be widened into one — the `cannot convert to …`
/// typed rejection.
#[test]
fn datetime_kinds_need_a_time_bearing_source() {
    let date = Value::Temporal(parse_iso("date", "1984-02-11").unwrap());
    let time = Value::Temporal(parse_iso("localtime", "12:00").unwrap());
    for rejected in [
        construct_datetime(Some(&date)),
        construct_localdatetime(Some(&date)),
        construct_datetime(Some(&time)),
        construct_localdatetime(Some(&time)),
    ] {
        let err = rejected.expect_err("a time-incompatible source must be rejected");
        assert!(matches!(err, Error::Type(_)), "{err}");
    }
    let local = Value::Temporal(parse_iso("localdatetime", "1984-02-11T12:00").unwrap());
    assert!(construct_datetime(Some(&local)).is_ok());
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
    assert_eq!(
        construct_datetime(Some(&component_map(&[
            ("year", Value::Int(1984)),
            ("month", Value::Int(11)),
            ("day", Value::Int(11)),
            ("hour", Value::Int(12)),
            ("timezone", Value::Int(42)),
        ]))),
        Err(Error::argument_error("timezone must be STRING"))
    );
    let d = construct_datetime(Some(&component_map(&[
        ("year", Value::Int(1984)),
        ("month", Value::Int(11)),
        ("day", Value::Int(11)),
        ("hour", Value::Int(12)),
    ])))
    .unwrap();
    // One zero-offset rule: both accessors spell a zero offset `Z`, like the
    // ISO suffix (verified against Neo4j 2026.09).
    assert_eq!(d.component("timezone"), Ok(Value::String("Z".into())));
    assert_eq!(d.component("offset"), Ok(Value::String("Z".into())));
}

/// LABS-261: `epochMillis`/`epochSeconds` map fields construct the temporal
/// value from the instant they name — the shared reference instant, so
/// constructor and accessor round-trip each other.
#[test]
fn datetime_from_epoch_millis_and_epoch_seconds_map_fields() {
    let cases: [(&str, i64, &str); 2] = [
        ("epochMillis", EPOCH_REF_MILLIS, "1984-11-11T11:31:14.645Z"),
        ("epochSeconds", EPOCH_REF_SECONDS, "1984-11-11T11:31:14Z"),
    ];
    for (field, value, iso) in cases {
        let built =
            construct_datetime(Some(&component_map(&[(field, Value::Int(value))]))).unwrap();
        // Compare on the ISO rendering: the constructor records the UTC zone
        // spelling explicitly, `parse_iso` leaves it unset, but both
        // render "Z".
        assert_eq!(built.iso_string(), iso, "{field}");
        assert_eq!(built.component(field), Ok(Value::Int(value)), "{field}");
    }
}

/// An explicit `timezone` converts the named instant into that zone.
#[test]
fn datetime_from_epoch_millis_with_timezone() {
    let dt = construct_datetime(Some(&component_map(&[
        ("epochMillis", Value::Int(EPOCH_REF_MILLIS)),
        ("timezone", Value::String("Europe/Stockholm".into())),
    ])))
    .unwrap();
    assert_eq!(dt.component("offset"), Ok(Value::String("+01:00".into())));
    assert_eq!(
        dt.component("epochMillis"),
        Ok(Value::Int(EPOCH_REF_MILLIS))
    );
}

/// Only `datetime` has the epoch-map form: every other constructor rejects
/// an epoch field the way the reference engine does (`Not supported:
/// epochMillis`), instead of reading the same instant at a coarser kind.
#[test]
fn non_datetime_kinds_reject_epoch_millis() {
    let ms = component_map(&[("epochMillis", Value::Int(EPOCH_REF_MILLIS))]);
    for rejected in [
        construct_date(Some(&ms)),
        construct_localdatetime(Some(&ms)),
        construct_time(Some(&ms)),
        construct_localtime(Some(&ms)),
    ] {
        let err = rejected.expect_err("an epoch map is datetime-only");
        assert!(
            matches!(err, Error::Type(_)),
            "the 22G05 Not-supported rejection, got {err:?}"
        );
    }
}

/// Epoch values outside chrono's timestamp range are the typed
/// out-of-range rejection, not a panic.
#[test]
fn epoch_fields_out_of_range_error() {
    let ms = component_map(&[("epochMillis", Value::Int(i64::MAX))]);
    let err = construct_datetime(Some(&ms)).expect_err("an out-of-range epoch must fail");
    assert!(matches!(err, Error::ArgumentError(_)), "{err}");
}

/// Non-integer epoch values are a typed error, not a silent default.
#[test]
fn epoch_fields_must_be_integers() {
    let ms = component_map(&[("epochMillis", Value::Float(469020674.645))]);
    let err = construct_datetime(Some(&ms)).expect_err("a non-integer epoch must be rejected");
    assert!(
        matches!(err, Error::ArgumentError(_)),
        "the 22G06 integer rejection, got {err:?}"
    );
}
