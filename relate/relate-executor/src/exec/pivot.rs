//! The pivot / start-selection mechanism for a `MATCH` path walk
//! (LABS-352): ADR-0350's bound-node pivot, ADR-0351's cheapest-start
//! selector, the backward walker and the `pattern_self_contained`
//! order-safety gate, and `choose_start` — the one orchestration of the
//! eligibility gates, the bound pivot, its estimate gate, and the
//! cheapest-start fallback. The forward walker stays in [`super::expand`];
//! this module owns the mechanism that chooses where a walk starts — the
//! one selection ([`choose_start`]) plus the machinery it gates — and
//! walks a pivot's prefix backwards.

#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};

use relate_core::{NodeId, RelId, Result, Value};
use relate_ir::{Direction, Expr, PathPattern, RelPattern};

use crate::overlay::ExecGraph;
use crate::pattern_walk::{
    bind_pattern_node, bound_node, commit_trail, node_candidates, RelHop, Selection, UsedRels,
};
use crate::row::{row_bound, Row};

use super::expand::{admit_candidate, admit_far_node_at, match_node, PathMatch};
use super::predicate_spec;
use super::pushdown::residual_ok_skipping;
use super::rel_seek::RelSeek;
use super::rider::BindingSink;

#[cfg(test)]
thread_local! {
    /// ADR-0350 test hook: hops the backward walker took, so a test can
    /// assert the pivot engaged (and, negated, that a fallback did not).
    static PIVOT_BACKWARD_HOPS: Cell<u64> = const { Cell::new(0) };
    /// ADR-0350 test hook: force the index-0 start so a test can compare
    /// the pivot's rows against the un-pivoted expansion.
    static PIVOT_DISABLED: Cell<bool> = const { Cell::new(false) };
}

/// The one test hook for start-selection A/B runs (stellar F-1-13):
/// `with_pivot_disabled` sets it for the closure's duration.
#[cfg(test)]
fn pivot_disabled() -> bool {
    PIVOT_DISABLED.with(Cell::get)
}

#[cfg(test)]
pub(super) fn take_pivot_backward_hops() -> u64 {
    PIVOT_BACKWARD_HOPS.with(|count| count.replace(0))
}

#[cfg(test)]
pub(super) fn with_pivot_disabled<R>(f: impl FnOnce() -> R) -> R {
    super::test_support::with_flag_disabled(&PIVOT_DISABLED, f)
}

/// The walk's chosen start as the metric vocabulary spells it (LABS-369):
/// the selection's element and direction plus the reason class — the
/// mapping lives beside the vocabulary it interprets, so the orchestration
/// site records one value.
pub(super) fn walk_start(
    selection: Option<&Selection>,
    seek_anchored: bool,
) -> crate::ctx::WalkStart {
    match selection {
        Some(Selection::Pivot((k, _))) => crate::ctx::WalkStart {
            element: *k,
            backward: true,
            reason: crate::ctx::StartReason::BoundPivot,
        },
        Some(Selection::Cheapest(k)) => crate::ctx::WalkStart {
            element: *k,
            backward: false,
            reason: crate::ctx::StartReason::Cheapest,
        },
        None => crate::ctx::WalkStart {
            element: 0,
            backward: false,
            reason: if seek_anchored {
                crate::ctx::StartReason::SeekAnchored
            } else {
                crate::ctx::StartReason::IndexZero
            },
        },
    }
}

/// ADR-0351's start loop: enumerate the chosen element's candidates — its
/// own pattern filters and residual conjuncts apply exactly as
/// `match_node`'s per-candidate checks would — bind each, and run the
/// ADR-0350 backward walker from it; the walker's suffix hand-off
/// short-circuits on that binding.
pub(super) fn cheapest_start_expansion(
    pm: &PathMatch<'_, '_>,
    k: usize,
    row: Row,
    used: UsedRels,
    sink: &mut dyn BindingSink,
) -> Result<()> {
    let PathMatch {
        graph,
        path,
        params,
        residual,
        ..
    } = *pm;
    let np = &path.nodes[k];
    let candidates = node_candidates(graph, np, &row, params, residual.conjuncts)?;
    // The residual's ready conjuncts, derived once for the whole candidate
    // loop (every candidate binds the same names before the residual
    // runs), not re-derived per scanned node — with the gate-settled
    // conjuncts already skipped (predicate_spec.rs: the gate evaluated
    // them under the candidate's own read, so the re-run is redundant).
    let ready = super::pushdown::ready_conjuncts(
        residual,
        &row,
        np.variable.as_ref(),
        pm.spec.settled_conjuncts(),
    );
    // The scan-predicate gate, hoisted out of the loop (predicate_spec.rs);
    // the admission pair below (`candidate_ok` + `admit`) owns what it
    // settles.
    let gate = pm.spec.gate_spec(np, &row);
    // The start bind's rung — the schema after the walk binds node k from
    // the entry row.
    let node_rung = &pm.ladder.node[k];
    for id in candidates {
        graph.cancel_tick()?;
        if sink.halted() {
            break;
        }
        if !predicate_spec::candidate_ok(graph, gate, id)? {
            continue;
        }
        // The admission's second half (predicate_spec.rs): a gated
        // candidate's one store read already covered `node_ok`'s
        // checks, so the pattern's own check runs only ungated.
        if !predicate_spec::admit(graph, gate, id, np, &row, params)? {
            continue;
        }
        let mut row2 = row.clone();
        if !bind_pattern_node(graph, &mut row2, np, id, k, path.has_path_name(), node_rung) {
            continue;
        }
        if !super::pushdown::conjuncts_hold(graph, &row2, &ready, params)? {
            continue;
        }
        // LABS-448: the walk phase, through the one wrapper — inclusive
        // of its nested phases.
        crate::ctx::phase_walk(|| match_backward(pm, k, k, id, row2, used.clone(), sink))?;
        if sink.halted() {
            break;
        }
    }
    Ok(())
}

/// One element's degree-scaled start estimate with its adjacent first
/// hop — the comparison both start selections run, spelled once
/// (stellar F-1-1/F-1-2/F-1-4): the bound-pivot gate and
/// `cheapest_start` rank candidates with the same numbers. Element 0's
/// first hop is the path's first rel; element k walks `rels[k - 1]`
/// backward. Element 0 is the start node the parsed-pattern invariant
/// guarantees (F-8-4's boundary, stated once here). Private to this
/// module: `choose_start` and `cheapest_start` are its callers.
fn element_estimate<'g, 'a>(
    stats: &crate::exec::graph_stats::GraphStats<'g>,
    path: &'a PathPattern,
    bound: relate_planner::Bound<'_>,
    conjuncts: impl Iterator<Item = &'a relate_ir::Expr> + Clone,
    k: usize,
) -> u64 {
    // The walk, derived once (round 2, F-2-1: one branch on `k == 0`, not
    // three spellings of it): k == 0 is the forward walker (seed
    // nodes[0], hop rels[0]); k > 0 is the backward walker's pivot
    // candidate — the walk from nodes[k] back through rels[k-1], the
    // reversed evaluation of the planned spelling (LABS-426).
    let (np, first_hop, reversed) = if k == 0 {
        (
            path.nodes
                .first()
                .expect("a parsed path pattern always has a start node"),
            path.rels.first(),
            false,
        )
    } else {
        (&path.nodes[k], path.rels.get(k - 1), true)
    };
    stats.estimate(np, bound, conjuncts, first_hop, reversed)
}

/// The walk's start selection, orchestrated in one place — the
/// eligibility gates, the bound pivot, its estimate gate, and the
/// cheapest-start fallback — beside the two selections it drives
/// ([`bound_pivot`], [`cheapest_start`]). `None` is the index-0
/// forward walk. The inputs arrive exploded — graph, path, params,
/// the residual conjuncts, the two seek facts — because the selection
/// runs before the walk's `PathMatch` exists: the binding-schema
/// ladder the context carries is built from the selection's result, so
/// the selection cannot read it.
///
/// The gates, spelled once for both selections: the LABS-161 first-rel
/// seek anchors node 0's walk specifically (both of its shapes travel as
/// one [`RelSeek`] — the anchored start node and the forced first hop),
/// so both mechanisms stand down in its presence; a shortest-path
/// function's uniqueness semantics are the GPM machinery's; and a
/// pattern whose element predicates reference a peer element's variable
/// is not element-wise self-contained ([`pattern_self_contained`]) —
/// both alternative starts evaluate in a different order than the
/// forward walk's left-to-right binding. `RepeatableElements` and GPM
/// paths never reach `match_resolved_path`: `match_one_path` and
/// `exists_probe`'s `plain_match` route them away before this point.
pub(super) fn choose_start(
    graph: &ExecGraph<'_>,
    path: &PathPattern,
    row: &Row,
    params: &BTreeMap<String, Value>,
    conjuncts: &[Expr],
    seek: &RelSeek,
) -> Option<Selection> {
    #[cfg(test)]
    if pivot_disabled() {
        return None;
    }
    // ADR-0350: the LABS-161 first-rel seek anchors node 0's walk
    // specifically, so BOTH start-selection mechanisms — the bound
    // pivot and the ADR-0351 cheapest start — stand down in its
    // presence and keep today's index-0 start. `Gone` returned at the
    // caller, so any non-`Decline` seek is the whole stand-down
    // condition.
    if !matches!(seek, RelSeek::Decline) {
        return None;
    }
    if path.is_shortest_path_function {
        return None;
    }
    // The order-safety gate is k-independent and decided once, as an
    // early return before any estimate runs (the estimate can descend
    // an index; a gate that voids every candidate must not pay for it
    // first — stellar F-2-14, and the per-k filter must not re-decide
    // it per candidate index — F-2-4). A non-self-contained pattern
    // takes no alternative start at all.
    if !pattern_self_contained(path, row) {
        return None;
    }
    // A start selection can actually run (a bound pivot to gate, or
    // the cheapest-start arm): the compound estimate inputs derive
    // once per row and are shared by both — the estimate can descend
    // indexes, so a dead derivation per row is not a small cost
    // (stellar F-8-x; q19 measured the unconditional form at 2x).
    // `est0` derives once, for the bound pivot's beats-element-0 gate
    // and the cheapest arm's per-element comparison alike.
    let stats = crate::exec::graph_stats::GraphStats::new(graph.base(), params);
    let bound = row_bound(row);
    let est0 = element_estimate(&stats, path, &bound, conjuncts.iter(), 0);
    if let Some((k, id)) = bound_pivot(path, row) {
        // The bound pivot only wins the start when it is genuinely
        // cheaper after its first hop's degree scaling (LABS-351,
        // q30's regression): a row-bound element estimates 1, but a
        // bound creator fanning ~95 posts per row is dearer than a
        // 1-hop forward start from the other end — the same compound
        // comparison the cheapest start runs.
        if element_estimate(&stats, path, &bound, conjuncts.iter(), k) < est0 {
            return Some(Selection::Pivot((k, id)));
        }
    }
    // ADR-0351: nothing is row-bound (or the bound pivot lost its
    // gate) — start at the pattern's cheapest element instead of
    // index 0.
    cheapest_start(path, &stats, &bound, conjuncts, est0).map(Selection::Cheapest)
}

/// ADR-0351/LABS-351: the pattern's cheapest element when no element is
/// row-bound (or the bound pivot lost its gate) — scored by the
/// planner's one canonical endpoint ladder
/// ([`relate_planner::endpoint_estimate`] over [`GraphStats`]): a
/// bound variable is 1, then the best measured index signal (RANGE
/// equality hits, the RANGE comparison window of ADR-0486, TEXT
/// CONTAINS candidates, label rows), then the unmeasured rank
/// sentinels, all on one scale. Only elements carrying
/// a variable are eligible: the start element's row binding is what
/// short-circuits the suffix walk's re-enumeration (`match_node`'s
/// bound arm), and an anonymous element cannot provide it. The
/// compound inputs — stats, the bound view, element 0's estimate, the
/// residual conjuncts — arrive derived once per row from
/// [`choose_start`], shared with the bound pivot's gate; only the
/// per-element estimates run here. `None` keeps today's index-0
/// start: index 0 is already (or ties as) cheapest, or every
/// alternative element's prefix is variable-length or quantified (the
/// backward walker is single-hop only).
fn cheapest_start(
    path: &PathPattern,
    stats: &crate::exec::graph_stats::GraphStats<'_>,
    bound: relate_planner::Bound<'_>,
    conjuncts: &[Expr],
    est0: u64,
) -> Option<usize> {
    // The estimates run through the one shared `element_estimate`
    // (the bound-pivot gate ranks with the same numbers — stellar
    // F-1-1/F-1-2/F-1-4).
    (1..path.nodes.len())
        .filter(|&k| path.nodes[k].variable.is_some() && prefix_plain(path, k))
        .map(|k| (k, element_estimate(stats, path, bound, conjuncts.iter(), k)))
        .filter(|&(_, est)| est < est0)
        .min_by_key(|&(k, est)| (est, k))
        .map(|(k, _)| k)
}

/// The one eligibility fact both start selections consult: the prefix
/// `rels[..k]` is plain — every hop single, no variable-length and no
/// quantified path — so the backward walker can walk it (LABS-351,
/// stellar F-1-4).
fn prefix_plain(path: &PathPattern, k: usize) -> bool {
    !path.rels[..k]
        .iter()
        .any(|rp| rp.var_length || rp.quantified)
}

/// ADR-0350: the smallest pattern index `k > 0` whose variable is row-bound
/// to a node — the pivot the expansion starts at instead of rescanning
/// index 0 — when the prefix `rels[..k]` is plain; the first row-bound
/// element is the candidate, and a variable-length or quantified prefix
/// rel declines the pivot (the backward walker is single-hop only in
/// this first cut — a documented gap, not a silent fallback). The other
/// eligibility facts — the rel-seek stand-down, a shortest-path
/// function, the order-safety gate — are [`choose_start`]'s, spelled
/// once for both start selections before this runs: the gate's term
/// that used to ride here was redundant (choose_start had already
/// returned on a non-self-contained pattern), and it is gone.
fn bound_pivot(path: &PathPattern, row: &Row) -> Option<(usize, NodeId)> {
    for k in 1..path.nodes.len() {
        // The canonical row-bound-node classification (the same one the
        // walk's candidate checks read), never a second `Value::Node`
        // shape-match here.
        if let Some(id) = bound_node(&path.nodes[k], row) {
            return prefix_plain(path, k).then_some((k, id));
        }
    }
    None
}

/// The backward walk's evaluation-order gate (the stellar High finding on
/// #346): the forward walk binds the pattern's elements left to right, so
/// an element's predicate may legally reference another element's
/// variable and see it bound. Both pivots bind in a different order — the
/// bound-node pivot walks the prefix right to left, and the cheapest
/// start evaluates its start element before anything else binds — so the
/// same reference can see an unbound (null) or differently-bound variable
/// and silently change the result. Both pivots are therefore only taken
/// when the pattern is **element-wise self-contained**: no element's
/// predicates reference another element's variable. A variable the
/// incoming row already binds (an outer pattern's binding) is safe in
/// every order.
/// `pub(super)` for the pivot tests' direct, planner-independent unit
/// coverage of the gate.
pub(super) fn pattern_self_contained(path: &PathPattern, row: &Row) -> bool {
    // The pattern's own names render once here (per-pattern setup); the
    // cross-reference probe below stays in the String domain of the
    // expr_vars sink.
    let node_vars: Vec<Option<String>> = path
        .nodes
        .iter()
        .map(|n| n.variable.as_ref().map(|v| v.to_string()))
        .collect();
    let rel_vars: Vec<Option<String>> = path
        .rels
        .iter()
        .map(|rp| rp.variable.as_ref().map(|v| v.to_string()))
        .collect();
    let entry_bound = row_bound(row);
    // The one cross-reference predicate, shared by the node and the rel
    // loop — a correctness gate never spelled twice.
    let cross_reference = |refs: &HashSet<String>, own: Option<&str>| -> bool {
        refs.iter().any(|v| {
            Some(v.as_str()) != own
                && (node_vars
                    .iter()
                    .any(|nv| nv.as_deref().is_some_and(|n| n == v))
                    || rel_vars
                        .iter()
                        .any(|rv| rv.as_deref().is_some_and(|r| r == v)))
                && !entry_bound(v)
        })
    };
    let mut refs = HashSet::new();
    for (i, np) in path.nodes.iter().enumerate() {
        refs.clear();
        if let Some(e) = &np.properties {
            crate::eval::expr_vars(e, &mut refs);
        }
        if let Some(e) = &np.where_clause {
            crate::eval::expr_vars(e, &mut refs);
        }
        if let Some(le) = &np.label_expr {
            crate::eval::label_expr_vars(le, &mut refs);
        }
        let own: Option<&str> = node_vars.get(i).and_then(|v| v.as_deref());
        if cross_reference(&refs, own) {
            return false;
        }
    }
    for (i, rp) in path.rels.iter().enumerate() {
        refs.clear();
        if let Some(e) = &rp.properties {
            crate::eval::expr_vars(e, &mut refs);
        }
        if let Some(e) = &rp.where_clause {
            crate::eval::expr_vars(e, &mut refs);
        }
        if let Some(te) = &rp.type_expr {
            crate::eval::label_expr_vars(te, &mut refs);
        }
        let own: Option<&str> = rel_vars.get(i).and_then(|v| v.as_deref());
        if cross_reference(&refs, own) {
            return false;
        }
    }
    true
}

/// ADR-0350: the backward walker for a pivot's prefix. From the bound node
/// at `idx` (its binding already in `row`, id `from`), traverse
/// `path.rels[idx - 1]` one single-hop rel at a time with the direction
/// flipped, binding the rel and the reached node under their **original**
/// indices so `bind_named_path` and the `__node0`/`__trail{i}` keys stay
/// correct (that is why this walker exists instead of a reversed pattern).
/// At `idx - 1 == 0` the prefix is complete and the suffix forward walk
/// (`match_node` at `pivot`, which sees the pivot's row binding as a
/// single-id candidate) finishes the path, `bind_named_path` firing exactly
/// once with the original indices. The far-node prefilter still applies:
/// [`admit_far_node_at`] arms node `idx - 1` with the flipped direction's
/// frontier, exactly as `expand_rel` arms the forward destination.
pub(super) fn match_backward(
    pm: &PathMatch<'_, '_>,
    idx: usize,
    pivot: usize,
    from: NodeId,
    row: Row,
    used: UsedRels,
    sink: &mut dyn BindingSink,
) -> Result<()> {
    let PathMatch { graph, path, .. } = *pm;
    let rp = &path.rels[idx - 1];
    // The pivot guarantees a single-hop prefix rel; admit_candidate's checks
    // below are the forward walk's, spelled once, walked the other way —
    // the flipped direction travels as a value, so the rel pattern itself
    // is never cloned per step.
    let back = rp.direction.flip();
    if !admit_far_node_at(pm, idx - 1, idx - 1, back, from) {
        return Ok(());
    }
    graph.try_for_each_neighbor_resolved(from, back, &pm.rel_filters[idx - 1].types, |rid| {
        backward_candidate(pm, idx, pivot, rp, back, from, &row, &used, rid, sink)
    })
}

/// One candidate backward hop — the per-candidate step of
/// [`match_backward`], mirroring `walk_candidate` for a single-hop rel: the
/// tick/halt prelude, the hop admission ([`admit_candidate`]) with the
/// direction flipped, then the rel commit ([`crate::pattern_walk::commit_trail`],
/// pre-bound reconciliation included), the node binding under the
/// original index (an existing non-matching binding fails this
/// candidate), `node_ok` + `residual_ok`, and the recursion toward
/// index 0.
#[allow(clippy::too_many_arguments)]
fn backward_candidate(
    pm: &PathMatch<'_, '_>,
    idx: usize,
    pivot: usize,
    rp: &RelPattern,
    traverse: Direction,
    from: NodeId,
    row: &Row,
    used: &UsedRels,
    rid: RelId,
    sink: &mut dyn BindingSink,
) -> Result<()> {
    let PathMatch {
        graph,
        path,
        params,
        residual,
        ..
    } = *pm;
    graph.cancel_tick()?;
    if sink.halted() {
        return Ok(());
    }
    let Some(prev) = admit_candidate(
        graph,
        row,
        params,
        RelHop {
            pattern: rp,
            filter: &pm.rel_filters[idx - 1],
        },
        idx - 1,
        traverse,
        used,
        &[],
        from,
        rid,
        pm.tallies,
    )?
    else {
        return Ok(());
    };
    // The scan-predicate gate over the reached node, before any of the
    // binding work below (predicate_spec.rs). The pre-clone `row` is the
    // right one: the node's variable is unbound in it. What a gated
    // candidate's one store read settles is `admit`'s policy
    // (predicate_spec.rs), stated once there.
    let np = &path.nodes[idx - 1];
    let gate = pm.spec.gate_spec(np, row);
    if !predicate_spec::candidate_ok(graph, gate, prev)? {
        return Ok(());
    }
    #[cfg(test)]
    PIVOT_BACKWARD_HOPS.with(|count| count.set(count.get() + 1));
    let rel_idx = idx - 1;
    // The two binds' rungs — the schemas after rel j's and node j's
    // backward binds (the rel's stamp lands before `node_ok` reads the
    // row, preserving the bind/check order both walk directions share).
    let rel_rung = &pm.ladder.rel[rel_idx];
    let node_rung = &pm.ladder.node[rel_idx];
    let mut row2 = row.clone();
    // The one commit step (reconcile → bind → consume, stamping the
    // walk's precomputed rel rung) — the same spelling the forward
    // walk's `walk` and both engines' exact-trail walks read; the
    // used-set clone is hoisted above the commit (commit_trail consumes
    // into it in place), so a candidate the reconciliation rejects pays
    // one clone it does not use. The original (unflipped) rel pattern
    // carries the variable, `bind_trail`'s policy for both walk
    // directions.
    let mut used2 = used.clone();
    if !commit_trail(
        &mut row2,
        &mut used2,
        &path.rels[rel_idx],
        std::slice::from_ref(&rid),
        graph,
        rel_idx,
        path.has_path_name(),
        rel_rung,
    ) {
        return Ok(());
    }
    // `node_ok` runs BEFORE the node's own binding, matching
    // `match_node`'s order — a self-referencing property map
    // (`(a {p: a.q})`) must evaluate identically in both walk
    // directions, and binding first would let the backward walk see
    // the candidate's own value where the forward walk sees unbound.
    // The admission's second half (predicate_spec.rs): a gated
    // candidate's one store read already covered `node_ok`'s checks,
    // so the pattern's own check runs only ungated.
    if !predicate_spec::admit(graph, gate, prev, np, &row2, params)? {
        return Ok(());
    }
    if !bind_pattern_node(
        graph,
        &mut row2,
        np,
        prev,
        rel_idx,
        path.has_path_name(),
        node_rung,
    ) {
        return Ok(());
    }
    // The gate-settled conjuncts skip the re-run (predicate_spec.rs): the
    // gate evaluated them under the candidate's own read.
    if !residual_ok_skipping(graph, &row2, params, residual, pm.spec.settled_conjuncts())? {
        return Ok(());
    }
    if rel_idx == 0 {
        // Prefix complete: the suffix forward walk from the pivot finishes
        // the path (`bind_named_path` fires at its end as today).
        match_node(pm, pivot, row2, used2, None, sink)
    } else {
        match_backward(pm, rel_idx, pivot, prev, row2, used2, sink)
    }
}
