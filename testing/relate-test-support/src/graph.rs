//! Graph-content comparison and construction helpers shared by the
//! engine's codec test suites and the measurement harnesses: the exact
//! text signature every cross-codec fidelity gate asserts on, and the
//! property-map builder the fixtures build rows with.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{PropertyMap, Value};
use relate_executor::Graph;

/// A property map from `(key, value)` pairs — the one builder the codec
/// fixtures and measurement harnesses build rows with.
pub fn props(entries: Vec<(&str, Value)>) -> Arc<PropertyMap> {
    let map: BTreeMap<String, Value> = entries
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
    PropertyMap::share(map)
}

/// One property map rendered canonically — the shape both signature loops
/// spell. `PropertyMap` stores its entries in sorted order, so no sort
/// here.
fn prop_text(properties: &PropertyMap) -> Vec<String> {
    properties
        .iter()
        .map(|(k, v)| format!("{k}={v:?}"))
        .collect()
}

/// The whole graph as exact text, properties included — the comparison
/// unit for cross-codec fidelity gates. A lossy checksum (a 64-bit hash
/// over the same rows) can let a lossy codec pass; this cannot. Rows are
/// canonicalized (sorted ids, sorted labels, sorted property pairs) so
/// two codecs that only differ in row order compare equal.
pub fn signature(graph: &Graph) -> String {
    let ts = graph.commit_ts();
    let mut rows = Vec::new();
    for id in graph.all_node_ids_at(ts) {
        let n = graph.node_at(id, ts).unwrap();
        let mut labels = n.labels.as_ref().clone();
        labels.sort();
        let props = prop_text(&n.properties);
        rows.push(format!("node {id} {labels:?} {props:?}"));
    }
    for id in graph.all_rel_ids_at(ts) {
        let r = graph.rel_at(id, ts).unwrap();
        let props = prop_text(&r.properties);
        rows.push(format!(
            "rel {id} {} {}->{} {:?}",
            r.rel_type, r.start, r.end, props
        ));
    }
    rows.push(format!(
        "next {} {} ts {}",
        graph.next_node_id(),
        graph.next_rel_id(),
        ts
    ));
    rows.join("\n")
}
