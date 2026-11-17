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
        active: u64,
        targets: &[NodeId],
        mut on_level: impl FnMut(u32, &[(NodeId, u64)]) -> Result<u64>,
    ) -> Result<()> {
        debug_assert!(origins.len() <= LANES && self.sweep_fits());
        let span = slot_of(self.graph.node_id_bound())?;
        let _charge = WorkspaceCharge::retain_with(|| {
            u64::try_from(span).map_or(u64::MAX, |n| n.saturating_mul(BYTES_PER_ID))
        })?;
        let mut sweep = Sweep::seed(self, span, origins, active, targets)?;
        let mut depth = 0;
        while sweep.active != 0 && !sweep.frontier_nodes.is_empty() && depth < self.max {
            // The final level the bound allows is pulled, never pushed.
            if depth + 1 == self.max {
                sweep.pull_final()?;
            } else {
                sweep.push_level()?;
            }
            depth += 1;
            if !sweep.reached.is_empty() {
                sweep.active &= !on_level(depth, &sweep.reached)?;
            }
        }
        Ok(())
    }
}

/// The tables of one multi-source sweep: per node-id slot, the origins that
/// have `visited` it and those on the current `frontier` (and, while a
/// level is pushed, on the `next` one), and the targets as a bitset.
struct Sweep<'s, 'a, 'g> {
    search: &'s Search<'a, 'g>,
    /// The reversed search, the final level's pull reads.
    back: Search<'a, 'g>,
    span: usize,
    targets: &'s [NodeId],
    target_bits: Vec<u64>,
    visited: Vec<u64>,
    frontier: Vec<u64>,
    next: Vec<u64>,
    frontier_nodes: Vec<NodeId>,
    next_nodes: Vec<NodeId>,
    /// The targets the last level reached, with the lanes that did.
    reached: Vec<(NodeId, u64)>,
    /// The lanes still advancing.
    active: u64,
}

impl<'s, 'a, 'g> Sweep<'s, 'a, 'g> {
    /// The tables with every `active` origin seeded at depth 0.
    fn seed(
        search: &'s Search<'a, 'g>,
        span: usize,
        origins: &[NodeId],
        active: u64,
        targets: &'s [NodeId],
    ) -> Result<Self> {
        let mut target_bits = vec![0u64; span.div_ceil(64)];
        for &target in targets {
            let at = slot_in(target, span)?;
            target_bits[at / 64] |= 1 << (at % 64);
        }
        let mut sweep = Self {
            search,
            back: search.reversed(),
            span,
            targets,
            target_bits,
            visited: vec![0u64; span],
            frontier: vec![0u64; span],
            next: vec![0u64; span],
            frontier_nodes: Vec::new(),
            next_nodes: Vec::new(),
            reached: Vec::new(),
            active,
        };
        for (lane, &origin) in origins.iter().enumerate() {
            if active & (1 << lane) == 0 {
                continue;
            }
            let at = slot_in(origin, span)?;
            sweep.visited[at] |= 1 << lane;
            if sweep.frontier[at] == 0 {
                sweep.frontier_nodes.push(origin);
            }
            sweep.frontier[at] |= 1 << lane;
        }
        Ok(sweep)
    }

    /// The last level the bound allows, pulled: each target reads its own
    /// neighbours' lanes. Fills `reached`; the frontier is left as it was.
    fn pull_final(&mut self) -> Result<()> {
        self.reached.clear();
        for &target in self.targets {
            self.search.graph.cancel_tick()?;
            let at = slot_in(target, self.span)?;
            let mut lanes = 0;
            self.back.each_hop(target, |peer, _| {
                lanes |= self.frontier[slot_in(peer, self.span)?];
                Ok(())
            })?;
            lanes &= self.active & !self.visited[at];
            if lanes != 0 {
                self.reached.push((target, lanes));
            }
        }
        Ok(())
    }

    /// One level pushed out of the frontier: the new frontier is the nodes
    /// first reached, and `reached` the targets among them.
    fn push_level(&mut self) -> Result<()> {
        for &node in &self.frontier_nodes {
            self.search.graph.cancel_tick()?;
            let lanes = self.frontier[slot_in(node, self.span)?] & self.active;
            if lanes == 0 {
                continue;
            }
            self.search.each_hop(node, |peer, _| {
                let at = slot_in(peer, self.span)?;
                let fresh = lanes & !self.visited[at];
                if fresh != 0 {
                    if self.next[at] == 0 {
                        self.next_nodes.push(peer);
                    }
                    self.next[at] |= fresh;
                }
                Ok(())
            })?;
        }
        for &node in &self.frontier_nodes {
            self.frontier[slot_in(node, self.span)?] = 0;
        }
        self.frontier_nodes.clear();
        self.reached.clear();
        for &node in &self.next_nodes {
            let at = slot_in(node, self.span)?;
            let lanes = std::mem::take(&mut self.next[at]) & self.active;
            if lanes == 0 {
                continue;
            }
            self.visited[at] |= lanes;
            self.frontier[at] = lanes;
            self.frontier_nodes.push(node);
            if self.target_bits[at / 64] & (1 << (at % 64)) != 0 {
                self.reached.push((node, lanes));
            }
        }
        self.next_nodes.clear();
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
