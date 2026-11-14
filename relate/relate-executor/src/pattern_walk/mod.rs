//! The pattern-walk vocabulary: the binding/hop-bound/stub helpers, the
//! path-walk carriers, and the bind-name/ladder family both walk
//! engines — the exec walkers and the eval walk engine
//! (`crate::eval_walk`) — read without a module cycle. Moved out of
//! `ctx` (LABS-668), which had accumulated the shared domain language
//! of a pattern walk beside its actual execution-context objects. A
//! directory module: the binding vocabulary, hop bounds, and
//! walk-state carriers live here in `mod.rs`; the admission family in
//! `admit.rs` — the cross-engine pattern policy
//! ([`map_from_expr`]/[`pattern_props`]/[`pattern_node_ok`]/[`rel_props_ok`]/[`rel_edge_ok`]/
//! [`bound_node`], relocated from `eval/pattern` in LABS-811's stellar
//! round, finishing the LABS-668 move, so the walk engines import
//! their admission policy downward from this tier instead of up from
//! an engine), the per-hop admission ([`admit_hop`], with the static
//! type filter [`static_rel_type_ok`] moved here from the candidates
//! funnel so the shared tier owns it), and the two shared walks — the
//! one exact-trail walk ([`walk_exact_trail`]) both engines' pre-bound
//! rel-list dispatches read, and the one var-length walk
//! ([`walk_var_length`]) both engines' free enumerations read — each
//! with the hop-bounds gate, the per-hop admission, and the commit
//! block in one spelling, the engines' own deltas riding in as
//! explicit arguments; the counted node-candidate funnel and its seek
//! family in `candidates/` — [`node_candidates`] with its LABS-448 seek
//! span and LABS-375 node-read stamping, the one funnel every
//! node-match path of both engines reads (the eval walk engine's
//! `pattern_node_candidates` delegation is the read that made its
//! engine-home untenable; the residual's scope stays exec's, the funnel
//! takes the bare conjunct slice); the rung-bearing binds and the path-value
//! reconstruction in `binds.rs` — the trail commit step
//! ([`commit_trail`], reconcile → bind → consume) both walks' offers
//! read, and the terminal path binders' shared orchestration skeleton
//! ([`bind_reconstructed_path`]) among them; the ladder in `ladder.rs`;
//! the anchored existence probe (LABS-193/LABS-249) in `probe.rs` — the
//! eval walk engine's `pattern_exists` and the exec exists probe both
//! read its one entry ([`resolved_endpoints_exist`]) downward, the
//! cross-engine home its shared vocabulary asked for.
//! The row schema the family stamps with lives one tier down at the
//! crate root (`crate::row`) — it is not walk vocabulary, so this
//! module imports it downward like every other consumer.
//!
//! Tier claims, now literal: `eval` is the expression core strictly
//! below this module — it imports no walk module, and this module
//! imports the core downward (`crate::eval`'s evaluator and label
//! matcher, read by the admission family) the same way it imports the
//! row schema. The eval-side walker left the eval tree for the crate
//! shared tier (`crate::eval_walk`, a pattern-walk client like the
//! exec walkers), closing the direct `eval` ↔ `pattern_walk`
//! two-cycle its old home (`eval/pattern.rs`) kept open; this
//! module's own edges all point down, and no engine imports another
//! engine through it. The one remaining cycle is stated precisely in
//! `eval_walk`'s module doc: the expression dispatcher's
//! pattern-expression arms delegate up to that engine, the same shape
//! as its subquery arms delegating to `exec`.
//!
//! The real set, grepped:
//! the binding vocabulary
//! ([`Binding`]/[`classify`]/[`binding`]/[`rel_binding`]/[`rel_list_binding`]
//! with the var-length list spelling [`RelListBinding`]/[`bound_id`]),
//! the stub binders ([`bind_node_stub`]/[`bind_rel_stub`]), the hop-bound
//! pair
//! ([`hop_bounds`]/[`hop_bounds_with`]), its containment predicate
//! ([`within_hop_bounds`]), and its unbounded ceiling
//! ([`DEFAULT_MAX_HOPS`]), the walk-state carriers
//! ([`UsedRels`]/[`PathRow`]/[`VarWalk`]), and — consolidated here from
//! the eval walker and `exec/pattern_policies.rs` — the bind-name
//! spellings ([`binds::node_bind_name`]/[`binds::rel_bind_name`] with the
//! anonymous stubs split out), the rung bind ([`bind_rung_name`]), the
//! trail/node bind policies ([`bind_trail`]/[`bind_pattern_node`]), the
//! path-name trio
//! ([`bind_path_names`]/[`binds::path_name_rung`]/[`memoized_path_rung`]),
//! the path-value reconstruction's chunk walk
//! ([`binds::walk_rel_chunks`]), the counted node-candidate funnel
//! ([`node_candidates`] and its [`label_index_seekable`] criterion,
//! [`leaf_seek`] recognizers, and the [`ServedIds`] exactness
//! compositions every served fold reads), and
//! the one binding-schema ladder ([`WalkLadder`], parameterized on the
//! exec walker's start [`Selection`]) — total over every pattern shape:
//! the anonymous `__trail{idx}` seat binds its empty list at a zero-hop
//! stop exactly as the named arm does, so no shape keeps an insert path
//! and no `Option<..>` rung threading survives (the former
//! `ladder_precomputable` gate, deleted). The family had lived split
//! across the two walkers' subtrees — the eval walker reached up into
//! `exec/pattern_policies` for the path-name spelling; that upward edge
//! is gone, both walkers import the family downward from here. The
//! anchored existence probe ([`resolved_endpoints_exist`], `probe.rs`)
//! closes the family's last engine-home question the same way: two
//! readers, two engines (the eval walk's `pattern_exists`, the exec
//! exists probe), one home at the tier whose vocabulary it already
//! speaks.
//!
//! Home judgment — this module vs the PROFILE phase clocks, which also
//! moved into `ctx` recently: **separate homes, deliberately.** The clocks
//! are statement-level instrumentation, armed and disarmed by the clause
//! metric tick and snapshotted into `PhaseTimes` beside the rest of the
//! metrics machinery they feed — they belong with the metrics types, not
//! with the walk vocabulary, which every pattern walk reads, PROFILEd or
//! not. Both homes sit at the crate's lowest shared tier for the same
//! reason (both walk engines import them downward, LABS-448's cycle
//! avoidance), but that is a placement constraint the two concerns share,
//! not subject matter they share.

use relate_core::{NodeId, RelId, Value};
use relate_ir::{NodePattern, RelPattern};
use relate_names::SmallName;

use crate::row::Row;
use crate::UsedSet;

mod admit;
mod binds;
mod candidates;
mod ladder;
mod probe;

pub(crate) use admit::{
    admit_hop, bound_node, pattern_match_props, pattern_node_ok, pattern_props, rel_edge_ok,
    rel_props_ok, static_rel_type_ok, walk_exact_trail, walk_var_length, PatternMapRole, RelFilter,
    RelHop,
};
pub(crate) use binds::{
    bind_node_stub, bind_path_names, bind_pattern_node, bind_reconstructed_path, bind_rel_stub,
    bind_rel_stub_list, bind_rung_name, bind_trail, commit_trail, memoized_path_rung,
    node_bind_name,
};
pub(crate) use candidates::{
    id_fn_conversion, label_index_seekable, leaf_seek, node_candidates, node_candidates_capped,
    point_seek_served, scan_answers_exactly, Capped, IdConversion, Seek, ServedIds,
};
pub(crate) use ladder::{Selection, WalkLadder};
pub(crate) use probe::resolved_endpoints_exist;

/// The unbounded var-length hop cap: the default `max_hops` when a
/// quantified rel gives none. One named constant for every walk engine
/// (exec/expand, eval_walk/pattern, eval_walk/shortest) — a cap change
/// must not desynchronize them.
pub(crate) const DEFAULT_MAX_HOPS: u32 = 32;

/// What a pattern variable is bound to in a row: the one canonical
/// vocabulary of the four states every binding-aware site distinguishes —
/// candidacy (exec/candidates, eval_walk/pattern), the fold's bound start, and
/// the existence probe all classify through this. Parameterized by the id
/// type ([`NodeBinding`] for nodes, [`RelBinding`] for relationships) and
/// by the pattern's borrow: `Unbound` carries the absent variable's name
/// so a binding site reads it from the classification instead of
/// re-deriving it from the pattern. Copy so the classification can be
/// read from `&self` at every call site (the borrow is still `np`'s).
#[derive(Clone, Copy)]
pub(crate) enum Binding<'a, Id> {
    /// A row-bound element: the single accepted id.
    Bound(Id),
    /// Bound to a null or wrong-typed value: matches nothing.
    Mismatch,
    /// A variable absent from this row: the variable's name (the IR's
    /// own representation, borrowed from the pattern). `Anonymous` is the
    /// nameless spelling, so the payload carries the named/nameless
    /// distinction exactly once.
    Unbound(&'a SmallName),
    /// A nameless pattern element.
    Anonymous,
}

/// The node-side and relationship-side spellings of the one vocabulary.
pub(crate) type NodeBinding<'a> = Binding<'a, NodeId>;
pub(crate) type RelBinding<'a> = Binding<'a, RelId>;

/// The var-length spelling of the one vocabulary: a var-length rel
/// variable binds a trail, so the [`Bound`](Binding::Bound) payload is
/// the trail's id sequence, not one id. Not `Copy` (the payload is a
/// `Vec`) — the classification is matched by reference in place, never
/// matched by value per candidate the way the `Copy` id spellings are.
pub(crate) type RelListBinding<'a> = Binding<'a, Vec<RelId>>;

impl<'a, Id> Binding<'a, Id> {
    /// Does a resolved occurrence satisfy this binding? A row-bound
    /// element accepts only its own payload; a null/wrong-typed binding
    /// accepts nothing; an absent or nameless variable accepts every
    /// candidate. Takes `&self` so a walk can hoist one classification
    /// and gate every candidate through it, and the candidate by
    /// reference so every spelling of the vocabulary reads the one
    /// predicate — the id spellings pass `&id`, the list spelling
    /// (`Binding<Vec<RelId>>`) passes the walked `&[RelId]` trail (std
    /// carries `Vec<T> == [U]`), so no spelling hand-rolls its own
    /// accept table.
    pub(crate) fn accepts<U: ?Sized>(&self, candidate: &U) -> bool
    where
        Id: PartialEq<U>,
    {
        match self {
            Binding::Bound(bound) => *bound == *candidate,
            Binding::Mismatch => false,
            Binding::Unbound(_) | Binding::Anonymous => true,
        }
    }
}

impl<'a, Id: Copy> Binding<'a, Id> {
    /// The id this classification fixes, if any: `Bound`'s payload,
    /// `None` for every other state. A caller that must treat a
    /// [`Mismatch`](Binding::Mismatch) as dead rather than free matches
    /// the variant first — this projection deliberately collapses it
    /// (the same collapse [`bound_node`] applies to a node pattern).
    pub(crate) fn bound_id(&self) -> Option<Id> {
        match self {
            Binding::Bound(id) => Some(*id),
            Binding::Mismatch | Binding::Unbound(_) | Binding::Anonymous => None,
        }
    }
}

/// The shared classifier: `variable`'s row binding, with `extract` pulling
/// the id out of a value of the pattern element's type.
pub(crate) fn classify<'a, Id>(
    variable: Option<&'a SmallName>,
    row: &Row,
    extract: impl Fn(&Value) -> Option<Id>,
) -> Binding<'a, Id> {
    match variable {
        // The String-keyed row schema is probed through the free text
        // bridge; the carried name stays the IR's representation.
        Some(variable) => match row.get_name(variable) {
            Some(value) => match extract(value) {
                Some(id) => Binding::Bound(id),
                None => Binding::Mismatch,
            },
            None => Binding::Unbound(variable),
        },
        None => Binding::Anonymous,
    }
}

pub(crate) fn node_id(value: &Value) -> Option<NodeId> {
    match value {
        Value::Node(node) => Some(node.id),
        _ => None,
    }
}

fn rel_id(value: &Value) -> Option<RelId> {
    match value {
        Value::Relationship(rel) => Some(rel.id),
        _ => None,
    }
}

/// Classifies `np`'s variable binding in `row`. The returned borrow is
/// `np`'s (the carried variable name is the pattern's own).
pub(crate) fn binding<'a>(np: &'a NodePattern, row: &Row) -> NodeBinding<'a> {
    classify(np.variable.as_ref(), row, node_id)
}

/// Classifies `rp`'s variable binding in `row` (the relationship-side
/// spelling of [`binding`]).
pub(crate) fn rel_binding<'a>(rp: &'a RelPattern, row: &Row) -> RelBinding<'a> {
    classify(rp.variable.as_ref(), row, rel_id)
}

/// The trail a var-length binding carries: the id sequence of a list
/// whose every element is a relationship. `None` for any other shape —
/// null, a non-list, or a list holding a non-relationship element —
/// which classifies as [`Binding::Mismatch`] under the same rule as
/// every other spelling of the vocabulary (a null or wrong-typed
/// binding matches nothing; the classic walker's Neo4j-verified
/// single-hop behavior, LABS-711, extended to the trail payload).
fn rel_list_ids(value: &Value) -> Option<Vec<RelId>> {
    match value {
        Value::List(xs) => xs.iter().map(rel_id).collect(),
        _ => None,
    }
}

/// Classifies `rp`'s variable binding in `row` for a var-length trail
/// (the list spelling of [`rel_binding`]): `Bound` carries the row-bound
/// trail's ids, so the one reconciliation site ([`binds::bind_trail`])
/// and the exec dispatch (`exec::expand::expand_rel`) read the same
/// classification — the LABS-661 exact-trail rule — instead of each
/// hand-spelling a `Value::List` shape-sniff over the same value.
pub(crate) fn rel_list_binding<'a>(rp: &'a RelPattern, row: &Row) -> RelListBinding<'a> {
    classify(rp.variable.as_ref(), row, rel_list_ids)
}

/// The effective hop bounds of a quantified rel — the one bound-pair
/// spelling every walk engine reads, parameterized over the unbounded
/// ceiling: the `var_length` clamp with the caller's default when the
/// pattern wrote none. The ceiling closure is consulted only in the
/// unbounded branch, so a bounded pattern never pays what its ceiling
/// would compute. The bound pair is shared; the unbounded ceiling is
/// per-family by design: the classic engines (walk_pattern,
/// eval_shortest, expand_rel) read [`hop_bounds`], the flat
/// [`DEFAULT_MAX_HOPS`] ceiling (ADR-0466's admission limit), while the
/// GPM walk reads this form with its own ceiling — the walk-memoized
/// `unbounded_dfs_cap` (exec/gpm/mod.rs), the live rel-table bound that
/// makes an unbounded GPM walk sound under DIFFERENT RELATIONSHIPS on a
/// cyclic graph (ADR-0471 §3). Within the GPM engine the ceiling is one
/// rule: both unbounded spellings — the quantified expansion and the
/// legacy var-length rel DFS — read the same walk-memoized cap, so they
/// explore to the same depth.
pub(crate) fn hop_bounds_with(
    rp: &relate_ir::RelPattern,
    unbounded: impl FnOnce() -> u32,
) -> (u32, u32) {
    if rp.var_length {
        (rp.min_hops, rp.max_hops.unwrap_or_else(unbounded))
    } else {
        (1, 1)
    }
}

/// The classic walk engines' hop bounds — [`hop_bounds_with`] with the
/// flat [`DEFAULT_MAX_HOPS`] unbounded ceiling.
pub(crate) fn hop_bounds(rp: &relate_ir::RelPattern) -> (u32, u32) {
    hop_bounds_with(rp, || DEFAULT_MAX_HOPS)
}

/// Does a walk of `hops` hops sit inside the `(min, max)` pair
/// [`hop_bounds`] produced — the containment predicate every offer gate
/// reads, spelled once so the offered-trail and rejected-trail forms
/// cannot drift apart.
pub(crate) fn within_hop_bounds(min_h: u32, max_h: u32, hops: u32) -> bool {
    hops >= min_h && hops <= max_h
}

/// The relationship-uniqueness carrier of one path walk, in its arena
/// form: an ordered, duplicate-free id vector the walk pushes and probes
/// in place (a linear `contains` — the walk's own `trail` precedent), so
/// a walk branch pays one flat allocation instead of constructing a
/// SipHash table per hop. The set form is reconstructed only at the sink
/// boundary, where a stored binding carries the type the multi-pattern
/// carriers ([`super::exec::pattern_policies::Matched`]) share — the
/// crate-root [`crate::UsedSet`] alias (LABS-1116), FxHash for
/// engine-generated ids (the `CoreMap` rule, ADR-0355).
#[derive(Clone, Default)]
pub(crate) struct UsedRels {
    ids: Vec<RelId>,
}

impl UsedRels {
    /// The empty carrier every walk starts from.
    pub(crate) fn new() -> Self {
        Self { ids: Vec::new() }
    }

    /// The walk-entry conversion from the set form a caller's pattern
    /// chain carries.
    pub(crate) fn from_set(set: &UsedSet) -> Self {
        Self {
            ids: set.iter().copied().collect(),
        }
    }

    /// The sink-boundary materialization into the set form a stored
    /// binding or by-value consumer carries — the one arena→set
    /// conversion, sized exactly to this row's rels (LABS-822; the
    /// fixed hasher since LABS-1116).
    pub(crate) fn to_set(&self) -> UsedSet {
        self.ids.iter().copied().collect()
    }

    /// Whether `rid` is already consumed on this walk.
    pub(crate) fn contains(&self, rid: RelId) -> bool {
        self.ids.contains(&rid)
    }

    /// Consume `rid`. A duplicate push is a no-op — the set semantics the
    /// carrier had before; the walk's own checks reject a reused rel
    /// before this fires.
    pub(crate) fn insert(&mut self, rid: RelId) {
        if !self.contains(rid) {
            self.ids.push(rid);
        }
    }

    /// How many rels the walk consumed — the accounting input the set
    /// form's `len` answered.
    pub(crate) fn len(&self) -> usize {
        self.ids.len()
    }
}

/// Row plus the relationship IDs consumed so far along a path walk.
#[derive(Clone)]
pub(crate) struct PathRow {
    pub row: Row,
    pub used: UsedRels,
}

/// Variable-length relationship walk bounds plus the current hop cursor.
pub(crate) struct VarWalk {
    pub min_h: u32,
    pub max_h: u32,
    pub hops: u32,
    pub trail: Vec<RelId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_set_carries_the_arenas_rels() {
        // LABS-822: the one arena→set conversion. Set equality is
        // content, not seed, so the fixed hasher changes nothing the
        // consumer can observe.
        let mut used = UsedRels::new();
        for id in [1, 2, 3] {
            used.insert(id);
        }
        assert_eq!(used.to_set(), UsedSet::from_iter([1, 2, 3]));
        assert_eq!(UsedRels::new().to_set(), UsedSet::default());
        assert!(UsedSet::default().is_empty());
    }
}
