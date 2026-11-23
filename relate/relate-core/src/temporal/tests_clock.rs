//! The clock-reading accessors: the `<kind>.<mode>` spelling vocabulary, the
//! timezone argument, the clock-reading map form, and the statement pin.

use super::clock::{now_date, now_dt, now_local_dt, now_local_time, now_time};
use super::test_support::{component_map, naive_utc};
use super::*;

/// The clock-access spellings name a typed kind plus a clock mode; every
/// other dotted name (a `truncate`/`fromepoch` sibling, a plain constructor)
/// stays out of the accessor vocabulary.
#[test]
fn clock_access_recognizes_only_the_suffixed_constructor_spellings() {
    assert_eq!(
        clock_access("date.realtime"),
        Some((ClockKind::Date, ClockMode::Realtime))
    );
    assert_eq!(
        clock_access("dAtE.traNsaction"),
        Some((ClockKind::Date, ClockMode::Transaction))
    );
    assert_eq!(
        clock_access("localdatetime.statement"),
        Some((ClockKind::LocalDateTime, ClockMode::Statement))
    );
    for name in [
        "date",
        "date.truncate",
        "datetime.fromepoch",
        "duration.between",
        "duration.realtime",
        ".realtime",
        "date.realtime.extra",
    ] {
        assert_eq!(clock_access(name), None, "{name}");
    }
}

/// The spelling registry, the two enum tables and the parser cannot
/// disagree: every kind x mode renders one spelling, that spelling parses
/// back to its own payload, and the registry holds exactly those fifteen.
#[test]
fn clock_spellings_round_trip_through_clock_access() {
    let kinds = [
        ClockKind::Date,
        ClockKind::Time,
        ClockKind::LocalTime,
        ClockKind::DateTime,
        ClockKind::LocalDateTime,
    ];
    let modes = [
        ClockMode::Transaction,
        ClockMode::Statement,
        ClockMode::Realtime,
    ];
    let mut expected = Vec::new();
    for kind in kinds {
        for mode in modes {
            let spelling = clock_spelling(kind, mode);
            assert_eq!(clock_access(&spelling), Some((kind, mode)), "{spelling}");
            expected.push(spelling);
        }
    }
    let mut registered: Vec<&str> = clock_spellings().collect();
    assert_eq!(registered.len(), 15);
    registered.sort_unstable();
    expected.sort_unstable();
    assert_eq!(registered, expected);
}

/// The pinned instant is what `transaction` and `statement` read, and the
/// timezone argument is honored at that instant: 12:00Z on 2021-06-01 is
/// still 2021-06-01 in UTC but already 2021-06-02 at UTC+14.
#[test]
fn clock_access_reads_the_pinned_instant_in_the_named_zone() {
    let _clock = QueryClockGuard::pinned_at(naive_utc(2021, 6, 1, 12, 0, 0), 0);

    let date = construct_clock(
        ClockKind::Date,
        ClockMode::Transaction,
        Some(&Value::String("Pacific/Kiritimati".into())),
    )
    .unwrap();
    assert_eq!(
        date,
        Value::Temporal(parse_iso("date", "2021-06-02").unwrap())
    );

    let same = construct_clock(
        ClockKind::Date,
        ClockMode::Statement,
        Some(&Value::String("UTC".into())),
    )
    .unwrap();
    assert_eq!(
        same,
        Value::Temporal(parse_iso("date", "2021-06-01").unwrap())
    );

    let time = construct_clock(
        ClockKind::Time,
        ClockMode::Transaction,
        Some(&Value::String("+02:00".into())),
    )
    .unwrap();
    assert_eq!(
        time,
        Value::Temporal(parse_iso("time", "14:00+02:00").unwrap())
    );

    let local = construct_clock(
        ClockKind::LocalTime,
        ClockMode::Transaction,
        Some(&Value::String("+02:00".into())),
    )
    .unwrap();
    assert_eq!(
        local,
        Value::Temporal(parse_iso("localtime", "14:00").unwrap())
    );

    let local_dt = construct_clock(
        ClockKind::LocalDateTime,
        ClockMode::Statement,
        Some(&Value::String("+02:00".into())),
    )
    .unwrap();
    assert_eq!(
        local_dt,
        Value::Temporal(parse_iso("localdatetime", "2021-06-01T14:00").unwrap())
    );

    let dt = construct_clock(
        ClockKind::DateTime,
        ClockMode::Transaction,
        Some(&Value::String("America/Los_Angeles".into())),
    )
    .unwrap();
    let Value::Temporal(dt) = &dt else {
        panic!("datetime clock access must yield a temporal, got {dt:?}");
    };
    assert_eq!(dt.component("hour"), Ok(Value::Int(5)));
    assert_eq!(
        dt.component("timezone"),
        Ok(Value::String("America/Los_Angeles".into()))
    );
    assert_eq!(dt.component("offset"), Ok(Value::String("-07:00".into())));
}

/// `realtime` bypasses the statement pin: a guard pinned in 2000 must not
/// make the accessor report 2000.
#[test]
fn realtime_reads_the_wall_clock_not_the_pin() {
    let _clock = QueryClockGuard::pinned_at(naive_utc(2000, 1, 1, 0, 0, 0), 0);
    let Value::Temporal(reading) =
        construct_clock(ClockKind::Date, ClockMode::Realtime, None).unwrap()
    else {
        panic!("date.realtime() must yield a DATE");
    };
    assert_ne!(
        reading,
        parse_iso("date", "2000-01-01").unwrap(),
        "realtime must not read the pinned statement clock"
    );
}

/// The accessor argument contract: null constructs null; a numeric timezone
/// is the call's own failure (the ProcedureCallFailed class, live-probed
/// 2026-11-20 on 2026.10.1); an unknown zone is the
/// typed zone error. Arity is the dispatcher's contract, not this entry
/// point's.
#[test]
fn clock_access_argument_contract() {
    let _clock = QueryClockGuard::pinned_at(naive_utc(2021, 6, 1, 12, 0, 0), 0);

    assert_eq!(
        construct_clock(ClockKind::Date, ClockMode::Transaction, Some(&Value::Null)).unwrap(),
        Value::Null
    );
    assert!(matches!(
        construct_clock(
            ClockKind::Date,
            ClockMode::Transaction,
            Some(&Value::Int(42))
        ),
        Err(Error::ProcedureCall(_))
    ));
    assert!(construct_clock(
        ClockKind::Date,
        ClockMode::Transaction,
        Some(&Value::String("Not/AZone".into()))
    )
    .is_err());
}

/// A map that names only a `timezone` is the clock-reading form: the
/// constructor reads the current instant in that zone instead of the
/// default `year: 0` field map.
#[test]
fn clock_reading_map_constructs_the_current_value_in_the_zone() {
    let _clock = QueryClockGuard::pinned_at(naive_utc(2021, 6, 1, 12, 0, 0), 0);
    let zone = |tz: &str| component_map(&[("timezone", Value::String(tz.into()))]);

    let date = construct_date(Some(&zone("Pacific/Kiritimati"))).unwrap();
    assert_eq!(date, parse_iso("date", "2021-06-02").unwrap());

    let time = construct_time(Some(&zone("+02:00"))).unwrap();
    assert_eq!(time, parse_iso("time", "14:00+02:00").unwrap());

    let local = construct_localtime(Some(&zone("+02:00"))).unwrap();
    assert_eq!(local, parse_iso("localtime", "14:00").unwrap());

    let local_dt = construct_localdatetime(Some(&zone("+02:00"))).unwrap();
    assert_eq!(
        local_dt,
        parse_iso("localdatetime", "2021-06-01T14:00").unwrap()
    );

    let dt = construct_datetime(Some(&zone("America/Los_Angeles"))).unwrap();
    assert_eq!(dt.component("hour"), Ok(Value::Int(5)));
    assert_eq!(
        dt.component("timezone"),
        Ok(Value::String("America/Los_Angeles".into()))
    );

    // A present but non-string timezone stays the typed rejection.
    let bad = component_map(&[("timezone", Value::Int(1))]);
    assert!(construct_date(Some(&bad)).is_err());
}

/// The clock-map predicate the literal fold consults: only the
/// timezone-only shape samples the clock. An epoch map is an instant (it
/// folds), and a field map is deterministic.
#[test]
fn samples_clock_sees_only_the_timezone_only_map() {
    let clock = component_map(&[("timezone", Value::String("UTC".into()))]);
    assert!(samples_clock(&clock));
    // A null-valued sibling is ignored, like every field reader ignores it.
    let with_null = component_map(&[
        ("timezone", Value::String("UTC".into())),
        ("year", Value::Null),
    ]);
    assert!(samples_clock(&with_null));

    for deterministic in [
        component_map(&[]),
        component_map(&[("year", Value::Int(1984))]),
        component_map(&[
            ("epochMillis", Value::Int(0)),
            ("timezone", Value::String("UTC".into())),
        ]),
        Value::String("2020-01-01".into()),
    ] {
        assert!(!samples_clock(&deterministic), "{deterministic:?}");
    }
}

/// The epoch map form is a clock reading at a fixed instant, and only
/// `datetime` has it: the instant is read in the map's `timezone`, so a
/// zone-less epoch map reads UTC and a zoned one shifts the wall clock,
/// while every other kind rejects the field.
#[test]
fn epoch_maps_read_the_fixed_instant_in_the_named_zone() {
    // 1970-01-01T23:30:00Z.
    let epoch = Value::Int(84_600_000);
    let utc_map = component_map(&[("epochMillis", epoch.clone())]);
    let zoned = component_map(&[
        ("epochMillis", epoch),
        ("timezone", Value::String("+01:00".into())),
    ]);

    for rejected in [
        construct_date(Some(&utc_map)),
        construct_time(Some(&zoned)),
        construct_localdatetime(Some(&zoned)),
        construct_localtime(Some(&utc_map)),
        construct_localtime(Some(&zoned)),
    ] {
        assert!(rejected.is_err(), "an epoch map is datetime-only");
    }
    let dt = construct_datetime(Some(&zoned)).unwrap();
    assert_eq!(dt.component("hour"), Ok(Value::Int(0)));
    assert_eq!(dt.component("minute"), Ok(Value::Int(30)));
    assert_eq!(dt.component("offset"), Ok(Value::String("+01:00".into())));
}

/// The default reading of `datetime` is UTC whatever the host offset is:
/// the pre-refactor `now_dt()` hardcoded offset 0 and zone `Z`, and the
/// clock reading must keep that. The zoneless kinds keep the host's local
/// wall clock, which the non-zero pin offset makes visible.
#[test]
fn datetime_default_stays_utc_on_a_non_utc_host() {
    // 23:00Z is already the next day at +02:00.
    let _clock = QueryClockGuard::pinned_at(naive_utc(2021, 6, 1, 23, 0, 0), 2 * 3600);

    let dt = now_dt();
    assert_eq!(
        dt,
        Temporal::DateTime {
            utc: naive_utc(2021, 6, 1, 23, 0, 0),
            offset: 0,
            zone: Some("Z".into()),
        },
        "datetime() must read UTC, not the host's local wall clock"
    );
    assert_eq!(
        dt.component("epochMillis"),
        Ok(Value::Int(1_622_588_400_000))
    );
    assert_eq!(now_epoch_millis(), 1_622_588_400_000);

    assert_eq!(now_date(), parse_iso("date", "2021-06-02").unwrap());
    assert_eq!(now_time(), parse_iso("time", "01:00+02:00").unwrap());
    assert_eq!(now_local_time(), parse_iso("localtime", "01:00").unwrap());
    assert_eq!(
        now_local_dt(),
        parse_iso("localdatetime", "2021-06-02T01:00").unwrap()
    );
}

/// `truncate(unit)` reads the pinned clock's value of its own kind.
#[test]
fn truncate_now_reads_the_pinned_clock() {
    let _clock = QueryClockGuard::pinned_at(naive_utc(2021, 6, 15, 12, 30, 45), 0);
    let implicit = truncate_now(ClockKind::Date, "millennium").unwrap();
    let explicit = truncate(ClockKind::Date, "millennium", &now_date(), None).unwrap();
    assert_eq!(implicit, explicit);
}

/// A nested guard restores the outer statement's pin on drop instead of
/// leaving the clock unpinned.
#[test]
fn a_nested_guard_restores_the_outer_pin() {
    let outer = naive_utc(2021, 6, 1, 12, 0, 0);
    let _outer = QueryClockGuard::pinned_at(outer, 0);
    {
        let _inner = QueryClockGuard::pinned_at(naive_utc(2000, 1, 1, 0, 0, 0), 0);
        assert_eq!(now_utc_dt(), naive_utc(2000, 1, 1, 0, 0, 0));
    }
    assert_eq!(
        now_utc_dt(),
        outer,
        "dropping the inner guard must restore the outer pin"
    );
}

/// The pin helper's instant is a UTC wall clock; a non-zero offset shifts
/// only the derived local reading.
#[test]
fn pinned_reading_derives_the_local_wall_clock_instead_of_storing_it() {
    let utc = naive_utc(2021, 6, 1, 12, 0, 0);
    let _clock = QueryClockGuard::pinned_at(utc, 5 * 3600);
    assert_eq!(now_date(), parse_iso("date", "2021-06-01").unwrap());
    assert_eq!(now_time(), parse_iso("time", "17:00+05:00").unwrap());
    assert_eq!(now_utc_dt(), utc);
}

/// Inside an explicit transaction `transaction` reads the begin-time
/// instant while `statement` and the default constructors read the
/// statement's own; the nested guard restores both pins on drop.
#[test]
fn transaction_mode_reads_the_transaction_instant_not_the_statement_pin() {
    let began = naive_utc(2021, 6, 1, 12, 0, 0);
    let stmt = naive_utc(2021, 6, 1, 12, 0, 5);
    let _clock = QueryClockGuard::pinned_in_transaction(stmt, began);
    let read = |mode| match construct_clock(ClockKind::LocalDateTime, mode, None).unwrap() {
        Value::Temporal(t) => t,
        other => panic!("expected a temporal, got {other:?}"),
    };
    assert_eq!(read(ClockMode::Transaction), Temporal::LocalDateTime(began));
    assert_eq!(read(ClockMode::Statement), Temporal::LocalDateTime(stmt));
    assert_eq!(read(ClockMode::Statement), now_local_dt());
    assert_eq!(now_utc_dt(), stmt);
    {
        let _next = QueryClockGuard::pinned_at(naive_utc(2000, 1, 1, 0, 0, 0), 0);
        // An autocommit pin is its own transaction: both modes agree.
        assert_eq!(read(ClockMode::Transaction), read(ClockMode::Statement));
    }
    assert_ne!(read(ClockMode::Transaction), read(ClockMode::Statement));
}
