//! Test-only graph seeding helpers shared by the graph's and the overlay's
//! test modules — and, for the adversarial corpus below, the exec-level
//! differential families. The exec-level `test_support` is for running
//! Cypher; helpers that touch only `Graph` APIs live here, beside the
//! graph's own `tests.rs`.

use std::collections::BTreeMap;

use relate_core::{Node, NodeId, RelId, Relationship, Temporal, Value};

use super::adjacency::AdjSide;
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

/// The reclaim sequence two sealed-arena tests share: a folded chunk whose
/// every rel is deleted and folded away, leaving the chunk's whole stream
/// dead and its region gone. Returns the node and the arena length the
/// stream held.
pub(crate) fn reclaimed_folded_chunk(g: &Graph) -> (NodeId, u64) {
    let (a, _, ids, _) = folded_floor_chunk(g);
    let (arena, dead) = g.out_side().sealed().reclaim_pair();
    assert_eq!(dead, 0);
    assert!(arena > 0, "the seal left the chunk's stream in the arena");
    g.begin_write().unwrap();
    for id in &ids {
        g.delete_rel(*id);
    }
    g.commit_write();
    g.fold_adjacency_at_watermark(u64::MAX);
    assert!(
        g.out_side().sealed().region(a).is_none(),
        "the emptied region goes"
    );
    (a, arena)
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

/// The out-of-window temporal the range-index tests build: a year past the
/// order key's window (`dt_nanos` cannot represent it), the one fixture
/// statement they share.
pub(crate) fn out_of_window_local_datetime() -> Value {
    Value::Temporal(Temporal::LocalDateTime(
        chrono::NaiveDate::from_ymd_opt(9999, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap(),
    ))
}

/// The length of `node`'s sealed chunk for `type_id` on one side — the one
/// spelling of "the sealed region must exist, read its chunk".
pub(super) fn sealed_chunk_len(side: &AdjSide<'_>, node: NodeId, type_id: u32) -> usize {
    side.sealed()
        .region(node)
        .expect("the node has a sealed region")
        .chunk_len(type_id)
}

/// One node's adjacency, raw and as each direction expands it — the
/// observation the restore and rebuild submodules compare graphs by.
#[derive(Debug, PartialEq)]
pub(crate) struct NodeAdjacency {
    pub(crate) id: NodeId,
    pub(crate) out: Vec<RelId>,
    pub(crate) r#in: Vec<RelId>,
    /// `Outgoing`, `Incoming`, then `Both` hops.
    pub(crate) expands: [Vec<RelId>; 3],
}

/// Every live node's [`NodeAdjacency`] at the graph's commit ts.
pub(crate) fn node_adjacencies(g: &Graph) -> Vec<NodeAdjacency> {
    use relate_ir::Direction;

    let ts = g.commit_ts();
    g.all_node_ids_at(ts)
        .into_iter()
        .map(|id| {
            let expands = [Direction::Outgoing, Direction::Incoming, Direction::Both]
                .map(|dir| g.neighbors_at(id, dir, ts));
            NodeAdjacency {
                id,
                out: g.degree_out(id),
                r#in: g.degree_in(id),
                expands,
            }
        })
        .collect()
}

/// Every live node's and rel's rows at `ts`, in id order — the dump a
/// checkpoint writes and a restore installs.
pub(crate) fn live_rows(g: &Graph, ts: u64) -> (Vec<Node>, Vec<Relationship>) {
    let nodes = g
        .all_node_ids_at(ts)
        .into_iter()
        .map(|id| g.node_at(id, ts).unwrap())
        .collect();
    let rels = g
        .all_rel_ids_at(ts)
        .into_iter()
        .map(|id| g.rel_at(id, ts).unwrap())
        .collect();
    (nodes, rels)
}

/// Every live node's raw adjacency as each direction expands it — the
/// narrower observation for tests whose subject is the adjacency alone,
/// without the derived degree lists [`node_adjacencies`] also carries.
pub(crate) fn node_expansions(g: &Graph) -> Vec<(NodeId, [Vec<RelId>; 3])> {
    use relate_ir::Direction;

    let ts = g.commit_ts();
    g.all_node_ids_at(ts)
        .into_iter()
        .map(|id| {
            let expands = [Direction::Outgoing, Direction::Incoming, Direction::Both]
                .map(|dir| g.neighbors_at(id, dir, ts));
            (id, expands)
        })
        .collect()
}
