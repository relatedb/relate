//! The resolved neighbor walk's committed + scratch arms (LABS-801) —
//! the unit-level coverage of `overlay/adjacency.rs`'s id-filtered walk,
//! beside the statement-level suites in `query_semantics_tests.rs`.

use crate::exec::test_support::write_scratch;
use crate::graph::Graph;

/// LABS-801: the resolved neighbor walk's write-overlay arm. The committed
/// side filters by interned id; the scratch's rel copies carry type
/// strings, so that arm compares the filter's names — a scratch rel of a
/// committed type and one of a type the store never interned both match
/// their filter, and a name nothing carries matches nothing.
#[test]
fn resolved_neighbor_walk_covers_committed_and_scratch_types() {
    use std::collections::BTreeMap;

    use relate_ir::Direction;

    use super::ExecGraph;
    use crate::store::RelTypes;

    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let committed = g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    g.commit_write();

    let scratch = write_scratch(&g);
    let view = ExecGraph::write(&g, &scratch);
    // A scratch rel of the committed type (interned id known) and one of
    // a type the store never interned (the name arm).
    let scratch_r = view.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    let scratch_new = view
        .create_rel(a, b, "NEWTYPE".into(), BTreeMap::new())
        .unwrap();

    let walk = |types: &RelTypes| {
        let mut out = Vec::new();
        view.try_for_each_neighbor_resolved(a, Direction::Outgoing, types, |rid| {
            out.push(rid);
            Ok::<(), ()>(())
        })
        .unwrap();
        out
    };

    // One committed type: the committed rel and the scratch copy of it.
    let mut got = walk(&view.rel_types(&["R"]));
    got.sort();
    let mut want = vec![committed, scratch_r];
    want.sort();
    assert_eq!(got, want);
    // The never-interned type: the name arm admits the scratch rel.
    assert_eq!(walk(&view.rel_types(&["NEWTYPE"])), vec![scratch_new]);
    // Multiple types: both arms' matches, deduplicated.
    let mut got = walk(&view.rel_types(&["R", "NEWTYPE"]));
    got.sort();
    let mut want = vec![committed, scratch_r, scratch_new];
    want.sort();
    assert_eq!(got, want);
    // A name nothing carries matches nothing, scratch included.
    assert!(walk(&view.rel_types(&["GHOST"])).is_empty());
    // The untyped filter walks everything.
    assert_eq!(walk(&RelTypes::Any).len(), 3);
}

/// The hop walk's two arms: a read-only view takes the base fast path
/// (peers from the walk's one version read), a write view answers the
/// scratch rels from the batch's records — including a self-loop under
/// `Both` (one hop, not two) and an Incoming walk's self-loop (kept).
/// The write view's merge order is unspecified (ADR-0203's order is the
/// committed chunk order, not the scratch's), so its assertions compare
/// sorted.
#[test]
fn resolved_hop_walk_covers_read_only_and_scratch_arms() {
    use std::collections::BTreeMap;

    use relate_ir::Direction;

    use super::ExecGraph;
    use crate::store::RelTypes;

    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let committed = g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    let loop_r = g.create_rel(a, a, "LOOP".into(), BTreeMap::new()).unwrap();
    g.commit_write();

    let hop_pairs = |view: &ExecGraph, dir: Direction, types: &RelTypes| {
        let mut out = Vec::new();
        view.try_for_each_hop_resolved(a, dir, types, |rid, peer| {
            out.push((rid, peer));
            Ok::<(), ()>(())
        })
        .unwrap();
        out
    };

    // Read-only view (the base fast path): every dir walks the committed
    // rels, the self-loop deduped to one hop under Both.
    let read = ExecGraph::read(&g);
    assert_eq!(
        hop_pairs(&read, Direction::Outgoing, &RelTypes::Any),
        vec![(committed, b), (loop_r, a)]
    );
    assert_eq!(
        hop_pairs(&read, Direction::Incoming, &RelTypes::Any),
        vec![(loop_r, a)],
        "the committed a->b rel is in b's in-list, not a's; the loop is"
    );
    assert_eq!(
        hop_pairs(&read, Direction::Both, &RelTypes::Any).len(),
        2,
        "the self-loop is one hop under Both"
    );

    // Write view: a scratch copy of the committed type and a scratch
    // self-loop; the deleted rel hides, the scratch peers answer.
    let scratch = write_scratch(&g);
    let view = ExecGraph::write(&g, &scratch);
    view.delete_rel(committed);
    let scratch_r = view.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    let scratch_loop = view
        .create_rel(a, a, "LOOP".into(), BTreeMap::new())
        .unwrap();
    // A non-self-loop in-list entry: only a peer of `c` (never `a`) tells
    // the in-pass arms' start/end apart.
    let c = view.create_node(vec!["C".into()], BTreeMap::new()).unwrap();
    let scratch_in = view.create_rel(c, a, "R".into(), BTreeMap::new()).unwrap();

    let sorted = |dir: Direction| {
        let mut pairs = hop_pairs(&view, dir, &RelTypes::Any);
        pairs.sort();
        pairs
    };
    assert_eq!(
        sorted(Direction::Outgoing),
        vec![(loop_r, a), (scratch_r, b), (scratch_loop, a)],
        "the deleted committed rel is hidden; the surviving committed rel \
         and the scratch rels answer with peers"
    );
    assert_eq!(
        sorted(Direction::Both),
        {
            let mut want = vec![
                (loop_r, a),
                (scratch_r, b),
                (scratch_loop, a),
                (scratch_in, c),
            ];
            want.sort();
            want
        },
        "one hop per self-loop under Both, and the in-list entry's peer \
         is its start"
    );
    assert_eq!(
        sorted(Direction::Incoming),
        {
            let mut want = vec![(loop_r, a), (scratch_loop, a), (scratch_in, c)];
            want.sort();
            want
        },
        "an Incoming walk's in pass is the only listing of the node's \
         scratch rels: the scratch self-loop answers, not skipped, and \
         the non-self-loop's peer is its start"
    );
}
