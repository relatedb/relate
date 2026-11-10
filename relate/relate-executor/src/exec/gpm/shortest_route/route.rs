//! One routed pattern against one input row: the row's endpoint anchoring,
//! resolved once, picks the searches; each (start, end) partition binds
//! its selected trails the way the GPM walker does.

use crate::UsedSet;
use std::borrow::Cow;
use std::cell::{Cell, OnceCell};
use std::collections::HashSet;

use relate_core::{Error, NodeId, RelId, Result, Value};
use relate_ir::{Expr, MatchMode, NodePattern, PathPattern};

use crate::ctx::ExecCtx;
use crate::exec::candidates::node_ok;
use crate::exec::gpm::{gpm_bind_node, unbounded_dfs_cap};
use crate::exec::pushdown::{conjunct_ready, expr_ready, Residual};
use crate::pattern_walk::{
    bind_path_names, bind_rel_stub_list, bound_node, memoized_path_rung, node_candidates,
    node_candidates_capped, Capped,
};
use crate::row::{Row, RowRung};
use crate::shortest::layered::Layer;
use crate::shortest::{pair, path_from_trail, Search};

use super::guard::Shape;
use super::sink::RouteSink;

/// The pair product's candidate bound: past it, the product's
/// bidirectional searches would out-count the fan's one ball per start,
/// and the fan covers the shape.
const SEEK_PAIR_LIMIT: usize = 16;

/// `Probe::Over` is only ever produced for a seekable side, so the full
/// seek answers `Some`.
const OVER_IS_SEEKABLE: &str = "an over-bound side was probed as seekable";

/// Which arm of [`Route::anchor`] answered one route run — the value the
/// anchor tests assert against (the `take_route_runs` precedent). The
/// variants mirror [`Anchor`]; the construction sites are the arm branches
/// of [`Route::run`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AnchorArm {
    Pair,
    FanForward,
    FanBackward,
    Loop,
}

#[cfg(test)]
thread_local! {
    static ANCHOR_ARMS: std::cell::RefCell<Vec<AnchorArm>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Record the anchor arm one route run took; a no-op outside test builds.
fn note_anchor(_arm: AnchorArm) {
    #[cfg(test)]
    ANCHOR_ARMS.with(|c| c.borrow_mut().push(_arm));
}

/// Take and reset this thread's recorded anchor arms.
#[cfg(test)]
pub(super) fn take_anchor_arms() -> Vec<AnchorArm> {
    ANCHOR_ARMS.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

/// How the endpoints of one input row resolve, once per row.
enum Anchor {
    /// Both endpoints resolved to real sides — a row-bound node is `One`,
    /// a seeked list is `Many`: pair partitions over the product, the
    /// bidirectional search per pair.
    Pair { starts: Side, ends: Side },
    /// One fan search per origin candidate, along `way` — the origins
    /// (a bound node or a resolved candidate list) come from `anchor`,
    /// never re-materialized.
    Fan { origins: Side, way: Way },
    /// One unbound variable at both ends (`(a)-[*]-(a)`): the (s, s)
    /// partition of every start candidate.
    Loop,
}

/// One resolved endpoint side of a routed pattern: a row-bound endpoint
/// is one node, a property seek or the start scan is a candidate list.
enum Side {
    One(NodeId),
    Many(Vec<NodeId>),
}

impl Side {
    /// Candidate count: a bound node is one.
    fn len(&self) -> usize {
        match self {
            Side::One(_) => 1,
            Side::Many(ids) => ids.len(),
        }
    }
}

/// One endpoint's bounded resolution (see [`Route::probe`]).
enum Probe {
    /// No property map to seek: scan territory the fan answers itself.
    Scan,
    /// A bound node, or a seeked list of at most [`SEEK_PAIR_LIMIT`].
    Listed(Side),
    /// A seek matching more than [`SEEK_PAIR_LIMIT`] nodes; the list was
    /// never built.
    Over,
}

/// The endpoint a fan search starts from.
#[derive(Clone, Copy)]
enum Way {
    /// From the start, along the pattern's direction.
    Forward,
    /// From the end, against the pattern's direction.
    Backward,
}

impl Way {
    /// The fan search along this way: forward follows the pattern's own
    /// direction from the start, backward runs the reversed search from the
    /// end.
    fn search<'a, 'g>(self, base: &Search<'a, 'g>) -> Search<'a, 'g> {
        match self {
            Way::Forward => base.clone(),
            Way::Backward => base.reversed(),
        }
    }

    /// The (start, end) partition of `far`, a node reached from `origin`.
    fn ends(self, origin: NodeId, far: NodeId) -> (NodeId, NodeId) {
        match self {
            Way::Forward => (origin, far),
            Way::Backward => (far, origin),
        }
    }

    /// Trails read from the origin, turned to run start → end.
    fn orient(self, mut trails: Vec<Vec<RelId>>) -> Vec<Vec<RelId>> {
        match self {
            Way::Forward => {}
            Way::Backward => trails.iter_mut().for_each(|t| t.reverse()),
        }
        trails
    }
}

/// The predicates a routed shortest path must satisfy during the search
/// (see [`super::path_filter`]): the MATCH-level `WHERE` conjuncts that
/// read the path itself, plus the pattern's own inline `WHERE`. `scope`
/// is the MATCH's bound-variable scope, the readiness rule's input (see
/// [`crate::exec::pushdown::conjunct_ready`]).
pub(super) struct PathFilter<'a> {
    pub conjuncts: Vec<&'a Expr>,
    pub inline: Option<&'a Expr>,
    pub scope: &'a HashSet<String>,
}

/// One routed pattern evaluated against one input row.
pub(super) struct Route<'a> {
    ctx: ExecCtx<'a>,
    path: &'a PathPattern,
    row: &'a Row,
    shape: Shape<'a>,
    search: Search<'a, 'a>,
    /// `Some` when a predicate filters the *search*: the route then walks
    /// trails in ascending length and answers the first length at which a
    /// trail satisfies it, instead of the one shortest trail.
    filter: Option<PathFilter<'a>>,
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
    /// The consumer's halt cell (a probe's verdict): raised once it needs no
    /// more rows, read only through [`Self::halted`].
    halt: Option<&'a Cell<bool>>,
}

impl<'a> Route<'a> {
    pub(super) fn new(
        ctx: ExecCtx<'a>,
        path: &'a PathPattern,
        row: &'a Row,
        shape: Shape<'a>,
        filter: Option<PathFilter<'a>>,
        halt: Option<&'a Cell<bool>>,
    ) -> Self {
        let rel = shape.rel;
        let search = Search {
            graph: ctx.graph,
            // LABS-801: the filter arrives resolved from the pattern-level
            // table; the clone shares its Arc, never re-resolves.
            types: shape.filter.types.clone(),
            dir: rel.direction,
            min: shape.min,
            max: shape.max.unwrap_or(u32::MAX),
        };
        Self {
            ctx,
            path,
            row,
            shape,
            search,
            filter,
            halt,
            unbounded_cap: OnceCell::new(),
            path_rung: Cell::new(None),
        }
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

    /// Push every selected row into `out` as it is bound: the row's
    /// anchoring, resolved once, picks the searches. A pattern whose minimum
    /// exceeds its maximum matches nothing; one whose predicate the route
    /// cannot decide declines (`false`) before any search runs or row is
    /// pushed, and the pattern enumerates instead. The run stops early once
    /// the sink is halted.
    pub(super) fn run(&self, out: &mut RouteSink<'_>) -> Result<bool> {
        if self.search.min > self.search.max {
            return Ok(true);
        }
        if let Some(filter) = &self.filter {
            if !self.predicates_decidable(filter) {
                return Ok(false);
            }
        }
        match self.anchor()? {
            Anchor::Pair { starts, ends } => {
                note_anchor(AnchorArm::Pair);
                self.each_side(&starts, |s| {
                    self.each_side(&ends, |e| self.pair_partition(s, e, out))
                })?;
            }
            Anchor::Fan { origins, way } => {
                note_anchor(match way {
                    Way::Forward => AnchorArm::FanForward,
                    Way::Backward => AnchorArm::FanBackward,
                });
                self.each_side(&origins, |origin| self.fan(origin, way, out))?;
            }
            Anchor::Loop => {
                note_anchor(AnchorArm::Loop);
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
        filter
            .conjuncts
            .iter()
            .copied()
            .chain(filter.inline)
            .all(|predicate| conjunct_ready(predicate, &probe, filter.scope))
    }

    fn anchor(&self) -> Result<Anchor> {
        let (start, end) = (self.shape.start, self.shape.end);
        // The one shape pre-check: one unbound variable at both ends
        // (`(a)-[*]-(a)`) is the (s, s) partition of every start
        // candidate.
        if start.variable.is_some() && start.variable == end.variable && self.bound(start).is_none()
        {
            return Ok(Anchor::Loop);
        }
        // Each endpoint resolves once, bounded: a row-bound node is a
        // one-element side, a property map is a seeked list of at most
        // `SEEK_PAIR_LIMIT`, and a bigger match is `Over` — its list is
        // built only where that side drives a fan (below).
        Ok(match (self.probe(start)?, self.probe(end)?) {
            (Probe::Listed(starts), Probe::Listed(ends)) => Self::listed(starts, ends),
            // One side over the bound and the other a short list: the
            // product is empty, or past the bound with the short side the
            // shorter — the fan runs from it, the over side stays unbuilt.
            (Probe::Listed(starts), Probe::Over) if starts.len() == 0 => Anchor::Pair {
                starts,
                ends: Side::Many(Vec::new()),
            },
            (Probe::Listed(starts), Probe::Over) => Anchor::Fan {
                origins: starts,
                way: Way::Forward,
            },
            (Probe::Over, Probe::Listed(ends)) if ends.len() == 0 => Anchor::Pair {
                starts: Side::Many(Vec::new()),
                ends,
            },
            (Probe::Over, Probe::Listed(ends)) => Anchor::Fan {
                origins: ends,
                way: Way::Backward,
            },
            // Both past the bound: the fan runs from the shorter side,
            // which needs both lists in full.
            (Probe::Over, Probe::Over) => Self::listed(
                Side::Many(self.seek_candidates(start)?.expect(OVER_IS_SEEKABLE)),
                Side::Many(self.seek_candidates(end)?.expect(OVER_IS_SEEKABLE)),
            ),
            (Probe::Listed(starts), Probe::Scan) => Anchor::Fan {
                origins: starts,
                way: Way::Forward,
            },
            // The end side drives only when its list is bounded: one
            // full-depth fan per property match must not beat the start
            // scan's one ball per start (a non-selective map makes the
            // backward fan pay |ends| balls). Past the bound the start
            // candidates fan forward.
            (Probe::Scan, Probe::Listed(ends)) => Anchor::Fan {
                origins: ends,
                way: Way::Backward,
            },
            (Probe::Over, Probe::Scan)
            | (Probe::Scan, Probe::Over)
            | (Probe::Scan, Probe::Scan) => Anchor::Fan {
                origins: Side::Many(self.start_candidates()?),
                way: Way::Forward,
            },
        })
    }

    /// Both endpoints as real sides: the pair product while it is within
    /// [`SEEK_PAIR_LIMIT`], else a fan from the shorter side (fewer
    /// full-depth balls).
    fn listed(starts: Side, ends: Side) -> Anchor {
        if starts.len().saturating_mul(ends.len()) <= SEEK_PAIR_LIMIT {
            Anchor::Pair { starts, ends }
        } else if ends.len() < starts.len() {
            Anchor::Fan {
                origins: ends,
                way: Way::Backward,
            }
        } else {
            Anchor::Fan {
                origins: starts,
                way: Way::Forward,
            }
        }
    }

    /// One endpoint's bounded resolution, once: a row-bound node is
    /// listed; a row-resolvable property map is seeked through the capped
    /// funnel — the scan stops at the first match past
    /// [`SEEK_PAIR_LIMIT`], so a non-selective map never materializes
    /// every match to learn the pair product is over the bound.
    fn probe(&self, np: &NodePattern) -> Result<Probe> {
        if let Some(id) = self.bound(np) {
            return Ok(Probe::Listed(Side::One(id)));
        }
        if !self.seekable(np) {
            return Ok(Probe::Scan);
        }
        Ok(match self.capped_candidates(np)? {
            Capped::Within(ids) => Probe::Listed(Side::Many(ids)),
            Capped::Over => Probe::Over,
        })
    }

    /// One side's candidates through the one per-candidate loop.
    fn each_side(&self, side: &Side, f: impl FnMut(NodeId) -> Result<()>) -> Result<()> {
        match side {
            Side::One(id) => self.each_of(&[*id], f),
            Side::Many(ids) => self.each_of(ids, f),
        }
    }

    /// Whether `np` is property-seekable: the map is present and
    /// row-resolvable. A bare label or unbound variable is scan
    /// territory, answered by the fan's own seek.
    fn seekable(&self, np: &NodePattern) -> bool {
        // The map must resolve from the input row: a variable the walk
        // binds later (the start's own `a` in `(a)-[*]->(b {id: a.id})`)
        // is fan territory — the fan resolves the start first, and the
        // partition's admission then reads the map with it bound. The
        // canonical readiness kernel (pushdown) answers it.
        np.properties
            .as_ref()
            .is_some_and(|props| expr_ready(props, self.row))
    }

    /// The pattern's full candidate list when it is property-seekable,
    /// `None` otherwise. Uncapped: only a side that drives a fan needs
    /// its whole list ([`Route::anchor`] probes bounded first).
    fn seek_candidates(&self, np: &NodePattern) -> Result<Option<Vec<NodeId>>> {
        if !self.seekable(np) {
            return Ok(None);
        }
        // The funnel's contract is ascending and deduplicated
        // (overlay/idlists.rs), so the seek's list needs no re-sorting.
        Ok(Some(self.candidates(np)?))
    }

    /// The node `np`'s variable is bound to in the input row. The guard
    /// admits only node bindings and unbound variables.
    fn bound(&self, np: &NodePattern) -> Option<NodeId> {
        bound_node(np, self.row)
    }

    fn node_ok(&self, id: NodeId, np: &NodePattern, row: &Row) -> Result<bool> {
        let ExecCtx {
            graph,
            params,
            cypher_version,
            ..
        } = self.ctx;
        node_ok(graph, id, np, row, params, cypher_version)
    }

    /// The route's one candidate-resolution call: the funnel's scan or
    /// seek for `np` (the counted funnel — the seek phase is stamped
    /// inside it), from the route's empty residual. The fan's start scan
    /// and the property seek share it.
    fn candidates(&self, np: &NodePattern) -> Result<Vec<NodeId>> {
        let ExecCtx {
            graph,
            params,
            cypher_version,
            ..
        } = self.ctx;
        let residual = Residual::empty();
        node_candidates(
            graph,
            np,
            self.row,
            params,
            cypher_version,
            residual.conjuncts,
        )
    }

    /// [`Route::candidates`] stopped at the first match past
    /// [`SEEK_PAIR_LIMIT`]: the pair bound's probe.
    fn capped_candidates(&self, np: &NodePattern) -> Result<Capped> {
        let ExecCtx {
            graph,
            params,
            cypher_version,
            ..
        } = self.ctx;
        let residual = Residual::empty();
        node_candidates_capped(
            graph,
            np,
            self.row,
            params,
            cypher_version,
            residual.conjuncts,
            SEEK_PAIR_LIMIT,
        )
    }

    /// The unbound start pattern's full candidate list (the fan anchor's
    /// driver — the same call `each_start` makes for the loop anchor).
    fn start_candidates(&self) -> Result<Vec<NodeId>> {
        self.candidates(self.shape.start)
    }

    /// Call `f` with every candidate of the unbound start pattern (the
    /// loop anchor's driver).
    fn each_start(&self, f: impl FnMut(NodeId) -> Result<()>) -> Result<()> {
        let starts = self.start_candidates()?;
        self.each_of(&starts, f)
    }

    /// The route's one per-candidate loop: tick the statement's
    /// cancellation window, then drive the candidate. `each_start`, the
    /// fan arms and the pair product all ride it.
    fn each_of(&self, ids: &[NodeId], mut f: impl FnMut(NodeId) -> Result<()>) -> Result<()> {
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
    fn halted(&self) -> bool {
        self.halt.is_some_and(Cell::get)
    }

    /// The input row with the start node bound, or `None` when `id` fails
    /// the start pattern.
    fn bind_start(&self, id: NodeId) -> Result<Option<Row>> {
        if !self.node_ok(id, self.shape.start, self.row)? {
            return Ok(None);
        }
        let mut row = self.row.clone();
        Ok(gpm_bind_node(self.ctx.graph, self.shape.start, id, &mut row).then_some(row))
    }

    /// Both endpoints fixed: the pair search for single-path selectors, the
    /// layered search stopped at `e` for all-path ones.
    fn pair_partition(&self, s: NodeId, e: NodeId, out: &mut RouteSink<'_>) -> Result<()> {
        let Some(srow) = self.bind_start(s)? else {
            return Ok(());
        };
        self.partition(s, e, &srow, out, || {
            if self.shape.all {
                self.search
                    .dag(s, self.shape.all, None, |n, _| Ok(n == e))?
                    .trails(self.ctx.graph, e)
            } else {
                Ok(pair(&self.search, s, e)?.into_iter().collect())
            }
        })
    }

    /// One endpoint fixed: one layered search from it, `way` saying which,
    /// reaches every node at the other end. Each reached node, and the
    /// origin itself, is one partition — emitted as its layer of the search
    /// completes (ADR-0555), so a halted consumer stops the search before
    /// its deeper layers are expanded.
    fn fan(&self, origin: NodeId, way: Way, out: &mut RouteSink<'_>) -> Result<()> {
        // Forward, the origin is the start, bound once; backward, each
        // reached node is a start, bound in its own partition.
        let origin_row = match way {
            Way::Forward => match self.bind_start(origin)? {
                Some(row) => Some(row),
                None => return Ok(()),
            },
            Way::Backward => None,
        };
        way.search(&self.search)
            .dag_layers(origin, self.shape.all, None, |layer| {
                self.fan_layer(&layer, origin, way, origin_row.as_ref(), out)
            })?;
        Ok(())
    }

    /// Drive one completed fan layer: each reached node is one partition,
    /// and the layer is emitted as the search completes it (ADR-0555), so a
    /// halted consumer stops the search before its deeper layers expand.
    /// Returns whether the consumer halted.
    fn fan_layer(
        &self,
        layer: &Layer<'_>,
        origin: NodeId,
        way: Way,
        origin_row: Option<&Row>,
        out: &mut RouteSink<'_>,
    ) -> Result<bool> {
        // The layer rides the route's one per-candidate loop.
        self.each_of(layer.nodes(), |far| {
            self.fan_partition(layer, origin, far, way, origin_row, out)
        })?;
        Ok(self.halted())
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
        way: Way,
        origin_row: Option<&Row>,
        out: &mut RouteSink<'_>,
    ) -> Result<()> {
        let (s, e) = way.ends(origin, far);
        let srow = match origin_row {
            Some(row) => Cow::Borrowed(row),
            None => match self.bind_start(s)? {
                Some(row) => Cow::Owned(row),
                None => return Ok(()),
            },
        };
        self.partition(s, e, &srow, out, || {
            Ok(way.orient(layer.trails(self.ctx.graph, far)?))
        })
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
        out: &mut RouteSink<'_>,
        trails: impl FnOnce() -> Result<Vec<Vec<RelId>>>,
    ) -> Result<()> {
        if !self.node_ok(e, self.shape.end, srow)? {
            return Ok(());
        }
        if let Some(filter) = &self.filter {
            return self.feasible_partition(s, e, srow, filter, out);
        }
        let trails = if s != e {
            trails()?
        } else if self.search.min == 0 {
            vec![Vec::new()]
        } else {
            self.search.closed(s, self.shape.all)?
        };
        for trail in &trails {
            if self.halted() {
                break;
            }
            self.emit(s, e, trail, srow, out)?;
        }
        Ok(())
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
        filter: &PathFilter<'_>,
        out: &mut RouteSink<'_>,
    ) -> Result<()> {
        // The one GPM bound policy: a bounded pattern keeps its written
        // bound, an unbounded one takes the engine's unbounded DFS cap
        // (`unbounded_dfs_cap`) — the same ceiling the GPM walk's two
        // spellings read (`pattern_walk::hop_bounds_with` for the
        // var-length rel, the quantifier's `unwrap_or_else` for the
        // quantified element) — so this walk explores to the same depth
        // the enumeration fallback would. Both bounds come from the one
        // carrier of the pattern's written bounds (`shape`); the
        // `unwrap_or_else` arm runs only for an unbounded pattern, so a
        // bounded one never pays the rel-table enumeration behind the
        // memoized cap.
        let max = self
            .shape
            .max
            .unwrap_or_else(|| self.memoized_unbounded_cap());
        let mut walk = TrailWalk {
            route: self,
            start: s,
            end: e,
            srow,
            filter,
            trail: Vec::new(),
            used: UsedSet::default(),
        };
        for length in self.shape.min..=max {
            if walk.any_of_length(s, length, out)? {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Whether one bound trail satisfies the route's predicate. Every
    /// predicate is decidable against a trail row — [`Self::predicates_decidable`]
    /// established that before the search started — so a predicate that is not
    /// ready here is a broken invariant, reported as a typed internal error
    /// rather than silently counted as satisfied.
    fn filter_ok(&self, row: &Row, filter: &PathFilter<'_>) -> Result<bool> {
        let ExecCtx {
            graph,
            params,
            cypher_version,
            ..
        } = self.ctx;
        for predicate in filter.conjuncts.iter().copied().chain(filter.inline) {
            if !conjunct_ready(predicate, row, filter.scope) {
                return Err(Error::internal(
                    "shortest-path route: predicate not decidable against a bound trail",
                ));
            }
            // The one predicate policy (ADR-0458 §7): a list/path converts
            // (live: `ANY SHORTEST ... -[r WHERE r.ls]-> ...` accepts a
            // non-empty list), a non-boolean is the typed rejection live
            // raises (22N01).
            if !crate::eval::predicate_value_holds(&crate::eval::eval_match_predicate(
                graph,
                row,
                predicate,
                params,
                cypher_version,
            )?)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Bind one trail the way `match_gpm_path` does: start node,
    /// relationship list, end node, then the path. `None` when the end
    /// node's binding refuses the pattern.
    fn bind_trail(&self, s: NodeId, e: NodeId, trail: &[RelId], srow: &Row) -> Option<Row> {
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

    /// Bind one selected trail and push it onto the result.
    fn emit(
        &self,
        s: NodeId,
        e: NodeId,
        trail: &[RelId],
        srow: &Row,
        out: &mut RouteSink<'_>,
    ) -> Result<()> {
        self.ctx.graph.cancel_tick()?;
        let Some(row) = self.bind_trail(s, e, trail, srow) else {
            return Ok(());
        };
        out.offer(self.ctx, row, trail)
    }
}

/// One predicate-filtered trail walk of a partition: the search frames it
/// recurses over (the trail and the relationships it consumed) beside the
/// partition's endpoints, row and predicate. Bundled so the recursion carries
/// the frame it extends and its buffer is allocated once per partition.
struct TrailWalk<'r, 'p, 'a> {
    route: &'r Route<'a>,
    start: NodeId,
    end: NodeId,
    srow: &'r Row,
    filter: &'r PathFilter<'p>,
    trail: Vec<RelId>,
    used: UsedSet,
}

impl TrailWalk<'_, '_, '_> {
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
    fn any_of_length(&mut self, at: NodeId, length: u32, out: &mut RouteSink<'_>) -> Result<bool> {
        self.route.ctx.graph.cancel_tick()?;
        if self.trail.len() == length as usize {
            if at != self.end {
                return Ok(false);
            }
            let Some(row) = self
                .route
                .bind_trail(self.start, self.end, &self.trail, self.srow)
            else {
                return Ok(false);
            };
            if !self.route.filter_ok(&row, self.filter)? {
                return Ok(false);
            }
            out.offer(self.route.ctx, row, &self.trail)?;
            return Ok(true);
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
            if !self.used.insert(rid) {
                continue;
            }
            self.trail.push(rid);
            let satisfied = self.any_of_length(next, length, out)?;
            self.trail.pop();
            self.used.remove(&rid);
            if satisfied && !self.route.shape.all {
                return Ok(true);
            }
            any_satisfied |= satisfied;
        }
        Ok(any_satisfied)
    }
}
