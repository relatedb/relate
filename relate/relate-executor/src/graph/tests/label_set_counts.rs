//! ADR-0589: the label-set count map is maintained exactly — the
//! incrementally bumped state equals the wholesale rebuild's (the abort
//! and restore path), every live node counts in exactly one set, and the
//! charge follows entries in both directions.

use super::*;

use crate::graph::test_support::labelled_node as node;

fn ids(g: &Graph, labels: &[&str]) -> Vec<u32> {
    labels.iter().map(|l| g.interner.intern(l)).collect()
}

fn set_count(g: &Graph, labels: &[&str]) -> u64 {
    let key = ids(g, labels);
    g.count_stats
        .lock()
        .unwrap()
        .label_set_counts
        .get(&key)
        .copied()
        .unwrap_or(0)
}

#[test]
fn incremental_label_set_counts_equal_the_rebuild_and_the_node_total() {
    let g = Graph::default();
    g.begin_write().unwrap();
    node(&g, &["A"]);
    let ab = node(&g, &["A", "B"]);
    let b = node(&g, &["B"]);
    node(&g, &[]);
    let gone = node(&g, &["A", "B"]);
    g.commit_write();
    g.begin_write().unwrap();
    g.add_labels(b, &["A".to_string()]);
    g.remove_labels(ab, &["A".to_string()]);
    g.add_labels(ab, &["A".to_string()]);
    g.delete_node(gone, true).unwrap();
    g.commit_write();

    assert_eq!(set_count(&g, &["A"]), 1);
    assert_eq!(set_count(&g, &["A", "B"]), 0, "AB became BA");
    assert_eq!(set_count(&g, &["B", "A"]), 2, "ab re-added A, b gained A");
    assert_eq!(set_count(&g, &["B"]), 0);
    assert_eq!(set_count(&g, &[]), 1);

    let incremental = {
        let stats = g.count_stats.lock().unwrap();
        let total: u64 = stats.label_set_counts.values().sum();
        assert_eq!(total, stats.node_total, "every live node is in one set");
        stats.clone()
    };
    let used = g.used_bytes();
    g.rebuild_adj();
    assert_eq!(*g.count_stats.lock().unwrap(), incremental);
    assert_eq!(g.used_bytes(), used, "the charge matches the retally");
}

#[test]
fn a_set_entry_charges_on_creation_and_refunds_on_drain() {
    let g = Graph::default();
    let labels = ids(&g, &["Solo", "Pair"]);
    let start = g.used_bytes();
    g.bump_node_population_stats(&labels, CountDelta::Add);
    let per_label = 2 * Graph::LABEL_COUNT_ENTRY_BYTES;
    assert_eq!(
        g.used_bytes() - start,
        per_label + Graph::label_set_entry_bytes(&labels),
        "the first node of a set creates its entry"
    );
    // A second node of the same set creates nothing: no charge growth.
    let one = g.used_bytes();
    g.bump_node_population_stats(&labels, CountDelta::Add);
    assert_eq!(g.used_bytes(), one);
    g.bump_node_population_stats(&labels, CountDelta::Remove);
    assert_eq!(g.used_bytes(), one);
    // The last node out drains the entry and refunds exactly its charge.
    g.bump_node_population_stats(&labels, CountDelta::Remove);
    assert_eq!(g.used_bytes(), start);
    assert!(g.count_stats.lock().unwrap().label_set_counts.is_empty());
}
