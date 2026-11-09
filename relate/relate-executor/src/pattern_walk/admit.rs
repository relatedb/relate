//! The admission half of the pattern-walk vocabulary — the `admit.rs`
//! arm of the family; `super`'s module doc maps the whole tier. The
//! no-rung, predicate-over-(graph, row, pattern) shapes: the one
//! canonical pattern-property evaluator ([`map_from_expr`] with its
//! [`pattern_props`] shorthand), the node admission
//! ([`pattern_node_ok`]), the rel admissions ([`rel_props_ok`],
//! [`rel_edge_ok`], the static type filter [`static_rel_type_ok`]),
//! the per-hop admission ([`admit_hop`]) the adjacency enumeration,
//! the rel seek, and both engines' exact-trail walks read, and the
//! bound-node projection ([`bound_node`]). Beside `admit_hop` sit the
//! two shared walks: the one exact-trail walk ([`walk_exact_trail`])
//! both engines' pre-bound rel-list dispatches read — the hop-bounds
//! gate, per-hop admission loop, and trail commit in one spelling —
//! and its larger sibling, the one var-length walk
//! ([`walk_var_length`]) both engines' free enumerations read — the
//! bounds-gated commit block, the max-hops/halt gate, and the
//! find-sentinel neighbor enumeration in one spelling; each engine's
//! own deltas ride in as explicit arguments. The
//! rung-bearing binds and the path-value reconstruction they commit
//! into live in `binds.rs`; the ladder in `ladder.rs`.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{Error, NodeId, RelId, Result, Value};
use relate_ir::{pattern_prop_key, Expr, NodePattern, PathPattern, RelPattern};
use relate_names::NamedToken;

use crate::eval::{eval, label_expr_matches};
use crate::overlay::ExecGraph;
use crate::props::{admit_props, props_all_match, NullPolicy};
use crate::row::{Row, RowRung};
use crate::store::RelTypes;

use super::binds::commit_trail;
use super::{binding, hop_bounds, within_hop_bounds, PathRow, UsedRels, VarWalk};

/// The one canonical pattern-property evaluator (LABS-668, F-1-1): every
/// engine that reads a pattern element's property map — CREATE/MERGE
/// materialization, the candidates funnel's seeks, the far probe, the
/// GPM walk's rel admission, the eval walker's node/rel admission
/// (pattern comprehensions, pattern predicates, expression shortest
/// paths), and the one-hop folds — evaluates it here, with one error
/// policy: a null map is empty, a non-storable value is the typed
/// `InvalidPropertyType` verdict of the crate-root property-policy walk
/// ([`crate::props::admit_props`]), a non-map value is a typed error,
/// and no caller silently rejects on a malformed map. Null *entries*
/// follow the caller's [`PatternMapRole`] (LABS-1086).
pub(crate) fn map_from_expr(
    graph: &ExecGraph<'_>,
    row: &Row,
    expr: &Expr,
    role: PatternMapRole,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<BTreeMap<String, Value>> {
    match eval(graph, row, expr, params, cypher_version)? {
        Value::Map(m) => admit_props(Arc::unwrap_or_clone(m), role.nulls()),
        // A null map constrains nothing and stores nothing: the
        // absent-map meaning, the same for both roles.
        Value::Null => Ok(BTreeMap::new()),
        _ => Err(Error::ty("pattern properties must be a map")),
    }
}

/// What a pattern element's property map is evaluated for — the one fact
/// that picks its [`NullPolicy`]. Only the two pattern policies are
/// representable: a pattern map is never `SET +=`'s removal map, so the
/// evaluator cannot be handed a policy whose null source it rejects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PatternMapRole {
    /// CREATE/MERGE materialization: the map is stored ([`NullPolicy::Drop`]
    /// — a null is not stored).
    Create,
    /// Every match reader: the map is a conjunction of equalities
    /// ([`NullPolicy::KeepAsUnmatchable`] — `{k: null}` matches nothing).
    Match,
}

impl PatternMapRole {
    /// The role's null-entry policy for the one admission walk.
    pub(crate) fn nulls(self) -> NullPolicy {
        match self {
            PatternMapRole::Create => NullPolicy::Drop,
            PatternMapRole::Match => NullPolicy::KeepAsUnmatchable,
        }
    }
}

/// The pattern element's property map: [`map_from_expr`] over the
/// element's properties, an absent property map the empty map. One
/// spelling for every pattern element — node and rel patterns both
/// carry `properties: Option<Expr>` — so a walker never re-derives the
/// absent-arm policy beside it. Readers: the candidates funnel's
/// binding arm (bound as a lazy thunk, `|| pattern_props(…)`, so the
/// map is evaluated at most once and only where needed), CREATE/MERGE
/// materialization (create_or_bind_node, create_rel), and the far
/// probe's property seeks (lenient by design — a malformed map just
/// means no seeks).
pub(crate) fn pattern_props(
    graph: &ExecGraph<'_>,
    row: &Row,
    properties: Option<&Expr>,
    role: PatternMapRole,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<BTreeMap<String, Value>> {
    match properties {
        Some(e) => map_from_expr(graph, row, e, role, params, cypher_version),
        None => Ok(BTreeMap::new()),
    }
}

/// [`pattern_props`] for a match reader, each wanted key beside its
/// plan-resolved token from the pattern's `prop_keys` — `Unresolved`
/// for a key the plan did not spell (a parameter map, a pushed-down
/// equality), which reads by name.
pub(crate) fn pattern_match_props(
    graph: &ExecGraph<'_>,
    row: &Row,
    properties: Option<&Expr>,
    prop_keys: &[NamedToken],
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Vec<(NamedToken, Value)>> {
    let want = pattern_props(
        graph,
        row,
        properties,
        PatternMapRole::Match,
        params,
        cypher_version,
    )?;
    Ok(want
        .into_iter()
        .map(|(name, value)| (pattern_prop_key(prop_keys, &name), value))
        .collect())
}

/// The eval walk engine's node admission, shared with the expression
/// shortest paths: visibility, the label expression, then the property
/// map — both halves with one error policy (LABS-668): a malformed or
/// non-storable property value ([`map_from_expr`]) or a mis-evaluating
/// label expression is a typed error, propagated, never a silent reject —
/// the spelling [`rel_edge_ok`], the funnel's `node_ok`, and the GPM
/// walk's admissions already use.
pub(crate) fn pattern_node_ok(
    graph: &ExecGraph<'_>,
    id: NodeId,
    np: &NodePattern,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<bool> {
    // Visibility first — through the label read when the pattern has a
    // label expression to check (an invisible node has no labels), the
    // bare existence read otherwise; neither clones the property map
    // (LABS-853: the full `graph.node` read paid it per candidate).
    match &np.label_expr {
        Some(label_expr) => {
            let Some(labels) = graph.node_labels(id) else {
                return Ok(false);
            };
            if !label_expr_matches(
                graph,
                row,
                params,
                label_expr,
                &labels,
                false,
                cypher_version,
            )? {
                return Ok(false);
            }
        }
        None => {
            if !graph.node_exists(id) {
                return Ok(false);
            }
        }
    }
    let want = pattern_match_props(
        graph,
        row,
        np.properties.as_ref(),
        &np.prop_keys,
        params,
        cypher_version,
    )?;
    // the wanted entries only, read key by key from the store
    Ok(props_all_match(&want, |key| {
        graph.node_prop_resolved(id, key)
    }))
}

/// The one-hop rel-property admission the walker's [`rel_edge_ok`] and
/// the agg-fold's neighbor collector share: no properties admits every
/// rel (no rel is even fetched), a missing rel rejects, and otherwise the
/// one canonical pattern-property evaluator ([`map_from_expr`],
/// LABS-668) produces the wanted map — a malformed or non-storable value
/// is its typed error, propagated, never a silent reject — with every
/// entry present and equal on the rel
/// ([`crate::props::props_all_match`]).
pub(crate) fn rel_props_ok(
    graph: &ExecGraph<'_>,
    rid: RelId,
    rp: &RelPattern,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<bool> {
    let Some(props) = rp.properties.as_ref() else {
        return Ok(true);
    };
    if !graph.rel_exists(rid) {
        return Ok(false);
    }
    let want = pattern_match_props(
        graph,
        row,
        Some(props),
        &rp.prop_keys,
        params,
        cypher_version,
    )?;
    // the wanted entries only, read key by key from the store — never
    // the rel's whole property map (LABS-853)
    Ok(props_all_match(&want, |key| {
        graph.rel_prop_resolved(rid, key)
    }))
}

/// One rel pattern's resolved walk facts (LABS-801), computed once at the
/// walk's construction: the interned-id type filter the adjacency
/// enumeration applies, plus whether the pattern's type expression IS
/// that static list (`RelPattern::type_expr_is_static_types`, the IR's
/// one spelling) — the per-edge
/// admissions ([`rel_edge_ok`], the GPM walk's `rel_admission_ok`) read
/// the flag instead of re-comparing the pattern's label list per
/// candidate.
#[derive(Clone)]
pub(crate) struct RelFilter {
    pub(crate) types: RelTypes,
    pub(crate) type_expr_static: bool,
}

impl RelFilter {
    /// Resolve `rp`'s walk facts against the view's interner — the one
    /// construction every walk's entry spells.
    pub(crate) fn resolve(graph: &ExecGraph<'_>, rp: &RelPattern) -> Self {
        RelFilter {
            types: graph.rel_types_of(rp),
            type_expr_static: rp.type_expr_is_static_types(),
        }
    }
}

/// One hop's rel: the pattern and its resolved filter as one value
/// (LABS-801) — the pair spelling the GPM walk's `GpmRelWalk` and the
/// agg fold's `HopRel` already use, so an admission's two inputs cannot
/// be mis-paired.
#[derive(Clone, Copy)]
pub(crate) struct RelHop<'p> {
    pub(crate) pattern: &'p RelPattern,
    pub(crate) filter: &'p RelFilter,
}

/// The rel-edge checks `walk_rel` and the existence probe share: the
/// pattern's type-expression and property constraints for `rid`.
/// `hop.filter` is the walk's resolved filter ([`RelFilter`]): when the
/// type expression spells exactly the static `types` list, the caller's
/// typed enumeration already applied it and the per-edge evaluation is
/// skipped; a dynamic expression (negation, conjunction, parameters)
/// keeps it.
pub(crate) fn rel_edge_ok(
    graph: &ExecGraph<'_>,
    rid: RelId,
    hop: RelHop<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<bool> {
    let rp = hop.pattern;
    if let Some(type_expr) = &rp.type_expr {
        if !hop.filter.type_expr_static {
            let Some(rel) = graph.rel_stub(rid) else {
                return Ok(false);
            };
            if !label_expr_matches(
                graph,
                row,
                params,
                type_expr,
                std::slice::from_ref(&rel.rel_type),
                true,
                cypher_version,
            )? {
                return Ok(false);
            }
        }
    }
    if !rel_props_ok(graph, rid, rp, row, params, cypher_version)? {
        return Ok(false);
    }
    Ok(true)
}

/// The pattern's static type filter for one rel, over the resolved
/// filter (LABS-801): [`RelTypes::Any`] matches any rel; a rel with no
/// resolvable type or none of the pattern's types does not match.
/// Spelled once for every user that walks rels the typed adjacency did
/// NOT filter — the exact-trail walk's pre-bound rels
/// ([`walk_exact_trail`]) and the LABS-161 rel seek (in place of the
/// adjacency enumeration's filter, `exec::candidates::resolve_rel_seek`).
/// An enumerated neighbor needs no re-check: the typed adjacency applied
/// the same filter by interned id.
pub(crate) fn static_rel_type_ok(graph: &ExecGraph<'_>, rid: RelId, types: &RelTypes) -> bool {
    types.is_any() || graph.rel_type(rid).is_some_and(|t| types.matches_name(&t))
}

/// The one per-hop admission every walk applies to a candidate rel —
/// the ordered checks the adjacency enumeration, the LABS-161 seek, and
/// both engines' exact-trail walks read: not already consumed on this
/// walk (`used` ∪ `trail`), the edge predicate ([`rel_edge_ok`]), and
/// the hop resolving the next node. Returns the next node when the hop
/// is offered. The caller owns the prelude — its tick/halt/counter
/// policy: exec's `hop_candidate` wraps this with the accepted-hop
/// counter, eval's exact-trail walk polls cancellation per hop.
///
/// The pattern's static type filter is deliberately NOT here (LABS-801):
/// the adjacency enumeration applied it by interned id while scanning,
/// the rel seek applied it once at resolve, and the exact-trail walk
/// checks its pre-bound rels per hop ([`walk_exact_trail`]) — the three
/// entry paths each filter exactly once.
// The walk's coordinate set (graph, row, params, pattern, direction,
// used, trail, current, rid) — the same argument family `hop_candidate`
// carried before the core moved here; bundled carriers would hide which
// coordinates the admission reads.
#[allow(clippy::too_many_arguments)]
pub(crate) fn admit_hop(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    hop: RelHop<'_>,
    traverse: relate_ir::Direction,
    used: &UsedRels,
    trail: &[RelId],
    current: NodeId,
    rid: RelId,
) -> Result<Option<NodeId>> {
    if used.contains(rid) || trail.contains(&rid) {
        return Ok(None);
    }
    if !rel_edge_ok(graph, rid, hop, row, params, cypher_version)? {
        return Ok(None);
    }
    Ok(graph.hop(current, rid, traverse))
}

/// The NodeId `np`'s variable is bound to in `row`, if any: the one
/// candidate a bound variable leaves the candidates funnel — the
/// vocabulary's [`Binding::bound_id`] projection over the node-side
/// classification ([`super::binding`]).
pub(crate) fn bound_node(np: &NodePattern, row: &Row) -> Option<NodeId> {
    binding(np, row).bound_id()
}

/// The one exact-trail walk: a pre-bound rel-list trail is the only
/// trail this rel can bind, so the walk hops exactly it — the shared
/// spelling of the hop-bounds gate, the per-hop admission loop, and the
/// commit block both engines' pre-bound dispatches (the eval walk
/// engine's `walk_bound_trail`, exec's `expand_bound_rel_list`)
/// hand-spelled token-identical before the hoist. Each engine keeps
/// only its own deltas, riding in as the two explicit arguments:
///
/// - `admit` — the per-hop admission over the walk's current row,
///   used-set, partial trail, and node (exec wraps [`admit_hop`] with
///   its accepted-hop counter; the eval walk reads [`admit_hop`] itself
///   — its cancellation poll is the shared loop's). The partial trail
///   it is handed at hop `i` is `ids[..i]` — the ids already hopped,
///   the exact slice a local accumulator would have re-gathered, so
///   the walk holds one spelling of the trail: the `ids` it was handed.
/// - `descend` — the committed state's continuation (exec's
///   `match_node` descent, the eval walk's `walk_pattern` descent).
///
/// A hop that fails admission ends the walk: the trail is fixed, there
/// is nothing else to enumerate. A bounds/reconciliation refusal
/// returns `Ok(())` with the descent untaken. The engines' own
/// pre-walk refusals (exec's far-node gate) run at their call sites —
/// the seam carries only true per-hop deltas.
// The walk's coordinate set (graph, path, rel index, rung, from-node,
// the row and used-set bundled in the walk's own carrier, ids) plus the
// two engine deltas — the same coordinate philosophy `admit_hop` and
// `commit_trail` carry; the carrier is the walks' own `PathRow`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn walk_exact_trail(
    graph: &ExecGraph<'_>,
    path: &PathPattern,
    rel_idx: usize,
    types: &RelTypes,
    rung: &RowRung,
    from: NodeId,
    state: PathRow,
    ids: &[RelId],
    mut admit: impl FnMut(&Row, &UsedRels, &[RelId], NodeId, RelId) -> Result<Option<NodeId>>,
    descend: impl FnOnce(Row, UsedRels, NodeId) -> Result<()>,
) -> Result<()> {
    let rp = &path.rels[rel_idx];
    // The pattern's effective bounds — the same pair the free
    // enumeration reads ([`hop_bounds`], the unbounded ceiling
    // included): a pre-bound trail outside them matches nothing,
    // exactly as no enumerated trail outside them is ever offered.
    let (min_h, max_h) = hop_bounds(rp);
    if !within_hop_bounds(min_h, max_h, ids.len() as u32) {
        return Ok(());
    }
    // LABS-801: the pre-bound rels never crossed the typed adjacency, so
    // the static type filter runs here — the caller resolved it once for
    // the walk, one check per pre-bound rel, in the hop loop's own order
    // (a hop's type refusal lands exactly where the admission's per-hop
    // check did).
    let PathRow { mut row, mut used } = state;
    let mut current = from;
    for (i, &rid) in ids.iter().enumerate() {
        // The cancellation poll both engines pay per hop — a
        // pre-bound trail can be as long as a user spells the pattern.
        graph.cancel_tick()?;
        if !static_rel_type_ok(graph, rid, types) {
            return Ok(());
        }
        // The admission loop is all-or-return, so a walk that reaches
        // the commit holds trail == ids element-for-element — hop i's
        // partial trail is always the prefix `ids[..i]`, never a
        // second, re-accumulated spelling of it.
        let Some(next) = admit(&row, &used, &ids[..i], current, rid)? else {
            return Ok(());
        };
        current = next;
    }
    // The one commit step (reconcile → bind → consume, stamping the
    // walk's precomputed rel rung); a trail that does not reconcile
    // with a pre-bound binding is skipped as an answer, nothing bound
    // and nothing consumed.
    if !commit_trail(
        &mut row,
        &mut used,
        rp,
        ids,
        graph,
        rel_idx,
        path.has_path_name(),
        rung,
    ) {
        return Ok(());
    }
    descend(row, used, current)
}

/// The one var-length walk — [`walk_exact_trail`]'s larger sibling: the
/// free enumeration of a rel's trails, with the bounds-gated commit
/// block, the max-hops/halt gate, and the neighbor enumeration both
/// engines' var-length walks (the eval walk engine's `walk_rel`, exec's
/// `expand::walk`) each hand-spelled before the hoist. The enumeration
/// driver is [`ExecGraph::try_find_neighbor_resolved`] over the
/// caller-resolved type filter (`types`, LABS-801) — the eval engine's
/// find-sentinel early stop (a limit-1 existence walk does not even
/// visit the edges past the match; without a halt the sentinel never
/// fires and the walk is the old exhaustive one). Each engine keeps
/// only its own deltas, riding in as the two explicit closures:
///
/// - `admit` — the per-hop admission over the walk's current row,
///   used-set, trail, and node (exec wraps [`admit_hop`] with its
///   accepted-hop and hop-expand counters; the eval walk reads
///   [`admit_hop`] itself). The frame polls cancellation itself, once
///   per candidate.
/// - `descend` — the committed state's continuation (exec's
///   `match_node` descent, the eval walk's `walk_pattern` descent),
///   returning the halt signal: whether the engine's sink filled or
///   halted over the descent — the halt predicate, evaluated where
///   each engine's own spelling read it (directly after the descent).
///   The frame threads it through the recursion as the find-sentinel,
///   so a halt deep in the walk stops every enclosing enumeration —
///   exactly what each engine's per-level halt re-read accomplished.
///
/// The commit step inside the bounds gate is [`commit_trail`]: a trail
/// that does not reconcile with a pre-bound binding is skipped as an
/// answer, never a prune of the walk (LABS-811) — a pre-bound trail
/// longer than this prefix extends through the rejected prefix, so the
/// enumeration below must keep going.
///
/// Returns whether the halt fired — the find-sentinel's value, each
/// recursion level's answer to its parent's enumeration; the engines'
/// entries discard it.
// The halt predicate rides as `descend`'s return value rather than a
// third closure: each engine holds ONE mutable sink handle (the
// classic walk's `BindingSink`, the eval walk's `Collect`), and a
// standalone halt closure reading it would double-borrow the handle
// the descent needs. The return-value spelling reads the halt at the
// same point each engine's own walk did — after the descent.
// The admit closure's type is the admission's own coordinate set (the
// same argument family `admit_hop` carries) — spelled inline, not
// aliased away.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) fn walk_var_length(
    graph: &ExecGraph<'_>,
    path: &PathPattern,
    rel_idx: usize,
    types: &RelTypes,
    rung: &RowRung,
    current: NodeId,
    state: PathRow,
    vw: VarWalk,
    admit: &dyn Fn(&Row, &UsedRels, &[RelId], NodeId, RelId) -> Result<Option<NodeId>>,
    descend: &mut dyn FnMut(Row, UsedRels, NodeId) -> Result<bool>,
) -> Result<bool> {
    let PathRow { row, used } = state;
    let VarWalk {
        min_h,
        max_h,
        hops,
        trail,
    } = vw;
    let rp = &path.rels[rel_idx];
    let mut halted = false;
    if within_hop_bounds(min_h, max_h, hops) {
        let mut row2 = row.clone();
        let mut used2 = used.clone();
        // The one commit step (reconcile → bind → consume, stamping the
        // walk's precomputed rel rung); a trail that does not reconcile
        // with a pre-bound binding is skipped as an answer, never a
        // prune of the walk (LABS-811): a pre-bound trail longer than
        // this prefix extends through the rejected prefix, so the
        // enumeration below must keep going.
        if commit_trail(
            &mut row2,
            &mut used2,
            rp,
            &trail,
            graph,
            rel_idx,
            path.has_path_name(),
            rung,
        ) {
            halted = descend(row2, used2, current)?;
        }
    }
    if hops >= max_h || halted {
        return Ok(halted);
    }
    // The recursion's halt answer is the find-sentinel: `Ok(true)`
    // stops the neighbor loop (the found rel is discarded), so a halt
    // deep in the walk leaves the edges past it unvisited. The caller
    // resolved the rel's type filter once for the whole walk
    // (LABS-801); the enumeration compares interned ids.
    let found = graph.try_find_neighbor_resolved::<Error>(current, rp.direction, types, |rid| {
        graph.cancel_tick()?;
        let Some(next) = admit(&row, &used, &trail, current, rid)? else {
            return Ok(false);
        };
        let mut trail2 = trail.clone();
        trail2.push(rid);
        let halted = walk_var_length(
            graph,
            path,
            rel_idx,
            types,
            rung,
            next,
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
            admit,
            descend,
        )?;
        Ok(halted)
    })?;
    Ok(found.is_some())
}
