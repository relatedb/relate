//! `data.keyStatistics` of `db.stats.retrieve('GRAPH COUNTS')` (ADR-0696):
//! the published per-key statistics, listed by a read that never builds.

use std::collections::BTreeMap;

use relate_core::Value;

use crate::exec::test_support::{column, exec_cypher, list_cell};
use crate::graph::Graph;

/// 200 `:City` nodes (`country` takes four values, `pop` is unique) and ten
/// `:Rare` nodes nobody will consult.
fn graph() -> Graph {
    let g = Graph::default();
    g.begin_write().unwrap();
    for i in 0..200i64 {
        g.create_node(
            vec!["City".into()],
            BTreeMap::from([
                (
                    "country".to_string(),
                    Value::String(format!("C{}", i % 4).into()),
                ),
                ("pop".to_string(), Value::Int(i)),
            ]),
        )
        .unwrap();
    }
    for i in 0..10i64 {
        g.create_node(
            vec!["Rare".into()],
            BTreeMap::from([("k".to_string(), Value::Int(i))]),
        )
        .unwrap();
    }
    g.commit_write();
    g
}

/// The entries of `data.keyStatistics`, each as its map.
fn entries(g: &Graph) -> Vec<BTreeMap<String, Value>> {
    let result = exec_cypher(
        g,
        "CALL db.stats.retrieve('GRAPH COUNTS') YIELD data \
         RETURN data.keyStatistics AS k",
    );
    let Value::List(list) = column(&result, "k", list_cell).remove(0) else {
        panic!("keyStatistics is not a list");
    };
    list.iter()
        .map(|v| match v {
            Value::Map(m) => (**m).clone(),
            other => panic!("entry is not a map: {other:?}"),
        })
        .collect()
}

fn text(s: &str) -> Value {
    Value::String(s.into())
}

#[test]
fn nothing_is_listed_before_any_statistics_are_built() {
    assert!(entries(&graph()).is_empty());
}

/// A read never builds and never asks for a build: a label a consult
/// requested but nothing has served is absent, the request stays pending,
/// and the statistics epoch does not move.
#[test]
fn reading_the_counts_never_requests_or_builds() {
    let g = graph();
    assert!(
        g.key_stats_named("City", "pop").is_none(),
        "a consult requests"
    );
    let epoch = g.key_stats_epoch();

    assert!(entries(&g).is_empty(), "requested is not built");
    assert!(
        g.has_pending_key_stats(),
        "the read did not serve the request"
    );
    assert_eq!(g.key_stats_epoch(), epoch, "and changed nothing");

    // A label no consult ever named is never listed or requested by a read.
    let before = entries(&g).len();
    assert_eq!(before, 0);
    assert!(
        g.key_stats_named("Rare", "k").is_none(),
        "only now requested"
    );
}

#[test]
fn built_labels_list_their_keys_in_label_property_order_with_every_figure() {
    let g = graph();
    assert!(g.key_stats_named("City", "pop").is_none());
    assert_eq!(g.build_pending_key_stats(1, || true), 1);
    let epoch = g.key_stats_epoch();

    let listed = entries(&g);
    assert_eq!(g.key_stats_epoch(), epoch, "a read moves no epoch");
    assert!(!g.has_pending_key_stats(), "and requests nothing");
    let names: Vec<_> = listed
        .iter()
        .map(|e| (e["label"].clone(), e["property"].clone()))
        .collect();
    assert_eq!(
        names,
        [(text("City"), text("country")), (text("City"), text("pop"))],
        "sorted by label then property; the unconsulted :Rare is absent"
    );

    let pop = &listed[1];
    let mut keys: Vec<&str> = pop.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "count",
            "declined",
            "distinct",
            "heavy",
            "label",
            "max",
            "mean",
            "min",
            "property",
            "provenance",
            "sum"
        ]
    );
    assert_eq!(pop["provenance"], text("sketched"));
    assert_eq!(pop["count"], Value::Int(200));
    assert_eq!(pop["declined"], Value::Int(0));
    let Value::Int(distinct) = pop["distinct"] else {
        panic!("distinct is an integer")
    };
    assert!(
        (190..=210).contains(&distinct),
        "200 unique values: {distinct}"
    );
    assert_eq!(pop["min"], Value::Int(0));
    assert_eq!(pop["max"], Value::Int(199));
    assert_eq!(pop["sum"], Value::Int(19_900));
    assert_eq!(pop["mean"], Value::Float(99.5));
    assert_eq!(
        pop["heavy"],
        Value::List(Vec::new().into()),
        "uniform: none"
    );

    // A string key: four values, a quarter of the rows each; no range, no sum.
    let country = &listed[0];
    assert_eq!(country["distinct"], Value::Int(4));
    for figure in ["min", "max", "sum", "mean"] {
        assert_eq!(country[figure], Value::Null, "{figure}");
    }
    let Value::List(heavy) = &country["heavy"] else {
        panic!("heavy is a list")
    };
    assert_eq!(heavy.len(), 4, "every value is a quarter of the rows");
    for h in heavy.iter() {
        let Value::Map(h) = h else {
            panic!("heavy entry")
        };
        assert_eq!(h["count"], Value::Int(50));
    }
}

/// Writes past the stale threshold age a label's statistics out, so a read
/// stops listing them: only BUILT statistics are shown.
#[test]
fn a_stale_label_is_no_longer_listed() {
    let g = graph();
    assert!(g.key_stats_named("City", "pop").is_none());
    assert_eq!(g.build_pending_key_stats(1, || true), 1);
    assert_eq!(entries(&g).len(), 2);

    g.begin_write().unwrap();
    for i in 0..200i64 {
        g.create_node(
            vec!["City".into()],
            BTreeMap::from([("pop".to_string(), Value::Int(1_000 + i))]),
        )
        .unwrap();
    }
    g.commit_write();
    assert!(entries(&g).is_empty(), "stale statistics are not served");
}
