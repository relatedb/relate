//! A function call as the map expression of a comprehension whose WHERE
//! filters by label: live Neo4j 2026.10 executes
//! `[x IN xs WHERE x:L2 | substring(x.p1, 0, 1)]` — the map expression
//! may begin with a function name, not only a variable the `.` and `{`
//! dead-ends already cover — so the pipe is the comprehension's map
//! separator there too. The label disjunction a `|` can also start
//! (`x:A|B`) keeps committing on operators, and a map expression that is
//! a plain variable keeps its reading.

use crate::common;

use common::{connection, first_column, list, map, string, strings};

/// Two labeled nodes so the label filter admits one and rejects the
/// other, making the map expression observable.
fn filter_store() -> relate::Connection {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 'alpha'}), (:L2 {p1: 'beta'})")
        .unwrap();
    c
}

/// The gap's exact shape: a function call after a label-filter WHERE,
/// with and without the collect feed the report probed.
#[test]
fn function_call_map_expression_after_a_label_filter_executes() {
    for q in [
        "MATCH (c) WITH collect(c) AS xs RETURN [x IN xs WHERE x:L2 | substring(x.p1, 0, 1)] AS y",
        "MATCH (c) RETURN [x IN collect(c) WHERE x:L2 | substring(x.p1, 0, 1)] AS y",
    ] {
        let mut c = filter_store();
        assert_eq!(
            first_column(&c.query(q).unwrap()),
            vec![strings(&["b"])],
            "{q}"
        );
    }
}

/// The same map expression on the non-matching side of the filter yields
/// the empty list, and every accepted map-expression spelling agrees
/// with the equivalent post-hoc projection.
#[test]
fn function_call_map_expression_controls() {
    let mut c = filter_store();
    // A label no node carries filters everything out; the matching one
    // keeps the whole substring.
    assert_eq!(
        first_column(
            &c.query("MATCH (c) WITH collect(c) AS xs RETURN [x IN xs WHERE x:L3 | substring(x.p1, 0, 1)] AS y")
                .unwrap()
        ),
        vec![strings(&[])],
    );
    assert_eq!(
        first_column(
            &c.query("MATCH (c) WITH collect(c) AS xs RETURN [x IN xs WHERE x:L1 | substring(x.p1, 0, 1)] AS y")
                .unwrap()
        ),
        vec![strings(&["a"])],
    );
    // A zero-argument call, a nested call, and a call over a filtered
    // variable beside a property predicate in the same WHERE.
    assert_eq!(
        first_column(
            &c.query(
                "MATCH (c) WITH collect(c) AS xs RETURN [x IN xs WHERE x:L2 | toString(x.p1)] AS y"
            )
            .unwrap()
        ),
        vec![strings(&["beta"])],
    );
    assert_eq!(
        first_column(
            &c.query("MATCH (c) WITH collect(c) AS xs RETURN [x IN xs WHERE x:L2 AND x.p1 IS NOT NULL | substring(toString(x.p1), 0, 2)] AS y")
                .unwrap()
        ),
        vec![strings(&["be"])],
    );
    // The pre-existing map-expression spellings keep their readings.
    assert_eq!(
        first_column(
            &c.query("MATCH (c) WITH collect(c) AS xs RETURN [x IN xs WHERE x:L2 | x.p1] AS y")
                .unwrap()
        ),
        vec![strings(&["beta"])],
    );
    assert_eq!(
        first_column(
            &c.query("MATCH (c) WITH collect(c) AS xs RETURN [x IN xs WHERE x:L2 | x {.*}] AS y")
                .unwrap()
        ),
        vec![list(vec![map(&[("p1", string("beta"))])])],
    );
}

/// A label disjunction in the comprehension WHERE still commits on
/// operators, so the pipe inside `x:A|B` keeps its disjunction reading
/// and the map separator is the later pipe.
#[test]
fn label_disjunction_in_comprehension_where_keeps_committing() {
    let mut c = common::abc_store();
    // `x:A|B` filters to two rows; the map expression is the later pipe.
    assert_eq!(
        first_column(
            &c.query("MATCH (c) WITH collect(c) AS xs RETURN [x IN xs WHERE x:A|B | x.p1] AS y")
                .unwrap()
        ),
        vec![strings(&["a", "b"])],
    );
}

/// A pattern comprehension takes the same function-call map after its
/// own label-filter WHERE.
#[test]
fn pattern_comprehension_function_call_map_expression_executes() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 1})-[:R]->(:L2 {p1: 22})")
        .unwrap();
    assert_eq!(
        first_column(
            &c.query("MATCH (a:L1) RETURN [(a)-[:R]->(b) WHERE a:L1 | toString(b.p1)] AS y")
                .unwrap()
        ),
        vec![strings(&["22"])],
    );
}
