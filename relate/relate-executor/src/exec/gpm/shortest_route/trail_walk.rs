//! One predicate-filtered trail walk of a partition — the sub-search
//! `Route::feasible_partition` recurses over (route.rs). Split out at the
//! guard.rs precedent: a self-contained walker with its own state, its own
//! recursion, and one entry point, so route.rs stays under the repo's
//! 1k-line ceiling.

use relate_core::{NodeId, RelId, Result};

use crate::row::Row;
use crate::UsedSet;

use super::super::sink::MatchedSink;
use super::route::Route;

/// One predicate-filtered trail walk of a partition: the search frames it
/// recurses over (the trail and the relationships it consumed) beside the
/// partition's endpoints, row and predicate. Bundled so the recursion carries
/// the frame it extends and its buffer is allocated once per partition.
pub(super) struct TrailWalk<'r, 'a> {
    route: &'r Route<'a>,
    start: NodeId,
    end: NodeId,
    srow: &'r Row,
    trail: Vec<RelId>,
    used: UsedSet,
}

impl<'r, 'a> TrailWalk<'r, 'a> {
    /// The walk's one construction: the partition's endpoints, start row and
    /// predicate, with the trail and used-set buffers empty.
    pub(super) fn new(route: &'r Route<'a>, start: NodeId, end: NodeId, srow: &'r Row) -> Self {
        Self {
            route,
            start,
            end,
            srow,
            trail: Vec::new(),
            used: UsedSet::default(),
        }
    }

    /// Enumerate the trails of exactly `length` relationships from `at` to the
    /// partition's end under the search's direction, types, and relationship
    /// uniqueness, deciding the predicate at each completed trail. A
    /// satisfying trail is bound and pushed where it is found — no candidate
    /// is copied out of the walk, and with a single-path selector the walk
    /// stops at the first one. Returns whether any trail satisfied the
    /// predicate.
    ///
    /// Every frame ticks the statement's cancellation window (ADR-0111), so a
    /// dense graph stays interruptible *inside* one length's enumeration
    /// instead of only at the next length's boundary.
    pub(super) fn any_of_length(
        &mut self,
        at: NodeId,
        length: u32,
        out: &mut MatchedSink<'_, '_>,
    ) -> Result<bool> {
        self.route.ctx.graph.cancel_tick()?;
        if self.trail.len() == length as usize {
            if at != self.end {
                return Ok(false);
            }
            return self
                .route
                .offer_trail(self.start, self.end, &self.trail, self.srow, out);
        }
        // Accumulate the satisfied flag across frames and return it after
        // the loop: `ALL SHORTEST` walks the whole length too, and the
        // caller (feasible_partition) must see that this length satisfied
        // so it stops at the shortest satisfying length instead of
        // walking past it (which returned non-shortest trails).
        let mut any_satisfied = false;
        for (next, rid) in self.route.search.hops(at)? {
            if self.route.halted() {
                break;
            }
            if !self.enter(rid) {
                continue;
            }
            let satisfied = self.any_of_length(next, length, out)?;
            self.leave(rid);
            if satisfied && !self.route.shape.all {
                return Ok(true);
            }
            any_satisfied |= satisfied;
        }
        Ok(any_satisfied)
    }

    /// Extend the walk by one hop — the trail's order and the uniqueness set
    /// move together, in one place, so the recursion has no second spot to
    /// keep in sync. `false` when the relationship is already consumed.
    fn enter(&mut self, rid: RelId) -> bool {
        if !self.used.insert(rid) {
            return false;
        }
        self.trail.push(rid);
        true
    }

    /// Undo one hop's extension.
    fn leave(&mut self, rid: RelId) {
        self.trail.pop();
        self.used.remove(&rid);
    }
}
