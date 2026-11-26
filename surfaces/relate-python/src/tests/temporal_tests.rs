//! The temporal fidelity crossing (LABS-756): a temporal result arrives
//! as its stdlib type when that type can carry it losslessly, and as
//! `relate.Temporal` when it cannot — sub-microsecond nanoseconds, or
//! an IANA zone name. The rich type carries the engine's own canonical
//! text, so what Python sees reproduces what the engine prints.

use super::*;

#[test]
fn test_microsecond_datetime_result_is_stdlib_datetime() {
    with_py(|py| {
        let (value, type_name) = temporal_result(py, "datetime('2015-06-01T12:34:56.123456Z')");
        assert_eq!(type_name, "datetime");
        let dtmod = py.import("datetime").unwrap();
        assert!(value
            .is_instance(&dtmod.getattr("datetime").unwrap())
            .unwrap());
        assert_eq!(temporal_attr::<u32>(&value, "microsecond"), 123_456);
        assert_eq!(
            value
                .call_method0("utcoffset")
                .unwrap()
                .getattr("total_seconds")
                .unwrap()
                .call0()
                .unwrap()
                .extract::<f64>()
                .unwrap(),
            0.0
        );
    });
}

#[test]
fn test_sub_microsecond_datetime_result_is_temporal() {
    with_py(|py| {
        let (value, type_name) = temporal_result(py, "datetime('2015-06-01T12:34:56.123456789Z')");
        assert_eq!(type_name, "Temporal");
        assert_eq!(temporal_attr::<String>(&value, "kind"), "datetime");
        assert_eq!(
            temporal_attr::<String>(&value, "text"),
            "2015-06-01T12:34:56.123456789Z"
        );
        assert_eq!(temporal_attr::<u32>(&value, "nanosecond"), 123_456_789);
        assert_eq!(
            temporal_attr::<Option<i32>>(&value, "offset_seconds"),
            Some(0)
        );
        assert!(temporal_attr::<Option<String>>(&value, "zone_name").is_none());
        // `str()` and `repr()` reproduce the engine's canonical form.
        assert_eq!(
            value.str().unwrap().to_string(),
            "2015-06-01T12:34:56.123456789Z"
        );
        assert!(value
            .repr()
            .unwrap()
            .to_string()
            .contains("2015-06-01T12:34:56.123456789Z"));
        // `to_native()` is the documented lossy projection: microseconds only.
        let native = value.call_method0("to_native").unwrap();
        assert_eq!(temporal_attr::<u32>(&native, "microsecond"), 123_456);
    });
}

#[test]
fn test_named_zone_datetime_keeps_zone_name_across_dst() {
    with_py(|py| {
        // The two halves of Stockholm's 2020 fall-back overlap: same wall
        // clock, different offsets, one zone name for both — the zone name is
        // what stdlib loses.
        for (expr, text, offset) in [
            (
                "datetime('2020-10-25T02:30+02:00[Europe/Stockholm]')",
                "2020-10-25T02:30+02:00[Europe/Stockholm]",
                7_200,
            ),
            (
                "datetime('2020-10-25T02:30+01:00[Europe/Stockholm]')",
                "2020-10-25T02:30+01:00[Europe/Stockholm]",
                3_600,
            ),
        ] {
            let (value, type_name) = temporal_result(py, expr);
            assert_eq!(type_name, "Temporal", "{expr}");
            assert_eq!(
                temporal_attr::<String>(&value, "kind"),
                "datetime",
                "{expr}"
            );
            assert_eq!(temporal_attr::<String>(&value, "text"), text, "{expr}");
            assert_eq!(
                temporal_attr::<Option<String>>(&value, "zone_name").as_deref(),
                Some("Europe/Stockholm"),
                "{expr}"
            );
            assert_eq!(
                temporal_attr::<Option<i32>>(&value, "offset_seconds"),
                Some(offset),
                "{expr}"
            );
            assert_eq!(temporal_attr::<u32>(&value, "nanosecond"), 0, "{expr}");
            // The stdlib projection keeps the offset but drops the name.
            let native = value.call_method0("to_native").unwrap();
            assert!(temporal_attr::<Option<Py<PyAny>>>(&native, "tzinfo").is_some());
        }
    });
}

#[test]
fn test_named_zone_canonical_text_omits_whole_minute_seconds() {
    with_py(|py| {
        // The engine's canonical rendering drops a zero seconds field; the rich
        // type must carry that form, not a re-spelled `12:00:00`.
        let (value, type_name) =
            temporal_result(py, "datetime('1984-10-11T12:00+01:00[Europe/Stockholm]')");
        assert_eq!(type_name, "Temporal");
        assert_eq!(
            temporal_attr::<String>(&value, "text"),
            "1984-10-11T12:00+01:00[Europe/Stockholm]"
        );
    });
}

#[test]
fn test_sub_microsecond_time_result_is_temporal() {
    with_py(|py| {
        let (value, type_name) = temporal_result(py, "time('12:31:14.645876123Z')");
        assert_eq!(type_name, "Temporal");
        assert_eq!(temporal_attr::<String>(&value, "kind"), "time");
        assert_eq!(
            temporal_attr::<String>(&value, "text"),
            "12:31:14.645876123Z"
        );
        assert_eq!(temporal_attr::<u32>(&value, "nanosecond"), 645_876_123);
        assert_eq!(
            temporal_attr::<Option<i32>>(&value, "offset_seconds"),
            Some(0)
        );
        assert!(temporal_attr::<Option<String>>(&value, "zone_name").is_none());
    });
}

#[test]
fn test_microsecond_time_result_is_stdlib_time() {
    with_py(|py| {
        let (value, type_name) = temporal_result(py, "time('12:31:14.645876Z')");
        assert_eq!(type_name, "time");
        assert_eq!(temporal_attr::<u32>(&value, "microsecond"), 645_876);
    });
}

#[test]
fn test_offset_only_zone_spellings_stay_stdlib() {
    with_py(|py| {
        // The engine stores offset-only datetimes with a zone *spelling*
        // (`Z`, the map constructor's default; `UTC`), not a name. A stdlib
        // datetime keeps the offset those spellings denote, so nothing is lost
        // and the type must stay stdlib — a named zone is the only zone a
        // stdlib datetime cannot carry.
        for expr in [
            "datetime({year: 2024, month: 1, day: 2})",
            "datetime({year: 2024, month: 1, day: 2, timezone: 'Z'})",
            "datetime({year: 2024, month: 1, day: 2, timezone: 'UTC'})",
        ] {
            let (value, type_name) = temporal_result(py, expr);
            assert_eq!(type_name, "datetime", "{expr}");
            assert_eq!(temporal_attr::<u32>(&value, "microsecond"), 0, "{expr}");
        }
    });
}

#[test]
fn test_sub_microsecond_localdatetime_result_is_temporal() {
    with_py(|py| {
        let (value, type_name) =
            temporal_result(py, "localdatetime('1984-10-11T12:31:14.645876123')");
        assert_eq!(type_name, "Temporal");
        assert_eq!(temporal_attr::<String>(&value, "kind"), "localdatetime");
        assert_eq!(
            temporal_attr::<String>(&value, "text"),
            "1984-10-11T12:31:14.645876123"
        );
        assert_eq!(temporal_attr::<u32>(&value, "nanosecond"), 645_876_123);
        assert!(temporal_attr::<Option<i32>>(&value, "offset_seconds").is_none());
        assert!(temporal_attr::<Option<String>>(&value, "zone_name").is_none());
    });
}

#[test]
fn test_microsecond_localdatetime_result_is_stdlib_datetime() {
    with_py(|py| {
        let (value, type_name) = temporal_result(py, "localdatetime('1984-10-11T12:31:14.645876')");
        assert_eq!(type_name, "datetime");
        assert_eq!(temporal_attr::<u32>(&value, "microsecond"), 645_876);
        assert!(value.call_method0("utcoffset").unwrap().is_none());
    });
}

#[test]
fn test_temporal_equality_uses_the_canonical_text() {
    with_py(|py| {
        let (a, _) = temporal_result(py, "datetime('2020-10-25T02:30+02:00[Europe/Stockholm]')");
        let (b, _) = temporal_result(py, "datetime('2020-10-25T02:30+02:00[Europe/Stockholm]')");
        let (c, _) = temporal_result(py, "datetime('2020-10-25T02:30+01:00[Europe/Stockholm]')");
        assert!(a.eq(&b).unwrap(), "same instant and zone must be equal");
        assert!(
            !a.eq(&c).unwrap(),
            "the two halves of the DST overlap are different instants"
        );
        assert_eq!(a.hash().unwrap(), b.hash().unwrap());
        // The rich type never equals its lossy stdlib projection.
        let native = a.call_method0("to_native").unwrap();
        assert!(!a.eq(&native).unwrap());
    });
}
