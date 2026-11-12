//! The anchored existence probe (LABS-193/LABS-249) — the `probe.rs` arm
//! of the pattern-walk family; `super`'s module doc maps the whole tier.
//! It is cross-engine code, so it lives here, not inside either engine:
//! the shape gate, endpoint classification, and orientation
//! ([`probe_plan`] over the [`Seats`]/[`ProbePlan`]/[`AnchoredPlan`]
//! vocabulary), and the anchored adjacency walk ([`anchored_exists`]).
//! Its whole vocabulary — the endpoint classification ([`binding`]), the
//! node admission ([`pattern_node_ok`]), the rel-edge admission
//! ([`rel_edge_ok`]), and the trail bind ([`bind_trail`]) — already
//! lives at this tier, so homing the probe beside them adds no edge.
//! The types stay private to this module; only
//! [`resolved_endpoints_exist`] is re-exported — its readers are the
//! eval walk engine's `pattern_exists` (the existence rung of the
//! limit-1 walk) and the exec exists probe's endpoint rung, both
//! importing it downward from this tier.

use std::collections::BTreeMap;

use relate_core::{NodeId, Result, Value};
use relate_ir::{NodePattern, PathPattern};

use crate::overlay::ExecGraph;
use crate::row::{Row, RowRung};

use super::{bind_trail, binding, pattern_node_ok, rel_edge_ok, Binding};

/// LABS-193/LABS-249: existence probe for a pattern predicate (or an
/// EXISTS { MATCH } body) with at least one row-bound endpoint: anchors
/// on the bound side(s) and walks only the smaller side's adjacency
/// (approximate degree, [`ExecGraph::adj_len`]) of the rel's direction
/// and types — a free endpoint is checked per reached node, never
/// enumerated (enumerating it cost a full label scan with per-node
/// materialization per row, LABS-249). Materializes no rows, stops at
/// the first matching edge. `Ok(None)` = shape not eligible (no
/// row-bound endpoint, a named path variable, a pre-bound rel variable,
/// or anything but one 1-hop rel); the caller falls back to the limit-1
/// walk (`walk_collect` with `Some(1)`, pattern.rs). An endpoint bound to a null/non-node makes
/// the predicate answer `Ok(Some(false))` directly — the walked path's
/// zero-row semantics, NOT a deferral.
pub(crate) fn resolved_endpoints_exist(
    graph: &ExecGraph<'_>,
    row: &Row,
    path: &PathPattern,
    params: &BTreeMap<String, Value>,
) -> Result<Option<bool>> {
    match probe_plan(graph, row, path) {
        ProbePlan::Ineligible => Ok(None),
        ProbePlan::NoMatch => Ok(Some(false)),
        ProbePlan::Anchored(anchor) => anchored_exists(graph, row, path, params, anchor).map(Some),
    }
}

/// The pattern-check seats, paired with the row each is checked against:
/// the walk checks `nodes[0]`'s pattern before the rel binds and
/// `nodes[1]`'s after bind_trail — positionally, whichever side the
/// probe walks from.
enum Seats {
    /// The anchor fills `nodes[0]`'s seat; `nodes[1]`'s seat is checked
    /// per reached node with the rel bound.
    Forward,
    /// The anchor fills `nodes[1]`'s seat; `nodes[0]`'s seat is checked
    /// per reached node with the pre-rel row.
    Flipped,
}

impl Seats {
    /// The consequences of the orientation, spelled once beside the
    /// vocabulary: the (anchor, far) node patterns the seats pair up —
    /// the anchor's pattern is checked against the pre-rel row, the far
    /// seat's per reached node.
    fn patterns<'p>(&self, path: &'p PathPattern) -> (&'p NodePattern, &'p NodePattern) {
        match self {
            Seats::Forward => (&path.nodes[0], &path.nodes[1]),
            Seats::Flipped => (&path.nodes[1], &path.nodes[0]),
        }
    }

    /// The far seat's row (the second half of [`Seats::patterns`]): the
    /// rel-bound row when the far seat is `nodes[1]`'s pattern, the
    /// pre-rel row when flipped.
    fn far_row<'r>(&self, pre_rel: &'r Row, rel_bound: &'r Row) -> &'r Row {
        match self {
            Seats::Forward => rel_bound,
            Seats::Flipped => pre_rel,
        }
    }
}

/// The decided probe: the walked adjacency (from/to/dir) plus the seat
/// pairing the orientation match produced, not re-derived below.
enum ProbePlan {
    Ineligible,
    NoMatch,
    Anchored(AnchoredPlan),
}

/// The decided probe, bundled: the walked adjacency plus the seat pairing
/// the orientation match produced.
struct AnchoredPlan {
    from: NodeId,
    to: Option<NodeId>,
    dir: relate_ir::Direction,
    seats: Seats,
}

/// Shape gate, endpoint classification, and orientation — everything
/// before the adjacency walk, in one place.
fn probe_plan(graph: &ExecGraph<'_>, row: &Row, path: &PathPattern) -> ProbePlan {
    if path.has_path_name()
        || path.nodes.len() != 2
        || path.rels.len() != 1
        || path.rels[0].var_length
        // GPM shapes (element predicates, selectors) take the walked path
        || path.needs_gpm()
    {
        return ProbePlan::Ineligible;
    }
    let rp = &path.rels[0];
    // A pre-bound rel variable is reconciled by the walked path's
    // bind_trail before the destination's node checks run — a binding
    // the walked trail must equal, not a fresh slot — so the probe
    // cannot skip it; walk it.
    if let Some(v) = &rp.variable {
        if row.get_name(v).is_some() {
            return ProbePlan::Ineligible;
        }
    }
    let e0 = binding(&path.nodes[0], row);
    let e1 = binding(&path.nodes[1], row);
    // A variable bound to null (or any non-node) matches nothing: the
    // predicate is false (NOT EXISTS negates it) — the walked path's
    // zero-row semantics.
    if matches!(e0, Binding::Mismatch) || matches!(e1, Binding::Mismatch) {
        return ProbePlan::NoMatch;
    }
    // Orientation decides anchor, flip, and seat pairing together. A free
    // endpoint never anchors: an absent (`Unbound`) variable is exactly
    // the seat the probe does not fill, so every such shape — and the
    // nameless-both-sides one — lands in the `_` arm, where the limit-1
    // walk serves it.
    match (e0, e1) {
        (Binding::Bound(a), Binding::Bound(b)) => {
            // Both bound: check the direct edge from the smaller side.
            if graph.adj_len(a, rp.direction) <= graph.adj_len(b, rp.direction.flip()) {
                ProbePlan::Anchored(AnchoredPlan {
                    from: a,
                    to: Some(b),
                    dir: rp.direction,
                    seats: Seats::Forward,
                })
            } else {
                ProbePlan::Anchored(AnchoredPlan {
                    from: b,
                    to: Some(a),
                    dir: rp.direction.flip(),
                    seats: Seats::Flipped,
                })
            }
        }
        (Binding::Bound(a), Binding::Anonymous) => ProbePlan::Anchored(AnchoredPlan {
            from: a,
            to: None,
            dir: rp.direction,
            seats: Seats::Forward,
        }),
        (Binding::Anonymous, Binding::Bound(b)) => ProbePlan::Anchored(AnchoredPlan {
            from: b,
            to: None,
            dir: rp.direction.flip(),
            seats: Seats::Flipped,
        }),
        _ => ProbePlan::Ineligible,
    }
}

/// The anchored adjacency walk. Pattern seats stay positional: the
/// anchor-seat pattern against the pre-rel row, the far-seat pattern per
/// reached node with the rel bound (bind_trail's order).
fn anchored_exists(
    graph: &ExecGraph<'_>,
    row: &Row,
    path: &PathPattern,
    params: &BTreeMap<String, Value>,
    anchor: AnchoredPlan,
) -> Result<bool> {
    let AnchoredPlan {
        from,
        to,
        dir,
        seats,
    } = anchor;
    let rp = &path.rels[0];
    // The anchor's seat: its own pattern, pre-rel row.
    let (anchor_np, far_np) = seats.patterns(path);
    if !pattern_node_ok(graph, from, anchor_np, row, params)? {
        return Ok(false);
    }
    // The rel bind's rung, one per probe: every probed row holds the
    // entering row's names plus the rel variable (the shape gate above
    // excluded a pre-bound one), so the first bind's rung serves them
    // all — the anonymous arm the shape gate left binds nothing.
    // Map-bridge: the rung's schema names are the String domain; the
    // seat name renders once, at this per-probe rung build.
    let seat: Option<String> = rp.variable.as_ref().map(|v| v.to_string());
    let rung = RowRung::past(row, [seat.as_deref()]);
    // Single-rel shape: walk_rel's used/trail rel-dedup has nothing to
    // guard here. The filter resolves once per probe (LABS-801).
    let filter = super::RelFilter::resolve(graph, rp);
    let found = graph.try_find_neighbor_resolved::<relate_core::Error>(
        from,
        dir,
        &filter.types,
        |rid| {
            graph.cancel_tick()?;
            let hop = super::RelHop {
                pattern: rp,
                filter: &filter,
            };
            if !rel_edge_ok(graph, rid, hop, row, params)? {
                return Ok(false);
            }
            let Some(reached) = graph.hop(from, rid, dir) else {
                return Ok(false);
            };
            if to.is_some_and(|to| reached != to) {
                return Ok(false);
            }
            let mut row2 = row.clone();
            // The shape gate excludes pre-bound rel variables, so the
            // reconciliation never fails here; the check stays for uniformity.
            if !bind_trail(
                &mut row2,
                rp,
                std::slice::from_ref(&rid),
                graph,
                0,
                false,
                &rung,
            ) {
                return Ok(false);
            }
            // The far seat: positionally checked — with the rel bound when it
            // is `nodes[1]`'s pattern, with the pre-rel row when flipped.
            let far_row = seats.far_row(row, &row2);
            pattern_node_ok(graph, reached, far_np, far_row, params)
        },
    )?;
    Ok(found.is_some())
}
