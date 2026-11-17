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
use super::far_set::FarSet;
use super::route::Route;
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
    /// Answer the fan from `origins` in batches, or `Ok(false)` when this
    /// fan keeps the per-origin search — before any row is pushed. The
    /// batch applies to a fan with a listed far side, several origins, and
    /// an unfiltered selection: a path predicate answers the shortest
    /// *satisfying* trail, which a distance does not decide; one origin
    /// has nothing to share; and a node-id span past the sweep's tables
    /// keeps the layered search's memory bounded by the ball.
    pub(super) fn fan_batched(
        &self,
        origins: &Side,
        way: Way,
        far: Option<&FarSet>,
        out: &mut MatchedSink<'_, '_>,
    ) -> Result<bool> {
        let (Some(far), Some(ids)) = (far, origins.ids()) else {
            return Ok(false);
        };
        if ids.len() < 2 || !enabled() || self.filters.path.is_some() || !self.search.sweep_fits() {
            return Ok(false);
        }
        let targets = far.ids();
        for chunk in ids.chunks(LANES) {
            if self.halted() {
                break;
            }
            // A lone trailing origin has nothing to share: the layered fan.
            match chunk {
                [origin] => self.fan(*origin, way, Some(far), out)?,
                _ => self.fan_chunk(chunk, way, (far, &targets), out)?,
            }
        }
        Ok(true)
    }

    /// One batch of at most [`LANES`] origins. Depth 0 first (an origin
    /// that is itself a far node), then one callback per sweep level that
    /// reaches far nodes, each reached (origin, far) pair one partition.
    fn fan_chunk(
        &self,
        ids: &[NodeId],
        way: Way,
        (far, targets): (&FarSet, &[NodeId]),
        out: &mut MatchedSink<'_, '_>,
    ) -> Result<()> {
        #[cfg(test)]
        BATCHES.with(|c| c.set(c.get() + 1));
        // Forward, the origin is the start, bound once; backward, each
        // reached node is a start, bound in its own partition. An origin
        // the start pattern refuses never joins the sweep.
        let mut rows = Vec::with_capacity(ids.len());
        let mut active = 0u64;
        for (lane, &origin) in ids.iter().enumerate() {
            self.ctx.graph.cancel_tick()?;
            match way {
                Way::Forward => {
                    let row = self.bind_start(origin)?;
                    active |= u64::from(row.is_some()) << lane;
                    rows.push(row);
                }
                Way::Backward => {
                    active |= 1 << lane;
                    rows.push(None);
                }
            }
        }
        // Far nodes each origin has reached, for the fan's early stop.
        let mut settled = vec![0usize; ids.len()];
        let mut retire = 0u64;
        for lane in lanes(active) {
            if self.halted() {
                return Ok(());
            }
            let origin = ids[lane];
            if far.contains(origin) {
                // The origin's own partition (s = e) needs no trails.
                self.fan_member(origin, origin, way, rows[lane].as_ref(), out, |_, _| {
                    Ok(Vec::new())
                })?;
                settled[lane] += 1;
                retire |= u64::from(settled[lane] == far.len()) << lane;
            }
        }
        active &= !retire;
        let search = way.search(&self.search);
        search.sweep_levels(ids, active, targets, |_, reached| {
            let any = reached.iter().fold(0, |all, &(_, mask)| all | mask);
            let mut retire = 0u64;
            for lane in lanes(any) {
                let before = out.folded();
                for &(node, mask) in reached {
                    if mask & (1 << lane) == 0 {
                        continue;
                    }
                    if self.halted() {
                        return Ok(u64::MAX);
                    }
                    settled[lane] += 1;
                    self.fan_member(ids[lane], node, way, rows[lane].as_ref(), out, |s, e| {
                        self.search.pair_trails(s, e)
                    })?;
                }
                // As the layered fan: every far node reached ends the
                // origin's search; a first-layer spelling ends it at
                // the first level that produced a row.
                let done = settled[lane] == far.len()
                    || (self.shape.spelling.first_layer_only() && out.folded() > before);
                retire |= u64::from(done) << lane;
            }
            Ok(if self.halted() { u64::MAX } else { retire })
        })
    }
}
