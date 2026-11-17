//! The multi-source level sweep: one breadth-first pass for up to
//! [`LANES`] origins at once, each origin one bit of a per-node word. A
//! node's `visited` word holds the origins that have reached it, so the
//! sweep learns, per level, which origins first arrive at each target
//! node, in work shared across the batch rather than one ball per origin
//! (Then et al., multi-source BFS). It keeps no predecessors: it answers
//! distance and reachability, and the caller rebuilds the trails of the
//! pairs it keeps with a pair search bounded by the depth reported here.

use relate_core::tokens::WorkspaceCharge;
use relate_core::{Error, NodeId, Result};

use super::Search;

/// Origins one sweep carries: the lanes of one `u64` word.
pub(crate) const LANES: usize = 64;

/// The widest node-id span a sweep tables, in ids. Three words per id are
/// charged to the workspace while the sweep runs (41 bytes per id, about
/// 330 MiB at the cap); a store with a wider id range keeps the per-origin
/// layered fan.
const MAX_SPAN: usize = 1 << 23;

/// Bytes charged per tabled id: the `visited`, `frontier` and `next` words
/// (24), the target bitset's bit rounded up to a byte (1), and the two
/// node-id lists a level holds at their worst, every id (16).
const BYTES_PER_ID: u64 = 41;

#[cfg(test)]
thread_local! {
    static SPAN_CAP: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// [`MAX_SPAN`], or the test override that lets a small graph reach it.
fn span_cap() -> usize {
    #[cfg(test)]
    if let Some(cap) = SPAN_CAP.with(std::cell::Cell::get) {
        return cap;
    }
    MAX_SPAN
}

/// Run `f` with the span cap lowered to `cap` on this thread.
#[cfg(test)]
pub(crate) fn with_span_cap<T>(cap: usize, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<usize>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SPAN_CAP.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(SPAN_CAP.with(|c| c.replace(Some(cap))));
    f()
}

impl Search<'_, '_> {
    /// Whether the view's node-id span fits the sweep's per-node tables.
    pub(crate) fn sweep_fits(&self) -> bool {
        usize::try_from(self.graph.node_id_bound()).is_ok_and(|span| span <= span_cap())
    }

    /// Breadth-first from every `active` origin (bit `i` of `active` is
    /// `origins[i]`, at most [`LANES`] of them) at once, at most `max`
    /// relationships deep. After each level that first reaches a `target`
    /// node, `on_level(depth, reached)` gets `(target, lanes)` pairs: the
    /// target nodes the level reached, each with the origins whose
    /// shortest distance to it is exactly `depth`. It returns the lanes to
    /// retire: a retired origin is never advanced again, and the sweep
    /// ends once none is left. An origin's own node is reached at depth 0
    /// and never reported, as in the layered search.
    ///
    /// `targets` are distinct node ids, ascending. The last level the
    /// bound allows is not pushed out of the frontier: only a target's
    /// arrival matters there, so each target reads its own neighbours'
    /// lanes instead (work in the targets' degrees, not the frontier's).
    ///
    /// A first arrival is a shortest walk, hence a trail, so each reported
    /// distance is the length of the shortest trail the layered search
    /// would find. The tables are three words per node id in the view's
    /// span, charged to the workspace; the callback sees at most one
    /// entry per target per level.
    ///
    /// Precondition: [`Self::sweep_fits`].
    pub(crate) fn sweep_levels(
        &self,
        origins: &[NodeId],
        mut active: u64,
        targets: &[NodeId],
        mut on_level: impl FnMut(u32, &[(NodeId, u64)]) -> Result<u64>,
    ) -> Result<()> {
        debug_assert!(origins.len() <= LANES && self.sweep_fits());
        let span = slot_of(self.graph.node_id_bound())?;
        let _charge = WorkspaceCharge::retain_with(|| {
            u64::try_from(span).map_or(u64::MAX, |n| n.saturating_mul(BYTES_PER_ID))
        })?;
        let mut target_bits = vec![0u64; span.div_ceil(64)];
        for &target in targets {
            let at = slot_in(target, span)?;
            target_bits[at / 64] |= 1 << (at % 64);
        }
        let back = self.reversed();
        let mut visited = vec![0u64; span];
        let mut frontier = vec![0u64; span];
        let mut next = vec![0u64; span];
        let mut frontier_nodes: Vec<NodeId> = Vec::new();
        for (lane, &origin) in origins.iter().enumerate() {
            if active & (1 << lane) == 0 {
                continue;
            }
            let at = slot_in(origin, span)?;
            visited[at] |= 1 << lane;
            if frontier[at] == 0 {
                frontier_nodes.push(origin);
            }
            frontier[at] |= 1 << lane;
        }
        let mut next_nodes: Vec<NodeId> = Vec::new();
        let mut reached: Vec<(NodeId, u64)> = Vec::new();
        let mut depth = 0;
        while active != 0 && !frontier_nodes.is_empty() && depth < self.max {
            if depth + 1 == self.max {
                // The final level: pull, never push.
                reached.clear();
                for &target in targets {
                    self.graph.cancel_tick()?;
                    let at = slot_in(target, span)?;
                    let mut lanes = 0;
                    back.each_hop(target, |peer, _| {
                        lanes |= frontier[slot_in(peer, span)?];
                        Ok(())
                    })?;
                    lanes &= active & !visited[at];
                    if lanes != 0 {
                        reached.push((target, lanes));
                    }
                }
                if !reached.is_empty() {
                    on_level(depth + 1, &reached)?;
                }
                break;
            }
            for &node in &frontier_nodes {
                self.graph.cancel_tick()?;
                let lanes = frontier[slot_in(node, span)?] & active;
                if lanes == 0 {
                    continue;
                }
                self.each_hop(node, |peer, _| {
                    let at = slot_in(peer, span)?;
                    let fresh = lanes & !visited[at];
                    if fresh != 0 {
                        if next[at] == 0 {
                            next_nodes.push(peer);
                        }
                        next[at] |= fresh;
                    }
                    Ok(())
                })?;
            }
            for &node in &frontier_nodes {
                frontier[slot_in(node, span)?] = 0;
            }
            frontier_nodes.clear();
            depth += 1;
            reached.clear();
            for &node in &next_nodes {
                let at = slot_in(node, span)?;
                let lanes = std::mem::take(&mut next[at]) & active;
                if lanes == 0 {
                    continue;
                }
                visited[at] |= lanes;
                frontier[at] = lanes;
                frontier_nodes.push(node);
                if target_bits[at / 64] & (1 << (at % 64)) != 0 {
                    reached.push((node, lanes));
                }
            }
            next_nodes.clear();
            if !reached.is_empty() {
                active &= !on_level(depth, &reached)?;
            }
        }
        Ok(())
    }
}

/// A node id as a table slot: engine ids are issued upward from zero.
fn slot_of(id: NodeId) -> Result<usize> {
    usize::try_from(id).map_err(|_| Error::internal("multi-source sweep: negative node id"))
}

/// `id`'s slot in a table of `span` entries; an id past it breaks the
/// view's id-bound invariant ([`crate::overlay::ExecGraph::node_id_bound`]).
fn slot_in(id: NodeId, span: usize) -> Result<usize> {
    match slot_of(id)? {
        at if at < span => Ok(at),
        _ => Err(Error::internal(
            "multi-source sweep: node id past the view's id bound",
        )),
    }
}
