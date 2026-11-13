//! The GPM walk engine: the element-by-element walk over a path
//! pattern's elements (`gpm_walk_elements`), the var-length relationship
//! DFS it dispatches to (`gpm_rel_dfs` over the `GpmRelWalk` cursor),
//! and the accept policies the walk applies at its binding points
//! (`gpm_accept_node`, `gpm_accept_target`). The pinned-or-fresh
//! rebinding policy (`prebound`), the node binding (`gpm_bind_node`),
//! and the unbounded DFS cap (`unbounded_dfs_cap`) live in the parent
//! module (`gpm/mod.rs`) — the lowest module every gpm consumer,
//! including the shortest-route child, already reaches; their consumers
//! sit throughout the gpm family, so the walk is not their home. The
//! orchestrator (`match_gpm_path`), the shared context types
//! (`GpmState`, `GpmCtx`), and the per-candidate completion policies
//! (`complete_state`, `bind_path_value`) stay in the parent module; the
//! walk calls into its siblings `emit` (the rider's delivery sites) and
//! `qpp` (the quantified expansion).
//!
//! `gpm_accept_node` is walk-side by its consumers: its only call sites
//! are the walk's own — the node element accept inside
//! `gpm_walk_elements` and every expansion's target accept through
//! `gpm_accept_target` (which `qpp`'s DFS also reads). The start-point
//! selection (`gpm_start_nodes`, orchestrator-side) spells its own
//! admission against the residual pushdown, so it is not one.

use super::*;

/// Variable-length relationship DFS bounds and cursor: the hop bounds,
/// the hop count, and the base index into `GpmState::rels` where this
/// expansion's segment begins — the segment walked so far is always
/// `state.rels[base..]` (each hop pushes its rel there), so the cursor
/// carries one rel list, the state's own, never a second one beside it.
/// The rel pattern and its resolved filter ride as one pair
/// (`RelFilter`, LABS-801): the pattern the DFS binds and the filter its
/// enumeration reads cannot be mis-paired.
struct GpmRelWalk<'a> {
    min: u32,
    max: u32,
    hops: u32,
    base: usize,
    rel: &'a RelPattern,
    filter: &'a RelFilter,
}

pub(super) fn gpm_walk_elements(
    gctx: &GpmCtx<'_>,
    elements: &[PatternElement],
    filters: &[super::ElementFilter],
    index: usize,
    mut states: Vec<GpmState>,
    emit: &mut Option<GpmEmit<'_, '_>>,
) -> Result<Vec<GpmState>> {
    let Some(element) = elements.get(index) else {
        // The walk's deepest point: the incoming states are the pattern's
        // complete bindings, and the terminal drain (the delivery
        // policy's bulk sibling) is their one spelling.
        return emit::deliver_all(gctx, emit, states);
    };
    // One dispatch over the element — the node arm is the walk's accept
    // stage, the two expansion arms share the per-state drive below. Each
    // expansion element reads its resolved filter from the pattern-level
    // tables by cursor (LABS-801) — never a per-frame re-resolution.
    match element {
        PatternElement::Node(node) => {
            let mut joined = Vec::new();
            for mut state in states.drain(..) {
                if gctx.halted() {
                    break;
                }
                // the four-stage node-accept pipeline (`gpm_accept_node`) — a
                // failed stage drops the state, before the following elements
                // enumerate for it
                if gpm_accept_node(gctx, node, state.current, &mut state.row)? {
                    joined.push(state);
                }
            }
            gpm_walk_elements(gctx, elements, filters, index + 1, joined, emit)
        }
        PatternElement::Relationship(rel) => {
            // The element's filter sits at its own index in the
            // pattern-level table (LABS-801) — `index` is the walk's
            // position, so no cursor exists to mistrack.
            let super::ElementFilter::Rel(filter) = &filters[index] else {
                panic!("GpmFilters is built from the same elements: a Relationship element's slot is a Rel")
            };
            gpm_walk_expansions(
                gctx,
                elements,
                filters,
                index,
                states,
                emit,
                |state, target, emit_here| {
                    gpm_expand_relationship(gctx, state, rel, filter, target, emit_here)
                },
            )
        }
        PatternElement::Quantified(qpp) => {
            let super::ElementFilter::Group(filter) = &filters[index] else {
                panic!("GpmFilters is built from the same elements: a Quantified element's slot is a Group")
            };
            gpm_walk_expansions(
                gctx,
                elements,
                filters,
                index,
                states,
                emit,
                |state, target, emit_here| {
                    gpm_expand_quantified(gctx, state, qpp, filter, target, emit_here)
                },
            )
        }
    }
}

/// The expansion elements' shared per-state drive (`gpm/walk.rs`): the
/// target lookup, the last-element rider routing, and the state loop
/// that feeds every entering state to the caller's expansion. The node
/// element never takes this path — its arm in `gpm_walk_elements` is
/// the accept stage, not an expansion.
fn gpm_walk_expansions<'g, 'e>(
    gctx: &GpmCtx<'_>,
    elements: &[PatternElement],
    filters: &[super::ElementFilter],
    index: usize,
    mut states: Vec<GpmState>,
    emit: &mut Option<GpmEmit<'g, 'e>>,
    expand: impl Fn(
        GpmState,
        Option<&NodePattern>,
        &mut Option<GpmEmit<'g, 'e>>,
    ) -> Result<Vec<GpmState>>,
) -> Result<Vec<GpmState>> {
    // The expansion's target: the node element that follows, when one
    // does — the walk then advances past the pair; an expansion with no
    // node element behind it (the pattern continues with another rel or
    // a quantification) advances one element and hands the expansion no
    // target.
    let (target, advance) = match elements.get(index + 1) {
        Some(PatternElement::Node(node)) => (Some(node), 2),
        _ => (None, 1),
    };
    // The last element's accepts are the walk's complete rows: a rider
    // receives them there, directly (an offer can halt the enumeration
    // mid-expansion — the whole point of the rider); every earlier
    // element's accepts only feed the following elements, so they
    // materialize as before.
    let mut no_rider = None;
    let emit_here: &mut Option<GpmEmit<'g, 'e>> = if index + advance >= elements.len() {
        emit
    } else {
        &mut no_rider
    };
    let mut next = Vec::new();
    for state in states.drain(..) {
        if gctx.halted() {
            break;
        }
        next.extend(expand(state, target, emit_here)?);
    }
    gpm_walk_elements(gctx, elements, filters, index + advance, next, emit)
}

fn gpm_expand_relationship(
    gctx: &GpmCtx<'_>,
    state: GpmState,
    rel: &RelPattern,
    filter: &RelFilter,
    target: Option<&NodePattern>,
    emit: &mut Option<GpmEmit<'_, '_>>,
) -> Result<Vec<GpmState>> {
    // The var-length bound pair — the one bound-pair spelling every
    // walk engine reads (`pattern_walk::hop_bounds_with`), with the GPM engine's
    // own unbounded ceiling: the walk's memoized cap
    // (`GpmCtx::unbounded_dfs_cap`, derived once per walk). The ceiling
    // closure runs only for an unbounded pattern — a bounded one never
    // pays the rel-count enumeration behind it.
    let (min, max) = crate::pattern_walk::hop_bounds_with(rel, || gctx.unbounded_dfs_cap());
    let mut out = Vec::new();
    // where this expansion's segment begins in the state's own rel list
    // (read before the state moves into the DFS)
    let base = state.rels.len();
    // The DFS descends and backtracks on this one state in place (see
    // `gpm_rel_dfs`); only an accepted stop clones it.
    let mut state = state;
    gpm_rel_dfs(
        gctx,
        &mut state,
        target,
        GpmRelWalk {
            min,
            max,
            hops: 0,
            base,
            rel,
            filter,
        },
        &mut out,
        emit,
    )?;
    Ok(out)
}

/// The var-length rel DFS over one in-place state: each hop pushes its
/// rel id (and its far endpoint's id, when visible) onto the state,
/// descends, and pops them again — the walk never clones a state per
/// hop, and never materializes an interior hop's node or rel (the ids
/// ride, `GpmState`; LABS-853). Only an in-bounds stop clones the state,
/// into the accepted binding it delivers.
fn gpm_rel_dfs(
    gctx: &GpmCtx<'_>,
    state: &mut GpmState,
    target: Option<&NodePattern>,
    walk: GpmRelWalk<'_>,
    out: &mut Vec<GpmState>,
    emit: &mut Option<GpmEmit<'_, '_>>,
) -> Result<()> {
    let GpmCtx { graph, params, .. } = *gctx;
    let GpmRelWalk {
        min,
        max,
        hops,
        base,
        rel,
        filter,
    } = walk;
    if gctx.halted() {
        return Ok(());
    }
    if hops >= min {
        let mut accepted = state.clone();
        // The rel variable's binding over the segment walked so far —
        // the state's own rel ids from this
        // expansion's base (see `GpmRelWalk`) — bound through the stub
        // spellings, the pinned-or-fresh policy `gpm_bind_rel` owns
        // (the anonymous case is its own, like `gpm_bind_node`'s)
        let bound = gpm_bind_rel(graph, &mut accepted.row, rel, &state.rels[base..]);
        // The early conjuncts whose reading completed at this rel
        // binding (`early`) — evaluated *before* the target accept: the
        // rel variable binds first, so a conjunct naming both it and the
        // target's variable becomes ready only at the target's own event
        // inside `gpm_accept_target` (each conjunct evaluated exactly
        // once per binding), while a conjunct naming only the rel
        // variable prunes before the target check. An anonymous rel
        // binds no name, so no conjunct completes at it — `holds_at`'s
        // `None` event is that same trivial pass.
        //
        // The shared accept tail both variable spellings (named and
        // anonymous) end in: the target accept, then the one
        // complete-state delivery — a rider (this element is the
        // pattern's last) receives the state and its answer halts the
        // walk.
        if bound
            && gctx
                .walk
                .early
                .holds_at(graph, &accepted.row, rel.variable.as_ref(), params)?
            && gpm_accept_target(gctx, &mut accepted, target)?
            && emit::deliver(gctx, emit, accepted, out)?
        {
            return Ok(());
        }
    }
    if hops >= max {
        return Ok(());
    }
    // A walk invariant, fixed for the whole walk — hoisted to the walk
    // entry (`WalkState::enforce_unique`), read here, never re-derived
    // per hop.
    let enforce_unique = gctx.walk.enforce_unique;
    let current = state.current;
    graph.try_for_each_hop_resolved(current, rel.direction, &filter.types, |rid, next_id| {
        if gctx.halted() {
            return Ok(());
        }
        graph.cancel_tick()?;
        if enforce_unique && state.used.contains(&rid) {
            return Ok(());
        }
        // The admission reads only what it checks — the type, the
        // wanted property entries, the inline WHERE over the rel's stub —
        // never the full record (`rel_admission_ok`).
        if !rel_admission_ok(gctx, &state.row, rel, filter, rid)? {
            return Ok(());
        }
        // A hop the rel does not resolve (it does not touch the current
        // node) never reaches this closure: the fused hop walk drops it
        // (`hop_other`'s `None`), the classic walker's own rule
        // (`hop_candidate`).
        // PROFILE: one relationship traversed — the walkers' counting
        // event (the relationship actually walked, tallied after the hop
        // resolves; the folds' enumerated-pair event differs — see
        // `count_neighbor_walks`' doc, exec/profiling.rs), tallied
        // exactly where the classic walker tallies it. The quantified
        // spelling additionally tallies the `hops` kind once per iteration
        // (gpm/qpp.rs — its repetition unit) where the classic walker
        // ticks it per expansion step: same read kind, per-spelling unit.
        gctx.walk.tallies.walk();
        // Descend in place: the hop's rel id, its consumption, and the
        // far endpoint's slice entry go onto the state, and come off
        // again after the descent — a rel the used-set already held (a
        // non-unique walk) stays held. The far endpoint's slice entry:
        // an invisible (overlay-deleted) endpoint contributes no slice
        // entry — the family's established spelling (the eval engine's
        // path builder omits it the same way, `bind_eval_path`'s
        // `if let Some(n) = graph.node(…)`), read here as existence only
        // (the id rides, `GpmState`). The admission pipeline stays the
        // authority for whether the walk may continue from it: a deleted
        // endpoint the row already binds walks on its tombstone
        // (`node_ok`'s deleted-but-bound arm), an unbound one fails the
        // next accept's bare visibility check.
        let frame = state.push_hop(graph, rid, next_id);
        let descended = gpm_rel_dfs(
            gctx,
            state,
            target,
            GpmRelWalk {
                min,
                max,
                hops: hops + 1,
                base,
                rel,
                filter,
            },
            out,
            emit,
        );
        frame.pop(state);
        descended
    })?;
    Ok(())
}

/// The per-neighbor rel admission pipeline — the type-expression label
/// match and the property-map comparison (the classic walker's own
/// spelling of the pair, `rel_edge_ok`: the type through the rel's stub,
/// the wanted property entries read key by key — never the full record,
/// LABS-853), then the inline `WHERE` (with the rel variable bound to
/// its stub for the predicate's read — property reads go through the
/// graph, `prop`'s graph-first policy) — the one spelling the walk's
/// per-neighbor closure reads; the classic walker reads the same
/// `rel_edge_ok` beside `static_rel_type_ok` (`hop_candidate`). Both
/// engines enumerate by the element's static `types` (interned ids,
/// LABS-801): the resolved `filter` is the walk's once-computed verdict,
/// and `rel_edge_ok` reads it through the `RelHop` carrier.
fn rel_admission_ok(
    gctx: &GpmCtx<'_>,
    row: &Row,
    rel: &RelPattern,
    filter: &RelFilter,
    rid: RelId,
) -> Result<bool> {
    let GpmCtx { graph, params, .. } = *gctx;
    if !rel_edge_ok(
        graph,
        rid,
        RelHop {
            pattern: rel,
            filter,
        },
        row,
        params,
    )? {
        return Ok(false);
    }
    if let Some(predicate) = &rel.where_clause {
        let mut predicate_row = row.clone();
        if let Some(name) = &rel.variable {
            predicate_row.insert(name.to_string(), bind_rel_stub(graph, rid));
        }
        if !inline_predicate_holds(graph, &predicate_row, predicate, params)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The four-stage node-accept pipeline — label/property admission
/// (`node_ok`), the node's own bindings (`gpm_bind_node`), the node's
/// inline `WHERE`, and the early conjuncts whose reading completed at
/// this binding — the one spelling both accept sites read: the walk's
/// node element (a failed stage drops the state before the following
/// elements enumerate for it) and every expansion's target (a failed
/// stage rejects the hop).
fn gpm_accept_node(
    gctx: &GpmCtx<'_>,
    node: &NodePattern,
    id: NodeId,
    row: &mut Row,
) -> Result<bool> {
    let GpmCtx { graph, params, .. } = *gctx;
    if !node_ok(graph, id, node, row, params)? || !gpm_bind_node(graph, node, id, row) {
        return Ok(false);
    }
    if let Some(predicate) = &node.where_clause {
        if !inline_predicate_holds(graph, row, predicate, params)? {
            return Ok(false);
        }
    }
    // The early conjuncts whose reading completed at this binding
    // (`WalkState::early`): a failed one drops the accept — the prefix
    // behind it enumerates no further hops.
    gctx.walk
        .early
        .holds_at(graph, row, node.variable.as_ref(), params)
}

pub(super) fn gpm_accept_target(
    gctx: &GpmCtx<'_>,
    state: &mut GpmState,
    target: Option<&NodePattern>,
) -> Result<bool> {
    let Some(node) = target else {
        return Ok(true);
    };
    gpm_accept_node(gctx, node, state.current, &mut state.row)
}
