//! REL8-2030: the one total order behind `ORDER BY` / `min` / `max` /
//! `coll.sort`, over every value kind, and the discrete percentile that
//! answers an input value. The corpus findings: maps and points compared
//! `Equal` (issue 46, unsorted output and wrong min/max), the sort panicked
//! once 21 maps exposed that non-transitive relation (issue 54),
//! `percentileDisc` returned an f64 image instead of the picked operand
//! (issue 27), and a far-year DATE compared as null against an in-range one
//! (issue 26). Every expectation is what live Neo4j 2026.10 answers (the
//! corpus evidence folders).

use crate::common;

use common::{connection, first_column, first_column_strings, list, map, row, rows, run};
use relate::Value;

fn int(value: i64) -> Value {
    Value::Int(value)
}

fn float(value: f64) -> Value {
    Value::Float(value)
}

/// The first column of `result` as display strings — the shape temporal
/// renderings are asserted through.
fn display_column(result: &relate::QueryResult) -> Vec<String> {
    first_column(result)
        .iter()
        .map(Value::to_display_string)
        .collect()
}

/// Issue 46: maps order by size, then keys, then the values under them, so
/// a cross-type pair or a null under a key decides the order instead of
/// making the maps equal.
#[test]
fn maps_order_by_their_keys_and_values() {
    let mut c = connection();
    let expected = vec![
        vec![map(&[("a", Value::String("x".into()))])],
        vec![map(&[("a", int(1))])],
    ];
    for q in [
        "UNWIND [{a: 1}, {a: 'x'}] AS m RETURN m ORDER BY m",
        "UNWIND [{a: 'x'}, {a: 1}] AS m RETURN m ORDER BY m",
    ] {
        assert_eq!(rows(&mut c, q), expected, "{q}");
    }
    // Descending puts the null-valued map first.
    assert_eq!(
        rows(
            &mut c,
            "UNWIND [{a: 1}, {a: null}] AS m RETURN m ORDER BY m DESC"
        ),
        vec![
            vec![map(&[("a", Value::Null)])],
            vec![map(&[("a", int(1))])]
        ]
    );
    // A shared key with null values on both sides: the remaining values
    // decide, which the old compare-equal made impossible.
    assert_eq!(
        rows(
            &mut c,
            "UNWIND [{a: null, b: 2}, {a: null, b: 1}] AS m RETURN m ORDER BY m"
        ),
        vec![
            vec![map(&[("a", Value::Null), ("b", int(1))])],
            vec![map(&[("a", Value::Null), ("b", int(2))])],
        ]
    );
    // min/max over maps read the same relation: STRING sorts before NUMBER.
    assert_eq!(
        row(
            &mut c,
            "UNWIND [{a: 1}, {a: 'x'}] AS m RETURN min(m) AS mn, max(m) AS mx"
        ),
        vec![
            map(&[("a", Value::String("x".into()))]),
            map(&[("a", int(1))])
        ]
    );
    // The same pair through a map accessor, and through `coll.sort`.
    assert_eq!(
        row(
            &mut c,
            "UNWIND [{a: 1}, {a: 'x'}] AS m \
             WITH min(m) AS mn, max(m) AS mx RETURN mn.a AS mn, mx.a AS mx"
        ),
        vec![Value::String("x".into()), int(1)]
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "CYPHER 25 RETURN coll.sort([{a: 1}, {a: 'x'}])[0].a AS first"
        )),
        vec![Value::String("x".into())]
    );
    // The six-map mix: STRING < NUMBER < null, so '1', '4', 3, 6, null, null.
    let sorted = first_column(&run(
        &mut c,
        "UNWIND range(1, 6) AS i \
         WITH CASE i % 3 WHEN 0 THEN {a: i} WHEN 1 THEN {a: toString(i)} ELSE {a: null} END AS m \
         WITH m ORDER BY m RETURN m.a AS a",
    ));
    assert_eq!(
        sorted,
        vec![
            Value::String("1".into()),
            Value::String("4".into()),
            int(3),
            int(6),
            Value::Null,
            Value::Null,
        ]
    );
    // A map nested in a list reaches the map arm.
    assert_eq!(
        rows(
            &mut c,
            "UNWIND [[{a: 1}], [{a: 'x'}]] AS l RETURN l ORDER BY l"
        ),
        vec![
            vec![list(vec![map(&[("a", Value::String("x".into()))])])],
            vec![list(vec![map(&[("a", int(1))])])],
        ]
    );
}

/// Issue 46: points order by CRS (SRID), then coordinates — a plain point
/// property is an ordinary sort key and a `min`/`max` over points answers
/// the extremes.
#[test]
fn points_order_by_crs_then_coordinates() {
    let mut c = connection();
    assert_eq!(
        first_column(&run(
            &mut c,
            "UNWIND [point({x: 2, y: 0}), point({x: 1, y: 0}), point({x: 0, y: 5})] AS p \
             RETURN p.x AS x ORDER BY p"
        )),
        vec![float(0.0), float(1.0), float(2.0)]
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "UNWIND [point({x: 1, y: 0, z: 1}), point({longitude: 1, latitude: 0}), \
                     point({x: 1, y: 0})] AS p \
             RETURN p.srid AS srid ORDER BY p"
        )),
        vec![int(4326), int(7203), int(9157)]
    );
    assert_eq!(
        row(
            &mut c,
            "UNWIND [point({x: 2, y: 0}), point({x: 1, y: 0}), point({x: 0, y: 5})] AS p \
             WITH min(p) AS mn, max(p) AS mx RETURN mn.x AS mn, mx.x AS mx"
        ),
        vec![float(0.0), float(2.0)]
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "UNWIND [[point({x: 2, y: 0})], [point({x: 1, y: 0})]] AS l \
             RETURN l[0].x AS x ORDER BY l"
        )),
        vec![float(1.0), float(2.0)]
    );
}

/// Issue 46: a map projection orders by the projected values, missing
/// properties included (they project as null and sort last).
#[test]
fn map_projections_order_by_their_projected_values() {
    let mut c = connection();
    run(
        &mut c,
        "UNWIND [{name: 'b', age: 3}, {name: 'a'}, {name: 'c', age: 1}] AS r \
         CREATE (:P {name: r.name, age: r.age})",
    );
    assert_eq!(
        first_column_strings(&run(
            &mut c,
            "MATCH (n:P) RETURN n.name AS name ORDER BY n {.age, .name}"
        )),
        vec!["c".to_string(), "b".to_string(), "a".to_string()]
    );
}

/// Issue 46: nodes, relationships, paths and lists of entities order by the
/// entity id (the creation order here), so a plain `ORDER BY n` sorts.
#[test]
fn entities_order_by_id() {
    let mut c = connection();
    run(
        &mut c,
        "CREATE (a:N {i: 3}), (b:N {i: 1}), (c:N {i: 2}), \
         (a)-[:R {i: 3}]->(b), (b)-[:R {i: 1}]->(c), (c)-[:R {i: 2}]->(a)",
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "MATCH (n:N) WITH n ORDER BY n RETURN collect(n.i) AS by_node"
        )),
        vec![list(vec![int(3), int(1), int(2)])]
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "MATCH ()-[r:R]->() WITH r ORDER BY r RETURN collect(r.i) AS by_rel"
        )),
        vec![list(vec![int(3), int(1), int(2)])]
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "MATCH p = (:N)-[:R]->() WITH p ORDER BY p \
             RETURN collect(head(nodes(p)).i) AS by_path"
        )),
        vec![list(vec![int(3), int(1), int(2)])]
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "MATCH (n:N) WITH [n] AS l ORDER BY l RETURN collect(l[0].i) AS by_list"
        )),
        vec![list(vec![int(3), int(1), int(2)])]
    );
    assert_eq!(
        row(
            &mut c,
            "MATCH (n:N) WITH min(n) AS mn, max(n) AS mx RETURN mn.i AS mn, mx.i AS mx"
        ),
        vec![int(3), int(2)]
    );
    assert_eq!(
        row(
            &mut c,
            "MATCH ()-[r:R]->() WITH min(r) AS mn, max(r) AS mx RETURN mn.i AS mn, mx.i AS mx"
        ),
        vec![int(3), int(2)]
    );
}

/// Issue 54: 21 maps exposing the non-transitive relation made the standard
/// library's sort panic (`does not correctly implement a total order`) and
/// the statement fail. Each shape must answer 21 rows.
#[test]
fn ordering_more_than_twenty_maps_does_not_panic() {
    // The two 21-map lists, each stated once and used by both the ORDER BY
    // and the `coll.sort` statement.
    const Q1_VALUES: &str = "[{a: 68}, {a: 39}, {a: 60}, {a: null}, {a: 40}, {a: null}, \
                             {a: null}, {a: 7}, {a: null}, {a: 90}, {a: null}, {a: 42}, \
                             {a: null}, {a: null}, {a: 90}, {a: null}, {a: 69}, {a: null}, \
                             {a: 56}, {a: null}, {a: 40}]";
    const Q2_VALUES: &str = "[{a: 561}, {a: 300}, {a: 489}, {a: 956}, {a: 315}, {a: 877}, \
                             {a: '309'}, {a: '855'}, {a: '306'}, {a: 111}, {a: '574'}, \
                             {a: '485'}, {a: '861'}, {a: '127'}, {a: '118'}, {a: 436}, \
                             {a: '309'}, {a: 752}, {a: 941}, {a: '641'}, {a: 825}]";
    let mut c = connection();
    for (label, values) in [
        ("integer-or-null", Q1_VALUES),
        ("integer-or-string", Q2_VALUES),
    ] {
        let count = first_column(&run(
            &mut c,
            &format!("UNWIND {values} AS m WITH m ORDER BY m RETURN count(*) AS rows"),
        ));
        assert_eq!(count, vec![int(21)], "{label}");
    }
    // The everyday form: a map projection with missing properties.
    run(
        &mut c,
        "UNWIND range(0, 20) AS i CREATE (:P {i: i, a: CASE WHEN i % 2 = 0 THEN i ELSE null END})",
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "MATCH (n:P) WITH n ORDER BY n {.a} RETURN count(*) AS rows"
        )),
        vec![int(21)]
    );
    // `properties(n)` over an INTEGER/STRING mix.
    run(
        &mut c,
        "UNWIND range(1, 21) AS i CREATE (:Q {a: CASE WHEN i % 2 = 0 THEN i ELSE 'x' END})",
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "MATCH (n:Q) WITH n ORDER BY properties(n) RETURN count(*) AS rows"
        )),
        vec![int(21)]
    );
    // `coll.sort` reads the same relation.
    assert_eq!(
        first_column(&run(
            &mut c,
            &format!("CYPHER 25 RETURN size(coll.sort({Q1_VALUES})) AS n")
        )),
        vec![int(21)]
    );
}

/// Issue 27: `percentileDisc` is the discrete percentile — it returns the
/// picked input value, type and exact bits included, above 2^53 too.
#[test]
fn percentile_disc_returns_the_input_value() {
    let mut c = connection();
    assert_eq!(
        row(
            &mut c,
            "UNWIND [9007199254740993, 1] AS x \
             RETURN percentileDisc(x, 1.0) AS a, max(x) AS m"
        ),
        vec![int(9_007_199_254_740_993), int(9_007_199_254_740_993)]
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "UNWIND [9223372036854775807] AS x RETURN percentileDisc(x, 0.5) AS a"
        )),
        vec![int(i64::MAX)]
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "UNWIND [1, 2, 3] AS x RETURN percentileDisc(x, 0.5) AS a"
        )),
        vec![int(2)]
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "UNWIND [1, 2.5, 3] AS x RETURN percentileDisc(x, 0.9) AS a"
        )),
        vec![int(3)]
    );
    // The type is part of the answer: an INTEGER sample yields an INTEGER.
    assert_eq!(
        first_column(&run(
            &mut c,
            "UNWIND [1, 2, 3] AS x RETURN valueType(percentileDisc(x, 0.5)) AS t"
        )),
        vec![Value::String("INTEGER NOT NULL".into())]
    );
    // The continuous kind still interpolates through an f64.
    assert_eq!(
        first_column(&run(
            &mut c,
            "UNWIND [1, 2, 3] AS x RETURN percentileCont(x, 0.5) AS a"
        )),
        vec![float(2.0)]
    );
}

/// Issue 26: a far-year DATE is the same Cypher type as an in-range one, so
/// the comparison operators answer and the two sort together.
#[test]
fn far_year_dates_compare_and_sort_with_in_range_dates() {
    let mut c = connection();
    assert_eq!(
        first_column(&run(
            &mut c,
            "RETURN date('+300000-06-15') > date('2000-01-01') AS compares"
        )),
        vec![Value::Bool(true)]
    );
    assert_eq!(
        display_column(&run(
            &mut c,
            "UNWIND [date('2000-01-01'), date('+300000-06-15'), date('1990-01-01')] AS d \
             RETURN d ORDER BY d"
        )),
        vec![
            "1990-01-01".to_string(),
            "2000-01-01".to_string(),
            "+300000-06-15".to_string(),
        ]
    );
    // The surface spelling a scenario asserts through `toString`.
    assert_eq!(
        first_column_strings(&run(&mut c, "RETURN toString(date('+300000-06-15')) AS d")),
        vec!["+300000-06-15".to_string()]
    );
    // The control the corpus pairs with it: two far-year dates already
    // compared, and the in-range control still reads its component.
    assert_eq!(
        row(
            &mut c,
            "RETURN date('+100000-06-15').year AS y, \
                    date('+300000-06-15') < date('+400000-01-01') AS civil_vs_civil"
        ),
        vec![int(100_000), Value::Bool(true)]
    );
}

/// Corpus 26 with a range index: the index's per-variant key order cannot
/// express DATE's total order, so a far-year value must not narrow the
/// seek — the comparison has to see both representations. (A civil date has
/// no order key; its presence marks the index unbounded and the seek scans.)
#[test]
fn a_range_index_seek_does_not_drop_far_year_dates() {
    let mut c = connection();
    run(&mut c, "CREATE RANGE INDEX xd FOR (n:X) ON (n.d)");
    run(
        &mut c,
        "CREATE (:X {d: date('2000-01-01')}), (:X {d: date('+300000-06-15')}), \
         (:X {d: date('-300000-06-15')})",
    );
    // An in-range bound keeps the positive far-year row too; a far-year
    // bound keeps every in-range row and the negative civil year; and a
    // negative civil year sorts below every in-range date.
    for (predicate, expected) in [
        ("n.d > date('1990-01-01')", 2),
        ("n.d < date('+400000-01-01')", 3),
        ("n.d < date('1990-01-01')", 1),
    ] {
        assert_eq!(
            first_column(&run(
                &mut c,
                &format!("MATCH (n:X) WHERE {predicate} RETURN count(*) AS c")
            )),
            vec![int(expected)],
            "{predicate}"
        );
    }
}
