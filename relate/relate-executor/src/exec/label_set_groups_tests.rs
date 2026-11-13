//! ADR-0589: the grouped label aggregates' answer from the label-set
//! counts — every recognized shape equals the scan's answer across the
//! write paths that move label sets (create, delete, label add and
//! remove, abort), the views that must decline do, and concurrent readers
//! never see a torn state.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::label_set_groups::label_set_groups_result;
use super::test_support::{
    clauses_of, commit_scratch, exec_autocommit, exec_cypher, exec_cypher_on, exec_write,
    try_exec_cypher_on, write_scratch,
};
use crate::ctx::ExecCtx;
use crate::graph::test_support::labelled_node as node;
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use relate_core::Value;

/// The label-set answer, or `None` when the statement declines to the scan.
fn census_answer(view: &ExecGraph<'_>, cypher: &str) -> Option<Vec<Vec<Value>>> {
    let params = BTreeMap::new();
    let ctx = ExecCtx::new(view, &params);
    label_set_groups_result(ctx, &clauses_of(cypher), None)
        .unwrap()
        .map(|(_, result)| result.rows)
}

/// The scan's answer to the same grouping: an extra `WITH n` stage makes
/// the statement a shape the recognizer never takes.
fn scan_answer(view: &ExecGraph<'_>, cypher: &str) -> Vec<Vec<Value>> {
    let scan = cypher.replacen("MATCH (n) RETURN", "MATCH (n) WITH n RETURN", 1);
    assert_ne!(scan, cypher, "the oracle rewrite must apply: {cypher}");
    assert!(
        census_answer(view, &scan).is_none(),
        "the oracle must be the scan: {scan}"
    );
    exec_cypher_on(view, &scan).rows
}

fn sorted(mut rows: Vec<Vec<Value>>) -> Vec<Vec<Value>> {
    rows.sort_by_key(|r| format!("{r:?}"));
    rows
}

/// The recognized shapes. Ordered ones compare exactly, the unordered
/// ones as bags (row order without ORDER BY is unspecified).
const ORDERED: &[&str] = &[
    "MATCH (n) RETURN labels(n)[0] AS kind, count(*) AS c ORDER BY kind",
    "MATCH (n) RETURN labels(n)[0] AS kind, count(n) AS c ORDER BY kind DESC",
    "MATCH (n) RETURN labels(n) AS ls, count(*) AS c ORDER BY ls",
    "MATCH (n) RETURN size(labels(n)) AS k, count(*) AS c ORDER BY k",
    "MATCH (n) RETURN n:B AS isb, count(*) AS c ORDER BY isb",
    "MATCH (n) RETURN labels(n)[1] AS second, count(*) AS c ORDER BY c DESC, second",
    "MATCH (n) RETURN labels(n)[0] AS kind, count(*) AS c ORDER BY count(*) DESC, labels(n)[0]",
    "MATCH (n) RETURN labels(n)[0] AS kind, count(*) AS c ORDER BY kind SKIP 1 LIMIT 2",
    "MATCH (n) RETURN count(*) AS c, labels(n)[0] AS kind, size(labels(n)) AS k ORDER BY kind, k",
];
const UNORDERED: &[&str] = &[
    "MATCH (n) RETURN labels(n)[0] AS kind, count(*) AS c",
    "MATCH (n) RETURN labels(n) AS ls, count(*)",
];

/// Every recognized shape answers from the label sets and equals the scan.
fn assert_matches_scan(g: &Graph, when: &str) {
    let view = ExecGraph::read(g);
    for q in ORDERED {
        let got = census_answer(&view, q)
            .unwrap_or_else(|| panic!("{when}: the label sets must answer: {q}"));
        assert_eq!(got, scan_answer(&view, q), "{when}: {q}");
    }
    for q in UNORDERED {
        let got = census_answer(&view, q)
            .unwrap_or_else(|| panic!("{when}: the label sets must answer: {q}"));
        assert_eq!(sorted(got), sorted(scan_answer(&view, q)), "{when}: {q}");
    }
}

#[test]
fn an_empty_graph_answers_no_groups() {
    let g = Graph::default();
    assert_matches_scan(&g, "empty");
    let view = ExecGraph::read(&g);
    assert_eq!(census_answer(&view, ORDERED[0]).unwrap().len(), 0);
}

#[test]
fn grouped_label_aggregates_follow_every_label_set_mutation() {
    let g = Graph::default();
    g.begin_write().unwrap();
    node(&g, &["A"]);
    let ab = node(&g, &["A", "B"]);
    let bc = node(&g, &["B", "C"]);
    node(&g, &[]);
    let lone_b = node(&g, &["B"]);
    node(&g, &["B"]);
    let doomed = node(&g, &["A", "C"]);
    g.commit_write();
    assert_matches_scan(&g, "after create");

    // Label add moves the node between sets; REMOVE then ADD changes the
    // label ORDER (labels(n)[0] differs) with the same label set.
    g.begin_write().unwrap();
    g.add_labels(lone_b, &["Z".to_string()]);
    g.remove_labels(bc, &["B".to_string()]);
    g.commit_write();
    assert_matches_scan(&g, "after label add and remove");
    g.begin_write().unwrap();
    g.add_labels(bc, &["B".to_string()]);
    g.commit_write();
    assert_matches_scan(&g, "after the order-changing re-add");

    // A node losing every label becomes the unlabeled set; a request that
    // changes nothing leaves the counts alone.
    g.begin_write().unwrap();
    g.remove_labels(ab, &["A".to_string(), "B".to_string()]);
    g.add_labels(lone_b, &["Z".to_string()]);
    g.remove_labels(lone_b, &["Nope".to_string()]);
    g.commit_write();
    assert_matches_scan(&g, "after emptying a node");

    g.begin_write().unwrap();
    g.delete_node(doomed, true).unwrap();
    g.commit_write();
    assert_matches_scan(&g, "after delete");

    // Cypher-level mutations ride the same seams.
    exec_autocommit(&g, "MATCH (n:C) SET n:Extra").unwrap();
    exec_autocommit(&g, "MATCH (n:Z) REMOVE n:Z").unwrap();
    exec_autocommit(&g, "CREATE (:A:B:C), (:Q)").unwrap();
    assert_matches_scan(&g, "after cypher writes");
}

#[test]
fn an_aborted_write_leaves_the_label_sets_untouched() {
    let g = Graph::default();
    g.begin_write().unwrap();
    node(&g, &["A"]);
    let b = node(&g, &["B"]);
    g.commit_write();
    g.begin_write().unwrap();
    node(&g, &["A", "B"]);
    g.add_labels(b, &["A".to_string()]);
    g.abort_write();
    assert_matches_scan(&g, "after abort");
    let view = ExecGraph::read(&g);
    let total: i64 = census_answer(&view, ORDERED[0])
        .unwrap()
        .iter()
        .map(|r| match r[1] {
            Value::Int(c) => c,
            ref other => panic!("count column: {other:?}"),
        })
        .sum();
    assert_eq!(total, 2, "the aborted create must not be counted");
}

/// The views the label sets cannot describe decline, and the scan answers
/// for them: an in-flight transaction reads its own writes, a pinned older
/// snapshot reads its own state.
#[test]
fn views_that_are_not_the_latest_commit_decline_to_the_scan() {
    let g = Graph::default();
    g.begin_write().unwrap();
    node(&g, &["A"]);
    g.commit_write();
    let q = ORDERED[0];

    let scratch = write_scratch(&g);
    exec_write(&g, &scratch, "CREATE (:Q)").unwrap();
    let view = ExecGraph::write(&g, &scratch);
    assert!(census_answer(&view, q).is_none(), "in-tx must decline");
    let in_tx = exec_cypher_on(&view, q).rows;
    assert_eq!(
        in_tx,
        vec![
            vec![Value::String("A".into()), Value::Int(1)],
            vec![Value::String("Q".into()), Value::Int(1)],
        ],
        "the scan reads the transaction's own write"
    );
    commit_scratch(&g, scratch);
    assert_matches_scan(&g, "after the commit");

    let stale = ExecGraph::read_at(&g, 0);
    assert!(census_answer(&stale, q).is_none(), "pinned must decline");
    assert!(
        exec_cypher_on(&stale, q).rows.is_empty(),
        "nothing is visible at ts 0"
    );
}

/// Shapes outside the admitted grammar decline: the label sets answer
/// only keys that read nothing but the label list.
#[test]
fn shapes_the_label_sets_cannot_answer_decline() {
    let g = Graph::default();
    g.begin_write().unwrap();
    node(&g, &["A"]);
    g.commit_write();
    let view = ExecGraph::read(&g);
    for q in [
        // a property key, an id key, a WHERE, a label, DISTINCT
        "MATCH (n) RETURN n.name AS k, count(*) AS c",
        "MATCH (n) RETURN id(n) AS k, count(*) AS c",
        "MATCH (n) WHERE n.x > 1 RETURN labels(n)[0] AS k, count(*) AS c",
        "MATCH (n:A) RETURN labels(n)[0] AS k, count(*) AS c",
        "MATCH (n) RETURN DISTINCT labels(n)[0] AS k, count(*) AS c",
        // a non-literal subscript, a slice, another function over labels
        "MATCH (n) RETURN labels(n)[size(labels(n)) - 1] AS k, count(*) AS c",
        "MATCH (n) RETURN labels(n)[0..1] AS k, count(*) AS c",
        "MATCH (n) RETURN toLower(labels(n)[0]) AS k, count(*) AS c",
        // two counts, a non-count aggregate, no key, no count
        "MATCH (n) RETURN labels(n)[0] AS k, count(*) AS c, count(n) AS d",
        "MATCH (n) RETURN labels(n)[0] AS k, collect(n) AS c",
        "MATCH (n) RETURN count(*) AS c",
        "MATCH (n) RETURN labels(n)[0] AS k",
        // count of something other than the node, a distinct count
        "MATCH (n) RETURN labels(n)[0] AS k, count(n.x) AS c",
        "MATCH (n) RETURN labels(n)[0] AS k, count(DISTINCT n) AS c",
        // an order key the grouped rows cannot evaluate
        "MATCH (n) RETURN labels(n)[0] AS k, count(*) AS c ORDER BY toLower(k)",
        // a relationship pattern, an anonymous node, an OPTIONAL match
        "MATCH (n)-[r]->() RETURN labels(n)[0] AS k, count(*) AS c",
        "MATCH () RETURN count(*) AS c",
        "OPTIONAL MATCH (n) RETURN labels(n)[0] AS k, count(*) AS c",
    ] {
        assert!(census_answer(&view, q).is_none(), "must decline: {q}");
    }
}

/// An out-of-range subscript is null in both paths: the key is the
/// ordinary evaluator's, not a second implementation.
#[test]
fn key_semantics_are_the_evaluators() {
    let g = Graph::default();
    g.begin_write().unwrap();
    node(&g, &["A"]);
    node(&g, &[]);
    g.commit_write();
    let view = ExecGraph::read(&g);
    let q = "MATCH (n) RETURN labels(n)[7] AS k, count(*) AS c";
    let got = census_answer(&view, q).unwrap();
    assert_eq!(got, vec![vec![Value::Null, Value::Int(2)]]);
    assert_eq!(got, scan_answer(&view, q));
}

/// Concurrent readers against a committing writer: the writer creates an
/// `:X` and a `:Y` in each transaction, so every answer the readers see,
/// from the label sets or from the scan they decline to, has equal counts.
#[test]
fn concurrent_readers_never_see_a_torn_label_set_state() {
    let g = Arc::new(Graph::default());
    let q = "MATCH (n) RETURN labels(n)[0] AS kind, count(*) AS c ORDER BY kind";
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        let readers: Vec<_> = (0..3)
            .map(|_| {
                s.spawn(|| {
                    while !stop.load(Ordering::Acquire) {
                        let rows = census_answer(&ExecGraph::read(&g), q)
                            .unwrap_or_else(|| exec_cypher(&g, q).rows);
                        let counts: Vec<&Value> = rows.iter().map(|r| &r[1]).collect();
                        assert!(
                            counts.len() <= 2 && counts.windows(2).all(|w| w[0] == w[1]),
                            "torn state: {rows:?}"
                        );
                    }
                })
            })
            .collect();
        for _ in 0..200 {
            g.begin_write().unwrap();
            node(&g, &["X"]);
            node(&g, &["Y"]);
            g.commit_write();
            std::thread::yield_now();
        }
        stop.store(true, Ordering::Release);
        for r in readers {
            r.join().unwrap();
        }
    });
    assert_matches_scan(&g, "after the writers");
}

fn is_expression_capacity(error: &relate_core::Error) -> bool {
    matches!(
        error,
        relate_core::Error::Capacity {
            kind: relate_core::CapacityKind::Expression,
            ..
        }
    )
}

/// 400 nodes, each in its own `Label{i}` set plus the shared label, under
/// the given expression-memory limit.
fn many_sets_graph(limit: Option<u64>) -> Graph {
    let mut g = Graph::default();
    g.configure_expression_memory_limit(limit);
    g.begin_write().unwrap();
    for i in 0..400 {
        node(&g, &[&format!("Label{i}"), "Shared"]);
    }
    g.commit_write();
    g
}

/// ADR-0463: the grouped answer's group table (and its copy of the set
/// map) is charged to the expression budget, so a paged statement over
/// many label sets refuses at the cap exactly as the scan's aggregation
/// does, instead of materializing the table unmetered.
#[test]
fn a_paged_grouped_answer_over_many_label_sets_hits_the_expression_budget() {
    let g = many_sets_graph(Some(30_000));
    let q = "MATCH (n) RETURN labels(n) AS ls, count(*) AS c ORDER BY ls SKIP 5 LIMIT 1";
    let view = ExecGraph::read(&g);
    let params = BTreeMap::new();
    let ctx = ExecCtx::new(&view, &params);
    let error = match label_set_groups_result(ctx, &clauses_of(q), None) {
        Err(error) => error,
        Ok(answer) => panic!(
            "the label-set answer must refuse at the cap: answered={}",
            answer.is_some()
        ),
    };
    assert!(is_expression_capacity(&error), "{error}");
    let scan = q.replacen("MATCH (n) RETURN", "MATCH (n) WITH n RETURN", 1);
    let scan_error = try_exec_cypher_on(&view, &scan)
        .err()
        .expect("the scan's aggregation refuses at the same cap");
    assert!(is_expression_capacity(&scan_error), "{scan_error}");
    // Under the default budget the same statement answers and equals the scan.
    let ok = many_sets_graph(None);
    let view = ExecGraph::read(&ok);
    assert_eq!(census_answer(&view, q).unwrap(), scan_answer(&view, q));
}

/// A graph whose label sets approach its node count has no speed to gain
/// from the map: above the floor and below the sharing minimum the
/// statement declines to the scan; a graph that shares sets keeps the map.
#[test]
fn a_graph_of_all_distinct_label_sets_declines_to_the_scan() {
    let q = ORDERED[0];
    let distinct = Graph::default();
    distinct.begin_write().unwrap();
    for i in 0..4200 {
        node(&distinct, &[&format!("U{i}")]);
    }
    distinct.commit_write();
    assert!(census_answer(&ExecGraph::read(&distinct), q).is_none());

    let shared = Graph::default();
    shared.begin_write().unwrap();
    for i in 0..4200 {
        node(&shared, &[&format!("S{}", i % 3)]);
    }
    shared.commit_write();
    assert!(census_answer(&ExecGraph::read(&shared), q).is_some());
}
