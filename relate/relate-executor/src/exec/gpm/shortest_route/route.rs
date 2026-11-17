//! One routed pattern against one input row: the row's endpoint anchoring,
//! resolved once, picks the searches; each (start, end) partition binds
//! its selected trails the way the GPM walker does.

use std::borrow::Cow;
use std::cell::{Cell, OnceCell};

use relate_core::{NodeId, RelId, Result, Value};
use relate_ir::{MatchMode, NodePattern, PathPattern};

use crate::ctx::ExecCtx;
use crate::exec::candidates::node_ok;
use crate::exec::gpm::{gpm_bind_node, unbounded_dfs_cap};
use crate::pattern_walk::{bind_path_names, bind_rel_stub_list, memoized_path_rung};
use crate::row::{Row, RowRung};
use crate::shortest::layered::Layer;
use crate::shortest::{path_from_trail, Search};

use super::super::sink::MatchedSink;
use super::anchor::{Anchor, Way};
use super::far_set::{retires, FarSet, OneLane};
use super::filters::{PathFilter, RouteFilters};
use super::guard::{Keep, Shape};
use super::k_select;
use super::reach_cache::ReachCache;
use super::trail_walk;

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
pub(super) enum OriginRow {
    Bound(Row),
    PerMember,
}

impl OriginRow {
    /// The way this bind runs — the one derivation: a bound origin row is
    /// a forward fan, a per-member binding a backward one.
    pub(super) fn way(&self) -> Way {
        match self {
            Self::Bound(_) => Way::Forward,
            Self::PerMember => Way::Backward,
        }
    }
}

/// How a (start, end) partition selects its trails, decided once per route
/// in [`Route::try_new`].
#[derive(Clone, Copy)]
enum PartitionArm {
    /// An inline predicate filters the search: walk trails in ascending
    /// length and answer the first satisfying length.
    Filtered,
    /// A count selector: the length-class walk picks its `k`.
    Counted(Keep),
    /// The search's own trails.
    Plain,
}

/// One routed pattern evaluated against one input row.
pub(super) struct Route<'a> {
    pub(super) ctx: ExecCtx<'a>,
    path: &'a PathPattern,
    pub(super) row: &'a Row,
    pub(super) shape: Shape<'a>,
    pub(super) search: Search<'a, 'a>,
    /// The `WHERE` conjuncts, split by what they read. `path` is `Some`
    /// when a predicate filters the *search*: the route then walks trails
    /// in ascending length and answers the first length at which a trail
    /// satisfies it, instead of the one shortest trail. `pair` drops a
    /// same-node pair before 51N23 can reject it.
    pub(super) filters: RouteFilters<'a>,
    /// The partition's selection arm, from the selector and the filters.
    arm: PartitionArm,
    /// The unbounded DFS cap memo — the route-granularity spelling of the
    /// walk memo `GpmCtx::unbounded_dfs_cap` takes: filled on the first
    /// unbounded filtered partition, read by every later one, so the rel
    /// table is enumerated once per route instead of once per partition.
    /// A bounded pattern never fills it.
    unbounded_cap: OnceCell<u32>,
    /// The trail rows' path-name rung memo — built from the first bound
    /// trail's row (every trail binds the same names against the one
    /// input row), one per route, stamped by every later trail.
    path_rung: Cell<Option<RowRung>>,
    /// The rows this route offered, whatever the sink did with them: the
    /// fan drivers' "did this layer produce a row" fact, read as a
    /// before/after around a layer.
    emitted: Cell<usize>,
    /// The consumer's halt cell — a probe's verdict, or the collecting
    /// sink's cap-full cell: raised once it needs no more rows, read only
    /// through [`Self::halted`]. Every sink has one, so it is not optional.
    halt: &'a Cell<bool>,
    /// The pattern's reach cache: consecutive pair partitions from one
    /// start share one reach set (see [`ReachCache`]).
    pub(super) reach: &'a ReachCache,
}

impl<'a> Route<'a> {
    /// The fallible constructor: `None` is the route declining the pattern
    /// — the decision happens here, before the sink exists in the caller's
    /// hands, so a declined pattern can never have had a row pushed into
    /// it. A pattern whose predicate the search cannot decide against the
    /// rows it binds enumerates instead.
    pub(super) fn try_new(
        ctx: ExecCtx<'a>,
        path: &'a PathPattern,
        row: &'a Row,
        shape: Shape<'a>,
        sink: &MatchedSink<'a, '_>,
        reach: &'a ReachCache,
    ) -> Option<Self> {
        // The route's halt cell and its path filter both come FROM the
        // sink: the route stops when its sink halts, and the filter is
        // the sink's residual applied to this pattern — the
        // correspondences are established here, not by two caller-supplied
        // arguments that must happen to agree.
        let halt = sink.halt_cell();
        let filters = RouteFilters::split(path, &shape, sink.residual());
        // A count selector under an inline predicate would have to pick its
        // `k` among the trails that satisfy it; the length-class walk does
        // not carry the predicate, so enumeration answers that shape.
        let arm = match (shape.selection.counted(), filters.path.is_some()) {
            (Some(_), true) => return None,
            (Some(keep), false) => PartitionArm::Counted(keep),
            (None, true) => PartitionArm::Filtered,
            (None, false) => PartitionArm::Plain,
        };
        let rel = shape.rel;
        let search = Search {
            graph: ctx.graph,
            // LABS-801: the filter arrives resolved from the pattern-level
            // table; the clone shares its Arc, never re-resolves.
            types: shape.filter.types.clone(),
            dir: rel.direction,
            min: shape.min,
            max: shape.max.unwrap_or(u32::MAX),
            all: shape.selection.all(),
            skip: None,
        };
        let route = Self {
            ctx,
            path,
            row,
            shape,
            search,
            filters,
            arm,
            halt,
            reach,
            unbounded_cap: OnceCell::new(),
            path_rung: Cell::new(None),
            emitted: Cell::new(0),
        };
        // A predicate the search cannot decide against the rows it binds
        // enumerates instead (see `predicates_decidable`).
        route
            .filters
            .path
            .as_ref()
            .is_none_or(|filter| route.predicates_decidable(filter))
            .then_some(route)
    }

    /// The route's unbounded DFS cap, resolved once — the
    /// [`unbounded_dfs_cap`] value this route's filtered partitions read
    /// when the pattern is unbounded. The route guard admits only
    /// DIFFERENT RELATIONSHIPS patterns (the admission fact's one home,
    /// `route_shape`), so the cap is that arm: the live rel count, the
    /// sound bound of a trail that never revisits a relationship.
    fn memoized_unbounded_cap(&self) -> u32 {
        *self
            .unbounded_cap
            .get_or_init(|| unbounded_dfs_cap(self.ctx.graph, MatchMode::DifferentRelationships))
    }

    /// Whether a count selector (`SHORTEST k`, ADR-0607) picks this
    /// route's trails.
    pub(super) fn counted(&self) -> bool {
        matches!(self.arm, PartitionArm::Counted(_))
    }

    /// The deepest length a trail walk explores: the one GPM bound policy
    /// — a bounded pattern keeps its written bound, an unbounded one takes
    /// the engine's unbounded DFS cap (`unbounded_dfs_cap`), the same
    /// ceiling the GPM walk's two spellings read (`pattern_walk::
    /// hop_bounds_with` for the var-length rel, the quantifier's
    /// `unwrap_or_else` for the quantified element), so a walk explores to
    /// the same depth the enumeration fallback would. The cap arm runs only
    /// for an unbounded pattern, so a bounded one never pays the rel-table
    /// enumeration behind the memo.
    pub(super) fn walk_max(&self) -> u32 {
        self.shape
            .max
            .unwrap_or_else(|| self.memoized_unbounded_cap())
    }

    /// Push every selected row into `out` as it is bound: the row's
    /// anchoring, resolved once, picks the searches. A pattern whose
    /// minimum exceeds its maximum matches nothing (no row, no decline —
    /// the route took it). Declining is the anchoring's job
    /// ([`Route::anchor`], beside the constructor's declines), never the
    /// run's: once an anchor exists, every row it binds is pushed. The run
    /// stops early once the sink is halted.
    pub(super) fn run(&self, out: &mut MatchedSink<'_, '_>) -> Result<bool> {
        if self.search.min > self.search.max {
            return Ok(true);
        }
        let Some(anchor) = self.anchor()? else {
            return Ok(false);
        };
        #[cfg(test)]
        super::anchor::note_anchor(anchor.arm());
        match anchor {
            Anchor::Pair { starts, ends } => {
                self.each_of(starts.ids(), |s| {
                    self.each_of(ends.ids(), |e| self.pair_partition(s, e, out))
                })?;
            }
            Anchor::Fan { origins, way, far } => match self.batch_plan(&origins, far.as_ref()) {
                // The plan is the dispatch: a batched fan runs the sweep,
                // anything else keeps the per-origin search.
                Some((ids, far)) => self.fan_batched(ids, way, far, out)?,
                None => self.each_of(origins.ids(), |origin| {
                    self.fan(origin, way, far.as_ref(), out)
                })?,
            },
            Anchor::Loop => {
                self.each_start(|s| self.pair_partition(s, s, out))?;
            }
        };
        Ok(true)
    }

    /// Whether the search can decide every predicate it must evaluate, against
    /// the rows it binds: the input row plus the pattern's own variables (the
    /// start, the relationship, the end and the path).
    ///
    /// A predicate that also reads a variable a *later* pattern in the same
    /// `MATCH` binds is not decidable while the search runs. The route would
    /// pick the trail it cannot check and the caller's later predicate pass
    /// would then reject that already-chosen answer, hiding a longer trail
    /// that satisfies it — so [`Self::run`] declines such a pattern to
    /// enumeration, which applies the predicate to the complete row instead of
    /// guessing at a partial one.
    fn predicates_decidable(&self, filter: &PathFilter<'_>) -> bool {
        let mut probe = self.row.clone();
        // Text render: the probe row's names are String.
        let element_names: Vec<String> = [
            self.shape.start.variable.as_ref(),
            self.shape.rel.variable.as_ref(),
            self.shape.end.variable.as_ref(),
        ]
        .into_iter()
        .flatten()
        .map(|name| name.to_string())
        .collect();
        for name in element_names
            .into_iter()
            .chain(self.path.path_names().map(|name| name.to_string()))
        {
            if !probe.contains_key(&name) {
                probe.insert(name, Value::Null);
            }
        }
        filter.decidable_on(&probe)
    }

    fn node_ok(&self, id: NodeId, np: &NodePattern, row: &Row) -> Result<bool> {
        let ExecCtx { graph, params, .. } = self.ctx;
        node_ok(graph, id, np, row, params)
    }

    /// The route's one per-candidate loop: tick the statement's
    /// cancellation window, then drive the candidate. `each_start`, the
    /// fan arms and the pair product all ride it.
    pub(super) fn each_of(
        &self,
        ids: &[NodeId],
        mut f: impl FnMut(NodeId) -> Result<()>,
    ) -> Result<()> {
        for &id in ids {
            if self.halted() {
                break;
            }
            self.ctx.graph.cancel_tick()?;
            f(id)?;
        }
        Ok(())
    }

    /// Whether the consumer needs no more rows — the one reader of the
    /// halt cell, consulted at the loop heads that fan out.
    pub(super) fn halted(&self) -> bool {
        self.halt.get()
    }

    /// The input row with the start node bound, or `None` when `id` fails
    /// the start pattern.
    pub(super) fn bind_start(&self, id: NodeId) -> Result<Option<Row>> {
        self.bind_candidate(id, self.shape.start)
    }

    /// What a fan running `way` binds for `origin` — the one home of the
    /// rule: a forward fan binds the origin once as its start
    /// ([`OriginRow::Bound`]), or the start pattern refuses it (`None`, the
    /// fan abandoned); a backward fan binds no row here
    /// ([`OriginRow::PerMember`]) — each reached node is a start, bound in
    /// its own partition.
    pub(super) fn bind_origin(&self, origin: NodeId, way: Way) -> Result<Option<OriginRow>> {
        match way {
            Way::Forward => Ok(self.bind_start(origin)?.map(OriginRow::Bound)),
            Way::Backward => Ok(Some(OriginRow::PerMember)),
        }
    }

    /// The input row with `id` bound as the endpoint `np`, or `None` when
    /// `id` fails the pattern or its binding — the one admission-then-bind
    /// spelling the start partition and the endpoint listing share.
    pub(super) fn bind_candidate(&self, id: NodeId, np: &NodePattern) -> Result<Option<Row>> {
        if !self.node_ok(id, np, self.row)? {
            return Ok(None);
        }
        let mut row = self.row.clone();
        Ok(gpm_bind_node(self.ctx.graph, np, id, &mut row).then_some(row))
    }

    /// Both endpoints fixed: the pair search for single-path selectors, the
    /// bidirectional layered search for all-path ones.
    fn pair_partition(&self, s: NodeId, e: NodeId, out: &mut MatchedSink<'_, '_>) -> Result<()> {
        let Some(srow) = self.bind_start(s)? else {
            return Ok(());
        };
        self.partition(
            s,
            e,
            &srow,
            out,
            Some(|| {
                // A reachable end still takes the search below.
                if self.reach.unreachable(&self.search, s, e)? {
                    return Ok(Vec::new());
                }
                self.search.pair_trails(s, e)
            }),
        )
    }

    /// One endpoint fixed: one layered search from it, `way` saying which,
    /// reaches every node at the other end. Each reached node, and the
    /// origin itself, is one partition — emitted as its layer of the search
    /// completes (ADR-0555), so a halted consumer stops the search before
    /// its deeper layers are expanded.
    pub(super) fn fan(
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
        let run = FanRun {
            origin,
            origin_row,
            far: far.map(OneLane::new),
        };
        way.search(&self.search)
            .dag_layers(origin, |layer| self.fan_layer(&layer, &run, out))?;
        Ok(())
    }

    /// Drive one completed fan layer: each reached node is one partition,
    /// and the layer is emitted as the search completes it (ADR-0555), so a
    /// halted consumer stops the search before its deeper layers expand.
    /// Returns whether the consumer halted.
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
            let admitted = match far.as_ref() {
                Some(settled) => settled.settle_if_hit(far_node),
                None => true,
            };
            if admitted {
                self.fan_partition(layer, *origin, far_node, origin_row, out)?;
            }
            Ok(())
        })?;
        // The origin's own layer (depth 0) carries its closed trails or
        // zero-length path, which a shorter open trail still beats: it
        // never ends the fan.
        // A filtered partition answers the shortest *satisfying* trail,
        // which can be longer than its layer's depth, so a deeper layer may
        // still hold a shorter answer: the stop applies to unfiltered
        // routes only.
        let productive =
            self.filters.path.is_none() && layer.nodes() != [*origin] && self.emitted() > before;
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

    /// The one fan partition body: the start row, then `trails(s, e)` for
    /// the oriented pair — the layered search's held trails
    /// ([`Self::fan_partition`]) or the batched sweep's pair rebuild
    /// ([`Self::fan_chunk`]).
    pub(super) fn fan_member(
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
        self.partition(s, e, &srow, out, trails.map(|trails| move || trails(s, e)))
    }

    /// Emit the (s, e) partition of a row with the start bound: nothing
    /// when `e` fails the end pattern, else every trail `trails` returns.
    /// The only home of the s = e partition: the zero-length path when `min`
    /// is 0, otherwise the shortest closed trail(s) through `s`.
    fn partition(
        &self,
        s: NodeId,
        e: NodeId,
        srow: &Row,
        out: &mut MatchedSink<'_, '_>,
        trails: Option<impl FnOnce() -> Result<Vec<Vec<RelId>>>>,
    ) -> Result<()> {
        if !self.node_ok(e, self.shape.end, srow)? {
            return Ok(());
        }
        if self.common_end_dropped(s, e, srow)? {
            return Ok(());
        }
        match self.arm {
            PartitionArm::Filtered => self.feasible_partition(s, e, srow, out),
            PartitionArm::Counted(keep) => k_select::select_k(self, (s, e), srow, keep, out),
            PartitionArm::Plain => self.plain_partition(s, e, srow, out, trails),
        }
    }

    /// The unselected partition: every trail the search holds between the
    /// pair, the zero-length path or the closed trails when `s = e`.
    fn plain_partition(
        &self,
        s: NodeId,
        e: NodeId,
        srow: &Row,
        out: &mut MatchedSink<'_, '_>,
        trails: Option<impl FnOnce() -> Result<Vec<Vec<RelId>>>>,
    ) -> Result<()> {
        // The s = e partition builds its own trails (the zero-length path
        // or the closed trails); only a real pair consults the source, so
        // a caller that has none — the self-pair — passes `None` and no
        // invented list is ever answered.
        let trails = if s != e {
            trails.expect("a non-self partition has a trail source")()?
        } else if self.search.min == 0 {
            vec![Vec::new()]
        } else {
            self.search.closed(s)?
        };
        for trail in &trails {
            if self.halted() {
                break;
            }
            self.offer_trail(s, e, trail, srow, out)?;
        }
        Ok(())
    }

    /// 51N23 for the (s, e) partition: the spelling's runtime policy (a
    /// positive lower bound rejects a pair that resolves to one node; the
    /// zero-length path answers it when the bound is 0). `Ok(true)` when
    /// the policy would reject the pair but the MATCH's own WHERE drops it
    /// first, as live plans it ahead of the search — the partition emits
    /// nothing; the rejection itself is the `Err`.
    fn common_end_dropped(&self, s: NodeId, e: NodeId, srow: &Row) -> Result<bool> {
        let policy = self.shape.spelling.common_end_nodes();
        let Err(rejection) = policy.reject(s, e, self.search.min) else {
            return Ok(false);
        };
        if let Some(pair) = &self.filters.pair {
            if pair.drops(self.ctx, self.shape.end, e, srow, &self.filters.endpoint)? {
                return Ok(true);
            }
        }
        Err(rejection)
    }

    /// The predicate-filtered partition: walk trails from `s` in ascending
    /// length and answer the first length at which one satisfies the
    /// filter. A `shortestPath` answers the first such trail; an
    /// all-shortest selector answers every satisfying trail of that
    /// length. This is the only place a shortest path is *chosen* against
    /// a predicate, so `WHERE NONE(n IN nodes(p) …)` no longer hides the
    /// longer path that satisfies it.
    fn feasible_partition(
        &self,
        s: NodeId,
        e: NodeId,
        srow: &Row,
        out: &mut MatchedSink<'_, '_>,
    ) -> Result<()> {
        let max = self.walk_max();
        let mut walk = trail_walk::TrailWalk::new(self, s, e, srow);
        for length in self.shape.min..=max {
            if walk.any_of_length(s, length, out)? {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Bind one trail the way `match_gpm_path` does: start node,
    /// relationship list, end node, then the path. `None` when the end
    /// node's binding refuses the pattern.
    pub(super) fn bind_trail(
        &self,
        s: NodeId,
        e: NodeId,
        trail: &[RelId],
        srow: &Row,
    ) -> Option<Row> {
        let graph = self.ctx.graph;
        let mut row = srow.clone();
        if let Some(name) = &self.shape.rel.variable {
            // the one trail-to-list binding spelling
            // (`bind_rel_stub_list`) — the walk's var-length arm and the
            // classic walker's `bind_trail` read the same
            row.insert(name.to_string(), bind_rel_stub_list(graph, trail));
        }
        if !gpm_bind_node(graph, self.shape.end, e, &mut row) {
            return None;
        }
        // the one path-name binding spelling — the path is built inside
        // the lazy value builder, so an anonymous pattern (no path
        // name) pays nothing for the materialization. The append
        // stamps the route's memoized rung (pattern_walk's
        // `memoized_path_rung`): every trail binds the same names
        // against the one input row.
        let rung = memoized_path_rung(&self.path_rung, &row, self.path);
        bind_path_names(&mut row, self.path, &rung, || {
            Value::from_path(path_from_trail(graph, s, trail))
        });
        Some(row)
    }

    /// Bind one selected trail and push it onto the result — the one
    /// "offer this trail" spelling, shared by the unfiltered partition walk
    /// and the predicate-filtered trail walk
    /// ([`super::trail_walk::TrailWalk`]): the cancellation tick, the
    /// binding, the route's own predicate gate when it has one (a filtered
    /// route accepts only a trail that satisfies it), and the push. Returns
    /// whether a row was offered.
    ///
    /// The gate reads `self.filters.path` rather than taking it as an argument:
    /// the route stores the one filter its search runs under, and a caller
    /// passing its own would be a second spelling of that fact.
    pub(super) fn offer_trail(
        &self,
        s: NodeId,
        e: NodeId,
        trail: &[RelId],
        srow: &Row,
        out: &mut MatchedSink<'_, '_>,
    ) -> Result<bool> {
        self.ctx.graph.cancel_tick()?;
        let Some(row) = self.bind_trail(s, e, trail, srow) else {
            return Ok(false);
        };
        if let Some(filter) = &self.filters.path {
            if !filter.holds(self.ctx, &row)? {
                return Ok(false);
            }
        }
        out.offer(self.ctx, row, || trail.iter().copied().collect())?;
        self.emitted.set(self.emitted.get() + 1);
        Ok(true)
    }

    /// The rows this route has offered so far — the fan drivers' emission
    /// fact (see the `emitted` field).
    pub(super) fn emitted(&self) -> usize {
        self.emitted.get()
    }
}
