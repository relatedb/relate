//! Unit tests for the result-side temporal and point structures
//! (ADR-0325, ADR-0326), the `utc` patch's DateTime forms (ADR-0329) and
//! their unconditional 5.x siblings (ADR-0484 decision 7): every engine
//! value the Neo4j 2026.09.0 probes pinned encodes to the structure
//! Neo4j sent, at any depth, and decodes back to itself through the
//! ADR-0177 parameter decoders.

use super::*;
use crate::dialect::Dialect;
use crate::param_structs::{struct_to_value, ParamPath};
use crate::value_codec::{MdbPackError, ValueCodec};
use relate::Value;
use relate_core::temporal::CivilZonedFields;
use relate_test_support::values::wrapped_in_lists;

/// Evaluate `expr` in the engine and return the single value.
fn eval(expr: &str) -> Value {
    let db = relate::Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let mut result = conn.query(&format!("RETURN {expr} AS v")).unwrap();
    result.rows.remove(0).remove(0)
}

/// Encode at `dialect` — the axis this file's tests vary: plain 4.4,
/// `utc`-patched 4.4, or 5.4.
fn encode(v: &Value, dialect: Dialect) -> Pack {
    let codec = ValueCodec::new(dialect, relate::Namespace::Neo4j);
    Pack::from_mdb(v, codec).unwrap()
}

/// The parameter decoder's view of an encoded structure: decoding the
/// encoding must give the original engine value back, under the same
/// `dialect` as the encode.
fn decode_back(pack: &Pack, dialect: Dialect) -> Value {
    let Pack::Struct { tag, fields } = pack else {
        panic!("expected a structure, got {pack:?}");
    };
    struct_to_value(*tag, fields, &ParamPath::param("p"), dialect).unwrap()
}

/// F-51..F-56 and F-58: the probe's exact values and structures.
#[test]
fn temporal_results_are_the_neo4j_structures() {
    let cases = [
        (
            "date('2024-01-02')",
            Pack::Struct {
                tag: DATE,
                fields: vec![Pack::Int(19724)],
            },
        ),
        (
            "time('03:04:05.123456789+01:00')",
            Pack::Struct {
                tag: TIME,
                fields: vec![Pack::Int(11045123456789), Pack::Int(3600)],
            },
        ),
        (
            "localtime('03:04:05.123456789')",
            Pack::Struct {
                tag: LOCAL_TIME,
                fields: vec![Pack::Int(11045123456789)],
            },
        ),
        (
            "datetime('2024-01-02T03:04:05.123456789+01:00')",
            Pack::Struct {
                tag: DATE_TIME,
                fields: vec![Pack::Int(1704164645), Pack::Int(123456789), Pack::Int(3600)],
            },
        ),
        (
            "datetime('2024-01-02T03:04:05[Europe/Stockholm]')",
            Pack::Struct {
                tag: DATE_TIME_ZONE_ID,
                fields: vec![
                    Pack::Int(1704164645),
                    Pack::Int(0),
                    Pack::String("Europe/Stockholm".into()),
                ],
            },
        ),
        (
            "localdatetime('2024-01-02T03:04:05.123456789')",
            Pack::Struct {
                tag: LOCAL_DATE_TIME,
                fields: vec![Pack::Int(1704164645), Pack::Int(123456789)],
            },
        ),
        (
            "duration('P1Y2M3DT4H5M6.5S')",
            Pack::Struct {
                tag: DURATION,
                fields: vec![
                    Pack::Int(14),
                    Pack::Int(3),
                    Pack::Int(14706),
                    Pack::Int(500000000),
                ],
            },
        ),
    ];
    for (expr, expected) in cases {
        let value = eval(expr);
        let pack = encode(&value, Dialect::plain_44());
        assert_eq!(pack, expected, "{expr}");
        assert_eq!(
            decode_back(&pack, Dialect::plain_44()),
            value,
            "{expr} must decode back to itself"
        );
    }
}

/// An offset-only zone is a DateTime whatever its spelling: `Z`, a numeric
/// offset, or the engine's default `Z` zone of a zoneless map.
#[test]
fn offset_only_zones_are_0x46() {
    for (expr, offset) in [
        ("datetime('2024-01-02T03:04:05Z')", 0),
        ("datetime({year: 2024, month: 1, day: 2, hour: 3})", 0),
        (
            "datetime({year: 2024, month: 1, day: 2, hour: 3, timezone: '+05:30'})",
            19800,
        ),
    ] {
        let Pack::Struct { tag, fields } = encode(&eval(expr), Dialect::plain_44()) else {
            panic!("{expr} must be a structure");
        };
        assert_eq!(tag, DATE_TIME, "{expr}");
        assert_eq!(fields[2], Pack::Int(offset), "{expr}");
    }
}

/// Negative epochs keep Bolt's floor-seconds / non-negative-nanos split, a
/// negative duration's sub-second part is the euclidean remainder, and an
/// out-of-chrono-range civil date counts its days exactly.
#[test]
fn signs_and_civil_dates_follow_the_bolt_layout() {
    assert_eq!(
        encode(
            &eval("localdatetime('1969-12-31T23:59:59.5')"),
            Dialect::plain_44()
        ),
        Pack::Struct {
            tag: LOCAL_DATE_TIME,
            fields: vec![Pack::Int(-1), Pack::Int(500_000_000)],
        }
    );
    assert_eq!(
        encode(&eval("duration('PT-1.5S')"), Dialect::plain_44()),
        Pack::Struct {
            tag: DURATION,
            fields: vec![
                Pack::Int(0),
                Pack::Int(0),
                Pack::Int(-2),
                Pack::Int(500_000_000)
            ],
        }
    );
    let civil = Value::Temporal(Temporal::CivilDate {
        year: 999_999_999,
        month: 12,
        day: 31,
    });
    let days = civil_epoch_days(999_999_999, 12, 31);
    assert_eq!(
        encode(&civil, Dialect::plain_44()),
        Pack::Struct {
            tag: DATE,
            fields: vec![Pack::Int(
                i64::try_from(days).expect("civil date days fit i64")
            )],
        }
    );
    let civil_dt = Value::Temporal(Temporal::CivilDateTime {
        year: -999_999_999,
        month: 1,
        day: 1,
        time: NaiveTime::from_hms_nano_opt(0, 0, 1, 7).unwrap(),
    });
    let seconds = civil_epoch_days(-999_999_999, 1, 1) * 86_400 + 1;
    assert_eq!(
        encode(&civil_dt, Dialect::plain_44()),
        Pack::Struct {
            tag: LOCAL_DATE_TIME,
            fields: vec![
                Pack::Int(i64::try_from(seconds).expect("civil datetime seconds fit i64")),
                Pack::Int(7),
            ],
        }
    );
}

/// ADR-0683's far-year zoned form packs through the same structure as an
/// offset-only chrono datetime, from the engine's one zoned view: the
/// wall clock's seconds-of-day are counted once, not twice, on every
/// dialect axis (the legacy tag carries the wall clock read as UTC, the
/// UTC tags the instant).
#[test]
fn a_far_year_zoned_datetime_packs_its_wall_clock_once() {
    let time = NaiveTime::from_hms_nano_opt(3, 4, 5, 123_000_000).unwrap();
    let value = Value::Temporal(Temporal::CivilZoned(Box::new(CivilZonedFields {
        year: -999_999_999,
        month: 1,
        day: 1,
        time,
        offset: 7200,
        zone: None,
    })));
    // The wall clock's seconds read from the same engine derivation the
    // packer's arm reads (never hand-rolled day/hour arithmetic).
    let wall = i64::try_from(civil_epoch_seconds(-999_999_999, 1, 1, time))
        .expect("the wall clock fits i64");
    assert_eq!(
        encode(&value, Dialect::plain_44()),
        Pack::Struct {
            tag: DATE_TIME,
            fields: vec![Pack::Int(wall), Pack::Int(123_000_000), Pack::Int(7200)],
        }
    );
    for dialect in [Dialect::utc_44(), Dialect::at_5_4()] {
        assert_eq!(
            encode(&value, dialect),
            Pack::Struct {
                tag: DATE_TIME_UTC,
                fields: vec![
                    Pack::Int(wall - 7200),
                    Pack::Int(123_000_000),
                    Pack::Int(7200)
                ],
            },
            "{dialect:?}"
        );
    }
}

/// A component beyond i64 fails the encode instead of falling back.
#[test]
fn an_out_of_range_component_fails_the_encode() {
    let huge = Value::Temporal(Temporal::Duration {
        months: 0,
        days: 0,
        nanos: i128::from(i64::MAX) * 1_000_000_000 * 4,
    });
    assert_eq!(
        Pack::from_mdb(&huge, ValueCodec::plain_44()),
        Err(MdbPackError::NotRepresentable(
            "temporal result does not fit Bolt structure 0x45: a component exceeds i64".into()
        ))
    );
}

/// F-57, the 0x69 form, and ADR-0484 decision 7: the UTC DateTime
/// structures apply on both axes that select them — negotiated through
/// the 4.4 `utc` patch, unconditional on a 5.4 connection — with the
/// same field layout on each: unshifted UTC epoch seconds beside the
/// original offset or zone id, so the legacy tags are never written.
/// Both decode back through the same-dialect decoders, and every other
/// temporal structure is unchanged on either axis.
#[test]
fn utc_datetimes_use_the_utc_structures_over_the_patch_and_at_5_4() {
    for dialect in [Dialect::utc_44(), Dialect::at_5_4()] {
        let offset = eval("datetime('2024-01-02T03:04:05.123456789+01:00')");
        let pack = encode(&offset, dialect);
        assert_eq!(
            pack,
            Pack::Struct {
                tag: DATE_TIME_UTC,
                fields: vec![Pack::Int(1704161045), Pack::Int(123456789), Pack::Int(3600)],
            },
            "{dialect:?}"
        );
        assert_eq!(decode_back(&pack, dialect), offset);

        let zoned = eval("datetime('2024-01-02T03:04:05[Europe/Stockholm]')");
        let pack = encode(&zoned, dialect);
        assert_eq!(
            pack,
            Pack::Struct {
                tag: DATE_TIME_ZONE_ID_UTC,
                fields: vec![
                    Pack::Int(1704161045),
                    Pack::Int(0),
                    Pack::String("Europe/Stockholm".into())
                ],
            },
            "{dialect:?}"
        );
        assert_eq!(decode_back(&pack, dialect), zoned);

        // Every other temporal structure is unchanged by the axis.
        for expr in ["date('2024-01-02')", "localdatetime('2024-01-02T03:04:05')"] {
            let value = eval(expr);
            assert_eq!(
                encode(&value, dialect),
                encode(&value, Dialect::plain_44()),
                "{expr} at {dialect:?}"
            );
        }
    }
}

/// F-59/F-60: points are Point2D/Point3D with Float coordinates, a whole
/// number included.
#[test]
fn point_results_are_point_structures_with_float_coordinates() {
    let cases = [
        (
            "point({srid: 7203, x: 1.5, y: 2.5})",
            Pack::Struct {
                tag: POINT_2D,
                fields: vec![Pack::Int(7203), Pack::Float(1.5), Pack::Float(2.5)],
            },
        ),
        (
            "point({longitude: 13, latitude: 55.6})",
            Pack::Struct {
                tag: POINT_2D,
                fields: vec![Pack::Int(4326), Pack::Float(13.0), Pack::Float(55.6)],
            },
        ),
        (
            "point({x: 1, y: 2, z: 3})",
            Pack::Struct {
                tag: POINT_3D,
                fields: vec![
                    Pack::Int(9157),
                    Pack::Float(1.0),
                    Pack::Float(2.0),
                    Pack::Float(3.0),
                ],
            },
        ),
        (
            "point({longitude: 1.5, latitude: 2.5, height: 3.5})",
            Pack::Struct {
                tag: POINT_3D,
                fields: vec![
                    Pack::Int(4979),
                    Pack::Float(1.5),
                    Pack::Float(2.5),
                    Pack::Float(3.5),
                ],
            },
        ),
    ];
    for (expr, expected) in cases {
        let value = eval(expr);
        let pack = encode(&value, Dialect::plain_44());
        assert_eq!(pack, expected, "{expr}");
        assert_eq!(decode_back(&pack, Dialect::plain_44()), value, "{expr}");
    }
}

/// The structures apply at any depth, and a vector stays a string.
#[test]
fn structures_apply_at_any_depth_and_vectors_stay_strings() {
    let value = eval("{xs: [date('1970-01-02'), point({x: 1.0, y: 2.0})]}");
    assert_eq!(
        encode(&value, Dialect::plain_44()),
        Pack::Map(vec![(
            Pack::String("xs".into()),
            Pack::List(vec![
                Pack::Struct {
                    tag: DATE,
                    fields: vec![Pack::Int(1)],
                },
                Pack::Struct {
                    tag: POINT_2D,
                    fields: vec![Pack::Int(7203), Pack::Float(1.0), Pack::Float(2.0)],
                },
            ]),
        )])
    );
    let vector = eval("vector([1.0, 2.0], 2, FLOAT32)");
    assert!(
        matches!(encode(&vector, Dialect::plain_44()), Pack::String(_)),
        "a vector result keeps its string encoding"
    );
}

/// A structure is a container on the wire: a temporal leaf under 127 lists
/// is the 128th container and encodes; one more list is rejected by the
/// shared nesting limit, like a 129th list.
#[test]
fn a_structure_counts_against_the_nesting_limit() {
    let value = wrapped_in_lists(eval("date('1970-01-01')"), 127);
    assert!(Pack::from_mdb(&value, ValueCodec::plain_44()).is_ok());
    let over = Value::List(vec![value].into());
    assert_eq!(
        Pack::from_mdb(&over, ValueCodec::plain_44()),
        Err(MdbPackError::DepthExceeded)
    );
}
