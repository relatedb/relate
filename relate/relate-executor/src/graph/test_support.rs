//! Test-only graph seeding helpers shared by the graph's and the overlay's
//! test modules. The exec-level `test_support` is for running Cypher;
//! helpers that touch only `Graph` APIs live here, beside the graph's own
//! `tests.rs`.

use std::collections::BTreeMap;

use relate_core::{NodeId, RelId, Value};

use super::Graph;
use crate::adjacency::COMPACTION_DELTA_FLOOR;

/// One node carrying `labels`, no properties, inside the caller's open
/// write.
pub(crate) fn labelled_node(g: &Graph, labels: &[&str]) -> NodeId {
    g.create_node(
        labels.iter().map(|l| l.to_string()).collect(),
        BTreeMap::new(),
    )
    .unwrap()
}

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

/// The first node id the store will not place in a dense slot array for
/// `slot` — the side-map boundary, advanced on the store's own
/// [`reservable_span_bytes`](crate::memory::reservable_span_bytes) gate,
/// never a use-site re-split of it.
pub(crate) fn first_side_map_id(slot: u64) -> i64 {
    let mut id = 1i64;
    while crate::memory::reservable_span_bytes(usize::try_from(id).unwrap().saturating_add(1), slot)
        .is_some()
    {
        id = id.saturating_mul(2);
    }
    id
}

/// A doomed rel of `rel_type` between `a` and `b`, plus a committed
/// survivor of the same type: deleting the doomed one leaves the
/// (start-label, type, end-label) count entry live — the stats estimator's
/// delete arm does not yet price a count entry's drop to zero (LABS-1386).
/// Returns the doomed rel's id.
pub(crate) fn doomed_rel_with_survivor(g: &Graph, a: NodeId, b: NodeId, rel_type: &str) -> RelId {
    g.begin_write().unwrap();
    let doomed = g
        .create_rel(a, b, rel_type.into(), BTreeMap::new())
        .unwrap();
    g.create_rel(a, b, rel_type.into(), BTreeMap::new())
        .unwrap();
    g.commit_write();
    doomed
}

/// Materialize a RANGE index over `(label, property)` on the committed
/// store — the materialized pair state a keyed seek serves (a cataloged
/// index alone is declared, not materialized). The bridge for test
/// modules outside the graph tree (the pattern-walk candidate tests,
/// LABS-1083): `materialize_range_index` itself stays `pub(super)`.
pub(crate) fn range_index(g: &Graph, label: &str, property: &str) {
    g.materialize_range_index(label, property);
}
