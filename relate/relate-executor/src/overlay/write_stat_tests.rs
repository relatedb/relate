//! Write-stat accounting over the overlay: the net CREATE/DELETE/SET/
//! REMOVE counts for nodes, labels, and properties a statement reports.

use std::collections::BTreeMap;
use std::sync::atomic::AtomicUsize;

use super::test_support::run_write;
use crate::exec::test_support::{
    exec_write_batched, exec_write_uncommitted, write_scratch, ApplySink,
};
use crate::graph::Graph;
use relate_core::Value;

#[test]
fn create_then_delete_cancels_create_stats() {
    let g = Graph::default();
    g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    let result = exec_write_uncommitted(&g, "MATCH () CREATE (n) DELETE n");
    assert_eq!(result.stats.nodes_created, 0);
    assert_eq!(result.stats.nodes_deleted, 0);
}

#[test]
fn create_then_set_same_query_does_not_recount() {
    let result = run_write("CREATE (n:A {name: 'Andres'}) WITH n SET n.name = 'Michael' RETURN n");
    assert_eq!(result.stats.properties_removed, 0);
    assert_eq!(result.stats.properties_set, 1);
}

#[test]
fn set_overwrite_counts_property_removed() {
    let g = Graph::default();
    let mut props = BTreeMap::new();
    props.insert("name".into(), Value::String("Andres".into()));
    g.create_node(vec!["A".into()], props).unwrap();
    let result = exec_write_uncommitted(&g, "MATCH (n:A) SET n.name = 'Michael' RETURN n");
    assert_eq!(result.stats.properties_removed, 1);
    assert_eq!(result.stats.properties_set, 1);
}

#[test]
fn remove_property_counts_removed() {
    let result = run_write("CREATE (n:A {name: 'Andres'}) WITH n REMOVE n.name RETURN n");
    assert_eq!(result.stats.properties_removed, 1);
}

#[test]
fn delete_rel_counts_properties_removed() {
    let result = run_write("CREATE ()-[r:T {x: 1}]->() WITH r DELETE r");
    assert_eq!(result.stats.relationships_created, 0);
    assert_eq!(result.stats.relationships_deleted, 0);
    assert_eq!(result.stats.properties_removed, 0);
}

#[test]
fn create_same_label_counts_once() {
    let result = run_write("CREATE (:N), (:N), (:N)");
    assert_eq!(result.stats.nodes_created, 3);
    assert_eq!(result.stats.labels_added, 1);
}

#[test]
fn create_shared_label_on_snapshot_does_not_add() {
    let g = Graph::default();
    g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let r = exec_write_uncommitted(&g, "CREATE (:A)");
    assert_eq!(r.stats.nodes_created, 1);
    assert_eq!(r.stats.labels_added, 0);
    assert_eq!(r.stats.labels_removed, 0);
}

#[test]
fn delete_last_snapshot_label_then_recreate_nets() {
    let g = Graph::default();
    g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let r = exec_write_uncommitted(&g, "MATCH (n:A) DELETE n CREATE (:A)");
    assert_eq!(r.stats.nodes_created, 1);
    assert_eq!(r.stats.nodes_deleted, 1);
    assert_eq!(r.stats.labels_added, 0);
    assert_eq!(r.stats.labels_removed, 0);
}

#[test]
fn remove_shared_label_does_not_count() {
    let g = Graph::default();
    g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let r = exec_write_uncommitted(&g, "MATCH (n:A) WITH n LIMIT 1 REMOVE n:A");
    assert_eq!(r.stats.labels_added, 0);
    assert_eq!(r.stats.labels_removed, 0);
}

#[test]
fn remove_last_snapshot_label_counts() {
    let g = Graph::default();
    g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let r = exec_write_uncommitted(&g, "MATCH (n:A) REMOVE n:A");
    assert_eq!(r.stats.labels_added, 0);
    assert_eq!(r.stats.labels_removed, 1);
}

#[test]
fn add_already_live_label_does_not_count() {
    let g = Graph::default();
    g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let r = exec_write_uncommitted(&g, "MATCH (n:B) SET n:A");
    assert_eq!(r.stats.labels_added, 0);
    assert_eq!(r.stats.labels_removed, 0);
}

/// CIP-162 overwrite: `SET labels(n) = …` makes the node's labels exactly
/// the listed set — the omitted label leaves, the new one arrives — via
/// the one `set_labels` compose of the add/remove primitives.
#[test]
fn set_labels_overwrite_makes_the_label_list_exact() {
    let g = Graph::default();
    g.create_node(vec!["A".into(), "B".into()], BTreeMap::new())
        .unwrap();
    let r = exec_write_uncommitted(
        &g,
        "MATCH (n:B) SET labels(n) = ['B', 'C'] RETURN labels(n) AS ls",
    );
    assert_eq!(
        r.rows,
        vec![vec![Value::List(
            vec![Value::String("B".into()), Value::String("C".into())].into()
        )]]
    );
    assert_eq!(r.stats.labels_added, 1);
    assert_eq!(r.stats.labels_removed, 1);
}

/// REL8-1188, the batched twin: a `CALL { … } IN TRANSACTIONS` batch that
/// deletes a node an earlier batch of the same statement already
/// committed counts the delete — and the create stays counted too (each
/// batch is a real commit, ADR-0210), so both counters report and
/// neither nets negative across the batch rebase.
#[test]
fn batched_delete_of_node_committed_by_earlier_batch_counts_both() {
    let g = Graph::default();
    let scratch = write_scratch(&g);
    let sink = ApplySink {
        graph: &g,
        calls: AtomicUsize::new(0),
    };
    let result = exec_write_batched(
        &g,
        &scratch,
        "UNWIND [1, 2] AS i \
         CALL { WITH i OPTIONAL MATCH (n:N) WHERE n.i < i DELETE n CREATE (:N {i: i}) } \
         IN TRANSACTIONS OF 1 ROWS",
        &sink,
    )
    .unwrap();
    assert_eq!(result.stats.nodes_created, 2);
    assert_eq!(result.stats.nodes_deleted, 1);
    assert_eq!(g.live_node_count(), 1);
}
