//! The batched hop reader against the per-edge walk it replaced, and its
//! per-node cancellation.

use relate_core::{CancelToken, NodeId, RelId};
use relate_ir::Direction;

use crate::exec::test_support::{build, Built};
use crate::overlay::ExecGraph;
use crate::store::RelTypes;

use super::test_support::{counting_expands, search};

/// The old reader: one callback per followed relationship, endpoints
/// resolved per hop, `Both` deduplicated by rel id.
fn per_edge(
    view: &ExecGraph<'_>,
    node: NodeId,
    dir: Direction,
    types: &RelTypes,
) -> Vec<(NodeId, RelId)> {
    let mut out = Vec::new();
    view.try_for_each_hop_resolved(node, dir, types, |rid, peer| {
        out.push((peer, rid));
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap();
    out.sort();
    out
}

#[test]
fn batch_reader_matches_the_per_edge_walk_on_a_random_graph() {
    const NODES: usize = 24;
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    let kinds = ["A", "B", "C"];
    // Dense enough for multi-edges and self-loops.
    let rels: Vec<(usize, usize, &str)> = (0..90)
        .map(|_| (next(NODES), next(NODES), kinds[next(kinds.len())]))
        .collect();
    assert!(
        rels.iter().any(|(a, b, _)| a == b),
        "fixture has a self-loop"
    );
    let Built {
        graph: g, nodes, ..
    } = build(&vec![&[][..]; NODES], &rels);
    let view = ExecGraph::read(&g);
    let typed = view.rel_types(&["A", "C"]);
    for dir in [Direction::Outgoing, Direction::Incoming, Direction::Both] {
        for types in [RelTypes::Any, typed.clone()] {
            let s = super::Search {
                types: types.clone(),
                ..search(&view, dir, 4)
            };
            for &n in &nodes {
                let mut got = s.hops(n).unwrap();
                got.sort();
                assert_eq!(got, per_edge(&view, n, dir, &types), "{dir:?} from {n:?}");
            }
        }
    }
}

#[test]
fn a_hub_reads_in_batches_that_equal_the_per_edge_walk() {
    use crate::graph::{HopBuf, HOP_BATCH};

    const DEGREE: usize = 50_000;
    let rels: Vec<(usize, usize, &str)> = (1..=DEGREE).map(|i| (0, i, "E")).collect();
    let Built {
        graph: g, nodes, ..
    } = build(&vec![&[][..]; DEGREE + 1], &rels);
    let view = ExecGraph::read(&g);
    let (mut got, mut batches) = (Vec::new(), 0);
    view.try_for_each_hop_batch(
        nodes[0],
        Direction::Outgoing,
        &RelTypes::Any,
        &mut HopBuf::default(),
        |hops| {
            assert!(hops.len() <= HOP_BATCH, "a batch is bounded by HOP_BATCH");
            got.extend(hops.iter().map(|hop| (hop.peer, hop.rid)));
            batches += 1;
            Ok::<(), relate_core::Error>(())
        },
    )
    .unwrap();
    assert_eq!(batches, DEGREE.div_ceil(HOP_BATCH));
    got.sort();
    assert_eq!(
        got,
        per_edge(&view, nodes[0], Direction::Outgoing, &RelTypes::Any)
    );
}

#[test]
fn a_cancelled_search_stops_at_the_node_entry_the_sampled_tick_reads() {
    const DEGREE: usize = 50_000;
    // Node 0 is the hub; the leaf walks below only advance the tick counter.
    let rels: Vec<(usize, usize, &str)> = (1..=DEGREE).map(|i| (0, i, "E")).collect();
    let Built {
        graph: g, nodes, ..
    } = build(&vec![&[][..]; DEGREE + 1], &rels);
    let token = CancelToken::new();
    let view = ExecGraph::read(&g).with_cancel(Some(&token));
    let s = search(&view, Direction::Outgoing, 3);
    let (open, expands) = counting_expands(|| s.hops(nodes[0]));
    assert_eq!(open.unwrap().len(), DEGREE);
    assert_eq!(expands, DEGREE as u64);

    // One tick is spent; the token is read on the 1024th.
    token.cancel();
    for _ in 1..crate::overlay::CANCEL_SAMPLE_INTERVAL - 1 {
        s.hops(nodes[1]).unwrap();
    }
    let (cancelled, expands) = counting_expands(|| s.hops(nodes[0]));
    assert!(matches!(
        cancelled.unwrap_err(),
        relate_core::Error::Cancelled
    ));
    assert_eq!(expands, 0, "no hop of the hub was followed");
}
