//! Pattern predicates/comprehensions: walk the pattern, collecting
//! rows — the eval-side walk itself. The anchored existence probe
//! (LABS-193/LABS-249) lives at the shared tier in
//! `crate::pattern_walk::probe`; the shortest-path arm in `shortest.rs`.

use std::collections::BTreeMap;

use relate_core::{NodeId, RelId, Result, Value};
use relate_ir::PathPattern;

use crate::ctx::ExecCtx;
use crate::overlay::ExecGraph;
use crate::pattern_walk::{
    bind_reconstructed_path, bind_rung_name, binding, pattern_node_ok, rel_list_binding, Binding,
    PathRow, RelFilter, RelHop, UsedRels, VarWalk, WalkLadder,
};
use crate::row::{Row, RowRung};

use crate::pattern_walk::resolved_endpoints_exist;

/// The row sink `walk_pattern`/`walk_rel` share: the collected rows plus
/// an optional cap. `full()` gates every recursion level, so a cap of 1
/// is LABS-246's limit-1 existence walk — it stops at the first full
/// match; `None` collects all matches. (Bundled to stay under
/// `clippy::too_many_arguments`.)
pub(crate) struct Collect<'a> {
    limit: Option<usize>,
    out: &'a mut Vec<Row>,
}

impl Collect<'_> {
    fn full(&self) -> bool {
        self.limit.is_some_and(|l| self.out.len() >= l)
    }
}

/// The walk setup `collect_pattern_rows` and `pattern_exists` share:
/// `limit` is the only difference — `None` collects every match, `Some(n)`
/// stops at the first n (LABS-246's existence walk passes `Some(1)`).
/// The walk's binding-schema ladder is precomputed here, once per walk,
/// before the candidate loop; every per-candidate bind stamps a rung
/// instead of rebuilding a schema. The ladder is total over every
/// pattern shape — the anonymous `__trail{idx}` seat binds its empty
/// list at a zero-hop stop exactly as the named arm does — so every
/// walk runs the ladder, no insert path anywhere.
fn walk_collect(
    graph: &ExecGraph<'_>,
    row: &Row,
    path: &PathPattern,
    params: &BTreeMap<String, Value>,
    limit: Option<usize>,
) -> Result<Vec<Row>> {
    if path.nodes.is_empty() {
        return Ok(vec![row.clone()]);
    }
    // The eval walk's ladder: the forward (k=0) arm of the one ladder
    // build (pattern_walk), node-named false — the eval walk binds no
    // anonymous `__node{idx}` stubs.
    let ladder = WalkLadder::new(row, path, None, false);
    // LABS-801: the path's rel filters resolve once per walk, beside the
    // ladder — the frames below index them by rel position, never
    // re-resolve per arrival.
    let rel_filters: Vec<RelFilter> = path
        .rels
        .iter()
        .map(|rp| RelFilter::resolve(graph, rp))
        .collect();
    let mut out = Vec::new();
    walk_pattern(
        ExecCtx::new(graph, params),
        path,
        &ladder,
        &rel_filters,
        0,
        row.clone(),
        UsedRels::new(),
        None,
        &mut Collect {
            limit,
            out: &mut out,
        },
    )?;
    Ok(out)
}

pub(crate) fn collect_pattern_rows(
    graph: &ExecGraph<'_>,
    row: &Row,
    path: &PathPattern,
    params: &BTreeMap<String, Value>,
) -> Result<Vec<Row>> {
    walk_collect(graph, row, path, params, None)
}

/// LABS-246: boolean existence for a pattern predicate — the anchored
/// probe ([`resolved_endpoints_exist`], LABS-193/LABS-249) first, then a
/// limit-1 walk that stops at the first full match for every shape the
/// probe rejects (multi-hop, var-length, named path, GPM-free or not,
/// pre-bound rel, no row-bound endpoint). Existence never materializes
/// more than one row; [`collect_pattern_rows`] is for consumers that
/// need the matches themselves.
pub(crate) fn pattern_exists(
    graph: &ExecGraph<'_>,
    row: &Row,
    path: &PathPattern,
    params: &BTreeMap<String, Value>,
) -> Result<bool> {
    if let Some(hit) = resolved_endpoints_exist(graph, row, path, params)? {
        return Ok(hit);
    }
    if path.nodes.is_empty() {
        return Ok(true);
    }
    Ok(!walk_collect(graph, row, path, params, Some(1))?.is_empty())
}

/// `collect` carries the row sink and its cap: once `collect.full()`
/// every recursion level returns without further work (LABS-246's
/// limit-1 existence walk). `ladder` is the walk's precomputed
/// binding-schema ladder — total over every pattern shape, so every
/// bind stamps its rung.
#[allow(clippy::too_many_arguments)]
fn walk_pattern(
    ctx: ExecCtx<'_>,
    path: &PathPattern,
    ladder: &WalkLadder,
    filters: &[RelFilter],
    node_idx: usize,
    row: Row,
    used: UsedRels,
    forced: Option<NodeId>,
    collect: &mut Collect<'_>,
) -> Result<()> {
    let ExecCtx { graph, params, .. } = ctx;
    let np = &path.nodes[node_idx];
    // The one boundary spelling of "a rel follows this node" (F-1-10's
    // third-spelling fix): hoisted once, read by the prune below, the
    // loop's terminal arm, and the rel walk's arming — never re-derived
    // as `node_idx >= path.rels.len()` or a second `get`.
    let next_rel = path.rels.get(node_idx);
    // The rel step, bundled once: the pattern, its hop bounds, and its
    // resolved filter from the walk's table (LABS-801) — one Option where
    // parallel derivations of "a rel follows this node" used to sit.
    let step = next_rel.map(|rp| RelStep {
        rp,
        hop_bounds: crate::pattern_walk::hop_bounds(rp),
        filter: &filters[node_idx],
    });
    // The var-length rel's row binding, hoisted with the boundary it
    // rides on (loop-invariant: the node binding the loop computes
    // cannot touch the rel variable) — the same classification the exec
    // dispatch reads, dispatched the same way (LABS-811): `Mismatch` (a
    // null or malformed list) prunes before the walk enumerates a space
    // whose every offer would reject; `Bound` walks exactly the
    // pre-bound trail instead of enumerating every trail to reconcile
    // one; every other arm takes the free enumeration.
    let bound_trail = match &step {
        Some(step) if step.rp.var_length => match rel_list_binding(step.rp, &row) {
            Binding::Bound(ids) => Some(ids),
            Binding::Mismatch => return Ok(()),
            _ => None,
        },
        _ => None,
    };
    // The descent's one state (F-1-7), derived once per frame from the
    // step and its trail: `descend_candidate` matches it exhaustively,
    // and the former Option pair's unreachable `(None, Some)` corner
    // cannot be constructed.
    let descent = match (&step, bound_trail.as_ref()) {
        (Some(step), Some(ids)) => Descent::Trail {
            step,
            ids: ids.as_slice(),
        },
        (Some(step), None) => Descent::Rel { step },
        (None, _) => Descent::Terminal,
    };
    // The one node-binding classification (`pattern_walk::binding`,
    // LABS-668's vocabulary), hoisted out of the candidate loop — it
    // reads only the pattern and the pre-walk row, and every candidate
    // is decided through the ONE admission table
    // ([`Binding::accepts`], spelled once in the vocabulary) plus the
    // unbound bind below: a row-bound variable accepts only its own id
    // and keeps its row binding, a null or non-node binding accepts
    // nothing, an absent or nameless variable accepts every candidate,
    // and only an unbound variable binds — so a future `Binding`
    // variant forces its decision in the one table, not implicitly
    // through a partial read. A `Mismatch` is dead — one spelling of
    // the rel-side prune's rule: return before the seek, the walk
    // would reject every candidate anyway.
    let node_binding = binding(np, &row);
    if matches!(node_binding, Binding::Mismatch) {
        return Ok(());
    }
    // LABS-448: the seek phase — resolving the node candidates — is
    // stamped inside the one counted funnel (`node_candidates`), which
    // `pattern_node_candidates` delegates to, so this walk pays no
    // stamping of its own.
    let candidates = if let Some(id) = forced {
        vec![id]
    } else {
        pattern_node_candidates(graph, np, &row, params)?
    };
    // The node bind's rung, resolved once for the whole candidate loop.
    let node_rung = &ladder.node[node_idx];
    for id in candidates {
        if collect.full() {
            return Ok(());
        }
        graph.cancel_tick()?;
        if !pattern_node_ok(graph, id, np, &row, params)? {
            continue;
        }
        let mut row2 = row.clone();
        // An unbound variable binds the FULL node value — the CIP-257
        // virtual-node fallback included — not the stub
        // `bind_pattern_node` binds; that difference is deliberate and
        // named in `bind_pattern_node`'s doc comment. The fresh append
        // stamps the walk's precomputed rung (`bind_rung_name`).
        // The accepts-guard form `bind_pattern_node` and `bind_trail`
        // already use: the ONE admission table decides the candidate
        // (a row-bound variable only its own id, a `Mismatch` nothing —
        // unreachable here, the hoisted prune above already returned —
        // an absent or nameless variable every candidate), and only the
        // unbound arm binds, so no spelling of the table is re-rolled
        // per arm.
        if !node_binding.accepts(&id) {
            continue;
        }
        if let Binding::Unbound(v) = node_binding {
            v.with_str(|name| bind_rung_name(&mut row2, name, graph.node_value(id), node_rung));
        }
        descend_candidate(
            ctx, path, ladder, filters, node_idx, descent, id, row2, &used, collect,
        )?;
    }
    Ok(())
}

/// The rel step following a node, bundled once per walk_pattern frame
/// (LABS-801): the pattern, its hop bounds, and its resolved filter from
/// the walk's table — one Option where parallel derivations of "a rel
/// follows this node" used to sit.
struct RelStep<'a> {
    rp: &'a relate_ir::RelPattern,
    hop_bounds: (u32, u32),
    filter: &'a RelFilter,
}

/// The descent's one step state (F-1-7): the terminal path-bind, the rel
/// walk, or the pre-bound trail walk — one enum where an `Option` pair
/// with an unreachable `(None, Some)` corner used to sit, so the
/// three-way state cannot be spelled any other way.
#[derive(Clone, Copy)]
enum Descent<'a> {
    Terminal,
    Rel {
        step: &'a RelStep<'a>,
    },
    Trail {
        step: &'a RelStep<'a>,
        ids: &'a [RelId],
    },
}

/// The per-candidate descent — the eval counterpart of the exec engine's
/// `expand_rel`: the bound-trail walk (a pre-bound rel list walks exactly
/// it, the eval spelling of the exec exact-trail walk), the terminal
/// path-bind, or the rel walk, side by side. `used` is shared read-only;
/// each arm clones it only into the state it walks with.
#[allow(clippy::too_many_arguments)]
fn descend_candidate(
    ctx: ExecCtx<'_>,
    path: &PathPattern,
    ladder: &WalkLadder,
    filters: &[RelFilter],
    node_idx: usize,
    descent: Descent<'_>,
    id: NodeId,
    mut row2: Row,
    used: &UsedRels,
    collect: &mut Collect<'_>,
) -> Result<()> {
    let ExecCtx { graph, params, .. } = ctx;
    match descent {
        // A pre-bound trail is the only trail this rel can bind: walk exactly
        // it, never the enumeration that would reconcile one trail out of the
        // whole space. (A bound trail implies a rel follows this node, so the
        // terminal arm below is never reached from here.)
        Descent::Trail { step, ids } => crate::ctx::phase_walk(|| {
            walk_bound_trail(
                ctx,
                path,
                ladder,
                filters,
                node_idx,
                id,
                PathRow {
                    row: row2,
                    used: used.clone(),
                },
                step.filter,
                ids,
                collect,
            )
        }),
        Descent::Terminal => {
            bind_eval_path(graph, path, &mut row2, &ladder.complete);
            // Element inline predicates are evaluated once the whole pattern
            // is bound, so a predicate may reference an element declared
            // later in the same pattern (CIP-17/CIP-18 5.2.1). The shared
            // inline-predicate checker (`eval::predicates::inline_predicates_ok`)
            // applies every element's `WHERE` and the path's own.
            if !crate::eval::inline_predicates_ok(graph, std::slice::from_ref(path), &row2, params)?
            {
                return Ok(());
            }
            collect.out.push(row2);
            Ok(())
        }
        // LABS-448: the walk phase — following relationships, the store
        // work behind the `Rels`/`Degree` kinds — through the one wrapper,
        // inclusive of the nested phases the recursion performs.
        Descent::Rel { step } => crate::ctx::phase_walk(|| {
            walk_rel(
                ctx,
                path,
                ladder,
                filters,
                node_idx,
                step.filter,
                id,
                PathRow {
                    row: row2,
                    used: used.clone(),
                },
                VarWalk {
                    min_h: step.hop_bounds.0,
                    max_h: step.hop_bounds.1,
                    hops: 0,
                    trail: Vec::new(),
                },
                collect,
            )
        }),
    }
}

/// The eval walk's per-hop admission closure — the one per-hop admission
/// every walk applies to a candidate rel
/// ([`crate::pattern_walk::admit_hop`]) with this engine's coordinates
/// bound: the shared context's graph, params, and version, the rel
/// pattern, and the pattern's own direction. Spelled once beside the two
/// frames that offer it to a shared walk (the exact-trail walk and the
/// var-length walk); neither frame varies the admission, so neither
/// re-spells it — the eval engine pays no counter (the exec engine's
/// `hop_candidate` carries those) and polls cancellation inside the
/// shared loops.
fn admit_for<'a>(
    ctx: ExecCtx<'a>,
    hop: RelHop<'a>,
) -> impl Fn(&Row, &UsedRels, &[RelId], NodeId, RelId) -> Result<Option<NodeId>> + 'a {
    let ExecCtx { graph, params, .. } = ctx;
    move |row, used, trail, current, rid| {
        crate::pattern_walk::admit_hop(
            graph,
            row,
            params,
            hop,
            hop.pattern.direction,
            used,
            trail,
            current,
            rid,
        )
    }
}

/// The exact-trail walk a `Bound` rel-list classification dispatches
/// (the eval spelling of the exec engine's `expand_bound_rel_list`): the
/// pre-bound ids are the only trail this rel can bind, so the walk hops
/// exactly them — through the one exact-trail walk
/// ([`crate::pattern_walk::walk_exact_trail`], pattern_walk, beside the
/// per-hop admission [`crate::pattern_walk::admit_hop`] it loops and the
/// commit step [`crate::pattern_walk::commit_trail`] it lands) with the
/// eval engine's own deltas riding in as its explicit arguments: the
/// plain per-hop admission (the shared loop polls cancellation itself)
/// and the `walk_pattern` descent. The row
/// and its used-set arrive bundled in the walk's own carrier
/// ([`PathRow`], the `walk_rel` convention — bundled to stay under
/// `clippy::too_many_arguments`).
#[allow(clippy::too_many_arguments)]
fn walk_bound_trail(
    ctx: ExecCtx<'_>,
    path: &PathPattern,
    ladder: &WalkLadder,
    filters: &[RelFilter],
    rel_idx: usize,
    from: NodeId,
    state: PathRow,
    filter: &RelFilter,
    ids: &[RelId],
    collect: &mut Collect<'_>,
) -> Result<()> {
    let ExecCtx { graph, .. } = ctx;
    let rp = &path.rels[rel_idx];
    crate::pattern_walk::walk_exact_trail(
        graph,
        path,
        rel_idx,
        &filter.types,
        &ladder.rel[rel_idx],
        from,
        state,
        ids,
        admit_for(
            ctx,
            RelHop {
                pattern: rp,
                filter,
            },
        ),
        |row, used, current| {
            walk_pattern(
                ctx,
                path,
                ladder,
                filters,
                rel_idx + 1,
                row,
                used,
                Some(current),
                collect,
            )
        },
    )
}

/// The var-length rel walk — a thin call over the one var-length walk
/// ([`crate::pattern_walk::walk_var_length`], pattern_walk, the
/// exact-trail walk's larger sibling) with the eval engine's own deltas
/// riding in as its explicit closures: the per-hop admission through
/// the canonized [`crate::pattern_walk::admit_hop`] (the one per-hop
/// admission every walk applies to a candidate rel — the enumeration's
/// own interned-id type filter already applied the pattern's static
/// types, LABS-801), and the `walk_pattern` descent, whose return is the
/// halt signal (`collect.full()` — LABS-246's limit: once the sink is
/// full every enclosing enumeration stops, the frame's find-sentinel).
#[allow(clippy::too_many_arguments)]
fn walk_rel(
    ctx: ExecCtx<'_>,
    path: &PathPattern,
    ladder: &WalkLadder,
    filters: &[RelFilter],
    rel_idx: usize,
    filter: &RelFilter,
    current: NodeId,
    state: PathRow,
    vw: VarWalk,
    collect: &mut Collect<'_>,
) -> Result<()> {
    let ExecCtx { graph, .. } = ctx;
    let rp = &path.rels[rel_idx];
    let admit = admit_for(
        ctx,
        RelHop {
            pattern: rp,
            filter,
        },
    );
    let mut descend = |row: Row, used: UsedRels, current: NodeId| {
        walk_pattern(
            ctx,
            path,
            ladder,
            filters,
            rel_idx + 1,
            row,
            used,
            Some(current),
            collect,
        )?;
        Ok(collect.full())
    };
    crate::pattern_walk::walk_var_length(
        graph,
        path,
        rel_idx,
        &filter.types,
        &ladder.rel[rel_idx],
        current,
        state,
        vw,
        &admit,
        &mut descend,
    )?;
    Ok(())
}

/// Bind the pattern's path value under every name it introduces, per
/// completed candidate row of the eval walk — the eval walker's terminal
/// binder, a thin call over the one orchestration skeleton both walkers
/// share ([`bind_reconstructed_path`], pattern_walk/binds.rs) with the
/// eval walker's three deliberate deltas: the first-node read selects
/// through the one bind-name spelling
/// ([`crate::pattern_walk::node_bind_name`], the variable alone —
/// named=false, no exec-only `__node0` seat, the eval walk binds none)
/// and re-materializes through the graph, the empty-chunk hook is
/// the no-op, and no last-node fallback runs. The names bind through
/// the one path-name spelling ([`crate::pattern_walk::bind_path_names`]
/// inside the skeleton — the classic walker's `bind_named_path` reads
/// the same): the fresh names stamp `rung`, the walk's precomputed
/// completed-row schema, a pre-bound name replaces its value in place,
/// and a pattern that binds no path name pays nothing.
pub(crate) fn bind_eval_path(
    graph: &ExecGraph<'_>,
    path: &PathPattern,
    row: &mut Row,
    rung: &RowRung,
) {
    bind_reconstructed_path(
        graph,
        path,
        row,
        rung,
        |row, nodes| {
            if let Some(name) = crate::pattern_walk::node_bind_name(&path.nodes[0], 0, false) {
                if let Some(Value::Node(n)) = row.get(&name) {
                    nodes.push(graph.node(n.id).unwrap_or_else(|| (**n).clone()));
                }
            }
        },
        |_, _, _| {},
        |_, _| {},
    );
}

/// The eval walk engine's node candidates — the one counted funnel
/// ([`crate::pattern_walk::node_candidates`], the shared walk tier) with
/// no seek material, exactly as the agg fold reads it: the eval paths
/// (pattern comprehensions, EXISTS predicates, expression-form
/// `shortestPath`) then count their node-store reads in PROFILE `Db Hits`
/// like every other match, instead of enumerating past the counter
/// (LABS-375) — and carry the funnel's LABS-448 seek span, so their
/// candidate resolution is seek time like every other engine's. The
/// delegation names the tier, not an engine: no eval read points up into
/// `exec` for the funnel both engines share.
pub(crate) fn pattern_node_candidates(
    graph: &ExecGraph<'_>,
    np: &relate_ir::NodePattern,
    row: &Row,
    params: &BTreeMap<String, Value>,
) -> Result<Vec<NodeId>> {
    crate::pattern_walk::node_candidates(graph, np, row, params, &[])
}
