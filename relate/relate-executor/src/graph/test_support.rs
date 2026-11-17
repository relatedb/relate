//! Test-only graph seeding helpers shared by the graph's and the overlay's
//! test modules — and, for the adversarial corpus below, the exec-level
//! differential families. The exec-level `test_support` is for running
//! Cypher; helpers that touch only `Graph` APIs live here, beside the
//! graph's own `tests.rs`.

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

/// The adversarial multigraph's nodes, `(name, labels)` in creation order
/// — the LABS-1293 differential corpus: self-loops at the start, middle
/// and end, parallel same-type and cross-type edges, reciprocal pairs, a
/// hub, unlabelled nodes.
const ADVERSARIAL_NODES: &[(&str, &[&str])] = &[
    ("a", &["U"]),
    ("n0", &["U"]),
    ("n1", &["U"]),
    ("n2", &["U"]),
    ("n3", &["U"]),
    ("n4", &["U"]),
    ("n5", &["U"]),
    ("n6", &["U"]),
    ("bare", &[]),
    ("hub", &["U", "Hub"]),
];

/// The adversarial multigraph's edges, `(from, to, type)` in creation
/// order — the pattern [`ADVERSARIAL_NODES`] names.
const ADVERSARIAL_EDGES: &[(&str, &str, &str)] = &[
    // Self-loops at the start, a middle node and an end node.
    ("a", "a", "F"),
    ("n1", "n1", "F"),
    ("n3", "n3", "F"),
    // Parallel same-type and cross-type edges.
    ("a", "n0", "F"),
    ("a", "n0", "F"),
    ("a", "n0", "G"),
    // Reciprocal pairs.
    ("n0", "n1", "F"),
    ("n1", "n0", "F"),
    ("n1", "n2", "F"),
    ("n2", "n3", "G"),
    ("n3", "n4", "F"),
    ("n4", "a", "F"),
    // Unlabelled node and the hub.
    ("n2", "bare", "F"),
    ("bare", "n3", "F"),
    ("n0", "hub", "F"),
    ("hub", "n0", "F"),
    ("n1", "hub", "F"),
    ("hub", "n1", "F"),
    ("n2", "hub", "F"),
    ("hub", "n2", "F"),
    ("n3", "hub", "F"),
    ("hub", "n3", "F"),
    ("n4", "hub", "F"),
    ("hub", "n4", "F"),
    ("n5", "hub", "F"),
    ("hub", "n5", "F"),
    ("n6", "hub", "F"),
    ("hub", "n6", "F"),
    ("a", "hub", "F"),
];

/// The adversarial multigraph's parts: the graph, its named nodes, and every
/// rel `(start, end, type, id)` in creation order.
pub(crate) type AdversarialMultigraph = (
    Graph,
    Vec<(&'static str, NodeId)>,
    Vec<(NodeId, NodeId, &'static str, RelId)>,
);

/// Build the adversarial multigraph, each of [`ADVERSARIAL_EDGES`]'s rows
/// `each` times. At 1 it is the hand-built graph the kernels' differential
/// families run; at [`COMPACTION_DELTA_FLOOR`] every (node, type) chunk
/// reaches the seal floor, the scale at which one checkpoint fold seals
/// the whole corpus. Returns the named nodes and every rel
/// `(start, end, type, id)` in creation order.
pub(crate) fn adversarial_multigraph(each: usize) -> AdversarialMultigraph {
    let g = Graph::default();
    g.begin_write().unwrap();
    let mut ids = BTreeMap::new();
    let nodes = ADVERSARIAL_NODES
        .iter()
        .map(|(name, labels)| {
            let id = g
                .create_node(
                    labels.iter().map(|l| l.to_string()).collect(),
                    BTreeMap::from([("id".into(), Value::String((*name).into()))]),
                )
                .unwrap();
            ids.insert(*name, id);
            (*name, id)
        })
        .collect();
    let mut rels = Vec::new();
    for (from, to, t) in ADVERSARIAL_EDGES {
        for _ in 0..each {
            let rid = g
                .create_rel(ids[from], ids[to], (*t).into(), BTreeMap::new())
                .unwrap();
            rels.push((ids[from], ids[to], *t, rid));
        }
    }
    g.commit_write();
    (g, nodes, rels)
}

/// The first node id the store will not place in a dense slot array for
/// `slot` — the side-map boundary, advanced on the store's own
/// [`SpanPlacement`](crate::memory::SpanPlacement) policy, never a use-site
/// re-split of it.
pub(crate) fn first_side_map_id(slot: u64) -> i64 {
    let mut id = 1i64;
    while crate::memory::SpanPlacement::classify(0, usize::try_from(id).unwrap(), slot)
        != crate::memory::SpanPlacement::SideEntry
    {
        id = id.saturating_mul(2);
    }
    id
}

/// A committed rel of `rel_type` between `a` and `b`, returned for a step
/// to delete. Deleting it may drain the last (start-label, type,
/// end-label) count entry — the stats estimator prices that refund
/// (LABS-1386, landed), so no survivor keeps the entry live.
pub(crate) fn doomed_rel(g: &Graph, a: NodeId, b: NodeId, rel_type: &str) -> RelId {
    g.begin_write().unwrap();
    let doomed = g
        .create_rel(a, b, rel_type.into(), BTreeMap::new())
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
