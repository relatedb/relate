//! The per-state guard memo the frontier BFS and the re-arrival search
//! share (LABS-1478): an NFA state's guards are a pure function of the
//! node over the run's fixed view (the empty row, the stage's parameters),
//! so one [`NodeMemo`] per guarded state answers each (state, node) pair
//! once per run once warm. Errors are never remembered; an unguarded
//! state never reaches the memo.

use std::cell::RefCell;
use std::collections::BTreeMap;

use relate_core::{NodeId, Result, Value};

use super::memo::{MemoPolicy, NodeMemo};
use super::nfa::FrontierNfa;
use crate::exec::candidates::node_ok;
use crate::metrics::MemoCounters;
use crate::overlay::ExecGraph;
use crate::row::Row;

/// One run's guard memos, one per automaton state, behind interior
/// mutability so the read-only guard bundles can answer through them.
pub(super) struct StateMemos(RefCell<Vec<NodeMemo>>);

impl Drop for StateMemos {
    /// Fold the run's memos into the PROFILE total (ADR-0654), summed over
    /// the automaton's states, when a PROFILE arm is armed; otherwise no
    /// memo is read.
    fn drop(&mut self) {
        let memos = self.0.get_mut();
        crate::exec::record_memos(|| {
            let mut sum = MemoCounters::default();
            for memo in memos.iter() {
                sum += memo.stats().counters();
            }
            sum
        });
    }
}

impl StateMemos {
    /// Empty memos for an automaton of `states` states: no table is
    /// allocated until a state's memo is warm.
    pub(super) fn new(graph: &ExecGraph<'_>, states: usize) -> Self {
        let live = graph.memo_live();
        Self(RefCell::new(
            (0..states)
                .map(|_| NodeMemo::new(MemoPolicy::moderate(&live), Some(live.clone())))
                .collect(),
        ))
    }

    /// Whether `node` satisfies every guard `nfa` carries at `state`: the
    /// one per-state admission policy both runners share. An unguarded
    /// state is `Ok(true)` without a read or a memo; otherwise the verdict
    /// is remembered, or the guards' own `node_ok` conjunction over the
    /// empty row and `params`. A failing guard is a plain `false`; a guard
    /// whose evaluation errors is the error, never a rejection (the walker
    /// aborts on the same evaluation).
    pub(super) fn state_admits(
        &self,
        graph: &ExecGraph<'_>,
        nfa: &FrontierNfa,
        params: &BTreeMap<String, Value>,
        state: u32,
        node: NodeId,
    ) -> Result<bool> {
        let guards = &nfa.state_guards[state as usize];
        if guards.is_empty() {
            return Ok(true);
        }
        self.0.borrow_mut()[state as usize].answer(graph, node, || {
            let row = Row::new();
            for guard in guards {
                if !node_ok(graph, node, guard, &row, params)? {
                    return Ok(false);
                }
            }
            Ok(true)
        })
    }
}
