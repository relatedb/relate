//! Node candidate selection and relationship expansion (variable-length
//! walks) for the classic `MATCH` path — the forward walker.
//!
//! The classic walker's terminal named-path binder lives in
//! [`super::named_path`] (one caller, the walk's last-element arm
//! below), and the LABS-161 first-hop relationship seek in
//! [`super::rel_seek`] — both extracted under LABS-805's round-2 split —
//! so the exec layer's reads stay strictly downward (the clause driver →
//! `one_path` → {`expand`, `gpm`}).
//!
//! The pivot / start-selection mechanism (ADR-0350's bound-node pivot,
//! ADR-0351's cheapest-start selector, the backward walker and the
//! `pattern_self_contained` order-safety gate) lives in [`super::pivot`]
//! (LABS-352); this module keeps the walk itself. The sink vocabulary
//! the walk offers bindings to (`Flow`, `BindingSink`, `CollectSink`)
//! lives at the rider's exec-level home, [`super::rider`], beside the
//! riders a one-path match carries.

use crate::UsedSet;
use std::collections::BTreeMap;

use relate_core::{
    bare_type_name, expected_value_type_detail, Error, NodeId, RelId, Result, Value,
};
use relate_ir::{Direction, NodePattern, PathPattern};

use crate::ctx::ExecCtx;
use crate::overlay::ExecGraph;
use crate::pattern_walk::{
    bind_pattern_node, node_candidates, rel_list_binding, Binding, PathRow, RelFilter, RelHop,
    Selection, UsedRels, VarWalk, WalkLadder,
};
use crate::row::Row;

use super::candidates::node_ok;
use super::far_probe::FarProbe;
use super::named_path::bind_named_path;
use super::pivot::{self, cheapest_start_expansion, choose_start, match_backward};
use super::predicate_spec;
use super::pushdown::Residual;
use super::rel_seek::{first_rel_seek, RelSeek};
use super::rider::BindingSink;

/// Per-path matching context shared by the node/relationship expansion
/// recursion: graph view, the path pattern, parameters, residuals, the
/// enclosing statement's CIP-150 `cypher_version`, the per-walk
/// compiled scan predicates, the path's relationship-type filters, and
/// the walk's precomputed binding-schema ladder.
#[derive(Clone, Copy)]
pub(crate) struct PathMatch<'a, 'data> {
    pub graph: &'a ExecGraph<'data>,
    pub path: &'a PathPattern,
    pub params: &'a BTreeMap<String, Value>,
    pub cypher_version: Option<u32>,
    pub residual: &'a Residual<'a>,
    pub probe: &'a FarProbe,
    /// The scan-predicate specialization (predicate_spec.rs): compiled
    /// once per row per path in `match_resolved_path`, gated by every
    /// walk loop before a candidate pays the binding work.
    pub spec: &'a predicate_spec::WalkSpec,
    /// LABS-801: the path's relationship-type filters resolved once per
    /// pattern (index `i` is `path.rels[i]`'s — [`RelFilter`] bundles the
    /// interned-id filter with the type expression's staticness verdict),
    /// so the per-edge adjacency walk compares ids and never a string.
    pub rel_filters: &'a [RelFilter],
    /// LABS-161: the first hop's forced rel id, when an
    /// `elementId(r)`/`id(r)` equality or a row-bound `r` resolves it —
    /// the walk seeks that rel instead of enumerating the adjacency.
    pub rel_seek: Option<RelId>,
    /// The walk's precomputed binding-schema ladder — built by
    /// `match_resolved_path` once per walk, after the start selection
    /// picked the arm and before the candidate loop it dispatches to.
    /// Total over every pattern shape: no walk keeps an insert path.
    pub ladder: &'a WalkLadder,
    /// The walk's hop/walk tallies (LABS-805): the admission funnels
    /// count into them per edge and `match_resolved_path` flushes once
    /// at the walk's end — never a thread-local access per edge.
    pub tallies: &'a super::WalkTallies,
}

/// Neo4j 2026's runtime entity-type check for a pre-bound pattern variable.
///
/// A node position whose variable the row already binds to a non-null
/// non-node value raises 22N27 (`Invalid input '<type>' for <var>. Expected
/// to be NODE.`); a map raises 22N01's "expected … but was of type MAP"
/// sentence. A fixed-length relationship position is the same for
/// `RELATIONSHIP`. Null stays a mismatch (no rows) — the pinned classic
/// walker behavior (`a_null_prebound_rel_variable_is_not_rebound_by_the_classic_walker`)
/// and Neo4j's own. A variable-length relationship keeps the legacy
/// list-pre-binding carve-out (`WITH [r1, r2] AS rs … ()-[rs*]->()`), so it
/// is exempt.
fn check_prebound_entities(path: &PathPattern, row: &Row) -> Result<()> {
    for node in &path.nodes {
        let Some(variable) = &node.variable else {
            continue;
        };
        // Row reads bridge through the allocation-free text read.
        let Some(value) = row.get_name(variable) else {
            continue;
        };
        if !matches!(value, Value::Null | Value::Node(_)) {
            return Err(variable.with_str(|text| entity_type_error(text, value, "NODE")));
        }
    }
    for rel in &path.rels {
        if rel.var_length {
            continue;
        }
        let Some(variable) = &rel.variable else {
            continue;
        };
        let Some(value) = row.get_name(variable) else {
            continue;
        };
        if !matches!(value, Value::Null | Value::Relationship(_)) {
            return Err(variable.with_str(|text| entity_type_error(text, value, "RELATIONSHIP")));
        }
    }
    Ok(())
}

/// The 22N27 / 22N01 error for a wrong-typed pre-bound entity: a map gets
/// 22N01's "expected … but was of type MAP" sentence, every other value
/// 22N27's invalid-entity sentence.
fn entity_type_error(variable: &str, value: &Value, expected: &str) -> Error {
    if matches!(value, Value::Map(_)) {
        Error::invalid_argument_type(expected_value_type_detail(expected, value))
    } else {
        Error::invalid_argument_type(format!(
            "Invalid input '{}' for {variable}. Expected to be {expected}.",
            bare_type_name(value)
        ))
    }
}

/// The resolve-then-walk orchestration shared by `match_one_path` and the
/// EXISTS probe: arm the destination prefilter ([`FarProbe::prepare`],
/// #303 — a blocking probe matches nothing), decode the LABS-161 rel seek
/// ([`first_rel_seek`] — `RelSeek::Gone` means the pattern matches
/// nothing; `RelSeek::Decline` declines the seek and the adjacency scan
/// runs), then
/// the walk from node 0 into `sink`.
pub(crate) fn match_resolved_path(
    ctx: ExecCtx<'_>,
    path: &PathPattern,
    rel_filters: &[RelFilter],
    residual: &Residual<'_>,
    row: Row,
    used: UsedSet,
    sink: &mut dyn BindingSink,
) -> Result<()> {
    // The walk entry reads the layer's bundle (F-1-3): the graph, params,
    // and cypher version arrive as the one `ExecCtx`, not three loose
    // parameters beside it.
    let ExecCtx {
        graph,
        params,
        cypher_version,
        ..
    } = ctx;
    // Neo4j 2026's runtime entity-type check: a pattern variable already
    // bound to a non-null value that is not the entity its position demands
    // is 22N27 (22N01 for a map), not the silent no-match the classic
    // walker's `Binding::Mismatch` produced. Checked per path, before any
    // candidate enumeration, so it fires even when the graph holds no
    // candidate for the other endpoint.
    check_prebound_entities(path, &row)?;
    let probe = FarProbe::prepare(graph, path, &row, params, cypher_version, residual);
    if probe.blocks_path() {
        return Ok(());
    }
    // The used-set enters in its set form (the caller's pattern-chain
    // carrier) and walks in its arena form from here down.
    let used = UsedRels::from_set(&used);
    // The per-walk scan-predicate specialization (predicate_spec.rs),
    // compiled once per row per path and carried by the `PathMatch` every
    // walk loop reads. The entry row's bindings are the compile's one
    // row-dependent input: a variable the row already owns never takes a
    // gate, so none of its conjuncts settle. The bound-name predicate is
    // the row tier's one spelling ([`crate::row::row_bound`]); its borrow
    // of the entry row ends with the compile, before the row moves into
    // the walk.
    let spec = {
        let entry_bound = crate::row::row_bound(&row);
        predicate_spec::WalkSpec::compile(graph, path, params, residual, &entry_bound)
    };
    // LABS-801: the path's rel-type filters arrive resolved — the caller
    // (the clause's pattern loop) builds them once per pattern, not per
    // row; every hop below filters the adjacency by interned id.
    let seek = match rel_filters.first() {
        Some(filter) => first_rel_seek(
            graph,
            path,
            &filter.types,
            &row,
            params,
            cypher_version,
            residual,
        ),
        None => RelSeek::Decline,
    };
    // The live seek's two facts, decoded once from the one value: the
    // anchor node 0's walk starts at (`Anchored`; a row-bound first node
    // keeps its own candidacy and only its hop is forced) and the rel the
    // first hop is forced to. `Gone` — the resolved rel cannot match —
    // ends the walk here: the pattern matches nothing.
    let (forced, rel_seek) = match &seek {
        RelSeek::Anchored(anchor, rid) => (Some(*anchor), Some(*rid)),
        RelSeek::Forced(rid) => (None, Some(*rid)),
        RelSeek::Gone => return Ok(()),
        RelSeek::Decline => (None, None),
    };
    // The start selection — eligibility, the bound pivot, its estimate
    // gate, and the cheapest-start fallback — in one place
    // (exec/pivot.rs, beside the two selections it orchestrates):
    // `None` is the index-0 forward walk.
    let selection = choose_start(graph, path, &row, params, residual.conjuncts, &seek);
    // LABS-369: name the start this clause's walk chose — the fact an
    // opaque `Expand` hid. Recorded per walk (the clause's FIRST walk
    // wins, so a per-row-varying pivot reports its representative start)
    // and read once at the clause's metric tick. The mapping lives
    // beside the vocabulary it interprets, so this site records one
    // value and the match below only picks the expansion.
    super::record_walk_start(pivot::walk_start(selection.as_ref(), forced.is_some()));
    // The walk's binding-schema ladder — the schema the row holds after
    // each bind, precomputed once per walk: after the start selection
    // picked the arm (the arm decides the backward prefix's rungs),
    // before the candidate loop the dispatch enters. The one ladder
    // build (pattern_walk), the classic walker's node-named policy,
    // total over every pattern shape — no insert path anywhere.
    let ladder = WalkLadder::new(&row, path, selection.as_ref(), path.has_path_name());
    // The walk's per-edge counts tally here through the whole dispatch
    // and flush on drop at the walk's end (LABS-805).
    let tallies = super::WalkTallies::default();
    // The per-walk context, constructed once and complete — after the
    // ladder build it carries, so no field is threaded in twice.
    let pm = PathMatch {
        graph,
        path,
        params,
        cypher_version,
        residual,
        probe: &probe,
        rel_seek,
        spec: &spec,
        ladder: &ladder,
        tallies: &tallies,
        rel_filters,
    };
    match selection {
        Some(Selection::Pivot((k, id))) => match_backward(&pm, k, k, id, row, used, sink),
        Some(Selection::Cheapest(k)) => cheapest_start_expansion(&pm, k, row, used, sink),
        None => match_node(&pm, 0, row, used, forced, sink),
    }
}

/// `None` means the forced id is outside the far-node index prefilter.
fn forced_candidate(np: &NodePattern, probe: &FarProbe, id: NodeId) -> Option<Vec<NodeId>> {
    if np
        .variable
        .as_ref()
        .is_some_and(|var| var.with_str(|text| probe.rejects(text, id)))
    {
        #[cfg(test)]
        super::far_probe::count_far_node_reject();
        return None;
    }
    Some(vec![id])
}

/// Arm the forward hop's destination prefilter (`node rel_idx + 1`).
/// `false` means that node cannot match.
fn admit_far_node(pm: &PathMatch<'_, '_>, rel_idx: usize, from: NodeId) -> bool {
    admit_far_node_at(
        pm,
        rel_idx + 1,
        rel_idx,
        pm.path.rels[rel_idx].direction,
        from,
    )
}

/// The prefilter arm shared by the forward hop ([`admit_far_node`], far
/// node `rel_idx + 1`) and ADR-0350's backward walker (far node `node_idx`
/// behind the current node): arm `path.nodes[node_idx]`'s prefilter for
/// `path.rels[rel_idx]`, the rel about to be traversed in `traverse`
/// direction (flipped by the backward walker — the direction travels as a
/// value). `false` means that node cannot match. The counted frontier is
/// always the traversal's own adjacency length over the rel's resolved
/// filter (`typed_adj_len_resolved`) — the var-length pre-bound switch
/// (#659) left the caller-supplied far-count arm with no caller, so no
/// parameter carries a frontier.
pub(super) fn admit_far_node_at(
    pm: &PathMatch<'_, '_>,
    node_idx: usize,
    rel_idx: usize,
    traverse: Direction,
    from: NodeId,
) -> bool {
    let PathMatch {
        graph, path, probe, ..
    } = *pm;
    let rel = &path.rels[rel_idx];
    let types = &pm.rel_filters[rel_idx].types;
    let Some(var) = path.nodes[node_idx].variable.as_ref() else {
        return true;
    };
    // An uncovered variable is the callees' own absent-arm: needs_frontier
    // answers false (so the counted path never arms it) and admit answers
    // true — no coverage guard re-checks their fact here.
    // A variable-length walk has no one-hop bound, so the frontier is unbounded.
    if rel.var_length {
        return var.with_str(|text| probe.admit(graph, text, usize::MAX));
    }
    if !var.with_str(|text| probe.needs_frontier(text)) {
        return var.with_str(|text| probe.admit(graph, text, 0));
    }
    var.with_str(|text| {
        probe.admit(
            graph,
            text,
            graph.typed_adj_len_resolved(from, traverse, types),
        )
    })
}

pub(crate) fn match_node(
    pm: &PathMatch<'_, '_>,
    idx: usize,
    row: Row,
    used: UsedRels,
    forced: Option<NodeId>,
    sink: &mut dyn BindingSink,
) -> Result<()> {
    let PathMatch {
        graph,
        path,
        params,
        cypher_version,
        residual,
        probe,
        ..
    } = *pm;
    let np = &path.nodes[idx];
    let candidates = match forced {
        Some(id) => match forced_candidate(np, probe, id) {
            Some(ids) => ids,
            None => return Ok(()),
        },
        None => node_candidates(graph, np, &row, params, cypher_version, residual.conjuncts)?,
    };
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
    // The scan-predicate gate, hoisted out of the loop: a gated
    // candidate's labels and predicates settle under one store read
    // (predicate_spec.rs), which also covers `node_ok`'s label and
    // visibility checks — the variable is unbound (no tombstone case)
    // and the pattern carries no property map (the gate's eligibility).
    let gate = pm.spec.gate_spec(np, &row);
    // The bind rungs, resolved once for the whole candidate loop: the
    // node's own bind and — on the last element — the finished row's
    // path-name bind.
    let node_rung = &pm.ladder.node[idx];
    let complete_rung = &pm.ladder.complete;
    for id in candidates {
        graph.cancel_tick()?;
        if sink.halted() {
            break;
        }
        if !predicate_spec::candidate_ok(graph, gate, id)? {
            continue;
        }
        if gate.is_none() && !node_ok(graph, id, np, &row, params, cypher_version)? {
            continue;
        }
        let mut row2 = row.clone();
        if !bind_pattern_node(
            graph,
            &mut row2,
            np,
            id,
            idx,
            path.has_path_name(),
            node_rung,
        ) {
            continue;
        }
        if !super::pushdown::conjuncts_hold(graph, &row2, &ready, params, cypher_version)? {
            continue;
        }
        if idx >= path.rels.len() {
            bind_named_path(graph, path, &mut row2, complete_rung);
            sink.offer(graph, row2, &used)?;
            if sink.halted() {
                return Ok(());
            }
        } else {
            // LABS-448: the walk phase, through the one wrapper —
            // inclusive of the nested phases the recursion performs.
            crate::ctx::phase_walk(|| expand_rel(pm, idx, id, row2, used.clone(), sink))?;
            if sink.halted() {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn expand_bound_rel_list(
    pm: &PathMatch<'_, '_>,
    rel_idx: usize,
    from: NodeId,
    row: Row,
    used: UsedRels,
    ids: &[RelId],
    sink: &mut dyn BindingSink,
) -> Result<()> {
    let PathMatch {
        graph,
        path,
        params,
        cypher_version,
        ..
    } = *pm;
    let rp = &path.rels[rel_idx];
    // The one exact-trail walk (pattern_walk): the hop-bounds gate, the
    // per-hop admission loop, and the commit block — the same spelling
    // the eval twin (`crate::eval_walk`) reads. This engine's own
    // deltas ride in as the explicit arguments: the counter-wrapped
    // per-hop admission ([`hop_candidate`], the accepted-hop counter
    // over the shared `admit_hop` — the exact-trail walk is the walk's
    // third entry path, never an admission exemption: a pre-bound rel
    // must satisfy the pattern's static type filter and edge predicate
    // like every enumerated candidate, LABS-811's convergence), and the
    // `match_node` descent.
    crate::pattern_walk::walk_exact_trail(
        graph,
        path,
        rel_idx,
        &pm.rel_filters[rel_idx].types,
        &pm.ladder.rel[rel_idx],
        from,
        PathRow { row, used },
        ids,
        |row, used, trail, current, rid| {
            hop_candidate(
                graph,
                row,
                params,
                cypher_version,
                RelHop {
                    pattern: rp,
                    filter: &pm.rel_filters[rel_idx],
                },
                rp.direction,
                used,
                trail,
                current,
                rid,
                pm.tallies,
            )
        },
        |row, used, current| match_node(pm, rel_idx + 1, row, used, Some(current), sink),
    )
}

fn expand_rel(
    pm: &PathMatch<'_, '_>,
    rel_idx: usize,
    from: NodeId,
    row: Row,
    used: UsedRels,
    sink: &mut dyn BindingSink,
) -> Result<()> {
    let PathMatch { path, .. } = *pm;
    let rp = &path.rels[rel_idx];
    // The far-node prefilter — the ONE pre-walk gate arming this rel's
    // destination node, hoisted above the var-length dispatch so every
    // rel_idx expansion arms it exactly once: the exact-trail entry and
    // the free enumeration below both descend through it here (the
    // shared walk's own doc names the engines' pre-walk refusals as
    // running at their call sites — this is exec's one).
    if !admit_far_node(pm, rel_idx, from) {
        return Ok(());
    }
    if rp.var_length {
        // The one rel-list classification ([`rel_list_binding`],
        // pattern_walk's list spelling of the walk vocabulary —
        // LABS-811): a row-bound trail dispatches the exact-trail
        // walk, a null or malformed binding — a list holding a
        // non-relationship element — matches nothing (the
        // `Mismatch` arm, the same rule as every other spelling),
        // and an absent or nameless variable takes the free
        // enumeration below. The classification this dispatch reads
        // is the same one `bind_trail` reconciles the walked trail
        // against, so the two sites cannot drift.
        match rel_list_binding(rp, &row) {
            Binding::Bound(ids) => {
                return expand_bound_rel_list(pm, rel_idx, from, row, used, &ids, sink);
            }
            Binding::Mismatch => return Ok(()),
            Binding::Unbound(_) | Binding::Anonymous => {}
        }
    }
    let (min_h, max_h) = crate::pattern_walk::hop_bounds(rp);
    // LABS-161: resolve the forced first hop before entering the walk, so
    // walk stays coordinate-free — the seeked rel goes through the same
    // per-candidate step ([`walk_candidate`]) the enumeration applies per
    // neighbor. A seek bypasses the adjacency's type filter, so
    // `resolve_rel_seek` applied the pattern's static type filter once,
    // at resolve time, in place of the enumeration's (LABS-801). A seek
    // only exists for a fixed-length first rel (min_h = max_h = 1), so no
    // zero-hop binding coexists with it.
    if rel_idx == 0 {
        if let Some(rid) = pm.rel_seek {
            return walk_candidate(
                pm,
                rel_idx,
                &row,
                &used,
                min_h,
                max_h,
                0,
                &[],
                from,
                rid,
                sink,
            );
        }
    }
    walk(
        pm,
        rel_idx,
        from,
        PathRow { row, used },
        VarWalk {
            min_h,
            max_h,
            hops: 0,
            trail: Vec::new(),
        },
        sink,
    )
}

/// The ordered per-hop checks the exact-trail walk applies to one
/// pre-bound candidate rel — the one per-hop admission
/// ([`crate::pattern_walk::admit_hop`], pattern_walk's cross-engine
/// policy) plus the exec engine's accepted-candidate tally, counted
/// once per hop the caller takes. The walk's other entry paths (the
/// LABS-161 seek, the adjacency enumeration, the backward walker) read
/// [`admit_candidate`], which spells the same admission with the
/// hop-expansion tally beside it. Returns the next node when the hop is
/// offered.
///
/// The pattern's static type filter is NOT re-checked per hop: the
/// adjacency enumeration applied it by interned id while scanning
/// (`RelTypes`, LABS-801), the rel seek applied it once at resolve time
/// (`resolve_rel_seek`), and the exact-trail walk checks its pre-bound
/// rels in [`crate::pattern_walk::walk_exact_trail`] — so no per-edge
/// rel-type read remains on the enumerated walk.
#[allow(clippy::too_many_arguments)]
pub(super) fn hop_candidate(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    hop: RelHop<'_>,
    traverse: Direction,
    used: &UsedRels,
    trail: &[RelId],
    current: NodeId,
    rid: RelId,
    tallies: &super::WalkTallies,
) -> Result<Option<NodeId>> {
    let next = crate::pattern_walk::admit_hop(
        graph,
        row,
        params,
        cypher_version,
        hop,
        traverse,
        used,
        trail,
        current,
        rid,
    )?;
    if next.is_some() {
        tallies.walk();
    }
    Ok(next)
}

/// This engine's per-hop admission — the hop-expansion tally (past the
/// first rel, the gate [`super::WalkTallies::expansion_hop`] owns) over
/// [`hop_candidate`]'s one admission-and-accepted-walk spelling — the
/// one form the shared var-length walk's admit closure, the LABS-161
/// seek's forced candidate ([`walk_candidate`]), and the backward
/// walker's flipped-direction hop (`exec/pivot.rs`'s
/// `backward_candidate`) all read. The direction rides in as
/// `traverse`, so the backward walker's mirror of this policy is the
/// one call with the flipped direction, never a second spelling. The
/// exact-trail walk's admission wraps [`hop_candidate`] alone: a
/// pre-bound trail is not an expansion, so it pays no hop-expand count.
#[allow(clippy::too_many_arguments)]
pub(super) fn admit_candidate(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    hop: RelHop<'_>,
    rel_idx: usize,
    traverse: Direction,
    used: &UsedRels,
    trail: &[RelId],
    current: NodeId,
    rid: RelId,
    tallies: &super::WalkTallies,
) -> Result<Option<NodeId>> {
    tallies.expansion_hop(rel_idx);
    hop_candidate(
        graph,
        row,
        params,
        cypher_version,
        hop,
        traverse,
        used,
        trail,
        current,
        rid,
        tallies,
    )
}

/// One candidate hop from the walk's current state — the per-candidate
/// step of the LABS-161 first-hop rel seek in `expand_rel` (the
/// enumeration's per-candidate step lives in the shared var-length
/// walk, `crate::pattern_walk::walk_var_length`, with this engine's
/// [`admit_candidate`] as its admit closure): the tick/halt prelude,
/// the ordered checks of [`admit_candidate`], then the walk recursion
/// with the hop taken.
#[allow(clippy::too_many_arguments)]
fn walk_candidate(
    pm: &PathMatch<'_, '_>,
    rel_idx: usize,
    row: &Row,
    used: &UsedRels,
    min_h: u32,
    max_h: u32,
    hops: u32,
    trail: &[RelId],
    current: NodeId,
    rid: RelId,
    sink: &mut dyn BindingSink,
) -> Result<()> {
    let PathMatch {
        graph,
        path,
        params,
        cypher_version,
        ..
    } = *pm;
    graph.cancel_tick()?;
    if sink.halted() {
        return Ok(());
    }
    let rp = &path.rels[rel_idx];
    let Some(next_node) = admit_candidate(
        graph,
        row,
        params,
        cypher_version,
        RelHop {
            pattern: rp,
            filter: &pm.rel_filters[rel_idx],
        },
        rel_idx,
        rp.direction,
        used,
        trail,
        current,
        rid,
        pm.tallies,
    )?
    else {
        return Ok(());
    };
    let mut trail2 = trail.to_vec();
    trail2.push(rid);
    walk(
        pm,
        rel_idx,
        next_node,
        PathRow {
            row: row.clone(),
            used: used.clone(),
        },
        VarWalk {
            min_h,
            max_h,
            hops: hops + 1,
            trail: trail2,
        },
        sink,
    )
}

/// The var-length rel walk — a thin call over the one var-length walk
/// (`crate::pattern_walk::walk_var_length`, pattern_walk, the
/// exact-trail walk's larger sibling) with this engine's own deltas
/// riding in as its explicit closures: the per-hop admission
/// ([`admit_candidate`] — the hop-expand and accepted-hop counters
/// over the shared `admit_hop`), and the `match_node` descent, whose
/// return is the halt signal (`sink.halted()` — a full cap or a halted
/// probe: once the sink halts, every enclosing enumeration stops, the
/// frame's find-sentinel).
fn walk(
    pm: &PathMatch<'_, '_>,
    rel_idx: usize,
    current: NodeId,
    state: PathRow,
    vw: VarWalk,
    sink: &mut dyn BindingSink,
) -> Result<()> {
    let PathMatch {
        graph,
        path,
        params,
        cypher_version,
        ..
    } = *pm;
    let rp = &path.rels[rel_idx];
    let admit = |row: &Row, used: &UsedRels, trail: &[RelId], current: NodeId, rid: RelId| {
        admit_candidate(
            graph,
            row,
            params,
            cypher_version,
            RelHop {
                pattern: rp,
                filter: &pm.rel_filters[rel_idx],
            },
            rel_idx,
            rp.direction,
            used,
            trail,
            current,
            rid,
            pm.tallies,
        )
    };
    let mut descend = |row: Row, used: UsedRels, current: NodeId| {
        match_node(pm, rel_idx + 1, row, used, Some(current), sink)?;
        Ok(sink.halted())
    };
    crate::pattern_walk::walk_var_length(
        graph,
        path,
        rel_idx,
        &pm.rel_filters[rel_idx].types,
        &pm.ladder.rel[rel_idx],
        current,
        state,
        vw,
        &admit,
        &mut descend,
    )?;
    Ok(())
}
