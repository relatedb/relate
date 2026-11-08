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
use crate::exec::pushdown::{conjunct_ready, Residual};
use crate::pattern_walk::{
    bind_path_names, bind_rel_stub_list, bound_node, memoized_path_rung, node_candidates,
};
use crate::row::{Row, RowRung};
use crate::shortest::layered::Dag;
use crate::shortest::{pair, path_from_trail, Search};

use super::guard::Shape;
use super::Matched;

/// Which endpoints one input row fixes, resolved once per row.
enum Anchor {
    /// Both endpoints bound: the one (s, e) partition.
    Pair(NodeId, NodeId),
    /// Only the start bound: one forward search reaches every end.
    Start(NodeId),
    /// Only the end bound: one backward search reaches every start.
    End(NodeId),
    /// Neither bound: one forward search per start candidate.
    Free,
    /// One unbound variable at both ends (`(a)-[*]-(a)`): the (s, s)
    /// partition of every start candidate.
    Loop,
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
}

impl<'a> Route<'a> {
    pub(super) fn new(
        ctx: ExecCtx<'a>,
        path: &'a PathPattern,
        row: &'a Row,
        shape: Shape<'a>,
        filter: Option<PathFilter<'a>>,
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

    /// Every selected row: the row's anchoring, resolved once, picks the
    /// searches. A pattern whose minimum exceeds its maximum matches nothing;
    /// one whose predicate the route cannot decide declines (`None`) before
    /// any search runs, and the pattern enumerates instead.
    pub(super) fn run(&self) -> Result<Option<Matched>> {
        let mut out = Vec::new();
        if self.search.min > self.search.max {
            return Ok(Some(out));
        }
        if let Some(filter) = &self.filter {
            if !self.predicates_decidable(filter) {
                return Ok(None);
            }
        }
        match self.anchor() {
            Anchor::Pair(s, e) => self.pair_partition(s, e, &mut out),
            Anchor::Start(s) => self.fan(s, Way::Forward, &mut out),
            Anchor::End(e) => self.fan(e, Way::Backward, &mut out),
            Anchor::Free => self.each_start(|s| self.fan(s, Way::Forward, &mut out)),
            Anchor::Loop => self.each_start(|s| self.pair_partition(s, s, &mut out)),
        }?;
        Ok(Some(out))
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

    fn anchor(&self) -> Anchor {
        let (start, end) = (self.shape.start, self.shape.end);
        match (self.bound(start), self.bound(end)) {
            (Some(s), Some(e)) => Anchor::Pair(s, e),
            (Some(s), None) => Anchor::Start(s),
            (None, Some(e)) => Anchor::End(e),
            (None, None) if start.variable.is_some() && start.variable == end.variable => {
                Anchor::Loop
            }
            (None, None) => Anchor::Free,
        }
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

    /// Call `f` with every candidate of the unbound start pattern.
    fn each_start(&self, mut f: impl FnMut(NodeId) -> Result<()>) -> Result<()> {
        let ExecCtx {
            graph,
            params,
            cypher_version,
            ..
        } = self.ctx;
        let residual = Residual::empty();
        // LABS-448: the seek phase — the route's start resolution — is
        // stamped inside the one counted funnel.
        let starts = node_candidates(
            graph,
            self.shape.start,
            self.row,
            params,
            cypher_version,
            residual.conjuncts,
        )?;
        for s in starts {
            graph.cancel_tick()?;
            f(s)?;
        }
        Ok(())
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

    /// A layered search from `origin`; an all-path selector keeps every
    /// shortest predecessor.
    fn dag(
        &self,
        search: Search<'_, '_>,
        origin: NodeId,
        stop: impl FnMut(NodeId, u32) -> Result<bool>,
    ) -> Result<Dag> {
        search.dag(origin, self.shape.all, None, stop)
    }

    /// Both endpoints fixed: the pair search for single-path selectors, the
    /// layered search stopped at `e` for all-path ones.
    fn pair_partition(&self, s: NodeId, e: NodeId, out: &mut Matched) -> Result<()> {
        let Some(srow) = self.bind_start(s)? else {
            return Ok(());
        };
        self.partition(s, e, &srow, out, || {
            if self.shape.all {
                self.dag(self.search.clone(), s, |n, _| Ok(n == e))?
                    .trails(self.ctx.graph, e)
            } else {
                Ok(pair(&self.search, s, e)?.into_iter().collect())
            }
        })
    }

    /// One endpoint fixed: one layered search from it, `way` saying which,
    /// reaches every node at the other end. Each reached node, and the
    /// origin itself, is one partition.
    fn fan(&self, origin: NodeId, way: Way, out: &mut Matched) -> Result<()> {
        // Forward, the origin is the start, bound once; backward, each
        // reached node is a start, bound in its own partition.
        let (search, origin_row) = match way {
            Way::Forward => {
                let Some(row) = self.bind_start(origin)? else {
                    return Ok(());
                };
                (self.search.clone(), Some(row))
            }
            Way::Backward => (self.search.reversed(), None),
        };
        let dag = self.dag(search, origin, |_, _| Ok(false))?;
        for &far in std::iter::once(&origin).chain(dag.order()) {
            self.ctx.graph.cancel_tick()?;
            let (s, e) = way.ends(origin, far);
            let srow = match &origin_row {
                Some(row) => Cow::Borrowed(row),
                None => match self.bind_start(s)? {
                    Some(row) => Cow::Owned(row),
                    None => continue,
                },
            };
            self.partition(s, e, &srow, out, || {
                Ok(way.orient(dag.trails(self.ctx.graph, far)?))
            })?;
        }
        Ok(())
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
        out: &mut Matched,
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
        out: &mut Matched,
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
            self.ctx.graph.cancel_tick()?;
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
            if crate::eval::eval_match_predicate(graph, row, predicate, params, cypher_version)?
                .truthy()
                != Some(true)
            {
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
        out: &mut Matched,
    ) -> Result<()> {
        self.ctx.graph.cancel_tick()?;
        let Some(row) = self.bind_trail(s, e, trail, srow) else {
            return Ok(());
        };
        self.push(row, trail, out);
        Ok(())
    }

    /// Push one trail already bound by [`Self::bind_trail`] — the one place a
    /// selected trail is copied into the result.
    fn push(&self, row: Row, trail: &[RelId], out: &mut Matched) {
        out.push((row, trail.iter().copied().collect()));
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
    fn any_of_length(&mut self, at: NodeId, length: u32, out: &mut Matched) -> Result<bool> {
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
            self.route.push(row, &self.trail, out);
            return Ok(true);
        }
        // Accumulate the satisfied flag across frames and return it after
        // the loop: `ALL SHORTEST` walks the whole length too, and the
        // caller (feasible_partition) must see that this length satisfied
        // so it stops at the shortest satisfying length instead of
        // walking past it (which returned non-shortest trails).
        let mut any_satisfied = false;
        for (next, rid) in self.route.search.hops(at)? {
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
