//! The overlay's neighbor-walk family (LABS-801) — the [`ExecGraph`]
//! mirror of the base layer's `graph/adjacency.rs`: every engine walk
//! enters through the resolved spellings, which compare interned ids on
//! the committed side and the filter's names on the write overlay's
//! string-typed rel copies. The name-keyed spellings left here
//! ([`ExecGraph::for_each_neighbor`], [`ExecGraph::try_find_neighbor`])
//! are the plan boundary's adapters for the callers that still hold
//! written names; each resolves once and delegates.

use std::collections::HashSet;

use relate_core::{NodeId, RelId};
use relate_ir::{Direction, RelPattern};

use crate::store::RelTypes;

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
    /// (LABS-801): the committed adjacency walk compares interned ids; the
    /// write overlay's rel copies carry type strings, so that arm compares
    /// the filter's names (`RelTypes::matches_name`).
    pub(crate) fn try_for_each_neighbor_resolved<E>(
        &self,
        id: NodeId,
        dir: Direction,
        types: &RelTypes,
        mut f: impl FnMut(RelId) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        let ts = self.snapshot_ts();
        let extras = self.scratch.map(|cell| {
            let s = cell.borrow();
            let mut v = Vec::new();
            match dir {
                Direction::Outgoing => {
                    if let Some(xs) = s.batch.out_adj.get(&id) {
                        v.extend_from_slice(xs);
                    }
                }
                Direction::Incoming => {
                    if let Some(xs) = s.batch.in_adj.get(&id) {
                        v.extend_from_slice(xs);
                    }
                }
                Direction::Both => {
                    if let Some(xs) = s.batch.out_adj.get(&id) {
                        v.extend_from_slice(xs);
                    }
                    if let Some(xs) = s.batch.in_adj.get(&id) {
                        v.extend_from_slice(xs);
                    }
                }
            }
            v
        });
        let mut seen = HashSet::new();
        self.base
            .try_for_each_neighbor_resolved_at(id, dir, ts, types, |rid| {
                // rel_exists is the canonical existence check: a rel deleted in
                // the overlay (or absent from the base) is not a neighbor.
                if !self.rel_exists(rid) {
                    return Ok(());
                }
                seen.insert(rid);
                f(rid)
            })?;
        for rid in extras.unwrap_or_default() {
            if !seen.insert(rid) {
                continue;
            }
            // The stub answers existence + type without cloning the
            // property map.
            let Some(rel) = self.rel_stub(rid) else {
                continue;
            };
            if !types.matches_name(&rel.rel_type) {
                continue;
            }
            f(rid)?;
        }
        Ok(())
    }
}
