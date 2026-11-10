//! Test-only graph seeding helpers shared by the graph's and the overlay's
//! test modules. The exec-level `test_support` is for running Cypher;
//! helpers that touch only `Graph` APIs live here, beside the graph's own
//! `tests.rs`.

use std::collections::BTreeMap;

use relate_core::NodeId;

use super::Graph;

/// Two committed `:N` endpoint nodes, shared by the relationship tests.
pub(crate) fn endpoints(g: &Graph) -> (NodeId, NodeId) {
    g.begin_write().unwrap();
    let a = g.create_node(vec!["N".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["N".into()], BTreeMap::new()).unwrap();
    g.commit_write();
    (a, b)
}

/// Materialize a RANGE index over `(label, property)` on the committed
/// store — the materialized pair state a keyed seek serves (a cataloged
/// index alone is declared, not materialized). The bridge for test
/// modules outside the graph tree (the pattern-walk candidate tests,
/// LABS-1083): `materialize_range_index` itself stays `pub(super)`.
pub(crate) fn range_index(g: &Graph, label: &str, property: &str) {
    g.materialize_range_index(label, property);
}
