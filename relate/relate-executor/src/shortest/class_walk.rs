//! The trails of one exact length between two nodes (ADR-0607): the
//! length-class enumeration `SHORTEST k` and `SHORTEST k GROUPS` select
//! from, one class at a time in ascending length.
//!
//! A distance ball grows from the end, against the pattern's direction. A
//! depth-first walk from the start then extends a trail only through
//! relationships it has not used, and abandons a prefix whose remaining
//! length is below a lower bound on the distance to the end: exact inside
//! the ball, one past the ball's radius outside it. Walk distances bound
//! trail distances from below, so no trail is lost.
//!
//! How far the ball grows decides the cost. A walk node is expanded without
//! a bound while the ball's radius plus one fits in what is left of the
//! class length, so a ball of radius `e` leaves the first `length - e - 1`
//! layers of the walk unbounded. The radii are chosen the way a
//! bidirectional search chooses them: a counting ball from the start (never
//! walked, only sized) and the end ball grow in turn, always the one with
//! the smaller frontier, until the two radii sum to the class length. A hub
//! start then stays at radius one while the end ball, whose layers are
//! small, does the rest, instead of the walk expanding every neighbour of
//! the hub.

use relate_core::tokens::WorkspaceCharge;
use relate_core::{NodeId, RelId, Result};
use relate_ir::Direction;

use crate::hash_seed::IdMap;
use crate::UsedSet;

use super::Search;

/// Bytes charged per ball entry: the id, its depth and the table's control
/// byte and load slack.
const ENTRY_BYTES: u64 = 24;

/// Bytes charged per recorded hop: its entry in the log and the `Vec` slot
/// behind it, each about one ball entry.
const RECORDED_BYTES: u64 = 2 * ENTRY_BYTES;

/// Every hop a recording ball's growth followed, keyed by the node it
/// reached: the relationship and the scanned node it came from. Read against
/// the walk's direction this is the walk's own step into the ball, answered
/// without scanning the far node's adjacency.
#[derive(Default)]
struct HopLog {
    into: IdMap<NodeId, Vec<(NodeId, RelId)>>,
    count: u64,
}

/// The distances to one node, layer by layer: a breadth-first ball grown
/// along `dir` over the walk's one [`Search`].
struct Ball<'a, 'g> {
    search: &'a Search<'a, 'g>,
    dir: Direction,
    depth: IdMap<NodeId, u32>,
    frontier: Vec<NodeId>,
    radius: u32,
    /// `Some` for a recording ball, `None` for a counting one.
    log: Option<HopLog>,
    /// The ball's retained size, charged while it lives (ADR-0463).
    charge: WorkspaceCharge,
}

impl<'a, 'g> Ball<'a, 'g> {
    fn new(
        search: &'a Search<'a, 'g>,
        dir: Direction,
        origin: NodeId,
        log: Option<HopLog>,
    ) -> Self {
        let mut depth = IdMap::default();
        depth.insert(origin, 0);
        Self {
            search,
            dir,
            depth,
            frontier: vec![origin],
            radius: 0,
            log,
            charge: WorkspaceCharge::empty(),
        }
    }

    /// A ball that only counts distances.
    fn counting(search: &'a Search<'a, 'g>, dir: Direction, origin: NodeId) -> Self {
        Self::new(search, dir, origin, None)
    }

    /// A ball that also logs the hops it follows.
    fn recording(search: &'a Search<'a, 'g>, dir: Direction, origin: NodeId) -> Self {
        Self::new(search, dir, origin, Some(HopLog::default()))
    }

    /// Whether every node the ball can reach is already in it.
    fn exhausted(&self) -> bool {
        self.frontier.is_empty()
    }

    /// Grow one layer. The caller guarantees the ball is not exhausted: an
    /// empty frontier has nothing to grow, and [`ClassWalk::balance`] is the
    /// one place that decides which ball may.
    fn grow_layer(&mut self) -> Result<()> {
        debug_assert!(!self.exhausted(), "an exhausted ball has no layer to grow");
        let next_depth = self.radius + 1;
        let mut next = Vec::new();
        for &cur in &self.frontier {
            for (node, rid) in self.search.hops_along(self.dir, cur)? {
                if let Some(log) = self.log.as_mut() {
                    log.into.entry(node).or_default().push((cur, rid));
                    log.count += 1;
                }
                if let std::collections::hash_map::Entry::Vacant(slot) = self.depth.entry(node) {
                    slot.insert(next_depth);
                    next.push(node);
                }
            }
        }
        self.frontier = next;
        self.radius = next_depth;
        self.charge.resize_with(|| {
            let logged = self.log.as_ref().map_or(0, |log| log.count);
            (self.depth.capacity() as u64 + self.frontier.capacity() as u64)
                .saturating_mul(ENTRY_BYTES)
                .saturating_add(logged.saturating_mul(RECORDED_BYTES))
        })
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

    /// The logged hops into `node`, when the log can answer a walk with
    /// `left` relationships to go: every hop that can still reach the origin
    /// lands within `left - 1` of it, so the log answers once the ball has
    /// scanned that far (`left` at most its radius) or holds everything.
    /// `None` when there is no log or it cannot answer yet.
    fn recorded_hops(&self, node: NodeId, left: usize) -> Option<Vec<(NodeId, RelId)>> {
        let log = self.log.as_ref()?;
        (self.radius as usize >= left || self.exhausted())
            .then(|| log.into.get(&node).cloned().unwrap_or_default())
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
    /// The end's distances, against the pattern's direction: the walk's
    /// lower bound and, through its hop log, its steps into the ball.
    ball: Ball<'a, 'g>,
    /// Whether the last class walk cut a prefix the ball could still admit
    /// at a longer length (see [`Self::complete`]).
    cut: bool,
    /// The start's distances, grown only to size the walk's free part
    /// against the end ball's growth ([`Self::balance`]). Its exhaustion is
    /// also a reachability proof ([`Self::unreachable`]).
    start_ball: Ball<'a, 'g>,
}

impl<'a, 'g> ClassWalk<'a, 'g> {
    /// The enumerator of `search`'s trails from `start` to `end`.
    pub(crate) fn new(search: &'a Search<'a, 'g>, start: NodeId, end: NodeId) -> Self {
        Self {
            search,
            start,
            end,
            ball: Ball::recording(search, search.dir.flip(), end),
            start_ball: Ball::counting(search, search.dir, start),
            cut: true,
        }
    }

    /// Grow the two balls, the smaller frontier first, until their radii sum
    /// to `length` or the end ball holds everything it can reach. An end ball of
    /// radius `length - r` leaves the walk unbounded only through the first
    /// `r - 1` layers, where `r` is the start ball's radius.
    fn balance(&mut self, length: u32) -> Result<()> {
        // An exhausted end ball bounds every node exactly, so the walk has no
        // free part left to size and the start ball stays where it is.
        while self.ball.radius + self.start_ball.radius < length && !self.ball.exhausted() {
            // The smaller frontier first. An exhausted start ball cannot
            // grow, so the end ball does: the radii still sum to `length`.
            if self.start_ball.exhausted()
                || self.ball.frontier.len() <= self.start_ball.frontier.len()
            {
                self.ball.grow_layer()?;
            } else {
                self.start_ball.grow_layer()?;
            }
        }
        Ok(())
    }

    /// The two balls' radii, end ball first.
    #[cfg(test)]
    pub(super) fn radii(&self) -> (u32, u32) {
        (self.ball.radius, self.start_ball.radius)
    }

    /// Whether no trail of any length can exist: either ball holds everything
    /// it can reach and the other ball's origin is not in it. The start ball
    /// is the only proof when `balance` stopped before the end ball ran out.
    pub(crate) fn unreachable(&self) -> bool {
        (self.ball.exhausted() && !self.ball.depth.contains_key(&self.start))
            || (self.start_ball.exhausted() && !self.start_ball.depth.contains_key(&self.end))
    }

    /// The hops a walk may take out of `node` with `left` relationships to
    /// go. Every one that can still reach the end lands on a node within
    /// `left - 1` of it, so once the end ball has scanned all of those
    /// (`left` at most its radius, or the ball holds everything) they are
    /// the ball's recorded hops into `node`: a hub is not scanned to find
    /// the few of its neighbours near the end. Past that the node's own
    /// adjacency is the answer. A recorded read leaves out the neighbours
    /// beyond the scanned layers, which the ball bound would have cut: unless
    /// the ball holds everything, that is a cut ([`Self::complete`]).
    fn walk_hops(&mut self, node: NodeId, left: usize) -> Result<Vec<(NodeId, RelId)>> {
        match self.ball.recorded_hops(node, left) {
            Some(hops) => {
                self.search.graph.cancel_tick()?;
                self.cut |= !self.ball.exhausted();
                Ok(hops)
            }
            None => self.search.hops(node),
        }
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
        self.balance(length)?;
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
            hops: self.walk_hops(self.start, length)?,
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
                hops: self.walk_hops(next, length - at)?,
                next: 0,
            });
        }
        Ok(false)
    }
}
