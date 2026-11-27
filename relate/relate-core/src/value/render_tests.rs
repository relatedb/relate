use super::test_fixtures::{list, map, node, node_at, rel, rel_at};
use super::*;

// CIP-271: the toString() display form, pinned against Neo4j 2026.09.0
// probes (unquoted strings, sorted map keys, recursive rendering).
#[test]
fn to_display_string_matches_neo4j_2026_09() {
    let cases: Vec<(Value, &str)> = vec![
        (
            map(&[
                ("a", Value::Int(1)),
                ("b", Value::String("x y".into())),
                ("c", list(&[Value::Int(1), Value::Int(2)])),
                ("d", map(&[("e", Value::Bool(true))])),
            ]),
            "{a: 1, b: x y, c: [1, 2], d: {e: true}}",
        ),
        // Sorted key order regardless of construction order.
        (
            map(&[("z", Value::Int(1)), ("a", Value::Int(2))]),
            "{a: 2, z: 1}",
        ),
        (map(&[]), "{}"),
        (list(&[]), "[]"),
        (
            list(&[Value::Int(1), Value::Null, Value::String("a".into())]),
            "[1, null, a]",
        ),
        (Value::Float(1.0), "1.0"),
        (Value::Float(1.5), "1.5"),
        // A point renders the CRS name and float coordinates (the harness's
        // `toString(point())` and `'hi' + point(...)` pins).
        (
            Value::Point(PointValue::cartesian_2d(1.0, 2.5)),
            "point({x: 1.0, y: 2.5, crs: 'cartesian'})",
        ),
        (
            Value::Point(PointValue::cartesian_3d(0.0, 4.0, 1.5)),
            "point({x: 0.0, y: 4.0, z: 1.5, crs: 'cartesian-3d'})",
        ),
        (
            Value::Point(PointValue::wgs84_2d(-73.9857, 40.7484)),
            "point({x: -73.9857, y: 40.7484, crs: 'wgs-84'})",
        ),
        (Value::Null, "null"),
        (Value::Bool(true), "true"),
        (node(&["X"], &[]), "(:X)"),
        (rel("REL", &[]), "[:REL]"),
        // A vector renders as its value constructor: the coordinate type is
        // `vector()`'s argument, never a `NOT NULL` type expression
        // (TypeConversion4 [3], StringInterpolation [1.10]).
        (
            Value::Vector {
                coord: VectorCoordKind::Int8,
                values: vec![Value::Int(1), Value::Int(2)].into(),
            },
            "vector([1, 2], 2, INTEGER8)",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(input.to_display_string(), expected, "input {input:?}");
    }
}

#[test]
fn vector_literal_is_a_value_constructor() {
    let vector = |coord, values: Vec<Value>| Value::Vector {
        coord,
        values: values.into(),
    };
    assert_eq!(
        vector(VectorCoordKind::Int8, vec![Value::Int(1), Value::Int(2)]).to_cypher_literal(),
        "vector([1, 2], 2, INTEGER8)"
    );
    assert_eq!(
        vector(
            VectorCoordKind::Float32,
            vec![Value::Float(1.0), Value::Float(2.5)]
        )
        .to_cypher_literal(),
        "vector([1.0, 2.5], 2, FLOAT32)"
    );
    assert_eq!(
        vector(VectorCoordKind::BFloat16, vec![Value::Float(257.0)]).to_cypher_literal(),
        "vector([257.0], 1, BFLOAT16)"
    );
}

/// The one float policy at the old vector-only `1e16` boundary: pinned
/// through the value's own forms, so the test proves the scalar literal, the
/// scalar display and the vector coordinate spelling all read one policy —
/// not merely that one helper returns the expected text.
#[test]
fn float_literal_policy_is_shared_by_scalars_and_vector_coordinates() {
    // One spelling table, both accessors: a scalar's literal and display
    // forms must agree with each other and with the vector coordinate text.
    let cases: [(f64, &str); 7] = [
        (1.0, "1.0"),
        (1.5, "1.5"),
        (1.0e16, "10000000000000000.0"),
        (9.007199254740992e15, "9007199254740992.0"),
        (f64::NAN, "NaN"),
        (f64::INFINITY, "Infinity"),
        (f64::NEG_INFINITY, "-Infinity"),
    ];
    for (value, expected) in cases {
        let scalar = Value::Float(value);
        assert_eq!(scalar.to_cypher_literal(), expected, "{value:?}");
        assert_eq!(scalar.to_display_string(), expected, "{value:?}");
    }
    for value in [1.0e16, 9.007199254740992e15, 1.7976931348623157e308] {
        assert_eq!(
            Value::Float(value)
                .to_cypher_literal()
                .parse::<f64>()
                .expect("round-trips"),
            value
        );
    }
    let vector = Value::Vector {
        coord: VectorCoordKind::Float64,
        values: vec![Value::Float(1.0e16)].into(),
    };
    // The vector's literal embeds the very coordinate spelling the table
    // pins above.
    assert_eq!(
        vector.to_cypher_literal(),
        "vector([10000000000000000.0], 1, FLOAT)"
    );
    assert_eq!(vector.to_display_string(), vector.to_cypher_literal());
}

// CIP-271: labels print alphabetically, and every name position whose
// spelling is not a plain identifier is backticked — the labels/types/keys
// of the spec's worked examples.
#[test]
fn to_display_string_quotes_special_names_and_sorts_labels() {
    // A node created as (:B:A:`special label`) renders sorted, with only the
    // name that needs escaping quoted.
    assert_eq!(
        node(&["B", "A", "special label"], &[]).to_display_string(),
        "(:A:B:`special label`)"
    );
    assert_eq!(node(&["X"], &[]).to_display_string(), "(:X)");
    assert_eq!(rel("REL TYPE", &[]).to_display_string(), "[:`REL TYPE`]");
    // A map prints sorted, and only the keys outside the plain identifier
    // shape are backticked.
    assert_eq!(
        map(&[
            ("first name", Value::String("Alice".into())),
            ("123", Value::String("digit key".into())),
            ("a.b", Value::String("dot".into())),
            ("z", Value::Int(1)),
        ])
        .to_display_string(),
        "{`123`: digit key, `a.b`: dot, `first name`: Alice, z: 1}"
    );
}

// REL8-217: the round-trippable literal shares the display's one name
// policy — every label/type/map key outside the plain identifier shape is
// backticked in BOTH forms, or `(:special label)` and `{first name: …}`
// do not re-parse. The two deliberate divergences: the literal keeps the
// stored label order (the display sorts, CIP-271) and quotes the string
// values (the display renders them bare).
#[test]
fn to_cypher_literal_quotes_special_names_keeps_label_order() {
    // Stored order, only the name that needs escaping quoted.
    assert_eq!(
        node(&["B", "A", "special label"], &[]).to_cypher_literal(),
        "(:B:A:`special label`)"
    );
    // A property key outside the identifier shape is backticked, its
    // string value quoted and escaped.
    assert_eq!(
        node(
            &["X"],
            &[("first name", Value::String("Alice O'Neil".into()))]
        )
        .to_cypher_literal(),
        "(:X {`first name`: 'Alice O\\'Neil'})"
    );
    assert_eq!(
        rel("REL TYPE", &[("a.b", Value::Int(1))]).to_cypher_literal(),
        "[:`REL TYPE` {`a.b`: 1}]"
    );
}

// REL8-217: the point literal is the round-trippable twin of the display
// form — the decoder-faithful `srid` spelling, coordinates through the
// engine's one float policy (an integral coordinate keeps its `.0`),
// never `PointValue`'s bare-`{}` diff-normalizer `Display`.
#[test]
fn point_literal_keeps_the_srid_spelling_and_float_text() {
    let cases: Vec<(Value, &str, &str)> = vec![
        (
            Value::Point(PointValue::cartesian_2d(1.0, 2.5)),
            "point({srid: 7203, x: 1.0, y: 2.5})",
            "point({x: 1.0, y: 2.5, crs: 'cartesian'})",
        ),
        (
            Value::Point(PointValue::cartesian_3d(0.0, 4.0, 1.5)),
            "point({srid: 9157, x: 0.0, y: 4.0, z: 1.5})",
            "point({x: 0.0, y: 4.0, z: 1.5, crs: 'cartesian-3d'})",
        ),
        (
            Value::Point(PointValue::wgs84_2d(-73.9857, 40.7484)),
            "point({srid: 4326, x: -73.9857, y: 40.7484})",
            "point({x: -73.9857, y: 40.7484, crs: 'wgs-84'})",
        ),
    ];
    for (input, literal, display) in cases {
        assert_eq!(input.to_cypher_literal(), literal, "input {input:?}");
        assert_eq!(input.to_display_string(), display, "input {input:?}");
    }
}

// `toString(path)` is the bare traversal chain — no enclosing `<…>` and
// no properties — while the round-trippable literal keeps both. Verified
// against Neo4j 2026.10.1:
// `toString(p)` of (:A {name:'a'})-[:FIRST]->(:B)<-[:SECOND]-(:C) is
// `(:A)-[:FIRST]->(:B)<-[:SECOND]-(:C)`.
#[test]
fn to_display_string_of_a_path_is_the_bare_traversal_chain() {
    let named = |id, label: &str| {
        node_at(
            id,
            &[label],
            &[("name", Value::String(label.to_ascii_lowercase().into()))],
        )
    };
    // The second relationship is traversed backwards.
    let path = Value::from_path(Path {
        nodes: vec![named(1, "A"), named(2, "B"), named(3, "C")],
        rels: vec![
            rel_at(10, 1, 2, "FIRST", &[("w", Value::Int(1))]),
            rel_at(11, 3, 2, "SECOND", &[("w", Value::Int(1))]),
        ],
    });
    assert_eq!(
        path.to_display_string(),
        "(:A)-[:FIRST]->(:B)<-[:SECOND]-(:C)"
    );
    assert_eq!(
        path.to_cypher_literal(),
        "<(:A {name: 'a'})-[:FIRST {w: 1}]->(:B {name: 'b'})<-[:SECOND {w: 1}]-(:C {name: 'c'})>"
    );
    // A zero-length path is its single node.
    let single = Value::from_path(Path {
        nodes: vec![named(1, "A")],
        rels: vec![],
    });
    assert_eq!(single.to_display_string(), "(:A)");
    // `toStringList([path])` renders the same bare chain.
    let list_of_paths = list(&[path]);
    assert_eq!(
        list_of_paths.to_display_string(),
        "[(:A)-[:FIRST]->(:B)<-[:SECOND]-(:C)]"
    );
}
