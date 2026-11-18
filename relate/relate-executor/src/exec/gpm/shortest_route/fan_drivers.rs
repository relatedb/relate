//! The fan drivers: the per-origin layered fan and the batched fan, which
//! runs the fan's origins [`LANES`] at a time through one multi-source
//! level sweep (ADR-0612) instead of one layered search per origin. The
//! sweep answers, per level, which origins first reach which far node;
//! each such pair is then one partition whose trails are rebuilt by the
//! pair search, bounded by the known distance. The partition body, the
//! end-pattern admission and the selector are the two drivers' shared
//! protocol; [`Route::fan_origins`] is the one dispatch.

use std::borrow::Cow;
#[cfg(test)]
use std::cell::Cell;

use relate_core::{NodeId, RelId, Result};

use crate::row::Row;
use crate::shortest::layered::Layer;
use crate::shortest::multi_source::LANES;

use super::anchor::{Side, Way};
use super::far_set::{retires, FarSet, OneLane, Settled};
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

/// One origin's fan run: what every layer of its search needs beside the
/// layer itself.
struct FanRun<'r> {
    origin: NodeId,
    /// The way this fan runs and where its partitions get their start row,
    /// one value ([`OriginRow`]): the way is derived from it, so the two
    /// cannot disagree.
    origin_row: OriginRow,
    /// The listed far side with this origin's reached count ([`OneLane`]):
    /// a settled counter never exists without its far set. `None`: no
    /// listed far side — each reached node is tested against the end
    /// pattern alone.
    far: Option<OneLane<'r>>,
}

/// Where a fan's partitions get their start row, and the way that follows
/// from it: the forward fan binds the origin once (the row), the backward
/// fan binds each reached node as its own start. One value, so the way and
/// the row cannot disagree — a refused forward origin is `None` at
/// [`Route::bind_origin`], never a third variant here.
enum OriginRow {
    Bound(Row),
    PerMember,
}

impl OriginRow {
    /// The way this bind runs — the one derivation: a bound origin row is
    /// a forward fan, a per-member binding a backward one.
    fn way(&self) -> Way {
        match self {
            Self::Bound(_) => Way::Forward,
            Self::PerMember => Way::Backward,
        }
    }
}

/// Which level of a batched fan one `fan_level` call drives — the mode as
/// a type, not a bool: the origins' own level (depth 0, the self-pair
/// partitions — no trail source, and no first-layer retirement) or a
/// swept level (real pairs through the search, retiring per the one
/// `retires` rule).
#[derive(Clone, Copy)]
enum SweepLevel {
    /// The origins' own level: the (origin, origin) self-pairs.
    Own,
    /// A level the sweep reached: real (s, e) pairs.
    Swept,
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
    fn batch_plan<'f>(&self, origins: &Side, far: Option<&'f FarSet>) -> Option<&'f FarSet> {
        far.filter(|_| {
            origins.ids().len() >= 2 && enabled() && !self.filtered() && self.search.sweep_fits()
        })
    }

    /// Answer the fan from `origins` along `way` against `far`: the
    /// batched sweep where the plan admits it ([`Self::batch_plan`]), the
    /// per-origin layered search otherwise — the one dispatch, beside the
    /// two drivers it picks between, so the run's anchor arms stay one
    /// call each.
    pub(super) fn fan_origins(
        &self,
        origins: &Side,
        way: Way,
        far: Option<&FarSet>,
        out: &mut MatchedSink<'_, '_>,
    ) -> Result<()> {
        match self.batch_plan(origins, far) {
            // The plan is the dispatch: a batched fan runs the sweep,
            // anything else keeps the per-origin search.
            Some(planned) => self.fan_batched(origins, way, planned, out),
            None => self.each_of(origins.ids(), |origin| self.fan(origin, way, far, out)),
        }
    }

    /// Answer the fan from the planned `ids` in batches, against the
    /// planned `far` set ([`Self::batch_plan`]'s one plan, the dispatch
    /// the caller already made) — before any row is pushed.
    fn fan_batched(
        &self,
        origins: &Side,
        way: Way,
        far: &FarSet,
        out: &mut MatchedSink<'_, '_>,
    ) -> Result<()> {
        let targets = far.ids();
        for chunk in origins.ids().chunks(LANES) {
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

    /// One endpoint fixed: one layered search from it, `way` saying which,
    /// reaches every node at the other end. Each reached node, and the
    /// origin itself, is one partition — emitted as its layer of the search
    /// completes (ADR-0555), so a halted consumer stops the search before
    /// its deeper layers are expanded.
    fn fan(
        &self,
        origin: NodeId,
        way: Way,
        far: Option<&FarSet>,
        out: &mut MatchedSink<'_, '_>,
    ) -> Result<()> {
        let Some(origin_row) = self.bind_origin(origin, way)? else {
            // A forward origin the start pattern refuses abandons the fan.
            return Ok(());
        };
        // The search's way is derived from the bound row — the one value
        // the run carries, so the two cannot disagree by construction.
        let search = origin_row.way().search(&self.search);
        let run = FanRun {
            origin,
            origin_row,
            far: far.map(OneLane::new),
        };
        search.dag_layers(origin, |layer| self.fan_layer(&layer, &run, out))?;
        Ok(())
    }

    /// Drive one completed fan layer: each reached node is one partition,
    /// and the layer is emitted as the search completes it (ADR-0555), so a
    /// halted consumer stops the search before its deeper layers expand.
    /// Returns whether the driving search should stop: the consumer
    /// halted, or the origin retired (every far node reached, or a
    /// first-layer spelling at its first productive level).
    fn fan_layer(
        &self,
        layer: &Layer<'_>,
        run: &FanRun<'_>,
        out: &mut MatchedSink<'_, '_>,
    ) -> Result<bool> {
        let FanRun {
            origin,
            origin_row,
            far,
        } = run;
        // The layer rides the route's one per-candidate loop.
        let before = self.emitted();
        self.each_of(layer.nodes(), |far_node| {
            // A listed far side is one bit test per reached node: a node
            // outside it fails the end pattern, so it is no partition
            // (and no count) — `settle_if_hit` owns the pairing.
            let admitted = far
                .as_ref()
                .is_none_or(|settled| settled.settle_if_hit(far_node));
            if admitted {
                self.fan_partition(layer, *origin, far_node, origin_row, out)?;
            }
            Ok(())
        })?;
        let productive = self.productive(layer.nodes() == [*origin], before);
        Ok(self.halted()
            || retires(
                far.as_ref().is_some_and(OneLane::done),
                self.shape.spelling.first_layer_only(),
                productive,
            ))
    }

    /// The partition of `far`, a node a fan search reached from `origin`:
    /// the start row (the forward fan's one bound origin row, or the reached
    /// node's own binding for a backward fan), then the trails the search
    /// holds to `far`.
    fn fan_partition(
        &self,
        layer: &Layer<'_>,
        origin: NodeId,
        far: NodeId,
        origin_row: &OriginRow,
        out: &mut MatchedSink<'_, '_>,
    ) -> Result<()> {
        self.fan_member(
            origin,
            far,
            origin_row,
            out,
            Some(|_, _| Ok(origin_row.way().orient(layer.trails(self.ctx.graph, far)?))),
        )
    }

    /// The one fan partition body: the start row, then the pair source's
    /// trails for the oriented pair — the layered search's held trails
    /// ([`Self::fan_partition`]) or the batched sweep's pair rebuild
    /// ([`Self::fan_chunk`]); the origin's own layer is the self-pair,
    /// which carries no source and constructs its trails.
    fn fan_member(
        &self,
        origin: NodeId,
        far: NodeId,
        origin_row: &OriginRow,
        out: &mut MatchedSink<'_, '_>,
        trails: Option<impl FnOnce(NodeId, NodeId) -> Result<Vec<Vec<RelId>>>>,
    ) -> Result<()> {
        let (s, e) = origin_row.way().ends(origin, far);
        let srow = match origin_row {
            OriginRow::Bound(row) => Cow::Borrowed(row),
            OriginRow::PerMember => match self.bind_start(s)? {
                Some(row) => Cow::Owned(row),
                None => return Ok(()),
            },
        };
        self.partition(s, e, &srow, out, trails)
    }

    /// What a fan running `way` binds for `origin` — the one home of the
    /// rule: a forward fan binds the origin once as its start
    /// ([`OriginRow::Bound`]), or the start pattern refuses it (`None`, the
    /// fan abandoned); a backward fan binds no row here
    /// ([`OriginRow::PerMember`]) — each reached node is a start, bound in
    /// its own partition.
    fn bind_origin(&self, origin: NodeId, way: Way) -> Result<Option<OriginRow>> {
        match way {
            Way::Forward => Ok(self.bind_start(origin)?.map(OriginRow::Bound)),
            Way::Backward => Ok(Some(OriginRow::PerMember)),
        }
    }

    /// The lanes this batch runs: one `(origin, row)` pair per origin that
    /// bound — a forward lane the start pattern refuses never joins the
    /// batch, and a halt leaves the remaining origins unbound, unjoined
    /// too. The batch's width is at most [`LANES`] (the caller chunks to
    /// it), so the sweep's masks always fit.
    fn bind_lanes(&self, ids: &[NodeId], way: Way) -> Result<Vec<(NodeId, OriginRow)>> {
        let mut lanes = Vec::with_capacity(ids.len());
        // The route's one per-candidate loop ([`Route::each_of`]): the
        // cancellation tick and the halt short-circuit come with it.
        self.each_of(ids, |origin| {
            // The one origin-bind rule ([`Route::bind_origin`]): a forward
            // lane binds its origin once; a backward lane binds no row
            // here.
            if let Some(row) = self.bind_origin(origin, way)? {
                lanes.push((origin, row));
            }
            Ok(())
        })?;
        Ok(lanes)
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
        let lanes = self.bind_lanes(ids, way)?;
        debug_assert!(lanes.len() <= LANES, "a batch is at most LANES lanes wide");
        let settled = Settled::new(lanes.len(), far);
        // Depth 0 is a level like any other: each lane whose origin is
        // itself a far node, its own self-pair partition and no trails.
        let depth0: Vec<(NodeId, u64)> = lanes
            .iter()
            .enumerate()
            .filter(|&(_, (origin, _))| far.contains(*origin))
            .map(|(lane, (origin, _))| (*origin, 1u64 << lane))
            .collect();
        let retire = self.fan_level(&lanes, &settled, SweepLevel::Own, &depth0, out)?;
        // Every lane of the narrowed batch runs: the mask is structural,
        // not re-derived. An empty batch (every origin refused, or a
        // halted bind) has nothing to sweep.
        let mut active = if lanes.is_empty() {
            0
        } else {
            u64::MAX >> (LANES - lanes.len())
        };
        // No lane left to run: a halted consumer retires every lane, and a
        // depth-0 pass may finish them all — either way the sweep has
        // nothing to allocate its tables for.
        active &= !retire;
        if active == 0 {
            return Ok(());
        }
        // The sweep is seeded with the batch's own origins — the
        // narrowed list, the one index space the lanes, the settled
        // counts and the partitions share: a refused origin never shifts
        // a later lane's key.
        let origins: Vec<NodeId> = lanes.iter().map(|&(origin, _)| origin).collect();
        let search = way.search(&self.search);
        search.sweep_levels(&origins, active, targets, |_, reached| {
            self.fan_level(&lanes, &settled, SweepLevel::Swept, reached, out)
        })
    }

    /// Drive one level's reached list: every `(far node, lane mask)` pair
    /// whose lane is in the mask is one partition, and the level retires
    /// the lanes it finishes. The level's kind ([`SweepLevel`]) carries
    /// its two consequences: the origins' own level is the self-pairs (no
    /// trail source, and no first-layer retirement — the origin's own
    /// layer carries its closed trails, which a shorter open trail still
    /// beats); a swept level rebuilds each real pair's trails.
    fn fan_level(
        &self,
        batch: &[(NodeId, OriginRow)],
        settled: &Settled<'_>,
        level: SweepLevel,
        reached: &[(NodeId, u64)],
        out: &mut MatchedSink<'_, '_>,
    ) -> Result<u64> {
        // The level's kind is invariant for the whole call, decided once:
        // every swept level is a real pair through the search, the
        // origins' own level the self-pair (no source, its constructed
        // trails).
        let swept = matches!(level, SweepLevel::Swept);
        let any = reached.iter().fold(0, |all, &(_, mask)| all | mask);
        let mut retire = 0u64;
        for lane in lanes(any) {
            let before = self.emitted();
            // A lane's (origin, row) pair, bound once (`bind_lanes`) — the
            // lane's row is structural, not re-derived.
            let (origin, origin_row) = &batch[lane];
            for &(node, mask) in reached {
                if mask & (1 << lane) == 0 {
                    continue;
                }
                if self.halted() {
                    return Ok(u64::MAX);
                }
                settled.settle(lane);
                let trails = swept.then_some(|s: NodeId, e: NodeId| self.search.pair_trails(s, e));
                self.fan_member(*origin, node, origin_row, out, trails)?;
            }
            // As the layered fan, one rule (`retires`): every far node
            // reached ends the origin's search; a first-layer spelling
            // ends it at the first level that produced a row (a swept
            // level — the origins' own layer never ends it). The
            // unfiltered half of the layered driver's precondition is
            // not restated here — `batch_plan` admits only unfiltered
            // routes, the admission's one home.
            let productive = self.productive(!swept, before);
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
