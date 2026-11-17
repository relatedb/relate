//! ADR-0611: the index-backed aggregates equal the scan's answer under the
//! writes that leave stale postings (overwrite, delete, label removal), the
//! views that must decline do, and the recognizer admits exactly its shapes.

use std::collections::BTreeMap;

use super::index_aggregates::recognize;
use super::stats_short_circuit::stats_short_circuit;
use super::test_support::{
    clauses_of, commit_scratch, exec_autocommit, exec_cypher_on, exec_write, sort_rows,
    with_n_stage, write_scratch,
};
use crate::ctx::ExecCtx;
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use crate::row::Row;
use relate_core::Value;

/// The shortcut ladder's answer, or `None` when it declines to the scan.
fn index_answer(view: &ExecGraph<'_>, cypher: &str) -> Option<Vec<Vec<Value>>> {
    let params = BTreeMap::new();
    let ctx = ExecCtx::new(view, &params);
    stats_short_circuit(ctx, &clauses_of(cypher), &[Row::new()], None)
        .unwrap()
        .map(|(_, result)| result.rows)
}

/// The scan's answer: a `WITH n` stage makes the statement a shape no
/// shortcut recognizer takes.
fn scan_answer(view: &ExecGraph<'_>, cypher: &str) -> Vec<Vec<Value>> {
    let scan = with_n_stage(cypher);
    assert!(
        index_answer(view, &scan).is_none(),
        "the oracle must be the scan: {scan}"
    );
    exec_cypher_on(view, &scan).rows
}

fn sorted(mut rows: Vec<Vec<Value>>) -> Vec<Vec<Value>> {
    sort_rows(&mut rows);
    rows
}

const GROUPED: &[&str] = &[
    "MATCH (n:P) RETURN n.age AS age, count(*) AS c",
    "MATCH (n:P) RETURN count(n) AS c, n.age",
];
/// The grouped shapes that carry an ORDER BY: their row order is the
/// statement's, so the oracle comparison is exact, never bag-sorted.
const GROUPED_ORDERED: &[&str] = &[
    "MATCH (n:P) RETURN n.age AS age, count(*) AS c ORDER BY c DESC, age",
    "MATCH (n:P) RETURN n.age AS age, count(*) AS c ORDER BY age SKIP 1 LIMIT 2",
];
const FOLDS: &[&str] = &[
    "MATCH (n:P) RETURN min(n.age) AS lo, max(n.age) AS hi",
    "MATCH (n:P) RETURN count(n.age) AS c",
    "MATCH (n:P) RETURN max(n.age), min(n.age), count(n.age)",
];

/// Every recognized shape answers from the postings and equals the scan:
/// the ordered ones row for row, the unordered ones as bags.
fn assert_matches_scan(g: &Graph, key: &str, when: &str) {
    let view = ExecGraph::read(g);
    for q in GROUPED.iter().chain(FOLDS) {
        let q = q.replace("age", key);
        let got = index_answer(&view, &q)
            .unwrap_or_else(|| panic!("{when}: the postings must answer: {q}"));
        assert_eq!(sorted(got), sorted(scan_answer(&view, &q)), "{when}: {q}");
    }
    // The ordered shapes keep the statement's row order: exact, never
    // bag-sorted.
    for q in GROUPED_ORDERED {
        let q = q.replace("age", key);
        let got = index_answer(&view, &q)
            .unwrap_or_else(|| panic!("{when}: the postings must answer: {q}"));
        assert_eq!(got, scan_answer(&view, &q), "{when}: {q}");
    }
}

/// The fixture with a declared range index on `(P, age)`.
fn setup_indexed() -> Graph {
    let g = Graph::default();
    exec_autocommit(&g, "CREATE RANGE INDEX p_age FOR (n:P) ON (n.age)").unwrap();
    setup_data(&g);
    g
}

/// The same fixture with no index of any kind.
fn setup_unindexed() -> Graph {
    let g = Graph::default();
    setup_data(&g);
    g
}

fn setup_data(g: &Graph) {
    exec_autocommit(
        g,
        "CREATE (:P {age: 3, name: 'a'}), (:P {age: 3}), (:P {age: 5}), (:P {age: 9}), \
         (:P {name: 'null'}), (:Q {age: 1}), (:Q {age: 99})",
    )
    .unwrap();
}

#[test]
fn a_range_index_answers_every_shape_across_writes() {
    let g = setup_indexed();
    assert_matches_scan(&g, "age", "after create");

    // Overwrites leave the old value posted until collection: the extremes
    // step past the stale ends, the counts skip the stale postings.
    exec_autocommit(&g, "MATCH (n:P {age: 9}) SET n.age = 4").unwrap();
    exec_autocommit(&g, "MATCH (n:P {age: 3, name: 'a'}) SET n.age = 100").unwrap();
    assert_matches_scan(&g, "age", "after overwrites");

    exec_autocommit(&g, "MATCH (n:P {age: 100}) DELETE n").unwrap();
    assert_matches_scan(&g, "age", "after delete");

    exec_autocommit(&g, "MATCH (n:P {age: 5}) REMOVE n:P").unwrap();
    exec_autocommit(&g, "MATCH (n:Q {age: 1}) SET n:P").unwrap();
    assert_matches_scan(&g, "age", "after label moves");

    exec_autocommit(&g, "MATCH (n:P) SET n.age = null").unwrap();
    let view = ExecGraph::read(&g);
    assert_eq!(
        index_answer(&view, FOLDS[0]).unwrap(),
        vec![vec![Value::Null, Value::Null]]
    );
}

#[test]
fn string_extremes_equal_the_scan() {
    let g = Graph::default();
    exec_autocommit(&g, "CREATE RANGE INDEX p_name FOR (n:P) ON (n.name)").unwrap();
    exec_autocommit(
        &g,
        "CREATE (:P {name: 'b'}), (:P {name: 'a'}), (:P {name: 'c'})",
    )
    .unwrap();
    exec_autocommit(&g, "MATCH (n:P {name: 'c'}) SET n.name = 'aa'").unwrap();
    assert_matches_scan(&g, "name", "string keys");
}

#[test]
fn mixed_types_and_floats_decline_the_extremes() {
    let g = setup_indexed();
    exec_autocommit(&g, "CREATE (:P {age: 'old'})").unwrap();
    let view = ExecGraph::read(&g);
    assert!(
        index_answer(&view, FOLDS[0]).is_none(),
        "Int and String keys"
    );
    let g = setup_indexed();
    exec_autocommit(&g, "MATCH (n:P {age: 9}) SET n.age = 9.5").unwrap();
    let view = ExecGraph::read(&g);
    assert!(
        index_answer(&view, FOLDS[0]).is_none(),
        "Int and Float keys"
    );
    let g = setup_indexed();
    exec_autocommit(&g, "MATCH (n:P {age: 3, name: 'a'}) SET n.age = [1]").unwrap();
    let view = ExecGraph::read(&g);
    assert!(
        index_answer(&view, FOLDS[0]).is_none(),
        "a value off the order"
    );
}

/// A NaN is posted nowhere, so a walk of the postings would miss its node:
/// every shape either declines or equals the scan.
#[test]
fn a_nan_value_never_makes_a_shape_disagree_with_the_scan() {
    let g = setup_indexed();
    exec_autocommit(&g, "CREATE (:P {age: 0.0 / 0.0})").unwrap();
    let view = ExecGraph::read(&g);
    // A NaN never makes a shape *disagree*: whichever shapes still answer
    // (the lazy-pair accounting changed with main's equality-MATCH work)
    // must equal the scan — bags for the order-free shapes, the
    // statement's row order for the ordered ones.
    for q in GROUPED.iter().chain(FOLDS) {
        if let Some(got) = index_answer(&view, q) {
            assert_eq!(sorted(got), sorted(scan_answer(&view, q)), "{q}");
        }
    }
    for q in GROUPED_ORDERED {
        if let Some(got) = index_answer(&view, q) {
            assert_eq!(got, scan_answer(&view, q), "{q}");
        }
    }
}

/// The equality pair drops a NaN-bearing value too (alone or in a list): a
/// built pair that has seen one declines instead of undercounting.
#[test]
fn a_nan_value_in_a_built_equality_pair_declines() {
    for nan in ["0.0 / 0.0", "[1, 0.0 / 0.0]"] {
        let g = setup_unindexed();
        exec_autocommit(&g, &format!("CREATE (:P {{age: {nan}}})")).unwrap();
        g.equality_postings("P", "age", &Value::Int(3));
        let view = ExecGraph::read(&g);
        // The pair cannot post the NaN: whichever shapes still answer
        // must equal the scan, and none may disagree.
        for q in GROUPED.iter().chain(FOLDS) {
            if let Some(got) = index_answer(&view, q) {
                assert_eq!(sorted(got), sorted(scan_answer(&view, q)), "{nan}: {q}");
            }
        }
        for q in GROUPED_ORDERED {
            if let Some(got) = index_answer(&view, q) {
                assert_eq!(got, scan_answer(&view, q), "{nan}: {q}");
            }
        }
    }
}

#[test]
fn counts_use_only_a_pair_that_already_exists() {
    let g = setup_unindexed();
    let view = ExecGraph::read(&g);
    // No declared index and no equality pair built: no answer, and asking
    // never builds one.
    assert!(index_answer(&view, GROUPED[0]).is_none());
    assert!(index_answer(&view, GROUPED[0]).is_none());
    assert!(
        index_answer(&view, FOLDS[0]).is_none(),
        "extremes need a range index"
    );
    // Something else builds the equality pair (a uniqueness probe, say);
    // the counts then ride it.
    drop(view);
    g.equality_postings("P", "age", &Value::Int(3));
    exec_autocommit(&g, "MATCH (n:P {age: 9}) SET n.age = 4").unwrap();
    let view = ExecGraph::read(&g);
    for q in [GROUPED[0], FOLDS[1]] {
        let got = index_answer(&view, q);
        assert_eq!(
            sorted(got.expect("the built pair answers")),
            sorted(scan_answer(&view, q))
        );
    }
}

#[test]
fn views_that_are_not_the_latest_state_decline() {
    let g = setup_indexed();
    let old = g.commit_ts();
    exec_autocommit(&g, "CREATE (:P {age: 7})").unwrap();
    let stale = ExecGraph::read_at(&g, old);
    for q in GROUPED.iter().chain(GROUPED_ORDERED).chain(FOLDS) {
        assert!(
            index_answer(&stale, q).is_none(),
            "pinned older snapshot: {q}"
        );
    }
    // The statement still answers correctly on that view, by scanning.
    assert_eq!(
        exec_cypher_on(&stale, "MATCH (n:P) RETURN count(n.age) AS c").rows,
        vec![vec![Value::Int(4)]]
    );
    // A seeded stage (not the one unbound row) declines.
    let view = ExecGraph::read(&g);
    let params = BTreeMap::new();
    let ctx = ExecCtx::new(&view, &params);
    let two = [Row::new(), Row::new()];
    assert!(stats_short_circuit(ctx, &clauses_of(FOLDS[0]), &two, None)
        .unwrap()
        .is_none());
}

#[test]
fn an_explicit_transaction_declines_and_reads_its_own_writes() {
    let g = setup_indexed();
    let scratch = write_scratch(&g);
    exec_write(&g, &scratch, "MATCH (n:P {age: 9}) SET n.age = 4").unwrap();
    exec_write(&g, &scratch, "CREATE (:P {age: 50})").unwrap();
    {
        let view = ExecGraph::write(&g, &scratch);
        for q in GROUPED.iter().chain(GROUPED_ORDERED).chain(FOLDS) {
            assert!(index_answer(&view, q).is_none(), "in a transaction: {q}");
        }
        // The scan sees the transaction's own writes.
        assert_eq!(
            exec_cypher_on(&view, FOLDS[0]).rows,
            vec![vec![Value::Int(3), Value::Int(50)]]
        );
    }
    commit_scratch(&g, scratch);
    assert_matches_scan(&g, "age", "after the transaction commits");
}

#[test]
fn the_recognizer_admits_its_shapes_and_declines_near_misses() {
    let admitted = [
        "MATCH (n:P) RETURN n.age, count(*)",
        "MATCH (n:P) RETURN count(*) AS c, n.age AS a",
        "MATCH (n:P) RETURN n.age, count(n) ORDER BY n.age",
        "MATCH (n:P) RETURN min(n.age)",
        "MATCH (n:P) RETURN max(n.age)",
        "MATCH (n:P) RETURN count(n.age)",
        "MATCH (n:P) RETURN min(n.age), max(n.age), count(n.age)",
    ];
    for q in admitted {
        assert!(recognize(&clauses_of(q)).is_some(), "must admit: {q}");
    }
    let declined = [
        "MATCH (n) RETURN n.age, count(*)",
        "MATCH (n) RETURN min(n.age)",
        "MATCH (n:P:Q) RETURN n.age, count(*)",
        "MATCH (n:P) RETURN avg(n.age)",
        "MATCH (n:P) RETURN sum(n.age)",
        "MATCH (n:P) WHERE n.age > 1 RETURN n.age, count(*)",
        "MATCH (n:P {name: 'a'}) RETURN min(n.age)",
        "MATCH (n:P) RETURN DISTINCT n.age, count(*)",
        "MATCH (n:P) RETURN count(DISTINCT n.age)",
        "MATCH (n:P) RETURN min(n.age), max(n.score)",
        "MATCH (n:P) RETURN min(n.age) ORDER BY min(n.age)",
        "MATCH (n:P) RETURN min(n.age) LIMIT 1",
        "MATCH (n:P) RETURN n.age, n.name, count(*)",
        "MATCH (n:P) RETURN n.age",
        "MATCH (n:P) RETURN n.age + 1, count(*)",
        "MATCH (n:P)-[:R]->() RETURN n.age, count(*)",
        "MATCH (n:P) RETURN n.age AS n, count(*)",
        "OPTIONAL MATCH (n:P) RETURN n.age, count(*)",
    ];
    for q in declined {
        assert!(recognize(&clauses_of(q)).is_none(), "must decline: {q}");
    }
}
