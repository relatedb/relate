//! The batched fan: the fan's origins run [`LANES`] at a time through one
//! multi-source level sweep (ADR-0612) instead of one layered search per
//! origin. The sweep answers, per level, which origins first reach which
//! far node; each such pair is then one partition whose trails are rebuilt
//! by the pair search, bounded by the known distance. The partition body,
//! the end-pattern admission and the selector are the per-origin fan's.

#[cfg(test)]
use std::cell::Cell;

use relate_core::{NodeId, Result};

use crate::shortest::multi_source::LANES;

use super::anchor::{Side, Way};
use super::far_set::{retires, FarSet, Settled};
use super::route::{OriginRow, Route};
use super::MatchedSink;

#[cfg(test)]
thread_local! {
    static BATCHED_OFF: Cell<bool> = const { Cell::new(false) };
    static BATCHES: Cell<u64> = const { Cell::new(0) };
}

/// Run `f` with the batched fan off on this thread: the per-origin layered
/// fan is the oracle of the parity tests.
#[cfg(test)]
pub(super) fn without_batched_fan<T>(f: impl FnOnce() -> T) -> T {
    crate::exec::test_support::with_flag_disabled(&BATCHED_OFF, f)
}

/// Take and reset this thread's count of batches the sweep ran.
#[cfg(test)]
pub(super) fn take_batches() -> u64 {
    BATCHES.with(|c| c.replace(0))
}

fn enabled() -> bool {
    #[cfg(test)]
    if BATCHED_OFF.with(Cell::get) {
        return false;
    }
    true
}

/// The lanes a batch's rows say the sweep runs: a lane whose origin bound
/// carries a row (`Some`), the one membership fact — the mask is derived
/// from it, never carried beside it. One batch is at most [`LANES`] lanes
/// (the sweep's own width, [`Route::fan_batched`]'s chunking), so a bit
/// per lane fits the mask.
fn active_lanes(rows: &[Option<OriginRow>]) -> u64 {
    debug_assert!(rows.len() <= LANES, "a batch is at most LANES lanes wide");
    rows.iter().enumerate().fold(0u64, |mask, (lane, row)| {
        mask | u64::from(row.is_some()) << lane
    })
}

/// One active lane's origin row — the invariant's one home: a lane the
/// sweep runs carries a row ([`OriginRow::Bound`] for a forward lane,
/// [`OriginRow::PerMember`] for a backward one; `bind_lanes` leaves `None`
/// only for a lane the start pattern refuses, and [`active_lanes`] reads
/// that as inactive).
fn lane_row(rows: &[Option<OriginRow>], lane: usize) -> &OriginRow {
    rows[lane]
        .as_ref()
        .expect("an active lane carries its origin row")
}

/// The set bits of `mask`, as lane numbers.
fn lanes(mut mask: u64) -> impl Iterator<Item = usize> {
    std::iter::from_fn(move || {
        (mask != 0).then(|| {
            let lane = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            lane
        })
    })
}

impl Route<'_> {
    /// The batch this fan runs as, or `None` when it keeps the per-origin
    /// search. The batch applies to a fan with a listed far side, several
    /// origins, and an unfiltered selection: a path predicate answers the
    /// shortest *satisfying* trail, which a distance does not decide; one
    /// origin has nothing to share; and a node-id span past the sweep's
    /// tables keeps the layered search's memory bounded by the ball.
    pub(super) fn batch_plan<'o, 'f>(
        &self,
        origins: &'o Side,
        far: Option<&'f FarSet>,
    ) -> Option<(&'o [NodeId], &'f FarSet)> {
        let far = far?;
        let ids = origins.ids();
        (ids.len() >= 2 && enabled() && self.filters.path.is_none() && self.search.sweep_fits())
            .then_some((ids, far))
    }

    /// Answer the fan from the planned `ids` in batches, against the
    /// planned `far` set ([`Self::batch_plan`]'s one plan, the dispatch
    /// the caller already made) — before any row is pushed.
    pub(super) fn fan_batched(
        &self,
        ids: &[NodeId],
        way: Way,
        far: &FarSet,
        out: &mut MatchedSink<'_, '_>,
    ) -> Result<()> {
        let targets = far.ids();
        for chunk in ids.chunks(LANES) {
            if self.halted() {
                break;
            }
            // A lone trailing origin has nothing to share: the layered fan.
            match chunk {
                [origin] => self.fan(*origin, way, Some(far), out)?,
                _ => self.fan_chunk(chunk, way, far, &targets, out)?,
            }
        }
        Ok(())
    }

    /// Each lane's start row — `None` where the start pattern refuses the
    /// lane, which the sweep then skips (the joining lanes are derived
    /// from these rows, [`active_lanes`]). Forward, the origin is the
    /// start, bound once; backward, each reached node is a start, bound in
    /// its own partition.
    fn bind_lanes(&self, ids: &[NodeId], way: Way) -> Result<Vec<Option<OriginRow>>> {
        let mut rows = Vec::with_capacity(ids.len());
        // The route's one per-candidate loop ([`Route::each_of`]): the
        // cancellation tick and the halt short-circuit come with it, and a
        // halt simply leaves the remaining lanes unbound — they are not
        // lanes the sweep runs ([`active_lanes`] reads the rows).
        self.each_of(ids, |origin| {
            // The one origin-bind rule ([`Route::bind_origin`]): a
            // forward lane binds its origin once and leaves the sweep when
            // the start pattern refuses it; a backward lane binds no row
            // here.
            rows.push(self.bind_origin(origin, way)?);
            Ok(())
        })?;
        Ok(rows)
    }

    /// One batch of at most [`LANES`] origins. Depth 0 first (an origin
    /// that is itself a far node), then one callback per sweep level that
    /// reaches far nodes, each reached (origin, far) pair one partition.
    fn fan_chunk(
        &self,
        ids: &[NodeId],
        way: Way,
        far: &FarSet,
        targets: &[NodeId],
        out: &mut MatchedSink<'_, '_>,
    ) -> Result<()> {
        #[cfg(test)]
        BATCHES.with(|c| c.set(c.get() + 1));
        let rows = self.bind_lanes(ids, way)?;
        let mut active = active_lanes(&rows);
        let settled = Settled::new(ids.len(), far);
        // Depth 0 is a level like any other: each active lane whose origin
        // is itself a far node, its own self-pair partition and no trails.
        let depth0: Vec<(NodeId, u64)> = lanes(active)
            .filter(|&lane| far.contains(ids[lane]))
            .map(|lane| (ids[lane], 1u64 << lane))
            .collect();
        let retire = self.fan_level(ids, &rows, &settled, true, &depth0, out)?;
        // No lane left to run: a halted consumer retires every lane, and a
        // depth-0 pass may finish them all — either way the sweep has
        // nothing to allocate its tables for.
        active &= !retire;
        if active == 0 {
            return Ok(());
        }
        let search = way.search(&self.search);
        search.sweep_levels(ids, active, targets, |_, reached| {
            self.fan_level(ids, &rows, &settled, false, reached, out)
        })
    }

    /// Drive one level's reached list: every `(far node, lane mask)` pair
    /// whose lane is in the mask is one partition, and the level retires
    /// the lanes it finishes. `own_level` marks the origins' own
    /// self-pairs — no trails, and no first-layer retirement (the origin's
    /// own layer carries its closed trails, which a shorter open trail
    /// still beats); a swept level rebuilds each pair's trails.
    fn fan_level(
        &self,
        ids: &[NodeId],
        rows: &[Option<OriginRow>],
        settled: &Settled<'_>,
        own_level: bool,
        reached: &[(NodeId, u64)],
        out: &mut MatchedSink<'_, '_>,
    ) -> Result<u64> {
        let any = reached.iter().fold(0, |all, &(_, mask)| all | mask);
        let mut retire = 0u64;
        for lane in lanes(any) {
            let before = self.emitted();
            // A lane's start row is bound once (`bind_lanes`): derive its
            // `OriginRow` once per lane, not per partition.
            let origin_row = lane_row(rows, lane);
            for &(node, mask) in reached {
                if mask & (1 << lane) == 0 {
                    continue;
                }
                if self.halted() {
                    return Ok(u64::MAX);
                }
                settled.settle(lane);
                let trails =
                    (!own_level).then_some(|s: NodeId, e: NodeId| self.search.pair_trails(s, e));
                self.fan_member(ids[lane], node, origin_row, out, trails)?;
            }
            // As the layered fan, one rule (`retires`): every far node
            // reached ends the origin's search; a first-layer spelling
            // ends it at the first level that produced a row. The
            // unfiltered half of the layered driver's precondition is
            // not restated here — `batch_plan` admits only unfiltered
            // routes, the admission's one home.
            let productive = !own_level && self.emitted() > before;
            let done = retires(
                settled.done(lane),
                self.shape.spelling.first_layer_only(),
                productive,
            );
            retire |= u64::from(done) << lane;
        }
        Ok(if self.halted() { u64::MAX } else { retire })
    }
}
