//! Fixed-length neighborhood aggregate fold.

use std::collections::{BTreeMap, HashMap};

use indexmap::map::Entry;
use indexmap::IndexMap;
use relate_core::{Error, NodeId, RelId, Result, Value};
use relate_ir::{Clause, Direction, Expr, NodePattern, PathPattern, Projection, RelPattern};
use relate_names::NamedToken;

use crate::eval::{binary, eval};
use crate::group_key::GroupKey;
use crate::overlay::{ExecGraph, RetainedTotal};
use crate::pattern_walk::{binding, node_candidates, rel_props_ok, Binding, NodeBinding};
use crate::row::Row;
use crate::store::RelTypes;

use super::super::candidates::node_ok;
use super::super::WalkTallies;
use super::plan::{compile_path, CountSeat, KeyPlan, PathPlan, PathPred};
use super::{
    finish_folded_rows, fold_return, note_path_binding, project_ranked_row, CachedKey,
    FoldedReturn, Rank, StreamAgg,
};

/// The path fold's group table: per group, the key values beside the
/// group's aggregate pairs — each count seat structurally paired with the
/// [`StreamAgg`] it feeds, so the offer site consumes pairs and never
/// zips two same-length vectors by position.
type Groups = IndexMap<Vec<GroupKey>, (Vec<Value>, Vec<(CountSeat, StreamAgg)>)>;

struct HopEnd {
    start: NodeId,
    end: NodeId,
    rel: RelId,
}

/// The path fold's graph-scoped caches: a node's properties and its
/// cached key are row-independent, so one map warms across the whole fold.
/// The row-scoped predicate verdicts live on [`FoldEnv`], which is built
/// fresh per row — a `PathPred::Cmp` verdict binds its other operand to
/// that row, so it cannot leak into the next by construction.
struct FoldCaches {
    props: rustc_hash::FxHashMap<NodeId, HashMap<String, Value>>,
    keys: rustc_hash::FxHashMap<NodeId, CachedKey>,
}

struct FoldEnv<'a> {
    graph: &'a ExecGraph<'a>,
    row: &'a Row,
    params: &'a BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    plan: &'a PathPlan,
    caches: &'a mut FoldCaches,
    /// The group table the fold accumulates into rides the env the way
    /// `caches` does, so the recursive `fold_steps` frames forward only
    /// the walk state, never these. The seat template stays with the
    /// plan (`plan.seats`) — the one canonical holder.
    groups: &'a mut Groups,
    /// ADR-0463 (LABS-781): the group table's retaining-total guard —
    /// every new group's landing (map entry, key, retained key-half
    /// values) grows it, enforced at the per-value cap while the groups
    /// land, the same protocol the match-then-aggregate group map
    /// follows. Rides the env the way `groups` does.
    group_retained: &'a mut RetainedTotal,
    /// The row-scoped predicate verdicts: `PathPred::Cmp`'s other operand
    /// evaluates against this row, so a verdict is valid only within it.
    /// The env is constructed fresh per row, so the map starts empty.
    preds: rustc_hash::FxHashMap<(usize, NodeId), bool>,
    /// The fold's walk tallies (LABS-805): the neighbor walks
    /// `collect_neighbors` counts accumulate here across every row and
    /// flush on drop at the fold's end — never a thread-local access
    /// per edge.
    tallies: &'a WalkTallies,
    /// The steps' resolved rel-type filters (LABS-801), one per plan
    /// step, resolved once beside the plan — never per row or anchor.
    step_types: &'a [RelTypes],
}

struct HopWalk<'a> {
    path: &'a PathPattern,
    rel_idx: usize,
    /// The step's resolved rel-type filter (LABS-801) — resolved once per
    /// fold beside the plan, read by every anchor's neighbor collection.
    types: &'a RelTypes,
    start_slot: usize,
    end_slot: usize,
    bound_start: Option<NodeId>,
    bound_end: Option<NodeId>,
    skip_rel: Option<RelId>,
}

fn path_pred_holds(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    pred: &PathPred,
    id: NodeId,
) -> Result<bool> {
    match pred {
        PathPred::Null { key, is_null } => {
            let value = graph.node_prop_resolved(id, key).unwrap_or(Value::Null);
            Ok(value.is_null() == *is_null)
        }
        PathPred::Cmp { key, op, other } => {
            let prop = graph.node_prop_resolved(id, key).unwrap_or(Value::Null);
            let other = eval(graph, row, other, params, cypher_version)?;
            Ok(binary(graph, *op, prop, other)?.truthy() == Some(true))
        }
    }
}

fn slot_preds_hold(env: &mut FoldEnv<'_>, slot: usize, id: NodeId) -> Result<bool> {
    if let Some(&ok) = env.preds.get(&(slot, id)) {
        return Ok(ok);
    }
    let mut ok = true;
    for pred in &env.plan.conjuncts[slot].preds {
        if !path_pred_holds(env.graph, env.row, env.params, env.cypher_version, pred, id)? {
            ok = false;
            break;
        }
    }
    env.preds.insert((slot, id), ok);
    Ok(ok)
}

fn accept_slot(
    env: &mut FoldEnv<'_>,
    node: &NodePattern,
    slot: usize,
    binding: NodeBinding<'_>,
    id: NodeId,
) -> Result<bool> {
    // The canonical row-binding classification (pattern_walk.rs), resolved
    // once by the caller: a row-bound node accepts only itself, a
    // null/non-node binding accepts nothing, an absent or nameless
    // variable accepts every node. A step whose two endpoints share a slot
    // (one repeated variable) resolves its target to the anchor, so the
    // vocabulary admits exactly the self-loop.
    if !binding.accepts(&id) {
        return Ok(false);
    }
    if !node_ok(env.graph, id, node, env.row, env.params, env.cypher_version)? {
        return Ok(false);
    }
    slot_preds_hold(env, slot, id)
}

/// Fills `out` with this origin's accepted neighbors (cleared at entry,
/// so the caller's buffer is reused across calls without carrying
/// anything over — LABS-337's dealloc fix). `types` arrives resolved
/// from the hop step (LABS-801) — one name→id crossing per step, never
/// per origin.
fn collect_neighbors(
    env: &FoldEnv<'_>,
    origin: NodeId,
    rel: &RelPattern,
    types: &RelTypes,
    dir: Direction,
    skip_rel: Option<RelId>,
    out: &mut Vec<(RelId, NodeId)>,
) -> Result<()> {
    out.clear();
    let graph = env.graph;
    graph.try_for_each_hop_resolved(origin, dir, types, |rid, id| {
        graph.cancel_tick()?;
        if skip_rel == Some(rid) {
            return Ok::<(), Error>(());
        }
        // The fold's counting event (LABS-375, unchanged by LABS-805):
        // the enumerated (origin, rel) pair, tallied after the
        // DIFFERENT RELATIONSHIPS skip filter — so a re-visited edge
        // the pattern excludes is not counted — and before the rel's
        // property filter and hop resolution below, the fold's own
        // contract where the walkers tally post-admission (see
        // `count_neighbor_walks`' doc for the per-engine events).
        env.tallies.walk();
        // The canonical one-hop rel-property admission (pattern_walk.rs's
        // `rel_props_ok`) — the same filter `rel_edge_ok` applies.
        if !rel_props_ok(graph, rid, rel, env.row, env.params, env.cypher_version)? {
            return Ok::<(), Error>(());
        }
        out.push((rid, id));
        Ok::<(), Error>(())
    })?;
    Ok(())
}

/// One end's binding resolution, in the canonical vocabulary: the walk's
/// explicit override (a previously walked hop's bound id) wins, folded in
/// as `Binding::Bound`, then the canonical row binding
/// (`pattern_walk::binding`) — a bound node is the single candidate, a
/// null/non-node binding matches nothing (`Mismatch`), an absent or
/// nameless variable is free.
fn resolve_bound<'a>(over: Option<NodeId>, node: &'a NodePattern, row: &Row) -> NodeBinding<'a> {
    match over {
        Some(id) => Binding::Bound(id),
        None => binding(node, row),
    }
}

fn expand_hop(env: &mut FoldEnv<'_>, walk: &HopWalk<'_>) -> Result<Vec<HopEnd>> {
    let start_node = &walk.path.nodes[walk.rel_idx];
    let end_node = &walk.path.nodes[walk.rel_idx + 1];
    let rel = &walk.path.rels[walk.rel_idx];
    let start_binding = resolve_bound(walk.bound_start, start_node, env.row);
    let end_binding = resolve_bound(walk.bound_end, end_node, env.row);
    // One exhaustive decision over the resolved bindings: which side
    // anchors the walk, the orientation it dictates, and the anchor
    // list — produced together, so the three cannot drift apart. The
    // bound end anchors a reversed walk (the neighbors are the starts),
    // exactly as the starts anchor the forward walk; walk_hop's own
    // anchor acceptance handles an unaccepted anchor (the verdict is
    // memoized in env.preds). `bound_id` (the vocabulary's projection)
    // reads the target side's filter: a free target filters nothing.
    let start_side = HopSide {
        node: start_node,
        slot: walk.start_slot,
    };
    let end_side = HopSide {
        node: end_node,
        slot: walk.end_slot,
    };
    let (anchors, orientation) = match (start_binding, end_binding) {
        // A null or wrong-typed binding on either side: no hop can match
        // it (the walk's zero-row semantics).
        (Binding::Mismatch, _) | (_, Binding::Mismatch) => return Ok(Vec::new()),
        (Binding::Bound(start_id), end_binding) => (
            vec![start_id],
            HopOrientation::Forward {
                anchor: start_side,
                target: end_side,
                bound_target: end_binding.bound_id(),
            },
        ),
        // The bound end is the single anchor of a reversed walk.
        (_, Binding::Bound(end_id)) => (
            vec![end_id],
            HopOrientation::Reversed {
                anchor: end_side,
                target: start_side,
            },
        ),
        // Both sides free: LABS-448's seek — the fold's unanchored
        // pattern starts, stamped inside the one counted funnel. A free
        // target filters nothing — `None` spelled, not projected.
        (_, _) => (
            node_candidates(
                env.graph,
                start_node,
                env.row,
                env.params,
                env.cypher_version,
                &env.plan.conjuncts[walk.start_slot].exprs,
            )?,
            HopOrientation::Forward {
                anchor: start_side,
                target: end_side,
                bound_target: None,
            },
        ),
    };
    walk_hop(
        env,
        HopRel {
            pattern: rel,
            types: walk.types,
            skip: walk.skip_rel,
        },
        anchors,
        orientation,
    )
}

/// One side of an oriented hop expansion: the pattern and its slot.
struct HopSide<'a> {
    node: &'a NodePattern,
    slot: usize,
}

/// The hop's relationship: its pattern, its resolved type filter
/// (LABS-801, the fold's one resolution), plus the DIFFERENT
/// RELATIONSHIPS skip.
struct HopRel<'a> {
    pattern: &'a RelPattern,
    types: &'a RelTypes,
    skip: Option<RelId>,
}

/// The walk's orientation: which side anchors it and which is the target —
/// the two valid combinations of the walk's direction knobs, decided once
/// by the resolved bounds, so a mismatched triple cannot be constructed.
enum HopOrientation<'a> {
    /// The start side anchors; the ends are the walk's targets.
    Forward {
        anchor: HopSide<'a>,
        target: HopSide<'a>,
        /// The row binding the end side resolved to, filtering the ends.
        bound_target: Option<NodeId>,
    },
    /// The bound end anchors; the walk runs reversed and the starts are
    /// the targets (nothing filters them — the anchor IS the bound).
    Reversed {
        anchor: HopSide<'a>,
        target: HopSide<'a>,
    },
}

impl HopOrientation<'_> {
    /// The hop's traversal direction (F-1-6): the orientation's own
    /// forward/reversed decision, spelled once here — the walk's call
    /// sites never hold a loose `reverse: bool` beside the enum.
    fn traverse_dir(&self, rel: &RelPattern) -> Direction {
        match self {
            Self::Forward { .. } => rel.direction,
            Self::Reversed { .. } => rel.direction.flip(),
        }
    }

    /// The emitted hop's endpoint roles: reversed, the anchor is the
    /// hop's end — the orientation names the roles once, so the emit
    /// never re-derives them from a bool.
    fn roles(&self, anchor_id: NodeId, other_id: NodeId) -> (NodeId, NodeId) {
        match self {
            Self::Forward { .. } => (anchor_id, other_id),
            Self::Reversed { .. } => (other_id, anchor_id),
        }
    }

    /// The target side's row-bound filter: a free target filters nothing
    /// (`None` spelled, not projected) — the Forward variant's fact.
    fn bound_target(&self) -> Option<NodeId> {
        match self {
            Self::Forward { bound_target, .. } => *bound_target,
            Self::Reversed { .. } => None,
        }
    }
}

/// The one oriented hop expansion: each accepted anchor node's
/// relationships (collected forward, or reversed when the anchor is the
/// path's end), each neighbor accepted on the target side and emitted as a
/// `HopEnd` with the roles the orientation dictates. `bound_target`
/// filters the neighbor side when the pattern bound it, and a step whose
/// endpoints share a slot (one repeated variable) admits only the
/// anchor's self-loop.
fn walk_hop(
    env: &mut FoldEnv<'_>,
    rel: HopRel<'_>,
    anchors: impl IntoIterator<Item = NodeId>,
    orientation: HopOrientation<'_>,
) -> Result<Vec<HopEnd>> {
    // The two sides, read off the orientation without a positional
    // re-tuple (F-1-6): the direction, roles, and target filter are the
    // enum's own methods below, so the orientation stays the one decided
    // form the whole walk reads.
    let (anchor, target) = match &orientation {
        HopOrientation::Forward { anchor, target, .. } => (anchor, target),
        HopOrientation::Reversed { anchor, target } => (anchor, target),
    };
    // A step whose two endpoints share a slot is one repeated variable
    // (`(x)-[:R]->(x)`, `plan.rs`'s shared-slot table): the second
    // occurrence classifies `Bound` in the walker (its mid-walk rung
    // bind), so the target must be the anchor itself — a self-loop. An
    // anonymous or single-occurrence endpoint never shares a slot, so the
    // common counting walk pays one integer comparison per step.
    let self_loop = anchor.slot == target.slot;
    // The traversal direction, resolved once from the orientation — every
    // anchor's neighbor traversal reads it below.
    let dir = orientation.traverse_dir(rel.pattern);
    let mut hops = Vec::new();
    // One neighbor buffer reused across the anchors: the previous
    // per-anchor `collect_neighbors` Vec was a dealloc hot spot on q19
    // (LABS-337).
    let mut found = Vec::new();
    // The anchor's own admission is the same for every candidate of this
    // step, so it resolves once, outside the anchor loop.
    let anchor_binding = binding(anchor.node, env.row);
    for anchor_id in anchors {
        env.graph.cancel_tick()?;
        if !accept_slot(env, anchor.node, anchor.slot, anchor_binding, anchor_id)? {
            continue;
        }
        // The target side's admission: a repeated variable's second
        // occurrence binds the anchor (one shared slot), so it resolves to
        // the anchor's id and admits exactly the self-loop — the same
        // vocabulary a row-bound target resolves through.
        let target_binding = resolve_bound(
            orientation
                .bound_target()
                .or_else(|| self_loop.then_some(anchor_id)),
            target.node,
            env.row,
        );
        // LABS-448: the walk phase — the anchor's neighbor traversal, the
        // same span spelling the one-hop fold wraps its
        // `try_for_each_neighbor_resolved` in, so the path fold's `Rels` reads
        // carry walk time like every other walk engine.
        crate::ctx::phase_walk(|| {
            collect_neighbors(
                env,
                anchor_id,
                rel.pattern,
                rel.types,
                dir,
                rel.skip,
                &mut found,
            )
        })?;
        for &(rid, other_id) in &found {
            if accept_slot(env, target.node, target.slot, target_binding, other_id)? {
                // The orientation names the roles once: reversed, the
                // anchor is the hop's end.
                let (start, end) = orientation.roles(anchor_id, other_id);
                hops.push(HopEnd {
                    start,
                    end,
                    rel: rid,
                });
            }
        }
    }
    Ok(hops)
}

fn path_prop(
    graph: &ExecGraph<'_>,
    caches: &mut FoldCaches,
    id: NodeId,
    key: &NamedToken,
) -> Value {
    let props = caches.props.entry(id).or_default();
    if let Some(found) = key.0.with_str(|text| props.get(text)) {
        return found.clone();
    }
    let value = graph.node_prop_resolved(id, key).unwrap_or(Value::Null);
    props.insert(key.0.to_string(), value.clone());
    value
}

fn path_key(
    graph: &ExecGraph<'_>,
    plan: &PathPlan,
    caches: &mut FoldCaches,
    ids: &[NodeId],
) -> CachedKey {
    let vals: Vec<Value> = plan
        .keys
        .iter()
        .map(|(slot, prop)| path_prop(graph, caches, ids[*slot], prop))
        .collect();
    CachedKey::from_vals(vals)
}

/// The runtime half of the one count-seat vocabulary (ADR-0485): the
/// seat pairs its operand shape with its distinct flag at compile time,
/// and the plan's seat list IS the aggregate template, so no site
/// re-walks the projection vocabulary. The seat constructs its
/// [`StreamAgg`] internally ([`CountSeat::fresh_agg`]), so the prototype
/// and the value source cannot be paired illegally — `Star` carries no
/// distinct flag, and the whole-node count feeds the one shared value
/// channel like every other operand.
impl CountSeat {
    /// The seat's prototype: the one construction site, so the
    /// accumulator and the value source can never drift apart. The
    /// distinct-to-prototype policy itself is stated once, in
    /// [`StreamAgg::count_of`].
    fn fresh_agg(&self) -> StreamAgg {
        match self {
            Self::Star => StreamAgg::count_star(),
            Self::Prop { distinct, .. } | Self::Whole { distinct, .. } => {
                StreamAgg::count_of(*distinct)
            }
        }
    }

    /// Offer one binding: the seat extracts its value from the slot ids
    /// and feeds the shared value channel — the whole-node seat offers
    /// the bound endpoint's id (non-null by construction; a deleted
    /// endpoint is unreachable in this fold since a deleted node has no
    /// live rels, so fold and general path agree across a DETACH
    /// DELETE).
    fn offer(
        &self,
        graph: &ExecGraph<'_>,
        caches: &mut FoldCaches,
        agg: &mut StreamAgg,
        ids: &[NodeId],
        retained: &mut crate::overlay::RetainedTotal,
    ) -> Result<()> {
        match self {
            Self::Star => agg.offer_value(None, graph, retained),
            Self::Prop { slot, prop, .. } => agg.offer_value(
                Some(&path_prop(graph, caches, ids[*slot], prop)),
                graph,
                retained,
            ),
            Self::Whole { slot, .. } => {
                agg.offer_value(Some(&Value::Int(ids[*slot])), graph, retained)
            }
        }
    }
}

/// The template's prototype row: the one seat→(seat, StreamAgg) pair
/// construction, shared by `absorb`'s group insert and the empty-tail
/// seed group so both build the accumulators the same way.
fn fresh_group_aggs(template: &[CountSeat]) -> Vec<(CountSeat, StreamAgg)> {
    template
        .iter()
        .map(|seat| (seat.clone(), seat.fresh_agg()))
        .collect()
}

/// Offer one binding to a group's pairs: each seat feeds the
/// [`StreamAgg`] it is structurally paired with.
fn offer_aggs(
    graph: &ExecGraph<'_>,
    caches: &mut FoldCaches,
    pairs: &mut [(CountSeat, StreamAgg)],
    ids: &[NodeId],
    retained: &mut crate::overlay::RetainedTotal,
) -> Result<()> {
    for (seat, agg) in pairs {
        seat.offer(graph, caches, agg, ids, retained)?;
    }
    Ok(())
}

/// The allocate-fill-absorb glue every path shape runs: allocate the
/// slot ids, read the shape's bound slots, then absorb the binding into
/// the groups — one match over the key plan yields the CachedKey, and
/// the group insert-or-find, offer, and note tail runs exactly once.
fn absorb(env: &mut FoldEnv<'_>, bindings: &[Option<NodeId>]) -> Result<()> {
    // Every slot is bound by construction: each is a hop-step endpoint and
    // this terminal arm runs only after all steps bound theirs.
    let ids: Vec<NodeId> = bindings
        .iter()
        .map(|id| id.expect("every slot is bound when the walk completes"))
        .collect();
    let cached = match env.plan.key_plan {
        KeyPlan::None => CachedKey::from_vals(Vec::new()),
        KeyPlan::One(slot) => {
            let id = ids[slot];
            match env.caches.keys.get(&id) {
                Some(cached) => cached.clone(),
                None => {
                    // The payload charge rides the group landing's
                    // `add_group_entry` below: its entry delta counts the
                    // key's payload once, count-blind to the `Arc` shares
                    // the key cache and the props cache hold.
                    let cached = path_key(env.graph, env.plan, env.caches, &ids);
                    env.caches.keys.insert(id, cached.clone());
                    cached
                }
            }
        }
        KeyPlan::Mixed => path_key(env.graph, env.plan, env.caches, &ids),
    };
    let seats = &env.plan.seats;
    // ADR-0463 (LABS-781): the group-table landing charge. An occupied
    // landing retains nothing new (this row's key is dropped by the
    // lookup and its cached vals with it); a new group's landing pays
    // the one `add_group_entry` charge ahead of the insert.
    let map_slots = env.groups.len();
    let entry = match env.groups.entry(cached.key) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => {
            env.group_retained.add_group_entry(
                env.graph,
                map_slots,
                entry.key(),
                cached.vals.len(),
            )?;
            // The per-group seats vector the landing builds (its
            // aggregate state is inline counters, so the slot buffer is
            // the whole figure).
            env.group_retained.add_bytes(
                env.graph,
                crate::workspace::buffer_bytes(
                    seats.len(),
                    size_of::<(CountSeat, StreamAgg)>() as u64,
                ),
            )?;
            entry.insert((cached.vals, fresh_group_aggs(seats)))
        }
    };
    offer_aggs(
        env.graph,
        env.caches,
        &mut entry.1,
        &ids,
        env.group_retained,
    )?;
    note_path_binding();
    Ok(())
}

/// Project one group into its output row: the path fold's aggregate
/// accessor over the shared [`project_ranked_row`] projection — each
/// count seat's [`StreamAgg`] finishes at its rank's position.
fn project_path_row(
    items: &[Projection],
    ranks: &[Rank],
    key_vals: &[Value],
    aggs: &[(CountSeat, StreamAgg)],
) -> Row {
    project_ranked_row(items, ranks, key_vals, |agg| aggs[agg].1.finish())
}

/// Walk the compiled hop steps, threading the slot→id binding map: each
/// step anchors on the slots earlier steps bound (`expand_hop`'s bound
/// ends), every accepted hop extends the map, the previous hop's rel is
/// `skip_rel` (the DIFFERENT RELATIONSHIPS rule), and the last step's full
/// binding is absorbed. The groups ride the env, so the
/// recursion forwards only the walk state.
fn fold_steps(
    env: &mut FoldEnv<'_>,
    patterns: &[PathPattern],
    step_idx: usize,
    skip_rel: Option<RelId>,
    bindings: &mut [Option<NodeId>],
) -> Result<()> {
    let Some(step) = env.plan.steps.get(step_idx) else {
        return absorb(env, bindings);
    };
    let hops = expand_hop(
        env,
        &HopWalk {
            path: &patterns[step.pattern],
            rel_idx: step.rel,
            types: &env.step_types[step_idx],
            start_slot: step.start_slot,
            end_slot: step.end_slot,
            bound_start: bindings[step.start_slot],
            bound_end: bindings[step.end_slot],
            skip_rel,
        },
    )?;
    // Each accepted hop sets the two slots and restores them after the
    // deeper fold: a deeper step's binding must not leak back as an
    // anchor when the walk backtracks to this step's next hop. The
    // save/restore replaces a per-hop `bindings.to_vec()` — the profiled
    // dealloc hot spot on q19 (LABS-337): one Vec allocation per hop for
    // the same isolation the two saved values provide.
    for hop in hops {
        let saved = (bindings[step.start_slot], bindings[step.end_slot]);
        bindings[step.start_slot] = Some(hop.start);
        bindings[step.end_slot] = Some(hop.end);
        // Restore before propagating, unconditionally: the slot
        // isolation is local to the loop, never left to a caller to
        // honor on the error path.
        let deeper = fold_steps(env, patterns, step_idx + 1, Some(hop.rel), bindings);
        bindings[step.start_slot] = saved.0;
        bindings[step.end_slot] = saved.1;
        deeper?;
    }
    Ok(())
}

/// Stream one fixed hop, a fixed two-hop path, or two comma-joined one-hop
/// patterns into group counts or a distinct set. Wider patterns, variable-length
/// walks, repeated relationship variables, and `sum` stay on match-then-aggregate.
pub(crate) fn try_fold_path_aggs(
    graph: &ExecGraph<'_>,
    rows: &[Row],
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    suffix: &[Clause],
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Option<FoldedReturn>> {
    let Some(output) = fold_return(suffix, params, cypher_version) else {
        return Ok(None);
    };
    let Some(plan) = compile_path(patterns, where_clause, output.items) else {
        return Ok(None);
    };
    let mut groups: Groups = IndexMap::new();
    let mut caches = FoldCaches {
        props: rustc_hash::FxHashMap::default(),
        keys: rustc_hash::FxHashMap::default(),
    };
    // LABS-805: the fold's whole hop/walk count tallies across every
    // row's env and flushes on drop, at the run's end.
    let tallies = WalkTallies::default();
    // ADR-0463 (LABS-781): the group table is one retaining container;
    // its guard starts empty and grows with every landing below.
    let mut group_retained = graph.retain_bytes(0)?;
    // LABS-801: the steps' rel-type filters resolve once beside the plan
    // — every row's env reads them, never a per-row or per-anchor
    // re-resolution.
    let step_types: Vec<RelTypes> = plan
        .steps
        .iter()
        .map(|step| graph.rel_types_of(&patterns[step.pattern].rels[step.rel]))
        .collect();
    for row in rows {
        // The env is per-row: the row-scoped `preds` map starts empty by
        // construction, while the graph-scoped `caches` maps stay warm
        // across rows.
        let mut env = FoldEnv {
            graph,
            row,
            params,
            cypher_version,
            plan: &plan,
            caches: &mut caches,
            groups: &mut groups,
            group_retained: &mut group_retained,
            preds: rustc_hash::FxHashMap::default(),
            tallies: &tallies,
            step_types: &step_types,
        };
        let mut bindings = vec![None; plan.nslots];
        fold_steps(&mut env, patterns, 0, None, &mut bindings)?;
    }
    // No accepted binding: `groups` stayed empty — `absorb`
    // inserts-or-finds a group entry on every call, so the table carries
    // the fact. `key_plan` carries the seed-group fact: `None` means
    // every item is an aggregate, so the seed group answers them; any key
    // plan means a key column exists and the empty tail below yields the
    // empty row set — one exit, no parallel early return.
    if groups.is_empty() && matches!(plan.key_plan, KeyPlan::None) {
        // ADR-0463 (LABS-781): the keyless seed lands through the same
        // `add_group_entry` charge every row-loop landing pays (an empty
        // key carries no payload).
        group_retained.add_group_entry(graph, 0, &[], 0)?;
        groups.insert(Vec::new(), (Vec::new(), fresh_group_aggs(&plan.seats)));
    }
    let out = groups
        .into_values()
        .map(|(key_vals, aggs)| project_path_row(output.items, &plan.ranks, &key_vals, &aggs))
        .collect();
    Ok(Some(finish_folded_rows(graph, &output, out)?))
}
