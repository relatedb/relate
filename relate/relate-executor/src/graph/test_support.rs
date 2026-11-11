//! Test-only graph seeding helpers shared by the graph's and the overlay's
//! test modules. The exec-level `test_support` is for running Cypher;
//! helpers that touch only `Graph` APIs live here, beside the graph's own
//! `tests.rs`.

use std::collections::BTreeMap;

use relate_core::{NodeId, RelId, Value};

use super::Graph;
use crate::adjacency::COMPACTION_DELTA_FLOOR;

/// Two committed `:N` endpoint nodes, shared by the relationship tests.
pub(crate) fn endpoints(g: &Graph) -> (NodeId, NodeId) {
    g.begin_write().unwrap();
    let a = g.create_node(vec!["N".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["N".into()], BTreeMap::new()).unwrap();
    g.commit_write();
    (a, b)
}

/// [`COMPACTION_DELTA_FLOOR`] committed `:T` rels of ascending `ts` between
/// two fresh [`endpoints`], folded by the checkpoint GC so the chunk seals.
/// Returns the endpoints, the rel ids in creation order, and `:T`'s type id.
pub(crate) fn folded_floor_chunk(g: &Graph) -> (NodeId, NodeId, Vec<RelId>, u32) {
    let (a, b) = endpoints(g);
    let mut ids = Vec::new();
    g.begin_write().unwrap();
    for i in 0..COMPACTION_DELTA_FLOOR as i64 {
        ids.push(
            g.create_rel(
                a,
                b,
                "T".into(),
                BTreeMap::from([("ts".into(), Value::Int(i))]),
            )
            .unwrap(),
        );
    }
    g.commit_write();
    let type_id = g.interner.get_id("T").unwrap();
    g.gc_versions();
    (a, b, ids, type_id)
}

/// Materialize a RANGE index over `(label, property)` on the committed
/// store — the materialized pair state a keyed seek serves (a cataloged
/// index alone is declared, not materialized). The bridge for test
/// modules outside the graph tree (the pattern-walk candidate tests,
/// LABS-1083): `materialize_range_index` itself stays `pub(super)`.
pub(crate) fn range_index(g: &Graph, label: &str, property: &str) {
    g.materialize_range_index(label, property);
}
