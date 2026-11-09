//! The overlay's neighbor-walk family (LABS-801) — the [`ExecGraph`]
//! mirror of the base layer's `graph/adjacency.rs`: every engine walk
//! enters through the resolved spellings, which compare interned ids on
//! the committed side and the filter's names on the write overlay's
//! string-typed rel copies. The name-keyed spellings left here
//! ([`ExecGraph::for_each_neighbor`], [`ExecGraph::try_find_neighbor`])
//! are the plan boundary's adapters for the callers that still hold
//! written names; each resolves once and delegates.

use std::cell::RefCell;

use relate_core::{NodeId, RelId};
use relate_ir::{Direction, RelPattern};

use crate::store::RelTypes;

use super::scratch::WriteScratch;
use super::ExecGraph;

impl ExecGraph<'_> {
    pub fn neighbors(&self, id: NodeId, dir: Direction) -> Vec<RelId> {
        let mut out = Vec::new();
        self.for_each_neighbor(id, dir, &[], |rid| out.push(rid));
        out
    }

    /// Overlay-visible neighbors without cloning the base adjacency `Vec`.
    /// The name-keyed boundary spelling: the pattern's types resolve once
    /// here, then [`Self::for_each_neighbor_resolved`] compares ids.
    pub fn for_each_neighbor(
        &self,
        id: NodeId,
        dir: Direction,
        types: &[String],
        f: impl FnMut(RelId),
    ) {
        let types = self.rel_types(types);
        self.for_each_neighbor_resolved(id, dir, &types, f);
    }

    /// [`Self::for_each_neighbor`] over the resolved filter (LABS-801).
    pub(crate) fn for_each_neighbor_resolved(
        &self,
        id: NodeId,
        dir: Direction,
        types: &RelTypes,
        mut f: impl FnMut(RelId),
    ) {
        self.try_for_each_neighbor_resolved(id, dir, types, |rid| {
            f(rid);
            Ok::<(), std::convert::Infallible>(())
        })
        .expect("the closure is infallible");
    }

    /// Approximate adjacency degree (see [`Graph::adj_len`]) — side
    /// selection for existence probes (LABS-193). Base delegation only:
    /// deliberately not snapshot-filtered (reads.rs's prologue contract).
    pub fn adj_len(&self, id: NodeId, dir: Direction) -> usize {
        self.base.adj_len(id, dir)
    }

    /// See [`Graph::typed_adj_len`] — over the resolved filter
    /// (LABS-801). Base delegation only, like [`Self::adj_len`].
    pub(crate) fn typed_adj_len_resolved(
        &self,
        id: NodeId,
        dir: Direction,
        types: &RelTypes,
    ) -> usize {
        self.base.typed_adj_len(id, dir, types)
    }

    /// The first neighbor of `id` (in `dir`, of `types`) for which `f`
    /// returns `true`, stopping the walk there — the early-exit idiom for
    /// existence probes (LABS-193). No match: `Ok(None)`. The name-keyed
    /// boundary spelling: resolves once, then delegates to
    /// [`Self::try_find_neighbor_resolved`].
    pub fn try_find_neighbor<E>(
        &self,
        id: NodeId,
        dir: Direction,
        types: &[String],
        f: impl FnMut(RelId) -> std::result::Result<bool, E>,
    ) -> std::result::Result<Option<RelId>, E> {
        let types = self.rel_types(types);
        self.try_find_neighbor_resolved(id, dir, &types, f)
    }

    /// [`Self::try_find_neighbor`] over the resolved filter (LABS-801).
    pub(crate) fn try_find_neighbor_resolved<E>(
        &self,
        id: NodeId,
        dir: Direction,
        types: &RelTypes,
        mut f: impl FnMut(RelId) -> std::result::Result<bool, E>,
    ) -> std::result::Result<Option<RelId>, E> {
        enum Stop<E> {
            Hit(RelId),
            Err(E),
        }
        let walk: std::result::Result<(), Stop<E>> =
            self.try_for_each_neighbor_resolved(id, dir, types, |rid| match f(rid) {
                Ok(true) => Err(Stop::Hit(rid)),
                Ok(false) => Ok(()),
                Err(e) => Err(Stop::Err(e)),
            });
        match walk {
            Ok(()) => Ok(None),
            Err(Stop::Hit(rid)) => Ok(Some(rid)),
            Err(Stop::Err(e)) => Err(e),
        }
    }

    /// `names` — a pattern's written type list — resolved to the interned
    /// domain once (LABS-801), the boundary every engine walk crosses
    /// before its per-edge loop.
    pub(crate) fn rel_types<T: AsRef<str>>(&self, names: &[T]) -> RelTypes {
        self.base.rel_types(names)
    }

    /// [`Self::rel_types`] for a planned pattern.
    pub(crate) fn rel_types_of(&self, rel: &RelPattern) -> RelTypes {
        self.base.rel_types_of(rel)
    }

    /// Overlay-visible neighbors under the resolved type filter
    /// (LABS-801) — the rid-only spelling of
    /// [`Self::try_for_each_hop_resolved`], the same relation the base
    /// layer states between its two walks.
    pub(crate) fn try_for_each_neighbor_resolved<E>(
        &self,
        id: NodeId,
        dir: Direction,
        types: &RelTypes,
        mut f: impl FnMut(RelId) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        self.try_for_each_hop_resolved(id, dir, types, |rid, _| f(rid))
    }

    /// The neighbor walk yielding the reached node with each relationship
    /// — the hop spelling of the traversal kernels and the shortest-path
    /// searches. The read-only arm: nothing to dedup against and nothing
    /// hidden, so the base walk's own visibility check at the pinned ts is
    /// the complete answer — the write overlay's per-hop existence
    /// re-check is an overlay cost a read-only traversal must not pay. The
    /// write arm is [`Self::try_for_each_hop_overlay`].
    pub(crate) fn try_for_each_hop_resolved<E>(
        &self,
        id: NodeId,
        dir: Direction,
        types: &RelTypes,
        f: impl FnMut(RelId, NodeId) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        match self.scratch {
            None => self
                .base
                .try_for_each_hop_resolved_at(id, dir, self.snapshot_ts(), types, f),
            Some(cell) => self.try_for_each_hop_overlay(cell, id, dir, types, f),
        }
    }

    /// The one write-overlay walk (LABS-801): committed rels ride the
    /// base's fused hop walk — one version-chain read per hop, the peer
    /// carried along — gated by the scratch prologue's existence check
    /// (a rel deleted in the overlay is not a neighbor); the scratch
    /// extras' rel copies answer type and endpoints from their one record
    /// (the copies carry type strings, so the filter compares names,
    /// `RelTypes::matches_name`).
    fn try_for_each_hop_overlay<E>(
        &self,
        scratch: &RefCell<WriteScratch>,
        id: NodeId,
        dir: Direction,
        types: &RelTypes,
        mut f: impl FnMut(RelId, NodeId) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        let ts = self.snapshot_ts();
        let extras = Self::scratch_hops(scratch, id, dir, types);
        self.base
            .try_for_each_hop_resolved_at(id, dir, ts, types, |rid, peer| {
                // rel_exists is the canonical existence check: a rel
                // deleted in the overlay is not a neighbor.
                if !self.rel_exists(rid) {
                    return Ok(());
                }
                f(rid, peer)
            })?;
        for (rid, peer) in extras {
            f(rid, peer)?;
        }
        Ok(())
    }

    /// The write scratch's hops out of `id` under `dir` and the type
    /// filter, with the node each reaches, gathered under the one borrow —
    /// no `RefCell` borrow is alive while the caller's closure runs. The
    /// direction picks the scratch lists (out, in, or out then in), the
    /// peer is the one `hop_other` rule the base walk applies, and a rel
    /// id already gathered (a self-loop sits in both lists) is skipped —
    /// the base walk's `Both` rule. Every entry is a key of `batch.rels`
    /// (absent = created-then-deleted in this batch), so one record read
    /// answers type, endpoints and peer.
    fn scratch_hops(
        scratch: &RefCell<WriteScratch>,
        id: NodeId,
        dir: Direction,
        types: &RelTypes,
    ) -> Vec<(RelId, NodeId)> {
        let s = scratch.borrow();
        let lists = match dir {
            Direction::Outgoing => [Some(&s.batch.out_adj), None],
            Direction::Incoming => [Some(&s.batch.in_adj), None],
            Direction::Both => [Some(&s.batch.out_adj), Some(&s.batch.in_adj)],
        };
        let mut seen = crate::UsedSet::default();
        let mut hops = Vec::new();
        for list in lists.into_iter().flatten() {
            for &rid in list.get(&id).into_iter().flatten() {
                let Some(rel) = s.batch.rels.get(&rid) else {
                    continue;
                };
                if !types.matches_name(&rel.rel_type) {
                    continue;
                }
                // Scratch adjacency is written only by `create_rel` under the
                // rel's own endpoint (`out_adj[start]`, `in_adj[end]`) and a
                // scratch rel never re-points, so the peer always resolves.
                let peer = crate::store::hop_other(id, rel.start, rel.end, dir).expect(
                    "a scratch adjacency entry lies under the endpoint list that yielded it",
                );
                if seen.insert(rid) {
                    hops.push((rid, peer));
                }
            }
        }
        hops
    }
}
