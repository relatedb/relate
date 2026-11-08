//! Scan-predicate specialization tests (the q8 61x family): the
//! differential contract — the specialized walk's rows are the generic
//! walk's rows — over randomized data, plus the error-surface identity
//! (a construction error raises identically on both paths).

use std::collections::BTreeMap;

use proptest::prelude::*;
use relate_core::Value;

use super::predicate_spec::{take_spec_gates, with_spec_disabled};
use super::test_support::{
    error_of, exec_cypher, exec_write_uncommitted, try_exec_cypher, try_exec_cypher_params,
};
use crate::graph::Graph;
use crate::test_support::config_with_floor;

/// One generated node: its label class, and the `n`/`s`/`b` properties
/// (ints, free strings, and date-shaped strings — padded ISO, compact,
/// and deliberately out-of-calendar forms among them).
#[derive(Debug, Clone)]
struct NodeCase {
    person: bool,
    n: Option<i64>,
    s: Option<String>,
    b: Option<String>,
}

fn any_string() -> impl Strategy<Value = String> {
    // A small alphabet: needles and haystacks collide often enough that
    // CONTAINS selects something, without quoting concerns.
    "[ab]{0,3}"
}

fn any_date_string() -> impl Strategy<Value = String> {
    // Padded ISO (the stored form the fast value recognizes), compact
    // and month-only forms (the general parse), and out-of-calendar
    // padded forms (the civil-date fallback) in one pool.
    prop_oneof![
        "[0-9]{4}-[0-9]{2}-[0-9]{2}",
        "[0-9]{8}",
        "[0-9]{4}-[0-9]{2}",
        Just("1990-02-30".to_string()),
        Just("2000-13-01".to_string()),
    ]
}

fn node_case() -> impl Strategy<Value = NodeCase> {
    (
        bool::arbitrary(),
        prop::option::of((-20i64..20i64).prop_map(|i| i)),
        prop::option::of(any_string()),
        prop::option::of(any_date_string()),
    )
        .prop_map(|(person, n, s, b)| NodeCase { person, n, s, b })
}

fn spec_graph(nodes: &[NodeCase]) -> Graph {
    let g = Graph::default();
    g.begin_write().unwrap();
    let mut ids = Vec::new();
    for (i, case) in nodes.iter().enumerate() {
        let mut props = BTreeMap::new();
        props.insert("id".to_string(), Value::Int(i as i64));
        if let Some(n) = case.n {
            props.insert("n".to_string(), Value::Int(n));
        }
        if let Some(s) = &case.s {
            props.insert("s".to_string(), Value::String(s.as_str().into()));
        }
        if let Some(b) = &case.b {
            props.insert("b".to_string(), Value::String(b.as_str().into()));
        }
        let labels = if case.person {
            vec!["P".into()]
        } else {
            vec!["Q".into()]
        };
        ids.push(g.create_node(labels, props).unwrap());
    }
    // One random `R` edge per node (self-loops included): the hop shapes
    // have adjacency to walk, in both directions and across labels.
    let mut state = nodes.len() as u64;
    let next = |state: &mut u64| {
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (*state >> 33) as usize
    };
    for id in &ids {
        let other = ids[next(&mut state) % ids.len().max(1)];
        g.create_rel(*id, other, "R".into(), BTreeMap::new())
            .unwrap();
    }
    g.commit_write();
    g
}

/// The query shapes the compiled predicates cover, each answered twice —
/// the specialized walk, and the generic one under
/// [`with_spec_disabled`] — over the same randomized graph.
fn differential_cases(nodes: &[NodeCase]) -> Vec<String> {
    let _ = nodes;
    vec![
        // The property comparison form.
        "MATCH (x:P) WHERE x.n > 3 RETURN x.id AS id ORDER BY id".into(),
        // The equality and CONTAINS forms, with a needle the alphabet
        // actually produces.
        "MATCH (x:P) WHERE x.s CONTAINS 'ab' RETURN x.id AS id ORDER BY id".into(),
        "MATCH (x:P) WHERE x.s = 'a' RETURN x.id AS id ORDER BY id".into(),
        // The regex form (whole-string match), over the same alphabet.
        "MATCH (x:P) WHERE x.s =~ 'a|ab' RETURN x.id AS id ORDER BY id".into(),
        // The temporal form: a padded-ISO bound (folded at lowering), the
        // property carrying padded, compact, month-only, civil, and
        // missing shapes.
        "MATCH (x:P) WHERE date(x.b) > date('1985-06-15') RETURN x.id AS id ORDER BY id".into(),
        // A bound outside the padded range keeps the typed comparison.
        "MATCH (x:P) WHERE date(x.b) < date('10000-01-01') RETURN x.id AS id ORDER BY id".into(),
        // All-or-nothing: the second conjunct does not compile, so the
        // whole variable takes the generic path.
        "MATCH (x:P) WHERE x.n > 3 AND toString(x.n) = '7' RETURN x.id AS id ORDER BY id".into(),
        // A multi-conjunct variable where every conjunct compiles.
        "MATCH (x:P) WHERE x.n >= -5 AND x.s CONTAINS 'b' RETURN x.id AS id ORDER BY id".into(),
        // The hop shapes: both endpoints carry compiled predicates, so
        // the start gate and the far-node gate both run — directed and
        // undirected, so the backward walker's gate is covered too.
        "MATCH (a:P)-[:R]->(b:Q) WHERE a.n > 3 AND b.s CONTAINS 'ab' \
         RETURN a.id AS aid, b.id AS bid ORDER BY aid, bid"
            .into(),
        "MATCH (a:P)-[:R]-(b:Q) WHERE date(a.b) > date('1985-06-15') AND b.n < 0 \
         RETURN a.id AS aid, b.id AS bid ORDER BY aid, bid"
            .into(),
        // The order-safety shape (the third compile rule): a
        // cross-variable conjunct written BEFORE an erroring
        // single-variable one. The generic walk short-circuits on the
        // cross-variable conjunct at the far node's bind, so the
        // malformed `b` never constructs there — the gate must not hoist
        // the construction ahead of that short-circuit.
        "MATCH (a:P)-[:R]->(b:Q) WHERE a.n < b.n AND date(b.b) > date('1985-06-15') \
         RETURN a.id AS aid, b.id AS bid ORDER BY aid, bid"
            .into(),
    ]
}

/// One answer's comparable form: its rows, or the error text it ended
/// with — the differential holds for both outcomes, and the generated
/// date shapes produce both.
fn comparable(result: std::result::Result<super::QueryResult, relate_core::Error>) -> String {
    match result {
        Ok(done) => format!("{:?}", done.rows),
        Err(err) => format!("error: {err}"),
    }
}

proptest! {
    #![proptest_config(config_with_floor(1000))]
    #[test]
    fn specialized_walk_rows_match_the_generic_walk(
        nodes in proptest::collection::vec(node_case(), 0..40),
    ) {
        let g = spec_graph(&nodes);
        for query in differential_cases(&nodes) {
            let specialized = comparable(try_exec_cypher(&g, &query));
            let generic = comparable(with_spec_disabled(|| try_exec_cypher(&g, &query)));
            prop_assert_eq!(specialized, generic, "for {}", query);
        }
        // The IN form: the bound is a parameter-resolved list (the one
        // canonical constant policy, `Expr::seek_value`, folds scalars
        // only), with a type-mismatched Int member so the membership's
        // non-match verdicts are covered too.
        let params = BTreeMap::from([(
            "tags".to_string(),
            Value::List(
                vec![
                    Value::String("a".into()),
                    Value::String("ab".into()),
                    Value::Int(7),
                ]
                .into(),
            ),
        )]);
        let query = "MATCH (x:P) WHERE x.s IN $tags RETURN x.id AS id ORDER BY id";
        let specialized = comparable(try_exec_cypher_params(&g, query, &params));
        let generic =
            comparable(with_spec_disabled(|| try_exec_cypher_params(&g, query, &params)));
        prop_assert_eq!(specialized, generic, "for {}", query);
    }
}

/// The error surface is the generic path's: a property the construction
/// cannot parse raises the same error whether the gate or the residual
/// evaluates the conjunct.
#[test]
fn construction_errors_match_the_generic_path() {
    let g = Graph::default();
    g.begin_write().unwrap();
    g.create_node(
        vec!["P".into()],
        BTreeMap::from([
            ("id".into(), Value::Int(0)),
            ("b".into(), Value::String("not a date".into())),
        ]),
    )
    .unwrap();
    g.commit_write();
    let query = "MATCH (x:P) WHERE date(x.b) > date('1990-01-01') RETURN x.id AS id";
    let err_of = |r: std::result::Result<super::QueryResult, relate_core::Error>| match r {
        Ok(_) => panic!("the malformed property must error"),
        Err(err) => err.to_string(),
    };
    let specialized = err_of(try_exec_cypher(&g, query));
    let generic = err_of(with_spec_disabled(|| try_exec_cypher(&g, query)));
    assert_eq!(specialized, generic);
    assert!(specialized.contains("cannot parse date"), "{specialized}");
}

/// The order-safety rule's short-circuit pin (the third compile rule): a
/// cross-variable conjunct written BEFORE the erroring compiled one
/// short-circuits the generic walk at the far node's bind — the
/// malformed `date(b.b)` never constructs there — so the variable takes
/// the generic path wholesale and the specialized walk answers the same
/// empty rows without raising.
#[test]
fn a_cross_variable_conjunct_short_circuits_before_the_gate_raises() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let persons = [5, 9];
    for (i, n) in persons.into_iter().enumerate() {
        let a = g
            .create_node(
                vec!["P".into()],
                BTreeMap::from([
                    ("id".into(), Value::Int(i as i64)),
                    ("n".into(), Value::Int(n)),
                ]),
            )
            .unwrap();
        // The far node: n below every `a` (the cross-variable conjunct is
        // false for both edges), b malformed (the construction would
        // raise if it ever evaluated).
        let b = g
            .create_node(
                vec!["Q".into()],
                BTreeMap::from([
                    ("id".into(), Value::Int(10 + i as i64)),
                    ("n".into(), Value::Int(4)),
                    ("b".into(), Value::String("not a date".into())),
                ]),
            )
            .unwrap();
        g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    }
    g.commit_write();
    let query = "MATCH (a:P)-[:R]->(b:Q) WHERE a.n < b.n \
                 AND date(b.b) > date(\"1990-01-01\") \
                 RETURN a.id AS aid, b.id AS bid";
    let rows_of = |r: std::result::Result<super::QueryResult, relate_core::Error>| match r {
        Ok(done) => done.rows,
        Err(err) => panic!("the short-circuit must suppress the error, got: {err}"),
    };
    let specialized = rows_of(try_exec_cypher(&g, query));
    let generic = rows_of(with_spec_disabled(|| try_exec_cypher(&g, query)));
    assert_eq!(specialized, generic);
    // Every cross-variable conjunct is false (5 < 4, 9 < 4): no rows.
    assert!(specialized.is_empty());
}

/// The order-safety rule's error side: when the cross-variable conjunct
/// PASSES, the generic walk evaluates the erroring conjunct and raises —
/// and so must the specialized one. The rule suppresses the order flip,
/// never a legitimate error.
#[test]
fn a_passing_cross_variable_conjunct_still_raises_the_construction_error() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g
        .create_node(
            vec!["P".into()],
            BTreeMap::from([("id".into(), Value::Int(0)), ("n".into(), Value::Int(5))]),
        )
        .unwrap();
    let b = g
        .create_node(
            vec!["Q".into()],
            BTreeMap::from([
                ("id".into(), Value::Int(1)),
                ("n".into(), Value::Int(9)),
                ("b".into(), Value::String("not a date".into())),
            ]),
        )
        .unwrap();
    g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    let query = "MATCH (a:P)-[:R]->(b:Q) WHERE a.n < b.n \
                 AND date(b.b) > date(\"1990-01-01\") \
                 RETURN a.id AS aid, b.id AS bid";
    let err_of = |r: std::result::Result<super::QueryResult, relate_core::Error>| match r {
        Ok(done) => panic!(
            "the passing conjunct must reach the error, got {:?}",
            done.rows
        ),
        Err(err) => err.to_string(),
    };
    let specialized = err_of(try_exec_cypher(&g, query));
    let generic = err_of(with_spec_disabled(|| try_exec_cypher(&g, query)));
    assert_eq!(specialized, generic);
    assert!(specialized.contains("cannot parse date"), "{specialized}");
}

/// The gate's Scratch arm (overlay/reads.rs): a write statement's MATCH
/// walks candidates the same statement's CREATE placed in the write
/// scratch, and the gate settles them through the scratch's copy — the
/// committed nodes through the live record, the created ones through the
/// scratch arm — with the generic path's rows.
#[test]
fn write_scratch_candidates_settle_through_the_gate() {
    let g = Graph::default();
    g.begin_write().unwrap();
    for (i, n) in [1, 7].into_iter().enumerate() {
        g.create_node(
            vec!["P".into()],
            BTreeMap::from([
                ("id".into(), Value::Int(i as i64)),
                ("n".into(), Value::Int(n)),
            ]),
        )
        .unwrap();
    }
    g.commit_write();
    // Two scratch candidates: one the predicate passes (id 2), one it
    // rejects (id 3) — the arm settles both verdicts.
    let query = "CREATE (x:P {id: 2, n: 5}) CREATE (y:P {id: 3, n: 1}) \
                 WITH x MATCH (z:P) WHERE z.n > 3 RETURN z.id AS id ORDER BY id";
    let _ = take_spec_gates();
    let specialized = exec_write_uncommitted(&g, query);
    let gates = take_spec_gates();
    assert!(
        gates > 0,
        "the scan gate must settle the statement's candidates"
    );
    let generic = with_spec_disabled(|| exec_write_uncommitted(&g, query));
    assert_eq!(specialized.rows, generic.rows);
    // The committed n=7 (id 1) and the scratch-created n=5 (id 2): the
    // scratch candidate is in the answer, not just rejected by the gate.
    assert_eq!(specialized.rows.len(), 2);
}

/// The settled skip's entry-bound boundary: a variable the entry row
/// already owns never takes a gate (the row owns the binding), so none
/// of its conjuncts settle — the second MATCH's WHERE still filters on
/// it, and the walk's residual pass still evaluates it.
#[test]
fn entry_bound_variables_keep_their_residual_conjuncts() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let p0 = g
        .create_node(
            vec!["P".into()],
            BTreeMap::from([("id".into(), Value::Int(0)), ("n".into(), Value::Int(1))]),
        )
        .unwrap();
    let p1 = g
        .create_node(
            vec!["P".into()],
            BTreeMap::from([("id".into(), Value::Int(1)), ("n".into(), Value::Int(7))]),
        )
        .unwrap();
    for (i, s) in ["ab", "zz"].into_iter().enumerate() {
        let q = g
            .create_node(
                vec!["Q".into()],
                BTreeMap::from([
                    ("id".into(), Value::Int(10 + i as i64)),
                    ("s".into(), Value::String(s.into())),
                ]),
            )
            .unwrap();
        // Both persons reach both Q nodes: only the WHERE can filter.
        g.create_rel(p0, q, "R".into(), BTreeMap::new()).unwrap();
        g.create_rel(p1, q, "R".into(), BTreeMap::new()).unwrap();
    }
    g.commit_write();
    let query = "MATCH (a:P) WITH a MATCH (a)-[:R]->(b:Q) \
                 WHERE a.n > 3 AND b.s CONTAINS 'ab' \
                 RETURN a.id AS aid, b.id AS bid ORDER BY aid, bid";
    let rows_of = |r: std::result::Result<super::QueryResult, relate_core::Error>| match r {
        Ok(done) => done.rows,
        Err(err) => panic!("the query is valid, got: {err}"),
    };
    let specialized = rows_of(try_exec_cypher(&g, query));
    let generic = rows_of(with_spec_disabled(|| try_exec_cypher(&g, query)));
    assert_eq!(specialized, generic);
    // Only person 1 (n=7) survives its own conjunct — the entry-bound
    // variable's predicate still filters — and only the 'ab' Q.
    assert_eq!(specialized.len(), 1);
}

/// The q8 shape itself, pinned: the walk settles its candidates through
/// the gated store read (the engagement the differential alone could
/// not prove), and its rows are the generic path's rows.
#[test]
fn the_q8_shape_engages_the_gate_and_matches_the_generic_rows() {
    let g = Graph::default();
    g.begin_write().unwrap();
    for (i, birthday) in ["1980-05-05", "1995-12-31", "1990-06-01", "2001-02-28"]
        .into_iter()
        .enumerate()
    {
        let person = g
            .create_node(
                vec!["Person".into()],
                BTreeMap::from([
                    ("id".into(), Value::Int(i as i64)),
                    ("birthday".into(), Value::String(birthday.into())),
                ]),
            )
            .unwrap();
        let forum = g
            .create_node(
                vec!["Forum".into()],
                BTreeMap::from([(
                    "title".into(),
                    Value::String(format!("forum {i} about Emilio Fernandez posts").into()),
                )]),
            )
            .unwrap();
        g.create_rel(forum, person, "hasModerator".into(), BTreeMap::new())
            .unwrap();
    }
    g.commit_write();
    let query = "MATCH (p:Person)<-[:hasModerator]-(f:Forum) \
                 WHERE date(p.birthday) > date(\"1990-01-01\") \
                   AND f.title CONTAINS \"Emilio Fernandez\" \
                 RETURN DISTINCT p.id AS id ORDER BY id";
    let _ = take_spec_gates();
    let specialized = exec_cypher(&g, query);
    let gates = take_spec_gates();
    assert!(gates > 0, "the scan gate must engage for the q8 shape");
    let generic = with_spec_disabled(|| {
        let _ = take_spec_gates();
        let rows = exec_cypher(&g, query);
        assert_eq!(take_spec_gates(), 0, "the disabled hook takes no gates");
        rows
    });
    assert_eq!(specialized.rows, generic.rows);
    // Every birthday after 1990-01-01 passes the bound and every forum
    // title matches the needle, so the three matching persons answer.
    assert_eq!(specialized.rows.len(), 3);
}

/// The In and Regex operators — admitted with the one canonical
/// `is_pred_op` table, evaluated through the shared `binary` — gate
/// soundly: both shapes engage the gate and answer the generic path's
/// rows. A literal `IN` list is not a seek constant, so that bound
/// declines and the shape stays on the generic path (no gates),
/// conservative like every other declined bound.
#[test]
fn in_and_regex_shapes_engage_the_gate_and_match_the_generic_rows() {
    let g = Graph::default();
    g.begin_write().unwrap();
    for (i, s) in ["a", "ab", "ba", ""].into_iter().enumerate() {
        g.create_node(
            vec!["P".into()],
            BTreeMap::from([
                ("id".into(), Value::Int(i as i64)),
                ("s".into(), Value::String(s.into())),
            ]),
        )
        .unwrap();
    }
    g.commit_write();
    let rows_of = |params: &BTreeMap<String, Value>, query: &str| {
        try_exec_cypher_params(&g, query, params).unwrap().rows
    };
    // The IN gate: a parameter-resolved list bound, with an Int member
    // the string property never matches.
    let tags = BTreeMap::from([(
        "tags".to_string(),
        Value::List(vec![Value::String("a".into()), Value::Int(7)].into()),
    )]);
    let in_query = "MATCH (x:P) WHERE x.s IN $tags RETURN x.id AS id ORDER BY id";
    let _ = take_spec_gates();
    let specialized = rows_of(&tags, in_query);
    assert!(take_spec_gates() > 0, "the IN shape must engage the gate");
    let generic = with_spec_disabled(|| rows_of(&tags, in_query));
    assert_eq!(specialized, generic);
    // Only the 'a' node is a member.
    assert_eq!(specialized.len(), 1);

    // The regex gate: a string pattern bound, whole-string match.
    let re_query = "MATCH (x:P) WHERE x.s =~ 'a|ab' RETURN x.id AS id ORDER BY id";
    let no_params = BTreeMap::new();
    let _ = take_spec_gates();
    let specialized = rows_of(&no_params, re_query);
    assert!(
        take_spec_gates() > 0,
        "the regex shape must engage the gate"
    );
    let generic = with_spec_disabled(|| rows_of(&no_params, re_query));
    assert_eq!(specialized, generic);
    // 'a' and 'ab' whole-match; 'ba' and '' do not.
    assert_eq!(specialized.len(), 2);

    // A literal IN list is not a seek constant: the bound declines, the
    // variable takes the generic path wholesale (no gates), and the
    // rows are — trivially, both paths generic — the same.
    let lit_query = "MATCH (x:P) WHERE x.s IN ['a', 'ab'] RETURN x.id AS id ORDER BY id";
    let _ = take_spec_gates();
    let specialized = rows_of(&no_params, lit_query);
    assert_eq!(take_spec_gates(), 0, "a literal list bound must decline");
    let generic = with_spec_disabled(|| rows_of(&no_params, lit_query));
    assert_eq!(specialized, generic);
    assert_eq!(specialized.len(), 2);
}

/// The In and Regex error surfaces are the generic path's: a non-list
/// `IN` bound and an invalid regex pattern raise the shared `binary`'s
/// error identically whether the gate or the residual evaluates the
/// conjunct.
#[test]
fn in_and_regex_errors_match_the_generic_path() {
    let g = Graph::default();
    g.begin_write().unwrap();
    g.create_node(
        vec!["P".into()],
        BTreeMap::from([
            ("id".into(), Value::Int(0)),
            ("s".into(), Value::String("a".into())),
        ]),
    )
    .unwrap();
    g.commit_write();
    let err_of = |params: &BTreeMap<String, Value>, query: &str| {
        error_of(try_exec_cypher_params(&g, query, params))
    };
    // A non-list IN bound: `in_op`'s type error, on both paths.
    let not_list = BTreeMap::from([("tags".to_string(), Value::Int(7))]);
    let in_query = "MATCH (x:P) WHERE x.s IN $tags RETURN x.id AS id";
    let specialized = err_of(&not_list, in_query);
    let generic = with_spec_disabled(|| err_of(&not_list, in_query));
    assert_eq!(specialized, generic);
    assert!(specialized.contains("IN requires a list"), "{specialized}");
    // An invalid pattern: the regex compile's syntax error, on both paths.
    let no_params = BTreeMap::new();
    let re_query = "MATCH (x:P) WHERE x.s =~ '([' RETURN x.id AS id";
    let specialized = err_of(&no_params, re_query);
    let generic = with_spec_disabled(|| err_of(&no_params, re_query));
    assert_eq!(specialized, generic);
    assert!(
        specialized.contains("invalid regular expression"),
        "{specialized}"
    );
}
