//! Graph pattern matching engine: quantified path patterns, inline
//! predicates, path modes, and selectors.

use crate::UsedSet;
use std::cell::{Cell, OnceCell};
use std::collections::{BTreeMap, HashSet};

use relate_core::{NodeId, Path, RelId, Result, Value};
use relate_ir::{Expr, MatchMode, NodePattern, PathPattern, PatternElement, RelPattern};

use crate::ctx::ExecCtx;
use crate::eval::{eval, predicate_holds, AllReduceFold, AllReduceStep};
use crate::exec::WalkTallies;
use crate::overlay::ExecGraph;

use super::candidates::{expect_enumerated_candidate, node_ok};
use super::pushdown::Residual;
use crate::pattern_walk::{
    bind_node_stub, bind_path_names, bind_rel_stub, bind_rel_stub_list, memoized_path_rung,
    node_candidates, rel_edge_ok, RelFilter, RelHop,
};
use crate::row::{Row, RowRung};

mod early;
mod emit;
mod qpp;
mod selector;
mod shortest_route;
mod sink;
mod walk;

use crate::exec::rider::Rider;
use crate::shortest::reject_static_common_end_nodes;
use early::{early_predicates, EarlyPredicates};
use emit::GpmEmit;
use qpp::gpm_expand_quantified;
pub(crate) use shortest_route::fold_shortest_route;
use shortest_route::match_shortest_route;
use sink::MatchedSink;
use walk::gpm_walk_elements;

// The family-neutral pattern policies the GPM engine and the frontier
// family share — their exec-level home (`exec/pattern_policies.rs`),
// imported here for this module and its children (`use super::*`
// re-sees them). The path-name binding family (`bind_path_names`, the
// memoized rung) is imported above from `crate::pattern_walk`, the one
// tier both walk engines reach. The GPM-only policies (the node binding
// `gpm_bind_node`, the pinned-or-fresh classification `prebound`, the
// unbounded DFS cap `unbounded_dfs_cap`) are defined below in this
// module — the parent every gpm consumer, including the
// shortest-route child, already reaches. The candidate-filter policy
// lives in its own home, `eval/predicates.rs` (the row-predicate
// family's `crate::eval` home), imported above.
use super::pattern_policies::{
    gpm_group_vocabulary, gpm_path_mode_allows, inlined_all_reduces,
    path_mode_requires_unique_rels, InlinedAllReduce, MatchTail, Matched,
};

/// One partial (or complete) binding of the GPM walk. The walked path
/// rides as ids — `nodes` the visible node slice entries (an invisible,
/// overlay-deleted endpoint contributes none — the family's established
/// spelling, see `gpm_rel_dfs`), `rels` every walked rel in order — never
/// as full records: an interior hop of a var-length chain is never bound
/// or returned, so its node and rel are materialized only where the path
/// is observed — the path-name bind and the quantified subpath value,
/// through [`materialize_path`] (LABS-853: the per-hop `graph.node` /
/// `graph.rel` clones were the seam's dominant cost). Row bindings ride
/// the stub machinery (`bind_node_stub`, `bind_rel_stub_list`).
#[derive(Clone)]
pub(crate) struct GpmState {
    pub row: Row,
    pub used: UsedSet,
    pub current: NodeId,
    pub nodes: Vec<NodeId>,
    pub rels: Vec<RelId>,
}

/// One hop's on-state mutation (`push_hop`) and its exact undo (`pop`)
/// — the descent's mutate/restore pairing as a value, so the three
/// mutations and their three restores cannot drift apart (the
/// used-set's conditional insert, the slice entry's visibility gate —
/// [`push_visible_node`] — and the rel push).
pub(crate) struct HopFrame {
    rid: RelId,
    consumed: bool,
    visible: bool,
    current: NodeId,
}

impl GpmState {
    /// Put one hop on the state: the rel id, its used-set consumption
    /// (only a not-yet-held rel counts as consumed), the far endpoint's
    /// slice entry ([`push_visible_node`]'s policy), and the `current`
    /// swap — all four of the hop's mutations. The returned
    /// [`HopFrame`] undoes exactly this hop.
    pub(crate) fn push_hop(
        &mut self,
        graph: &ExecGraph<'_>,
        rid: RelId,
        next_id: NodeId,
    ) -> HopFrame {
        self.rels.push(rid);
        let current = self.current;
        self.current = next_id;
        HopFrame {
            rid,
            consumed: self.used.insert(rid),
            visible: push_visible_node(graph, next_id, &mut self.nodes),
            current,
        }
    }
}

impl HopFrame {
    /// Take the hop back off — the exact mirror of [`GpmState::push_hop`].
    pub(crate) fn pop(self, state: &mut GpmState) {
        state.current = self.current;
        if self.visible {
            state.nodes.pop();
        }
        let popped = state
            .rels
            .pop()
            .expect("hop frame: the frame's own rel push");
        debug_assert_eq!(popped, self.rid, "hop frames pop LIFO");
        if self.consumed {
            state.used.remove(&self.rid);
        }
    }
}

/// Context shared by the GPM walker: graph view, parameters, the enclosing
/// statement's CIP-150 `cypher_version`, the walk's own state
/// ([`WalkState`] — the match mode, the two predicate bundles, the memos
/// and the tallies), and the halt cell a cap or probe rider sets to stop
/// the enumeration (`emit`). This view declares no walk state of its own:
/// the fields it lends the hops are [`WalkState`]'s, so the two cannot
/// drift. The path mode reaches the walk only through the state's
/// whole-walk invariant — the per-candidate mode check
/// (`gpm_path_mode_allows`) reads the pattern's own mode directly.
#[derive(Clone, Copy)]
struct GpmCtx<'a> {
    graph: &'a ExecGraph<'a>,
    params: &'a BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    /// The walk's own state, lent whole.
    walk: &'a WalkState<'a>,
    /// The mount's halt cell — the one decision this view adds to the
    /// walk's state. A mounted probe rider brings its own verdict cell
    /// (`MatchedSink::new` reads `probe.halt`) and it takes this place; a
    /// cap rider carries no cell, so the state's `cap_halt` stands here
    /// and the sink raises it. Always a real cell either way. Every
    /// enumeration loop consults [`GpmCtx::halted`].
    halt: &'a Cell<bool>,
}

/// A GPM pattern's resolved rel-type filters (LABS-801), built once per
/// pattern at the clause's pattern loop: one table entry per pattern
/// ELEMENT, in element order — the elements walk reads `filters[index]`
/// at its own position, so no cursor exists to mistrack. A quantified
/// path's compact `path.rels` projection is empty (the front's
/// `compact_projections` declines it), so the GPM engine's table keys on
/// elements, not `rels`. The quantified DFS re-enters the elements walk
/// per iteration over the group's own inner table — never a
/// re-resolution per row, frame, or iteration.
pub(crate) struct GpmFilters {
    pub(crate) filters: Vec<ElementFilter>,
}

/// One pattern element's resolved filter slot — see [`GpmFilters`].
pub(crate) enum ElementFilter {
    /// A Node element: nothing to filter.
    Node,
    /// A Relationship element's filter.
    Rel(RelFilter),
    /// A Quantified element: its own elements' table (quantified groups
    /// cannot nest — the parser's Rule 4 — so one level is exact).
    Group(Vec<ElementFilter>),
}

impl GpmFilters {
    pub(crate) fn resolve(graph: &ExecGraph<'_>, path: &PathPattern) -> Self {
        GpmFilters {
            filters: Self::resolve_elements(graph, &path.elements),
        }
    }

    /// One element list's table — the pattern's top level or a
    /// quantified group's body. The match is exhaustive over the element
    /// vocabulary: a nested Quantified is the parser's Rule-4 reject, so
    /// its arm cannot silently drop a filter.
    fn resolve_elements(graph: &ExecGraph<'_>, elements: &[PatternElement]) -> Vec<ElementFilter> {
        elements
            .iter()
            .map(|element| match element {
                PatternElement::Node(_) => ElementFilter::Node,
                PatternElement::Relationship(rp) => {
                    ElementFilter::Rel(RelFilter::resolve(graph, rp))
                }
                PatternElement::Quantified(qpp) => {
                    let inner = Self::resolve_elements(graph, &qpp.elements);
                    // Postcondition of the parser's Rule 4 (no nested
                    // quantification): the inner table holds no Group.
                    assert!(
                        inner.iter().all(|e| !matches!(e, ElementFilter::Group(_))),
                        "quantified path patterns cannot be nested (parser Rule 4)"
                    );
                    ElementFilter::Group(inner)
                }
            })
            .collect()
    }
}

impl GpmCtx<'_> {
    /// Whether a rider has answered and the enumeration should stop.
    fn halted(&self) -> bool {
        self.halt.get()
    }

    /// The unbounded DFS depth cap of this walk — [`unbounded_dfs_cap`]
    /// over the walk's own graph view and match mode, memoized on first
    /// use: the pair is fixed for the whole walk (the statement's
    /// snapshot and the clause's match mode — the same whole-walk
    /// premise [`WalkState::enforce_unique`] rests on), so an unbounded
    /// walk enumerates the rel table once per walk instead of once per
    /// expansion entry, and a bounded walk — whose `unwrap_or_else`
    /// never consults this — pays nothing.
    fn unbounded_dfs_cap(&self) -> u32 {
        *self
            .walk
            .unbounded_cap
            .get_or_init(|| unbounded_dfs_cap(self.graph, self.walk.match_mode))
    }
}

/// The walk's own state for one materializing GPM match (LABS-1404): the
/// match-mode facts, the two predicate bundles the walk reads, the mount's
/// halt cell, the two whole-walk memos and the tallies — one owned value,
/// and the one field list: [`GpmCtx`] lends these fields whole rather than
/// redeclaring them, so `match_gpm_path` reads route → start → walk →
/// deliver instead of six inline locals that exist only to be borrowed by
/// a `GpmCtx` literal.
/// The mounted emit and its output vector stay with the caller: the sink
/// owns `&mut` the vector, so one struct holding both would be
/// self-referential.
struct WalkState<'c> {
    match_mode: MatchMode,
    /// The whole-walk rel-uniqueness invariant, derived once at the walk
    /// entry — the mode pair is fixed for the walk, so every hop reads the
    /// same answer.
    enforce_unique: bool,
    /// The MATCH-WHERE allReduce conjuncts inlined into the quantified
    /// expansions (mid-path pruning, `qpp`): evaluated at every expansion
    /// step, so a path that already fails the WHERE stops early instead of
    /// completing and being filtered at the end.
    inlined: Vec<InlinedAllReduce<'c>>,
    /// The MATCH-WHERE conjuncts evaluated early at binding points: each
    /// conjunct rides the element whose bind completes its last free
    /// variable, so the walk prunes at the earliest point it can.
    early: EarlyPredicates<'c>,
    /// The mount's halt cell — the one a cap rider raises through the
    /// sink. A mounted probe rider brings its own verdict cell and it
    /// takes this place in the `GpmCtx` [`Self::gctx`] hands the walk.
    cap_halt: Cell<bool>,
    /// The walk's unbounded-cap memo (see the field): empty until the first
    /// unbounded expansion fills it — `OnceCell`, so the type owns the
    /// filled-exactly-once contract the method used to spell by hand.
    unbounded_cap: OnceCell<u32>,
    /// The walk's path-name rung memo (see the field): empty until the
    /// first complete state fills it.
    path_rung: Cell<Option<RowRung>>,
    /// The walk's hop/walk tallies (LABS-805): the per-edge counts
    /// accumulate here through the whole elements walk and flush on drop at
    /// the walk's end.
    tallies: WalkTallies,
}

impl<'c> WalkState<'c> {
    fn new(
        path: &PathPattern,
        row: &Row,
        match_mode: MatchMode,
        where_clause: &Residual<'c>,
    ) -> Self {
        Self {
            match_mode,
            enforce_unique: match_mode == MatchMode::DifferentRelationships
                || path.mode.is_some_and(path_mode_requires_unique_rels),
            inlined: inlined_all_reduces(path, row, where_clause.conjuncts),
            early: early_predicates(path, row, where_clause),
            cap_halt: Cell::new(false),
            unbounded_cap: OnceCell::new(),
            path_rung: Cell::new(None),
            tallies: WalkTallies::default(),
        }
    }

    /// The walk's context: the statement's graph view, parameters and
    /// `cypher_version`, this state lent whole, and `halt` — the mount's
    /// one decision, which is the mounted probe's own verdict cell when one
    /// rides and this state's `cap_halt` otherwise.
    fn gctx<'s>(&'s self, ctx: ExecCtx<'s>, halt: &'s Cell<bool>) -> GpmCtx<'s>
    where
        'c: 's,
    {
        GpmCtx {
            graph: ctx.graph,
            params: ctx.params,
            cypher_version: ctx.cypher_version,
            walk: self,
            halt,
        }
    }
}

/// The meaning of an existing binding when a pattern element rebinds its
/// name — the pinned-or-fresh policy every GPM rebinding site reads:
/// absent, or `null`, is [`Prebound::Fresh`] (the variable rebinds); any
/// other value is [`Prebound::Pinned`] (the site reconciles its new
/// binding against it). A `null` pre-binding is the walker's unbound,
/// not a pin to compare against: a failed OPTIONAL MATCH pre-binds its
/// variables to null, a CIP-28 non-local singleton is pre-bound to null
/// and rebound at its own site (ADR-0335) — `Value::eq_non_null` is
/// false for any null operand, so a site that compared its new binding
/// against a null pre-binding would silently reject the row instead of
/// rebinding (the node binding, the walk's rel-variable binding, and
/// the quantified group's exact-trail pins all read this one spelling;
/// the frontier start selection spells the same classification at
/// exec/kernels/frontier_run.rs — this home becomes the family's when a
/// second family reads the classification instead of spelling its own).
enum Prebound<'a> {
    /// Absent or null — the name is unbound as far as this rebinding is
    /// concerned; the site binds it.
    Fresh,
    /// A value the site must reconcile its new binding against (a node
    /// or rel by identity, a var-length trail as the same list).
    Pinned(&'a Value),
}

/// Classify an existing binding for a pattern rebinding — see
/// [`Prebound`].
fn prebound<'a>(existing: Option<&'a Value>) -> Prebound<'a> {
    match existing {
        None | Some(Value::Null) => Prebound::Fresh,
        Some(value) => Prebound::Pinned(value),
    }
}

/// Bind (or reconcile) the node pattern's variable against `id`: a fresh
/// name binds the node's stub (null when the stub is gone — a deleted
/// node's tombstone reads from the row binding, never the store); a
/// pinned node binding must name the same node; a pinned value of any
/// other kind refuses the binding. The one spelling every GPM node
/// binding site reads — the walk's node and target accepts
/// (`gpm/walk.rs`), the start-point selection (below), the quantified
/// iteration's boundary node (`gpm/qpp.rs`), and the shortest-route row
/// builder (`gpm/shortest_route/route.rs`).
fn gpm_bind_node(graph: &ExecGraph<'_>, node: &NodePattern, id: NodeId, row: &mut Row) -> bool {
    let Some(name) = &node.variable else {
        return true;
    };
    match prebound(row.get_name(name)) {
        Prebound::Fresh => {
            // Text render: the row schema's names are String.
            row.insert(name.to_string(), bind_node_stub(graph, id));
            true
        }
        Prebound::Pinned(Value::Node(existing)) => existing.id == id,
        Prebound::Pinned(_) => false,
    }
}

/// The rel variable's binding, the twin of [`gpm_bind_node`] — the
/// pinned-or-fresh policy over the walked segment: a fresh name (absent,
/// or null — a failed OPTIONAL MATCH pre-binds its variables to null)
/// binds here, exactly as `gpm_bind_node` rebinds a null-bound node and
/// the classic walker's `bind_trail` rebinds a null-bound rel; a pinned
/// value must name the same binding (a single rel by identity, a
/// var-length trail as the same list — the LABS-661 exact-trail pin).
/// The row carries stubs (`bind_rel_stub` / `bind_rel_stub_list`),
/// inflated where a value leaves the engine.
fn gpm_bind_rel(graph: &ExecGraph<'_>, row: &mut Row, rel: &RelPattern, segment: &[RelId]) -> bool {
    let Some(name) = &rel.variable else {
        return true;
    };
    let value = if rel.var_length {
        bind_rel_stub_list(graph, segment)
    } else {
        bind_rel_stub(
            graph,
            *segment
                .last()
                .expect("a non-var-length walk always parks one relationship"),
        )
    };
    match prebound(row.get_name(name)) {
        Prebound::Fresh => {
            row.insert(name.to_string(), value);
            true
        }
        Prebound::Pinned(pinned) => pinned.eq_non_null(&value),
    }
}

/// The unbounded (`*` / `{m,}`) DFS depth cap of one match mode — the one
/// rule every unbounded GPM traversal spelling takes (LABS-661 parity,
/// the stellar B8 unification: the quantified expansion, the legacy
/// var-length rel DFS, and the shortest route's predicate-filtered trail
/// walk consult this same helper, so every spelling of one unbounded
/// pattern explores to the same depth). Under the default
/// DIFFERENT RELATIONSHIPS mode every hop consumes a distinct rel, so the
/// rel count is a sound termination bound — the longest distinct-rel
/// trail. REPEATABLE ELEMENTS never terminates by uniqueness (rel reuse is
/// legal, so a cyclic graph walks forever without a cap), so it takes the
/// satiety cap: one hop past the DIFFERENT bound — the repeatable
/// enumeration is a depth-superset of the different one — capped at the
/// classic walk's documented unbounded ceiling
/// [`DEFAULT_MAX_HOPS`](crate::pattern_walk::DEFAULT_MAX_HOPS) (ADR-0466 records
/// that 32-hop cap; ADR-0471 §3 keeps it for REPEATABLE ELEMENTS).
///
/// The count is spelled as an enumeration because no count spelling
/// exists: the live rel-id set is *computed* (the base's snapshot set
/// minus the overlay's deletions plus its creations,
/// [`ExecGraph::all_rel_ids`]), never stored as a number. The walk
/// consults it through the per-walk memo ([`GpmCtx::unbounded_dfs_cap`])
/// — once per walk, only when an expansion is actually unbounded, never
/// per expansion entry or hop. The shortest route owns no walk context,
/// so its filtered trail walk memoizes the helper's value in its own
/// per-route cell (`Route::unbounded_cap`), filled on the first
/// unbounded filtered partition — the rel table is enumerated once per
/// route, never once per partition, and a bounded pattern never fills
/// it. One integer domain throughout: the
/// `usize` count crosses into `u32` once, saturating at the domain edge
/// — a rel count past `u32::MAX` is unreachable in any addressable
/// store, and the `min` clamp keeps the REPEATABLE arm's value exact
/// regardless.
fn unbounded_dfs_cap(graph: &ExecGraph<'_>, match_mode: MatchMode) -> u32 {
    let rels = u32::try_from(graph.all_rel_ids().len()).unwrap_or(u32::MAX);
    if match_mode == MatchMode::DifferentRelationships {
        rels.max(1)
    } else {
        rels.saturating_add(1)
            .min(crate::pattern_walk::DEFAULT_MAX_HOPS)
    }
}

// The reduce tail rides as one `MatchTail` (F-1-3, LABS-668), but the
// count stays one over clippy's default seven-argument threshold: the
// rider is a separate argument by design (a parallel vocabulary to the
// tail, never a member of it — `rider::Rider`), and nothing else here is
// one concept. The ledger's original remedy reached a deletable count
// only through its other shape — the rider riding *in* the tail — which
// the parallel-state rule declines.
#[allow(clippy::too_many_arguments)]
pub(crate) fn match_gpm_path(
    ctx: ExecCtx<'_>,
    path: &PathPattern,
    rel_filters: &GpmFilters,
    row: Row,
    used: UsedSet,
    match_mode: MatchMode,
    tail: MatchTail<'_>,
    where_clause: &Residual<'_>,
    mut rider: Option<&mut Rider<'_, '_>>,
) -> Result<Matched> {
    let graph = ctx.graph;
    let params = ctx.params;
    let cypher_version = ctx.cypher_version;
    // 51N23: a function-form shortestPath/allShortestPaths naming one node
    // variable at both ends rejects each input row before either engine
    // runs — the GPM enumeration reached here answers closed walks the
    // route rejects, and live rejects them on an empty graph too.
    reject_static_common_end_nodes(path)?;
    // The MATCH-level residual rides along for the shortest-path route,
    // which must choose its path *against* the WHERE predicate, and for
    // the start-point selection (index seeks on the start variable, see
    // below); the walk itself carries no residual (it checks inline
    // pattern predicates per element and the caller applies the WHERE
    // afterwards).
    // The route pushes its rows through the sink as it binds them: a probe
    // answers and halts in place, and a cap rides the sink's `finish_row`
    // (LABS-1301 — the caller's row loop reads `MatchCap::full` to stop, so
    // a routed `LIMIT` whose cap no path ever charges enumerates every
    // input row for a prefix).
    let route_halt = Cell::new(false);
    let mut routed = Matched::new();
    let sink = MatchedSink::new(
        rider.as_deref_mut(),
        &route_halt,
        where_clause,
        tail,
        &mut routed,
    );
    if match_shortest_route(sink, ctx, path, rel_filters, &row, &used, match_mode)? {
        return Ok(routed);
    }
    if path.elements.is_empty() {
        return Ok(Vec::new());
    }
    let elements = &path.elements;
    // The walk's own state — each field's meaning is documented on
    // `WalkState` itself.
    let walk_state = WalkState::new(path, &row, match_mode, where_clause);
    // LABS-661: the start-point selection consults the MATCH-level WHERE
    // residual, exactly as the classic walker's `match_node` does —
    // `node_candidates` turns an equality/CONTAINS conjunct on the start
    // variable into an index seek (NodeReads=1 instead of a full label
    // scan), and `residual_ok` drops starts whose already-bound conjuncts
    // fail. A conjunct naming a variable this walk has not bound yet is
    // skipped (not ready) and stays with the caller's post-filter. The
    // walk carries no residual of its own: inline element predicates are
    // checked per element and the caller applies the WHERE afterwards.
    // the anonymous stand-in for a path that does not open on a node
    // element: one match, one start-point call — the one spelling of the
    // shape, `NodePattern::anonymous()` (relate-ir, the type's home),
    // which the front's lowering and the NFA kernel's boundary nodes
    // build too
    let anonymous = NodePattern::anonymous();
    let (start, first) = match elements.first() {
        Some(PatternElement::Node(node)) => (1, node),
        _ => (0, &anonymous),
    };
    let mut states = gpm_start_nodes(
        graph,
        first,
        row,
        used,
        params,
        cypher_version,
        where_clause,
    )?;
    // The rider (`emit`): with no selector choosing across candidates, a
    // cap or an EXISTS probe rides the walk itself — every complete state
    // flows through the per-candidate pipeline as it emerges and the walk
    // halts the moment the rider is answered. A selector keeps the
    // materializing walk (its choice is cross-row); the probe then still
    // answers from the materialized rows below, without the early exit.
    // The rider mounts itself — one match over its vocabulary, the emit
    // and the halt cell returned together; no rider (or a selector
    // keeping the materializing walk) yields no emit and the local cell.
    let mut emitted: Matched = Vec::new();
    let (mut emit, halt) = emit::mount_rider(
        ctx,
        rider.as_deref_mut(),
        path,
        where_clause,
        &walk_state.cap_halt,
        tail,
        &mut emitted,
    );
    let gctx = walk_state.gctx(ctx, halt);
    // LABS-448: the walk phase, through the one wrapper — the whole
    // elements walk this clause performs, inclusive of its nested
    // resolutions (the recursion nests inside the span).
    states = crate::ctx::phase_walk(|| {
        gpm_walk_elements(
            &gctx,
            elements,
            &rel_filters.filters,
            start,
            states,
            &mut emit,
        )
    })?;
    // The rider consumed every complete state as it emerged (the walk's
    // deepest element offered them and returned nothing); its rows are
    // the answer, and the materializing tail below never runs.
    if emit.is_some() {
        return Ok(emitted);
    }

    // A selector keeps the materializing walk (`mount_rider` declines every
    // rider for one: the choice is cross-candidate, so the walk cannot halt
    // mid-enumeration); the per-candidate completion and the selector's own
    // choice are one function beside `apply_gpm_selector`.
    let candidates = selector::complete_and_select_candidates(&gctx, path, states)?;
    // A probe that could not ride the walk answers from the selector's
    // chosen rows instead — no early exit, and the same filter policy (the
    // residual pass the riding spelling applies per offered row), so the
    // verdict is the same by construction. The cap that would otherwise
    // ride the walk is charged here, through the same sink the shortest
    // route pushes through, so the count and the stopping rule are one
    // spelling for both materializing tails. An uncharged cap never fills,
    // so the caller's row loop never breaks and a `LIMIT` prefix costs the
    // whole input enumeration (LABS-1301).
    let selector_halt = Cell::new(false);
    let mut out = Matched::new();
    let mut sink = MatchedSink::new(rider, &selector_halt, where_clause, tail, &mut out);
    for state in candidates {
        if sink.halted() {
            break;
        }
        sink.offer(ctx, state.row, || state.used)?;
    }
    Ok(out)
}

/// The per-candidate completion pipeline every complete state runs — the
/// path-mode gate, the path-name binding, the pattern's own WHERE. The
/// path variable is bound before that predicate: a parenthesized path
/// pattern's `WHERE` names it (`... WHERE q <> p`), and that predicate is
/// a restrictor the selector must see — it narrows the candidate paths
/// the selector picks from, rather than filtering the one path the
/// selector already chose (CIP-60 §5.4.4.2, CIP-73 §5.4 "the restrictor
/// runs before the selector"). A predicate that also names a variable
/// this pattern does not bind stays a post-filter
/// (`crate::eval::candidate_passes`). The one spelling both
/// complete-state consumers read — the materializing candidate loop and
/// the rider's offer (`emit`) — so a rider-emitted row is the row the
/// materialized walk would emit by construction, not by two
/// hand-maintained copies agreeing.
fn complete_state(gctx: &GpmCtx<'_>, path: &PathPattern, state: &mut GpmState) -> Result<bool> {
    let GpmCtx {
        graph,
        params,
        cypher_version,
        ..
    } = *gctx;
    if !gpm_path_mode_allows(
        path.mode,
        state.nodes.iter().copied(),
        state.rels.iter().copied(),
    ) {
        return Ok(false);
    }
    // The completion's path-name rung through the walk's memo cell —
    // every complete state binds the same names, so one rung serves them
    // all (pattern_walk's `memoized_path_rung`, the one spelling).
    let rung = memoized_path_rung(&gctx.walk.path_rung, &state.row, path);
    bind_path_value(
        graph,
        &mut state.row,
        path,
        &state.nodes,
        &state.rels,
        &rung,
    );
    if let Some(predicate) = path.where_clause.as_deref() {
        if !inline_predicate_holds(graph, &state.row, predicate, params, cypher_version)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The walked path's full value from the walk's id slices — the one
/// materialization point of a GPM path (see [`GpmState`]): the path-name
/// bind ([`bind_path_value`]) and the quantified subpath value
/// (`gpm/qpp.rs`) read it, only where the path is observed. Every id was
/// visible when the walk took it (the neighbor enumeration yields only
/// rels that exist; a node id enters the slice only past its visibility
/// read) and the statement's read snapshot does not move under one walk,
/// so a miss is the enumeration invariant's break — the family's one
/// spelling for it (`expect_enumerated_candidate`), never a skip.
fn materialize_path(graph: &ExecGraph<'_>, nodes: &[NodeId], rels: &[RelId]) -> Path {
    Path {
        nodes: nodes
            .iter()
            .map(|&id| expect_enumerated_candidate(graph.node(id), id))
            .collect(),
        rels: rels
            .iter()
            .map(|&id| expect_enumerated_candidate(graph.rel(id), id))
            .collect(),
    }
}

/// Bind the pattern's path value (materialized from the walk's id slices,
/// [`materialize_path`]) under every name it introduces — the
/// complete-state consumers' spelling, so a rider-emitted row carries the
/// same bindings the materialized walk would emit. The name loop and the
/// append live in the shared `bind_path_names` (pattern_walk — the
/// frontier family's emission reads the same spelling), stamping the
/// walk's memoized rung; the path is built only when a name consumes it.
fn bind_path_value(
    graph: &ExecGraph<'_>,
    row: &mut Row,
    path: &PathPattern,
    nodes: &[NodeId],
    rels: &[RelId],
    rung: &RowRung,
) {
    bind_path_names(row, path, rung, || {
        Value::from_path(materialize_path(graph, nodes, rels))
    });
}

/// The slice-entry policy every GpmState builder rides (one spelling):
/// a visible node's id joins the walk's node slice; an invisible
/// (overlay-deleted) endpoint contributes no slice entry — the family's
/// established spelling (the eval engine's path builder omits it the
/// same way, `bind_eval_path`'s `if let Some(n) = graph.node(…)`),
/// read as an existence check, never the full record. Returns whether
/// the id joined — the walk's push/pop pairing and the start-node
/// selection gate their state on it; the QPP iteration's start discards
/// it on purpose (an invisible start walks on an empty slice, the
/// binding's tombstone policy).
pub(crate) fn push_visible_node(
    graph: &ExecGraph<'_>,
    id: NodeId,
    nodes: &mut Vec<NodeId>,
) -> bool {
    let visible = graph.node_exists(id);
    if visible {
        nodes.push(id);
    }
    visible
}

fn gpm_start_nodes(
    graph: &ExecGraph<'_>,
    node: &NodePattern,
    row: Row,
    used: UsedSet,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    residual: &Residual<'_>,
) -> Result<Vec<GpmState>> {
    let mut out = Vec::new();
    // The residual's ready conjuncts, derived once for the whole candidate
    // loop (every candidate binds the same names before the residual
    // runs), not re-derived per scanned node.
    let ready = super::pushdown::ready_conjuncts(residual, &row, node.variable.as_ref(), &[]);
    // LABS-448: the seek phase — the start resolution — is stamped inside
    // the one counted funnel; bound first, iterated second, the shape
    // every seek site shares.
    let candidates = node_candidates(
        graph,
        node,
        &row,
        params,
        cypher_version,
        residual.conjuncts,
    )?;
    for id in candidates {
        graph.cancel_tick()?;
        if !node_ok(graph, id, node, &row, params, cypher_version)? {
            continue;
        }
        let mut bound = row.clone();
        if !gpm_bind_node(graph, node, id, &mut bound) {
            continue;
        }
        if let Some(predicate) = &node.where_clause {
            if !inline_predicate_holds(graph, &bound, predicate, params, cypher_version)? {
                continue;
            }
        }
        if !super::pushdown::conjuncts_hold(graph, &bound, &ready, params, cypher_version)? {
            continue;
        }
        // the start's slice entry rides as its id (`GpmState`); an
        // invisible start contributes no state — the one slice-entry
        // policy (`push_visible_node`).
        let mut nodes = Vec::new();
        if push_visible_node(graph, id, &mut nodes) {
            out.push(GpmState {
                row: bound,
                used: used.clone(),
                current: id,
                nodes,
                rels: Vec::new(),
            });
        }
    }
    Ok(out)
}

fn gpm_expr_ready(expr: &Expr, row: &Row) -> bool {
    super::pushdown::expr_ready(expr, row)
}

/// Whether `predicate` holds on `row`. A row that does not yet bind every
/// variable the predicate names leaves the predicate to the post-filter
/// (through `crate::eval::candidate_passes`) instead of rejecting the row.
fn inline_predicate_holds(
    graph: &ExecGraph<'_>,
    row: &Row,
    predicate: &Expr,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<bool> {
    Ok(!gpm_expr_ready(predicate, row)
        || predicate_holds(graph, row, predicate, params, cypher_version)?)
}

/// PROFILE's hop counter (the `hops` read kind behind `Db Hits`), as the
/// delta across one query: reset, run, take — the one spelling the gpm
/// test modules' counter comparisons share (the allReduce inliner's and
/// the early-predicate tests').
#[cfg(test)]
pub(super) fn qpp_hops(graph: &crate::graph::Graph, query: &str) -> u64 {
    super::take_hop_expands();
    crate::exec::test_support::exec_cypher(graph, query);
    super::take_hop_expands()
}

#[cfg(test)]
mod allreduce_inline_tests;
#[cfg(test)]
mod early_tests;
#[cfg(test)]
mod emit_tests;
#[cfg(test)]
mod gpm_tests;
#[cfg(test)]
mod unify_tests;
