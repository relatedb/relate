//! The per-state guard memo the frontier BFS and the re-arrival search
//! share (LABS-1478): an NFA state's guards are a pure function of the
//! node over the run's fixed view (the empty row, the stage's parameters),
//! so one [`NodeMemo`] per guarded state answers each (state, node) pair
//! once per run once warm. Errors are never remembered; an unguarded
//! state never reaches the memo.

use std::cell::RefCell;

use relate_core::{NodeId, Result};

use super::memo::{MemoPolicy, NodeMemo};
use crate::overlay::ExecGraph;

/// One run's guard memos, one per automaton state, behind interior
/// mutability so the read-only guard bundles can answer through them.
pub(super) struct StateMemos(RefCell<Vec<NodeMemo>>);

impl StateMemos {
    /// Empty memos for an automaton of `states` states: no table is
    /// allocated until a state's memo is warm.
    pub(super) fn new(graph: &ExecGraph<'_>, states: usize) -> Self {
        let live = graph.memo_live();
        Self(RefCell::new(
            (0..states)
                .map(|_| NodeMemo::new(MemoPolicy::MODERATE, Some(live.clone())))
                .collect(),
        ))
    }

    /// `node`'s guard verdict at `state`: remembered, or `compute()`'s.
    pub(super) fn answer(
        &self,
        graph: &ExecGraph<'_>,
        state: u32,
        node: NodeId,
        compute: impl FnOnce() -> Result<bool>,
    ) -> Result<bool> {
        self.0.borrow_mut()[state as usize].answer(graph, node, compute)
    }
}
