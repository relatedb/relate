//! Shortest-path test support: the expansion-counting combinator over
//! [`take_shortest_expands`] and the ternary-tree builder helpers the
//! counterexample graphs share.

use relate_core::{Error, NodeId};

use super::take_shortest_expands;
use crate::exec::test_support::Builder;

/// The 51N23 text every common-end-node rejection carries.
pub(crate) const COMMON_END_NODES: &str =
    "The shortest path algorithm does not work when the start and end nodes are the same.";

/// Assert `err` is the 51N23 common-end-node rejection.
pub(crate) fn assert_common_end_nodes(err: &Error, context: &str) {
    assert!(
        err.to_string().contains(COMMON_END_NODES),
        "{context}: {err}"
    );
}

/// `f`'s result and the relationships shortest-path searches followed
/// while it ran.
pub(crate) fn counting_expands<T>(f: impl FnOnce() -> T) -> (T, u64) {
    let _ = take_shortest_expands();
    let out = f();
    (out, take_shortest_expands())
}

/// Ternary trees hung under a root: three unlabelled children per node,
/// each joined to its parent by one `E` relationship.
pub(crate) trait TernaryTree {
    /// Hangs a ternary tree `levels` deep under `root`, every relationship
    /// pointing toward `root`.
    fn fan_in_tree(&self, root: NodeId, levels: usize);
}

impl TernaryTree for Builder {
    fn fan_in_tree(&self, root: NodeId, levels: usize) {
        // Toward `root`: the child points at its parent.
        ternary_tree(self, root, levels, |b, parent, child| b.edge(child, parent));
    }
}

/// The tree of [`TernaryTree::fan_in_tree`], built level by level, each parent-child pair joined by one `E`
/// relationship the passed edge builder directs.
fn ternary_tree(b: &Builder, root: NodeId, levels: usize, edge: impl Fn(&Builder, NodeId, NodeId)) {
    let mut level = vec![root];
    for _ in 0..levels {
        let mut next = Vec::new();
        for &parent in &level {
            for _ in 0..3 {
                let child = b.anon(&[]);
                edge(b, parent, child);
                next.push(child);
            }
        }
        level = next;
    }
}
