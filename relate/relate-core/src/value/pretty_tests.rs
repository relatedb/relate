//! Neo4j `AnyValue.prettyPrint()` pins: the probed-form table of the
//! message-embedded value rendering — split from `value/tests.rs` for the
//! file-size rule (the `depth_tests.rs` convention), so the pretty
//! concern owns both its source module and its pins.

use super::*;
use std::sync::Arc;

#[test]
fn pretty_print_matches_neo4j_2026_10() {
    let m = |pairs: &[(&str, Value)]| {
        Value::Map(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect::<BTreeMap<_, _>>()
                .into(),
        )
    };
    let bare_node = |id| {
        Value::from_node(Node {
            id,
            labels: Arc::new(vec!["L".into()]),
            properties: crate::PropertyMap::share(BTreeMap::from([("p".into(), Value::Int(1))])),
        })
    };
    let rel = |id, start, end| {
        Value::from_relationship(Relationship {
            id,
            start,
            end,
            rel_type: "T".into(),
            properties: crate::PropertyMap::share(BTreeMap::new()),
        })
    };
    let temporal = |kind, text| Value::Temporal(crate::temporal::parse_iso(kind, text).unwrap());
    let cases: Vec<(Value, &str)> = vec![
        (Value::String("abc".into()), "String(\"abc\")"),
        // Live does not escape the payload (probed).
        (Value::String("a\"b'c".into()), "String(\"a\"b'c\")"),
        (Value::Int(123), "Long(123)"),
        (Value::Bool(true), "Boolean('true')"),
        (Value::Bool(false), "Boolean('false')"),
        // Java %e over the shortest round-trip digits, half-up at six
        // fractional places — every row probed.
        (Value::Float(1.5), "Double(1.500000e+00)"),
        (Value::Float(0.0), "Double(0.000000e+00)"),
        (Value::Float(-2.25), "Double(-2.250000e+00)"),
        (Value::Float(1.0e20), "Double(1.000000e+20)"),
        (Value::Float(1.0e-7), "Double(1.000000e-07)"),
        (Value::Float(1.0e300), "Double(1.000000e+300)"),
        (Value::Float(f64::NAN), "Double(NaN)"),
        (Value::Float(f64::INFINITY), "Double(Infinity)"),
        (Value::Float(f64::NEG_INFINITY), "Double(-Infinity)"),
        // 129/128: the tie rounds half-up, not half-even (probed).
        (Value::Float(1.0078125), "Double(1.007813e+00)"),
        (Value::Float(-1.0078125), "Double(-1.007813e+00)"),
        // Rounding carries out of the mantissa's integer digit, both
        // exponent signs: the 9.999999x shapes renormalize to
        // 1.000000 with the exponent bumped, 1.9999999 just becomes
        // 2.000000 (every row probed).
        (Value::Float(0.099999995), "Double(1.000000e-01)"),
        (Value::Float(0.0099999999), "Double(1.000000e-02)"),
        (Value::Float(9.9999999), "Double(1.000000e+01)"),
        (Value::Float(99.999999), "Double(1.000000e+02)"),
        (Value::Float(9999999.9), "Double(1.000000e+07)"),
        (Value::Float(1.9999999), "Double(2.000000e+00)"),
        (Value::Float(0.19999999), "Double(2.000000e-01)"),
        // The smallest subnormal: live's legacy formatter prints
        // `Double(4.900000e-324)` (digits past the shortest that
        // round-trip); the engine prints the shortest correctly-rounded
        // form — the one documented boundary divergence in pretty.rs.
        (Value::Float(5.0e-324), "Double(5.000000e-324)"),
        (Value::Float(1.23456789012345e-5), "Double(1.234568e-05)"),
        // Containers: wrapped, recursive, sorted keys (probed).
        (
            Value::List(vec![Value::Int(1), Value::Int(2)].into()),
            "List{Long(1), Long(2)}",
        ),
        (Value::List(vec![bare_node(0)].into()), "List{(0)}"),
        (
            m(&[("b", Value::Int(2)), ("a", Value::Int(1))]),
            "Map{a -> Long(1), b -> Long(2)}",
        ),
        // Entities: bare ids, no labels/types/properties (probed).
        (bare_node(0), "(0)"),
        (rel(1152921504606846976, 0, 1), "-[1152921504606846976]-"),
        (
            Value::from_path(Path {
                nodes: vec![
                    Node {
                        id: 0,
                        labels: Arc::new(vec![]),
                        properties: crate::PropertyMap::share(BTreeMap::new()),
                    },
                    Node {
                        id: 1,
                        labels: Arc::new(vec![]),
                        properties: crate::PropertyMap::share(BTreeMap::new()),
                    },
                ],
                rels: vec![Relationship {
                    id: 1152921504606846976,
                    start: 0,
                    end: 1,
                    rel_type: "R".into(),
                    properties: crate::PropertyMap::share(BTreeMap::new()),
                }],
            }),
            "Path{(0)-[1152921504606846976]-(1)}",
        ),
        // Temporals and points print their display forms (probed).
        (temporal("date", "2021-01-01"), "2021-01-01"),
        (
            temporal("datetime", "2021-01-01T00:00:00+01:00"),
            "2021-01-01T00:00:00+01:00",
        ),
        (
            temporal("datetime", "2021-01-01T00:00:00[Europe/Stockholm]"),
            "2021-01-01T00:00:00+01:00[Europe/Stockholm]",
        ),
        (
            temporal("localdatetime", "2021-01-01T00:00:00"),
            "2021-01-01T00:00:00",
        ),
        (temporal("time", "12:30:00"), "12:30:00Z"),
        (temporal("localtime", "12:30:00"), "12:30:00"),
        (temporal("duration", "P1Y2M"), "P1Y2M"),
        (temporal("duration", "PT1M30S"), "PT1M30S"),
        (
            Value::Point(PointValue::cartesian_2d(1.0, 2.0)),
            "point({x: 1.0, y: 2.0, crs: 'cartesian'})",
        ),
        // A UUID wraps its bare hyphenated form (probed).
        (
            Value::Uuid(0x01a21d99_d9a3_7d5d_914c_c6135a2db6ba),
            "UUID(01a21d99-d9a3-7d5d-914c-c6135a2db6ba)",
        ),
        // A byte array (probed via a Bolt parameter).
        (Value::Bytes(vec![120u8, 121].into()), "ByteArray[120, 121]"),
        // Engine-only scalar: the container family's form (live 2026.10.1
        // has no vector values to probe).
        (
            Value::Vector {
                coord: VectorCoordKind::Int8,
                values: vec![Value::Int(1), Value::Int(2)].into(),
            },
            "Vector{Long(1), Long(2)}",
        ),
        // Never reaches an operand-echoing message (null propagation
        // answers first); Neo4j's NO_VALUE form kept for totality.
        (Value::Null, "NULL"),
    ];
    for (input, expected) in cases {
        assert_eq!(input.pretty_print(), expected, "input {input:?}");
    }
}

/// The empty path's pretty form (its walk is the empty chain — the
/// composed `Path{}`; an unprobed shape, live cannot produce one).
#[test]
fn an_empty_path_pretty_prints_the_empty_path_form() {
    let empty = Value::from_path(super::Path {
        nodes: vec![],
        rels: vec![],
    });
    assert_eq!(empty.pretty_print(), "Path{}");
}
