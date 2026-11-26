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
    admitted, binding, bound_node, node_candidates, node_candidates_capped,
    point_candidates_capped, Admitted, Capped, NodeBinding,
};
use crate::shortest::Search;

use super::end_filter::SideCell;
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

/// The far-side set of a listed side, `None` when its ids span too wide or
/// the test override turns the far set off (the per-reached-node end test
/// is the oracle).
fn far_set(ids: &[NodeId]) -> Option<FarSet> {
    #[cfg(test)]
    if FAR_SET_OFF.with(Cell::get) {
        return None;
    }
    FarSet::of(ids)
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
        /// side is a pattern or over the pair bound, or its ids span too
        /// wide ([`far_set`]'s `None` reasons, the one spelling) — tested
        /// per reached node.
        far: Option<FarSet>,
    },
    /// One unbound variable at both ends (`(a)-[*]-(a)`): the (s, s)
    /// partition of every start candidate — the loop's starts are a
    /// materialized list by construction (the pre-check above excludes a
    /// row-bound start), resolved once here.
    Loop { starts: Vec<NodeId> },
}

impl Anchor {
    /// A fan from `origins` along `way` with no listed far side: each
    /// reached node is tested against the end pattern alone.
    fn fan(origins: Side, way: Way) -> Self {
        Anchor::Fan {
            origins,
            way,
            far: None,
        }
    }

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
            Anchor::Loop { .. } => AnchorArm::Loop,
        }
    }
}

/// One resolved endpoint side of a routed pattern: a row-bound endpoint
/// is one node, a property seek or the start scan is a candidate list.
/// A side past [`SEEK_PAIR_LIMIT`] is not a `Side`: its list was never
/// built ([`Probed::Over`]).
pub(super) enum Side {
    One(NodeId),
    Many(Vec<NodeId>),
}

impl Side {
    /// The candidates, in list order.
    pub(super) fn ids(&self) -> &[NodeId] {
        match self {
            Side::One(id) => std::slice::from_ref(id),
            Side::Many(ids) => ids,
        }
    }
}

/// One endpoint's bounded resolution ([`Route::probe`]).
enum Probed {
    /// Scan territory: the fan answers the endpoint itself.
    Scan,
    /// More than [`SEEK_PAIR_LIMIT`] nodes match: no list was built, and
    /// the side counts as longer than any listed one. The ask that
    /// produced it rides along: the full listing ([`Route::listed_side`])
    /// re-asks through the same entry, never a re-derived one.
    Over(OverAsk),
    /// The complete candidate list, at most [`SEEK_PAIR_LIMIT`] long.
    Within(Side),
}

impl Probed {
    /// The far side's pattern `np` when this probe left it scan territory:
    /// the only far side a fan lists on its own behalf. An over-bound side
    /// is `None`; a listed side never reaches a fan's far position.
    fn scan_side(self, np: &NodePattern) -> Option<&NodePattern> {
        matches!(self, Probed::Scan).then_some(np)
    }
}

/// The funnel entry an over-bound side was asked through — the two asks
/// that can produce [`Probed::Over`]; a scan side never probes over, so
/// `Scan` is unrepresentable here.
#[derive(Clone, Copy)]
enum OverAsk {
    Point,
    Map,
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
    /// `None` declines a count selector (ADR-0607) before any row is
    /// pushed: a count selector answers pair partitions only, so the rule
    /// has one home, the early block after the probes. It declines before
    /// any side is listed or the starts are scanned, so a two-over pair
    /// degenerates to no route rather than the ADR-0567 refusal
    /// `listed_side` would raise.
    pub(super) fn anchor(&self) -> Result<Option<Anchor>> {
        let counted = self.counted();
        let (start, end) = (self.shape.start, self.shape.end);
        // The one shape pre-check: one unbound variable at both ends
        // (`(a)-[*]-(a)`) is the (s, s) partition of every start
        // candidate.
        if start.variable.is_some() && start.variable == end.variable && self.bound(start).is_none()
        {
            return Ok(Some(Anchor::Loop {
                starts: self.start_candidates()?,
            }));
        }
        // Each endpoint resolves once, bounded: a row-bound node is a
        // one-element side, a property map is a seeked list of at most
        // `SEEK_PAIR_LIMIT`, and a bigger match is [`Probed::Over`] — its
        // list is built only where both sides are over (below).
        // [`Probed::Scan`] is territory the fan answers itself.
        let side_caches = &self.caches.ends;
        let probed = (
            self.probe(start, &side_caches.start)?,
            self.probe(end, &side_caches.end)?,
        );
        // A count selector answers pair partitions only (ADR-0607), decided
        // once, before any listing or fan construction: only the listed
        // pair product within the bound is answered; every other shape is
        // declined without listing a side or scanning the starts.
        if counted {
            return Ok(match probed {
                (Probed::Within(starts), Probed::Within(ends)) => {
                    Some(Self::listed(starts, ends)).filter(|a| matches!(a, Anchor::Pair { .. }))
                }
                _ => None,
            });
        }
        let anchor = match probed {
            // Both past the bound: the fan runs from the shorter side,
            // which needs both lists in full.
            (Probed::Over(start_ask), Probed::Over(end_ask)) => Self::listed(
                self.listed_side(start, start_ask)?,
                self.listed_side(end, end_ask)?,
            ),
            (Probed::Within(starts), Probed::Within(ends)) => Self::listed(starts, ends),
            // One listed side against a side with no list (over the bound,
            // or scan territory): the fan runs from the listed side, or
            // nothing when the listed side is empty — the same empty-side
            // policy either way. The end side drives only when its list is
            // bounded: one full-depth fan per property match must not beat
            // the start scan's one ball per start (a non-selective map
            // makes the backward fan pay |ends| balls).
            (Probed::Within(starts), far @ (Probed::Over(_) | Probed::Scan)) => {
                self.fan_or_empty(starts, Way::Forward, far.scan_side(end))?
            }
            (far @ (Probed::Over(_) | Probed::Scan), Probed::Within(ends)) => {
                self.fan_or_empty(ends, Way::Backward, far.scan_side(start))?
            }
            // Neither side has a list: the start candidates fan forward —
            // through the same empty-side policy, so an empty scan is the
            // empty pair like an empty listed side.
            (_, far) => self.fan_or_empty(
                Side::Many(self.start_candidates()?),
                Way::Forward,
                far.scan_side(end),
            )?,
        };
        Ok(Some(anchor))
    }

    /// The pair of nothing: an empty side makes the product empty, whatever
    /// the other side is.
    fn empty_pair() -> Anchor {
        Anchor::Pair {
            starts: Side::Many(Vec::new()),
            ends: Side::Many(Vec::new()),
        }
    }

    /// A fan from the listed side of a pair whose other side is not
    /// listed within the pair bound; an empty listed side is the empty
    /// pair. `scan_far` is the far side's pattern when that side is scan
    /// territory ([`Probed::scan_side`]); it is listed on the fan's
    /// behalf, for a fan of several origins, when it fits the full-listing
    /// bound ([`Self::far_listing`]): the fan then has a far set, the
    /// batched sweep and the early stop of ADR-0612. An over-bound side
    /// (`None`) stays unlisted (ADR-0567: its listing is never
    /// materialized unasked).
    fn fan_or_empty(
        &self,
        origins: Side,
        way: Way,
        scan_far: Option<&NodePattern>,
    ) -> Result<Anchor> {
        if origins.ids().is_empty() {
            return Ok(Self::empty_pair());
        }
        // One origin has nothing to share, and a row-by-row loop would
        // re-list the side per input row: the listing is for fans with at
        // least two origins, whose own list is already in full.
        let listing = match scan_far {
            // A label-less side has no bucket to list: it would read the
            // whole node store per input row, so it stays the fan's to test.
            Some(np) if origins.ids().len() >= 2 && !np.labels.is_empty() => {
                self.far_listing(np)?
            }
            _ => None,
        };
        Ok(match listing {
            // An exactly listed side with no member matches nothing.
            Some(far) if far.is_empty() => Self::empty_pair(),
            Some(far) => Anchor::Fan {
                far: far_set(&far),
                origins,
                way,
            },
            None => Anchor::fan(origins, way),
        })
    }

    /// The nodes the unlisted far side of a fan matches, when they number
    /// at most [`SEEK_LIST_LIMIT`] and the scan answers them exactly. Unlike
    /// [`Self::listed_side`] this never refuses: a side over the bound or
    /// with no exact scan answer stays unlisted and each reached node is
    /// tested against the end pattern, as before.
    fn far_listing(&self, np: &NodePattern) -> Result<Option<Vec<NodeId>>> {
        // A property map reading a variable the walk binds later cannot be
        // resolved from the input row: the side stays the fan's to test.
        if np
            .properties
            .as_ref()
            .is_some_and(|props| !expr_ready(props, self.row))
        {
            return Ok(None);
        }
        Ok(match self.asked(np, OverAsk::Map, list_limit())? {
            Some(Capped::Within(ids)) => Some(ids),
            Some(Capped::Over) | None => None,
        })
    }

    /// Both endpoints as listed sides: the pair product while it is within
    /// [`SEEK_PAIR_LIMIT`], else a fan from the shorter side (fewer
    /// full-depth balls), the other side being its far set. An empty side
    /// makes the product empty.
    fn listed(starts: Side, ends: Side) -> Anchor {
        let (s, e) = (starts.ids().len(), ends.ids().len());
        if s == 0 || e == 0 {
            Self::empty_pair()
        } else if s.saturating_mul(e) <= SEEK_PAIR_LIMIT {
            Anchor::Pair { starts, ends }
        } else if e < s {
            Anchor::Fan {
                far: far_set(starts.ids()),
                origins: ends,
                way: Way::Backward,
            }
        } else {
            Anchor::Fan {
                far: far_set(ends.ids()),
                origins: starts,
                way: Way::Forward,
            }
        }
    }

    /// One endpoint's bounded resolution, once: a row-bound node is
    /// listed; a row-resolvable property map (or a served `WHERE` point
    /// seek) is seeked through the capped funnel — the scan stops at the first match past
    /// [`SEEK_PAIR_LIMIT`], so a non-selective map never materializes
    /// every match to learn the pair product is over the bound.
    fn probe(&self, np: &NodePattern, cell: &SideCell) -> Result<Probed> {
        let class = binding(np, self.row);
        // The classification's one table ([`admitted`]), read here as the
        // sides it already answers — a row-bound endpoint is one node, a
        // null or non-node binding matches nothing, never scan territory,
        // whose fan would run one search per row the admission rejects —
        // and the single fact the sizing needs: whether a named unbound
        // variable the funnel may seek is present.
        let unbound = match admitted(class) {
            Admitted::One(id) => return Ok(Probed::Within(Side::One(id))),
            Admitted::Empty => return Ok(Probed::Within(Side::Many(Vec::new()))),
            Admitted::Scan => false,
            Admitted::Seek(_) => true,
        };
        // The sizing, decided once: no ask answers a scan-territory side
        // through the fan; the ask it chose rides into the over answer —
        // one vocabulary, no re-projection.
        let Some(ask) = self.side_sizing(np, unbound) else {
            return self.scanned_side(np, class, cell);
        };
        Ok(match self.asked(np, ask, SEEK_PAIR_LIMIT)? {
            Some(Capped::Within(ids)) => Probed::Within(Side::Many(ids)),
            Some(Capped::Over) => Probed::Over(ask),
            // No ask answers this side: the side keeps the fan's own
            // handling.
            None => self.scanned_side(np, class, cell)?,
        })
    }

    /// The side no funnel ask answers (scan territory, or a point ask
    /// that declined): a scan filtered by the side's own `WHERE` may still
    /// list it (see `end_filter`); the fan's own seek answers it otherwise
    /// — the one spelling of that fallback, every path into it.
    fn scanned_side(
        &self,
        np: &NodePattern,
        class: NodeBinding<'_>,
        cell: &SideCell,
    ) -> Result<Probed> {
        Ok(self
            .filtered_side(np, class, cell)?
            .map_or(Probed::Scan, Probed::Within))
    }

    /// Which funnel ask, if any, answers `np`'s side — the one home of
    /// the sizing, over the classification's single fact (`unbound`: a
    /// named unbound variable the funnel may seek is present): a
    /// row-resolvable property map is seeked through the full capped
    /// funnel ([`OverAsk::Map`]), and that named, unbound variable's
    /// endpoint-only `WHERE` conjuncts are asked point-only (an index
    /// point lookup must serve it — [`OverAsk::Point`]). `None`: scan
    /// territory, the fan's own answer.
    ///
    /// A shape whose scan is a superset (the funnel's own no-answer) is
    /// not tested
    /// here: the funnel reports it in its own answer, so the sizing stays
    /// a statement about the facts the ask needs, never about how the
    /// funnel would answer.
    ///
    /// The map must resolve from the input row: a variable the walk binds
    /// later (the start's own `a` in `(a)-[*]->(b {id: a.id})`) is fan
    /// territory — the fan resolves the start first, and the partition's
    /// admission then reads the map with it bound. The canonical readiness
    /// kernel (pushdown) answers it. An unready map keeps the side
    /// unseeked even beside a seekable conjunct: the funnel would read the
    /// map against the same unresolvable row.
    ///
    /// The split: a row-resolvable property map asks as
    /// [`OverAsk::Map`]; a named, unbound variable with no map asks as
    /// [`OverAsk::Point`] (a point seek serves it, or the side keeps the
    /// fan's handling); anything else — a nameless pattern, an unready
    /// map — is `None` (scan territory, the fan's own answer).
    fn side_sizing(&self, np: &NodePattern, unbound: bool) -> Option<OverAsk> {
        match np.properties.as_ref() {
            Some(props) if expr_ready(props, self.row) => Some(OverAsk::Map),
            None if unbound => Some(OverAsk::Point),
            _ => None,
        }
    }

    /// The node `np`'s variable is bound to in the input row. The guard
    /// admits only node bindings and unbound variables.
    fn bound(&self, np: &NodePattern) -> Option<NodeId> {
        bound_node(np, self.row)
    }

    /// A probed-over side's full list, through the capped funnel: a side
    /// matching more than `limit` nodes is refused with the typed capacity
    /// error (ADR-0567) — never truncated, never listed unbounded. The
    /// funnel's contract is ascending and deduplicated, so the list needs
    /// no re-sorting. The ask is the one the probe handed over
    /// ([`Probed::Over`]): the listing re-asks through the same entry,
    /// never a re-derived sizing.
    fn listed_side(&self, np: &NodePattern, ask: OverAsk) -> Result<Side> {
        let limit = list_limit();
        let capped = self.asked(np, ask, limit)?.ok_or_else(|| {
            Error::internal("shortest-path route: a probed side lost its point seek")
        })?;
        Ok(match capped {
            Capped::Within(ids) => Side::Many(ids),
            // Over the bound: the first count past it is `limit + 1`.
            Capped::Over => {
                return Err(Error::capacity_seek_candidates(
                    limit as u64 + 1,
                    limit as u64,
                ))
            }
        })
    }

    /// The ask-to-entry dispatch, the one table ([`OverAsk`]'s two
    /// entries), from the route's endpoint-only residual through the
    /// counted funnel (the seek phase stamped inside it), stopped at the
    /// first match past `cap` — the pair bound's probe
    /// ([`SEEK_PAIR_LIMIT`]) and the full listing's bound
    /// ([`SEEK_LIST_LIMIT`]): the full funnel for a map side
    /// ([`OverAsk::Map`]), the point ask for a conjunct side
    /// ([`OverAsk::Point`], no property map). Both answer
    /// `Option<Capped>` — `None` is "no ask answers this side", the one
    /// fact each caller declines on (a superset scan, or a point seek
    /// that served nothing).
    fn asked(&self, np: &NodePattern, ask: OverAsk, cap: usize) -> Result<Option<Capped>> {
        let ExecCtx { graph, params, .. } = self.ctx;
        let conjuncts = &self.filters.endpoint;
        Ok(match ask {
            OverAsk::Point => point_candidates_capped(graph, np, self.row, params, conjuncts, cap)?,
            OverAsk::Map => node_candidates_capped(graph, np, self.row, params, conjuncts, cap)?,
        })
    }

    /// The unbound start pattern's full candidate list through the counted
    /// funnel (the fan anchor's and the loop anchor's one driver); a
    /// `WHERE`-borne `=` / `IN` on the side seeks it.
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
}
