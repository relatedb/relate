//! The trails of one exact length between two nodes (ADR-0607): the
//! length-class enumeration `SHORTEST k` and `SHORTEST k GROUPS` select
//! from, one class at a time in ascending length.
//!
//! A distance ball grows from the end, against the pattern's direction, to
//! half the class length. A depth-first walk from the start then extends a
//! trail only through relationships it has not used, and abandons a prefix
//! whose remaining length is below a lower bound on the distance to the end:
//! exact inside the ball, one past the ball's radius outside it. Walk
//! distances bound trail distances from below, so no trail is lost. The
//! free part of the walk is about half the class length deep; past that the
//! ball keeps it on routes that can still reach the end.

use relate_core::tokens::WorkspaceCharge;
use relate_core::{NodeId, RelId, Result};

use crate::hash_seed::IdMap;
use crate::UsedSet;

use super::Search;

/// Bytes charged per ball entry: the id, its depth and the table's control
/// byte and load slack.
const ENTRY_BYTES: u64 = 24;

/// The distances to one node, layer by layer: a breadth-first ball.
struct Ball<'a, 'g> {
    search: Search<'a, 'g>,
    depth: IdMap<NodeId, u32>,
    frontier: Vec<NodeId>,
    radius: u32,
    /// The ball's retained size, charged while it lives (ADR-0463).
    charge: WorkspaceCharge,
}

impl<'a, 'g> Ball<'a, 'g> {
    fn new(search: Search<'a, 'g>, origin: NodeId) -> Self {
        let mut depth = IdMap::default();
        depth.insert(origin, 0);
        Self {
            search,
            depth,
            frontier: vec![origin],
            radius: 0,
            charge: WorkspaceCharge::empty(),
        }
    }

    /// Whether every node the ball can reach is already in it.
    fn exhausted(&self) -> bool {
        self.frontier.is_empty()
    }

    /// Grow whole layers until the ball reaches `radius` or runs out.
    fn grow_to(&mut self, radius: u32) -> Result<()> {
        while self.radius < radius && !self.exhausted() {
            let next_depth = self.radius + 1;
            let mut next = Vec::new();
            for &cur in &self.frontier {
                for (node, _) in self.search.hops(cur)? {
                    if let std::collections::hash_map::Entry::Vacant(slot) = self.depth.entry(node)
                    {
                        slot.insert(next_depth);
                        next.push(node);
                    }
                }
            }
            self.frontier = next;
            self.radius = next_depth;
            self.charge.resize_with(|| {
                (self.depth.capacity() as u64 + self.frontier.capacity() as u64)
                    .saturating_mul(ENTRY_BYTES)
            })?;
        }
        Ok(())
    }

    /// A lower bound on the walk distance from `node` to the origin: exact
    /// inside the ball, one past its radius outside it, and unbounded once
    /// the ball holds everything it can reach.
    fn lower_bound(&self, node: NodeId) -> u32 {
        match self.depth.get(&node) {
            Some(&d) => d,
            None if self.exhausted() => u32::MAX,
            None => self.radius + 1,
        }
    }
}

/// One walk frame: the hops out of a node and how many were tried.
struct Frame {
    hops: Vec<(NodeId, RelId)>,
    next: usize,
}

/// The trail enumerator of one (start, end) partition. Classes may be asked
/// in any order, but the ball only ever grows, so ascending is cheapest.
pub(crate) struct ClassWalk<'a, 'g> {
    search: &'a Search<'a, 'g>,
    start: NodeId,
    end: NodeId,
    ball: Ball<'a, 'g>,
    /// Whether the last class walk cut a prefix the ball could still admit
    /// at a longer length (see [`Self::complete`]).
    cut: bool,
}

impl<'a, 'g> ClassWalk<'a, 'g> {
    /// The enumerator of `search`'s trails from `start` to `end`.
    pub(crate) fn new(search: &'a Search<'a, 'g>, start: NodeId, end: NodeId) -> Self {
        Self {
            search,
            start,
            end,
            ball: Ball::new(search.reversed(), end),
            cut: true,
        }
    }

    /// Whether no trail of any length can exist: the end's ball holds
    /// everything it can reach and the start is not in it.
    pub(crate) fn unreachable(&self) -> bool {
        self.ball.exhausted() && !self.ball.depth.contains_key(&self.start)
    }

    /// Whether the last class walk proved no longer trail exists: it found no
    /// trail and cut no prefix on a ball bound that a longer class could
    /// admit, so every trail from the start ended before its length. A cut
    /// at a node the exhausted ball cannot reach is permanent and does not
    /// count. This ends an unbounded pattern's class loop at the real
    /// longest trail instead of the relationship count.
    pub(crate) fn complete(&self) -> bool {
        !self.cut
    }

    /// Call `visit(trail, nodes)` for every trail of exactly `length`
    /// relationships, `nodes` being the `length + 1` nodes it passes, until
    /// `visit` returns `true` (stop). Returns whether it stopped. Each frame
    /// ticks the statement's cancellation window through the hop reads.
    pub(crate) fn each_trail(
        &mut self,
        length: u32,
        mut visit: impl FnMut(&[RelId], &[NodeId]) -> Result<bool>,
    ) -> Result<bool> {
        self.ball.grow_to(length.div_ceil(2))?;
        // Conservative until the walk proves otherwise: an early return, a
        // stop and the zero-length class (closed trails may follow) leave it.
        self.cut = true;
        if self.unreachable() {
            return Ok(false);
        }
        let length = length as usize;
        let mut used = UsedSet::default();
        let mut trail: Vec<RelId> = Vec::with_capacity(length);
        let mut nodes: Vec<NodeId> = Vec::with_capacity(length + 1);
        nodes.push(self.start);
        if length == 0 {
            return if self.start == self.end {
                visit(&trail, &nodes)
            } else {
                Ok(false)
            };
        }
        self.cut = false;
        let mut frames = vec![Frame {
            hops: self.search.hops(self.start)?,
            next: 0,
        }];
        while let Some(frame) = frames.last_mut() {
            let Some(&(next, rid)) = frame.hops.get(frame.next) else {
                frames.pop();
                // Leave the hop that opened this frame, if any.
                if let Some(rid) = trail.pop() {
                    used.remove(&rid);
                    nodes.pop();
                }
                continue;
            };
            frame.next += 1;
            let at = trail.len() + 1;
            let remaining = (length - at) as u64;
            if used.contains(&rid) {
                continue;
            }
            let bound = self.ball.lower_bound(next);
            if u64::from(bound) > remaining {
                // A node outside the exhausted ball never reaches the end.
                self.cut |= bound != u32::MAX;
                continue;
            }
            if at == length {
                // The bound is zero only at the end: this is a trail of
                // the class.
                // A trail ending here may extend through the end again.
                self.cut = true;
                trail.push(rid);
                nodes.push(next);
                let stop = visit(&trail, &nodes)?;
                trail.pop();
                nodes.pop();
                if stop {
                    return Ok(true);
                }
                continue;
            }
            used.insert(rid);
            trail.push(rid);
            nodes.push(next);
            frames.push(Frame {
                hops: self.search.hops(next)?,
                next: 0,
            });
        }
        Ok(false)
    }
}
