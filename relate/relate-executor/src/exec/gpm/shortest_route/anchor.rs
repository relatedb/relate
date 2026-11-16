//! The anchoring of one routed pattern against one input row: how the two
//! endpoints resolve, once per row, and which searches that picks — the
//! pair product, a fan from one side, or the same-node loop. The endpoint
//! resolution rides the counted candidate funnel with the pair bound as
//! its cap, so a non-selective seek never lists every match.

#[cfg(test)]
use std::cell::Cell;

use relate_core::{Error, NodeId, RelId, Result};
use relate_ir::NodePattern;

use crate::ctx::ExecCtx;
use crate::exec::pushdown::expr_ready;
use crate::pattern_walk::{
    binding, bound_node, node_candidates, node_candidates_capped, prepared, scan_answers_exactly,
    Binding, Capped, NodeBinding, Prepared, Seek,
};
use crate::shortest::Search;

use super::end_filter::End;
use super::far_set::FarSet;
use super::route::Route;

/// The pair product's candidate bound: past it, the product's
/// bidirectional searches would out-count the fan's one ball per start,
/// and the fan covers the shape.
pub(super) const SEEK_PAIR_LIMIT: usize = 16;

/// The most candidates one over-bound side is listed in full — the
/// both-sides-over fan needs both lists to pick the shorter (ADR-0567).
/// Past it the route refuses with a typed capacity error rather than
/// materializing every match.
const SEEK_LIST_LIMIT: usize = 1 << 20;

#[cfg(test)]
thread_local! {
    static LIST_LIMIT: Cell<Option<usize>> = const { Cell::new(None) };
}

/// [`SEEK_LIST_LIMIT`], or the test override that lets a small graph
/// reach the refusal; the override is absent outside test builds.
fn list_limit() -> usize {
    #[cfg(test)]
    if let Some(limit) = LIST_LIMIT.with(Cell::get) {
        return limit;
    }
    SEEK_LIST_LIMIT
}

/// Run `f` with the listing limit lowered to `limit` on this thread.
#[cfg(test)]
pub(super) fn with_list_limit<T>(limit: usize, f: impl FnOnce() -> T) -> T {
    super::with_override(&LIST_LIMIT, limit, f)
}

#[cfg(test)]
thread_local! {
    static FAR_SET_OFF: Cell<bool> = const { Cell::new(false) };
}

/// The far-side set of a listed side, `None` for a length-only side or when
/// the test override turns the far set off (the per-reached-node end test
/// is the oracle).
fn far_set(side: &Side) -> Option<FarSet> {
    #[cfg(test)]
    if FAR_SET_OFF.with(Cell::get) {
        return None;
    }
    side.ids().and_then(FarSet::of)
}

/// Run `f` with the far set disabled on this thread.
#[cfg(test)]
pub(super) fn without_far_set<T>(f: impl FnOnce() -> T) -> T {
    crate::exec::test_support::with_flag_disabled(&FAR_SET_OFF, f)
}

/// Which arm of [`Route::anchor`] answered one route run — the value the
/// anchor tests assert against (the `take_route_runs` precedent). The
/// variants are derived from [`Anchor`] by [`Anchor::arm`].
#[cfg(test)]
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

/// Record the anchor arm one route run took.
#[cfg(test)]
pub(super) fn note_anchor(arm: AnchorArm) {
    ANCHOR_ARMS.with(|c| c.borrow_mut().push(arm));
}

/// Take and reset this thread's recorded anchor arms.
#[cfg(test)]
pub(super) fn take_anchor_arms() -> Vec<AnchorArm> {
    ANCHOR_ARMS.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

/// How the endpoints of one input row resolve, once per row.
pub(super) enum Anchor {
    /// Both endpoints resolved to real sides — a row-bound node is `One`,
    /// a seeked list is `Many`: pair partitions over the product, the
    /// bidirectional search per pair.
    Pair { starts: Side, ends: Side },
    /// One fan search per origin candidate, along `way` — the origins
    /// (a bound node or a resolved candidate list) come from `anchor`,
    /// never re-materialized.
    Fan {
        origins: Side,
        way: Way,
        /// The listed side the fan does not drive from; `None` when that
        /// side is a pattern or length-only, tested per reached node.
        far: Option<FarSet>,
    },
    /// One unbound variable at both ends (`(a)-[*]-(a)`): the (s, s)
    /// partition of every start candidate.
    Loop,
}

impl Anchor {
    /// The arm this anchoring is — the one derivation of [`AnchorArm`], so
    /// the recorded vocabulary cannot drift from the variants.
    #[cfg(test)]
    pub(super) fn arm(&self) -> AnchorArm {
        match self {
            Anchor::Pair { .. } => AnchorArm::Pair,
            Anchor::Fan {
                way: Way::Forward, ..
            } => AnchorArm::FanForward,
            Anchor::Fan {
                way: Way::Backward, ..
            } => AnchorArm::FanBackward,
            Anchor::Loop => AnchorArm::Loop,
        }
    }
}

/// One resolved endpoint side of a routed pattern: a row-bound endpoint
/// is one node, a property seek or the start scan is a candidate list, and
/// a seek matching more than [`SEEK_PAIR_LIMIT`] nodes is a length-only
/// side — its list was never built, so it counts one past the bound and
/// the one pair-versus-fan decision ([`Route::listed`]) reads it like any
/// other side.
pub(super) enum Side {
    One(NodeId),
    Many(Vec<NodeId>),
    Over,
}

impl Side {
    /// Candidate count: a bound node is one, an over-bound side one past
    /// the pair bound.
    fn len(&self) -> usize {
        match self {
            Side::One(_) => 1,
            Side::Many(ids) => ids.len(),
            Side::Over => SEEK_PAIR_LIMIT + 1,
        }
    }

    /// The candidates, in list order; `None` for a length-only side.
    fn ids(&self) -> Option<&[NodeId]> {
        match self {
            Side::One(id) => Some(std::slice::from_ref(id)),
            Side::Many(ids) => Some(ids),
            Side::Over => None,
        }
    }

    /// Whether the side's list was never built.
    fn is_over(&self) -> bool {
        matches!(self, Side::Over)
    }
}

/// The endpoint a fan search starts from.
#[derive(Clone, Copy)]
pub(super) enum Way {
    /// From the start, along the pattern's direction.
    Forward,
    /// From the end, against the pattern's direction.
    Backward,
}

impl Way {
    /// The fan search along this way: forward follows the pattern's own
    /// direction from the start, backward runs the reversed search from the
    /// end.
    pub(super) fn search<'a, 'g>(self, base: &Search<'a, 'g>) -> Search<'a, 'g> {
        match self {
            Way::Forward => base.clone(),
            Way::Backward => base.reversed(),
        }
    }

    /// The (start, end) partition of `far`, a node reached from `origin`.
    pub(super) fn ends(self, origin: NodeId, far: NodeId) -> (NodeId, NodeId) {
        match self {
            Way::Forward => (origin, far),
            Way::Backward => (far, origin),
        }
    }

    /// Trails read from the origin, turned to run start → end.
    pub(super) fn orient(self, mut trails: Vec<Vec<RelId>>) -> Vec<Vec<RelId>> {
        if matches!(self, Way::Backward) {
            trails.iter_mut().for_each(|t| t.reverse());
        }
        trails
    }
}

impl Route<'_> {
    /// `None` declines a count selector (ADR-0607) before the one step that
    /// would materialize a fan's origins: a count selector answers pair
    /// partitions only, so a fan it cannot use is never built.
    pub(super) fn anchor(&self) -> Result<Option<Anchor>> {
        let counted = self.shape.selection.counted().is_some();
        let (start, end) = (self.shape.start, self.shape.end);
        // The one shape pre-check: one unbound variable at both ends
        // (`(a)-[*]-(a)`) is the (s, s) partition of every start
        // candidate.
        if start.variable.is_some() && start.variable == end.variable && self.bound(start).is_none()
        {
            return Ok(Some(Anchor::Loop));
        }
        // Each endpoint resolves once, bounded: a row-bound node is a
        // one-element side, a property map is a seeked list of at most
        // `SEEK_PAIR_LIMIT`, and a bigger match is the length-only
        // `Side::Over` — its list is built only where both sides are over
        // (below). `None` is scan territory the fan answers itself.
        let probed = (self.probe(start, End::Start)?, self.probe(end, End::End)?);
        Ok(Some(match probed {
            // Both past the bound: the fan runs from the shorter side,
            // which needs both lists in full.
            (Some(Side::Over), Some(Side::Over)) if counted => return Ok(None),
            (Some(Side::Over), Some(Side::Over)) => {
                Self::listed(self.listed_side(start)?, self.listed_side(end)?)
            }
            (Some(starts), Some(ends)) => Self::listed(starts, ends),
            (Some(starts), None) if !starts.is_over() => Anchor::Fan {
                origins: starts,
                way: Way::Forward,
                far: None,
            },
            // The end side drives only when its list is bounded: one
            // full-depth fan per property match must not beat the start
            // scan's one ball per start (a non-selective map makes the
            // backward fan pay |ends| balls). Past the bound the start
            // candidates fan forward.
            (None, Some(ends)) if !ends.is_over() => Anchor::Fan {
                origins: ends,
                way: Way::Backward,
                far: None,
            },
            _ if counted => return Ok(None),
            _ => Anchor::Fan {
                origins: Side::Many(self.start_candidates()?),
                way: Way::Forward,
                far: None,
            },
        }))
    }

    /// Both endpoints as sides, an over-bound one length-only: the pair
    /// product while it is within [`SEEK_PAIR_LIMIT`], else a fan from the
    /// shorter side (fewer full-depth balls) — the over side, one past the
    /// bound, is never the shorter. An empty side makes the product empty:
    /// a pair of nothing, whatever the other side is, so a length-only
    /// side never reaches [`Route::each_side`].
    fn listed(starts: Side, ends: Side) -> Anchor {
        let (s, e) = (starts.len(), ends.len());
        if s == 0 || e == 0 {
            Anchor::Pair {
                starts: Side::Many(Vec::new()),
                ends: Side::Many(Vec::new()),
            }
        } else if s.saturating_mul(e) <= SEEK_PAIR_LIMIT {
            Anchor::Pair { starts, ends }
        } else if e < s {
            Anchor::Fan {
                far: far_set(&starts),
                origins: ends,
                way: Way::Backward,
            }
        } else {
            Anchor::Fan {
                far: far_set(&ends),
                origins: starts,
                way: Way::Forward,
            }
        }
    }

    /// One endpoint's bounded resolution, once: a row-bound node is
    /// listed (`None`: scan territory); a row-resolvable property map (or a served `WHERE` point seek) is seeked through the capped
    /// funnel — the scan stops at the first match past
    /// [`SEEK_PAIR_LIMIT`], so a non-selective map never materializes
    /// every match to learn the pair product is over the bound.
    fn probe(&self, np: &NodePattern, which: End) -> Result<Option<Side>> {
        // The one admission table, read once — the funnel's own
        // classification of the endpoint: a null or non-node binding
        // matches nothing (its empty answer, never scan territory, whose
        // fan would run one search per row that the admission then
        // rejects); a row-bound endpoint is one node; a nameless pattern
        // has no variable a seek targets.
        let class = binding(np, self.row);
        match class {
            Binding::Mismatch => return Ok(Some(Side::Many(Vec::new()))),
            Binding::Bound(id) => return Ok(Some(Side::One(id))),
            Binding::Anonymous | Binding::Unbound(_) => {}
        }
        if !self.seekable(np, class) {
            // No index serves the side: a scan filtered by its own `WHERE`
            // may still list it (see `end_filter`).
            return self.filtered_side(np, class, which);
        }
        Ok(Some(match self.capped_candidates(np, SEEK_PAIR_LIMIT)? {
            Capped::Within(ids) => Side::Many(ids),
            Capped::Over(_) => Side::Over,
        }))
    }

    /// One side's candidates through the one per-candidate loop.
    pub(super) fn each_side(&self, side: &Side, f: impl FnMut(NodeId) -> Result<()>) -> Result<()> {
        match side {
            Side::One(id) => self.each_of(&[*id], f),
            Side::Many(ids) => self.each_of(ids, f),
            // `listed` turns every product with an over-bound side into a
            // fan from the other side or an empty pair, so no caller
            // iterates a list that was never built.
            Side::Over => Err(Error::internal(
                "shortest-path route: an over-bound side has no candidate list",
            )),
        }
    }

    /// Whether `np` is seekable: the funnel's scan answers its label shape
    /// with exact matches ([`scan_answers_exactly`]; a negated label's
    /// all-nodes superset would read as a match count and could trip the
    /// listing refusal on a handful of real matches), and either a
    /// row-resolvable property map is present or an endpoint-only `WHERE`
    /// conjunct is a point seek (`=` / `IN` / id) an index serves
    /// ([`point_seek_served`]; the conjunct form never seeks a side no
    /// index serves, so that side keeps the fan's own handling). A bare
    /// label or unbound variable is scan territory, answered by the fan's
    /// own seek.
    fn seekable(&self, np: &NodePattern, class: NodeBinding<'_>) -> bool {
        // The map must resolve from the input row: a variable the walk
        // binds later (the start's own `a` in `(a)-[*]->(b {id: a.id})`)
        // is fan territory — the fan resolves the start first, and the
        // partition's admission then reads the map with it bound. The
        // canonical readiness kernel (pushdown) answers it. An unready map
        // keeps the side unseeked even beside a seekable conjunct: the
        // funnel would read the map against the same unresolvable row.
        if !scan_answers_exactly(np) {
            return false;
        }
        match np.properties.as_ref() {
            Some(props) => expr_ready(props, self.row),
            // The conjunct form seeks only a named, unbound variable, and
            // only a point seek (`=` / `IN` / id) an index actually serves:
            // an endpoint no index serves keeps its scan-territory handling
            // instead of being sized from a label scan the residual never
            // narrowed. A union-only seek (an indexed seek under a
            // label-disjunction fold, LABS-1083) yields — a sizing decision
            // cannot use a union answer — while a fold's label-blind ids
            // seek still answers, point-shaped, as the funnel serves it.
            None => {
                let Binding::Unbound(v) = class else {
                    return false;
                };
                let ExecCtx { graph, params, .. } = self.ctx;
                // `prepared` answers `None` under `USING SCAN`: no seek may
                // answer the variable, the funnel's own arm scans it.
                prepared(v, np, &self.filters.endpoint, graph, self.row, params).is_some_and(
                    |p: Prepared<'_>| p.first_served_seek(graph).is_some_and(Seek::is_point),
                )
            }
        }
    }

    /// The node `np`'s variable is bound to in the input row. The guard
    /// admits only node bindings and unbound variables.
    fn bound(&self, np: &NodePattern) -> Option<NodeId> {
        bound_node(np, self.row)
    }

    /// A probed-`Over` side's full list, through the capped funnel: a side
    /// matching more than `limit` nodes is refused with the typed capacity
    /// error (ADR-0567) — never truncated, never listed unbounded. The
    /// funnel's contract is ascending and deduplicated, so the list needs
    /// no re-sorting.
    fn listed_side(&self, np: &NodePattern) -> Result<Side> {
        let limit = list_limit();
        match self.capped_candidates(np, limit)? {
            Capped::Within(ids) => Ok(Side::Many(ids)),
            Capped::Over(stop) => Err(Error::capacity_seek_candidates(stop, limit as u64)),
        }
    }

    /// The funnel's capped scan or seek for `np` (the counted funnel — the
    /// seek phase is stamped inside it), from the route's endpoint-only
    /// residual, stopped at the first match past `cap`: the pair bound's probe ([`SEEK_PAIR_LIMIT`]) and the full listing's
    /// bound ([`SEEK_LIST_LIMIT`]).
    fn capped_candidates(&self, np: &NodePattern, cap: usize) -> Result<Capped> {
        let ExecCtx { graph, params, .. } = self.ctx;
        node_candidates_capped(graph, np, self.row, params, &self.filters.endpoint, cap)
    }

    /// The unbound start pattern's full candidate list through the counted
    /// funnel (the fan anchor's driver — the same call `each_start` makes for
    /// the loop anchor); a `WHERE`-borne `=` / `IN` on the side seeks it.
    fn start_candidates(&self) -> Result<Vec<NodeId>> {
        let ExecCtx { graph, params, .. } = self.ctx;
        node_candidates(
            graph,
            self.shape.start,
            self.row,
            params,
            &self.filters.endpoint,
        )
    }

    /// Call `f` with every candidate of the unbound start pattern (the
    /// loop anchor's driver).
    pub(super) fn each_start(&self, f: impl FnMut(NodeId) -> Result<()>) -> Result<()> {
        let starts = self.start_candidates()?;
        self.each_of(&starts, f)
    }
}
