//! LABS-1116's per-statement label-interning memo: the memoized
//! `ExecGraph::node_has_label` answers exactly what direct interning
//! answers, across every distinction the per-candidate callers read —
//! carried, carried-elsewhere, never-interned, invisible — and repeated
//! checks (memo hits) keep answering the same.

use std::collections::BTreeMap;

use relate_core::NodeId;

use super::ExecGraph;
use crate::graph::Graph;

fn create(g: &Graph, label: &str) -> NodeId {
    g.begin_write().unwrap();
    let id = g.create_node(vec![label.into()], BTreeMap::new()).unwrap();
    g.commit_write();
    id
}

#[test]
fn memoized_label_checks_match_direct_interning() {
    let g = Graph::default();
    let with_a = create(&g, "A");
    let with_b = create(&g, "B");
    let view = ExecGraph::read(&g);
    assert_eq!(view.node_has_label(with_a, "A"), Some(true));
    // "B" is interned (the other node carries it) but this node does not.
    assert_eq!(view.node_has_label(with_a, "B"), Some(false));
    // "C" was never interned: still a visible node, so `Some(false)`,
    // never a visibility `None`.
    assert_eq!(view.node_has_label(with_a, "C"), Some(false));
    // Memo hits (every check above populated the memo) answer the same.
    assert_eq!(view.node_has_label(with_a, "A"), Some(true));
    assert_eq!(view.node_has_label(with_a, "C"), Some(false));
    assert_eq!(view.node_has_label(with_b, "B"), Some(true));
}

#[test]
fn memoized_label_check_answers_none_for_an_invisible_node() {
    let g = Graph::default();
    let with_a = create(&g, "A");
    g.begin_write().unwrap();
    g.delete_node(with_a, false).unwrap();
    g.commit_write();
    let view = ExecGraph::read(&g);
    // The `None` arm is the candidates path's visibility signal — a
    // memo that collapsed it into `Some(false)` would resurrect deleted
    // nodes as label-less candidates.
    assert_eq!(view.node_has_label(with_a, "A"), None);
}

#[test]
fn label_memo_answers_at_the_views_pinned_ts() {
    let g = Graph::default();
    let with_a = create(&g, "A");
    let view = ExecGraph::read(&g);
    assert_eq!(view.node_has_label(with_a, "A"), Some(true));
    // A node a LATER statement created is invisible at this view's
    // pinned ts — its label check answers `None`, not `Some(false)`,
    // and the memo keeps the distinction after the statement reset.
    let later = create(&g, "C");
    view.begin_statement();
    assert_eq!(view.node_has_label(with_a, "C"), Some(false));
    assert_eq!(view.node_has_label(later, "C"), None);
}

/// LABS-1116: a `CALL { … } IN TRANSACTIONS` batch commits mid-statement,
/// landing its labels in the base interner — a memoized `None` must not
/// survive it. The pollution is only reachable at this seam: a query's
/// own pattern labels are interned at plan time, before any check runs,
/// so only a check the view answers directly (not the planner) can
/// memoize a not-yet-interned label — and the batch that interns it
/// commits under the same view.
#[test]
fn batch_commit_invalidates_the_label_memo() {
    use std::sync::atomic::AtomicUsize;

    use super::batch_commit_tests::ApplySink;
    use crate::exec::test_support::{exec_autocommit, exec_write, write_scratch};

    let g = Graph::default();
    exec_autocommit(&g, "CREATE (:A)").unwrap();
    let a = g
        .lookup_nodes_at(&["A".into()], &BTreeMap::new(), g.commit_ts())
        .unwrap()[0];

    let scratch = write_scratch(&g);
    let view = ExecGraph::write(&g, &scratch);
    // The polluting check: "Z" is not yet interned, so the memo caches
    // `None` (a visible node without the label — `Some(false)`).
    assert_eq!(view.node_has_label(a, "Z"), Some(false));

    // The batch: a write whose label the base has never seen, committed
    // under the same view. The sink is the overlay seam's one double
    // (F-1-27, `batch_commit_tests::ApplySink`), its call counter unused.
    exec_write(&g, &scratch, "CREATE (:Z)").unwrap();
    let sink = ApplySink {
        graph: &g,
        calls: AtomicUsize::new(0),
    };
    view.commit_batch(&sink).unwrap();
    let z = g
        .lookup_nodes_at(&["Z".into()], &BTreeMap::new(), g.commit_ts())
        .unwrap()[0];

    // The memo must have re-resolved: the batch's `:Z` node carries the
    // label the polluting check cached as never-interned. The pre-fix
    // memo answered `Some(false)` here.
    assert_eq!(view.node_has_label(z, "Z"), Some(true));
}
