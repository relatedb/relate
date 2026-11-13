//! The bind policies of a pattern walk — the `binds.rs` arm of the
//! pattern-walk vocabulary; `super`'s module doc maps the whole family.
//! This module keeps the rung-bearing family — the stub binders, the
//! bind-name spellings ([`node_bind_name`]/[`rel_bind_name`]), the
//! trail bind and commit ([`bind_trail`], [`commit_trail`]), the node
//! bind ([`bind_pattern_node`]), the path-name trio
//! ([`bind_path_names`]/[`path_name_rung`]/[`memoized_path_rung`]) —
//! and the path-value reconstruction
//! ([`walk_rel_chunks`]/[`bind_reconstructed_path`]) the terminal
//! binders orchestrate. The admission family — the no-rung,
//! predicate-over-(graph, row, pattern) shapes those binds are gated
//! by — lives in the sibling `admit.rs`, relocated from `eval/pattern`
//! by LABS-811's stellar round and split out of this module beside its
//! per-hop atom: the exec walkers and the eval walk engine
//! (`crate::eval_walk`) import both arms downward from the tier.

use std::borrow::Cow;
use std::cell::Cell;

use relate_core::{Node, NodeId, Path, RelId, Relationship, Value};
use relate_ir::{NodePattern, PathPattern, RelPattern};
use relate_names::SmallName;

use crate::overlay::ExecGraph;
use crate::row::{Row, RowRung};

use super::{binding, rel_binding, rel_list_binding, Binding, UsedRels};

/// The node value a pattern binds `id` to (null when the stub is gone)
/// — the stub form: labels, no property map, no virtual-node fallback.
/// The exec walker's node binds ([`bind_pattern_node`]) read it: their
/// rows are internal carries whose property reads go through the graph
/// at evaluation time (`prop`'s graph-first policy, eval/expr.rs),
/// inflated to full snapshots only where a value leaves the engine
/// (ADR-0170's RETURN-time refresh). The eval walker's own node bind
/// deliberately does NOT read this stub — it binds
/// `ExecGraph::node_value` (the fully materialized node: the property
/// map in the value, the CIP-257 virtual-node fallback included),
/// because the eval walker's rows are expression payloads whose bound
/// values are observable as values themselves (a pattern comprehension
/// projects the binding, not a property of it).
pub(crate) fn bind_node_stub(graph: &ExecGraph<'_>, id: NodeId) -> Value {
    graph
        .node_stub(id)
        .map(Value::from_node)
        .unwrap_or(Value::Null)
}

/// The relationship value a pattern binds `id` to (null when the stub is
/// gone). Both walkers' trail binds read it, through [`bind_trail`] —
/// the one trail-binding spelling — including the eval walker's (whose
/// node binds deliberately differ; see [`bind_node_stub`]'s note).
pub(crate) fn bind_rel_stub(graph: &ExecGraph<'_>, id: RelId) -> Value {
    graph
        .rel_stub(id)
        .map(Value::from_relationship)
        .unwrap_or(Value::Null)
}

/// The list value a var-length (or multi-hop) trail binds: each rel's
/// stub ([`bind_rel_stub`]) in trail order. The one trail-to-list
/// spelling — [`bind_trail`]'s list arm, the GPM walk's var-length rel
/// bind, and the shortest-route row builder all read it, so every
/// engine binds the same list shape (stubs, inflated where a value
/// leaves the engine — ADR-0170's RETURN-time refresh).
pub(crate) fn bind_rel_stub_list(graph: &ExecGraph<'_>, trail: &[RelId]) -> Value {
    Value::List(
        trail
            .iter()
            .map(|&id| bind_rel_stub(graph, id))
            .collect::<Vec<_>>()
            .into(),
    )
}

/// The named path's first-seat row key (`__node0`) — the one spelling of
/// the anonymous node seat: this module's writer ([`bind_pattern_node`],
/// seat 0 only) and its one reader (`bind_named_path`,
/// exec/expand.rs) consult it through [`anon_node_name`], so a
/// scheme change is one edit, never a hand-propagated rename that
/// silently empties a seat read.
pub(crate) fn anon_node_name(idx: usize) -> String {
    format!("__node{idx}")
}

/// The anonymous rel stub name a rel element of a named path binds:
/// `__trail{idx}` — the one spelling, as [`anon_node_name`] is for
/// nodes: [`bind_trail`] writes it and the path-value reconstruction's
/// chunk walk ([`walk_rel_chunks`]) reads it back, so the two agree by
/// construction.
pub(crate) fn anon_rel_name(idx: usize) -> String {
    format!("__trail{idx}")
}

/// The name a node element binds: its variable, or — anonymous FIRST
/// element of a named path — the [`anon_node_name`] stub. `None`: an
/// anonymous node binds nothing otherwise — every later node of a named
/// path value is rebuilt from the rel endpoints and named variables, so
/// no `__node{idx > 0}` seat has a reader (LABS-811's stellar round).
/// The one spelling the bind policy ([`bind_pattern_node`]) and every
/// ladder build ([`super::ladder::WalkLadder::new`]) read, so the
/// precomputed schemas and the runtime binds agree by construction.
/// Borrowed when the element names its variable (no caller consumes the
/// name owned — every read site re-borrows it), owned only for the one
/// synthesized seat.
pub(crate) fn node_bind_name<'a>(
    np: &'a NodePattern,
    idx: usize,
    named: bool,
) -> Option<Cow<'a, str>> {
    match &np.variable {
        // The bind-name feed renders the IR name once (the row schema's
        // names are the String domain; rung builds are per-walk, memoized).
        Some(variable) => Some(Cow::Owned(variable.to_string())),
        None if named && idx == 0 => Some(Cow::Owned(anon_node_name(idx))),
        None => None,
    }
}

/// The name a rel element binds: its variable, or — anonymous on a named
/// path — the [`anon_rel_name`] stub. `None`: an anonymous rel of an
/// unnamed pattern binds nothing. The one spelling of the
/// variable-else-seat selection every rel-name reader consults — the
/// ladder build, the rel bind arm ([`bind_trail`], which the pre-bound
/// rel-list expansion also binds through), and the path-value
/// reconstruction's chunk walk ([`walk_rel_chunks`]) — so the
/// precomputed schemas and the runtime binds agree by construction.
/// Borrowed/owned on the same rule as [`node_bind_name`]: the variable
/// is never consumed owned, the synthesized seat is.
pub(crate) fn rel_bind_name<'a>(
    rp: &'a RelPattern,
    idx: usize,
    named: bool,
) -> Option<Cow<'a, str>> {
    match &rp.variable {
        // Same render-once rule as [`node_bind_name`].
        Some(variable) => Some(Cow::Owned(variable.to_string())),
        None if named => Some(Cow::Owned(anon_rel_name(idx))),
        None => None,
    }
}

/// Bind one name through the walk's rung: a fresh name stamps the rung's
/// precomputed schema (the append path — one `Arc` clone, no hashing); a
/// name the row already binds replaces its value in place (the insert
/// path's cheap existing-key arm). The one spelling every single-name
/// rung-binding site reads; the rung is always present — the ladder is
/// total over every pattern shape (the anonymous `__trail{idx}` seat
/// binds its empty list at a zero-hop stop exactly as the named arm
/// does), so no call site keeps an insert-path arm for a shape a ladder
/// cannot serve.
pub(crate) fn bind_rung_name(row: &mut Row, name: &str, value: Value, rung: &RowRung) {
    if row.contains_key(name) {
        row.insert(name.to_string(), value);
    } else {
        row.stamp(rung, [value]);
    }
}

/// Binds the trail to the pattern's rel variable (or, on a named path
/// — `named`, the same fact every seat-binding helper receives under
/// this one name — the anonymous `__trail` seat) at every stop the walk
/// can make, the zero-hop stop included: an empty trail binds the empty
/// list, exactly the value the named arm's variable binds there — the
/// totality the binding-schema ladder is built on (a stop that skipped
/// the seat would leave the row's schema past it unlike any rung, the
/// shape the former `ladder_precomputable` gate kept on the insert
/// path). Returns false when a pre-bound rel variable holds a different
/// relationship than the trail's single hop — the path does not match;
/// true otherwise. The fresh binds stamp `rung`, the walk's precomputed
/// schema for this bind. The one trail-binding spelling: the eval walk's
/// rel stops, the classic walk's forward and backward arms, the agg
/// fold's hop bind, and the pre-bound rel-list expansion
/// (`expand_bound_rel_list`) all bind through it.
pub(crate) fn bind_trail(
    row: &mut Row,
    rp: &RelPattern,
    trail: &[RelId],
    graph: &ExecGraph<'_>,
    rel_idx: usize,
    named: bool,
    rung: &RowRung,
) -> bool {
    // The one value-shape decision, derived once for both the named
    // variable and the named-path seat: a var-length trail (or any
    // multi-hop trail) binds a List of relationships, a single-hop
    // fixed-length trail binds the relationship itself.
    let single_hop = !rp.var_length && trail.len() == 1;
    let value = || {
        if single_hop {
            bind_rel_stub(graph, trail[0])
        } else {
            bind_rel_stub_list(graph, trail)
        }
    };
    if let Some(v) = &rp.variable {
        // Pre-bound reconciliation, every trail shape through the one
        // vocabulary and the one accept predicate ([`Binding::accepts`],
        // candidate by reference): a single-hop trail reconciles through
        // the id spelling ([`rel_binding`]); a var-length trail — or a
        // multi-hop fixed-length one — reconciles through the list
        // spelling ([`rel_list_binding`]), the LABS-661 exact-trail rule
        // the GPM engine's `Prebound` already pins at statement level (a
        // row-bound variable accepts only the same trail). A null or
        // wrong-typed binding — a NULL-bound variable imported from an
        // outer scope, e.g. the `r` of an unmatched `OPTIONAL MATCH`, or
        // a list holding a non-relationship element — matches nothing,
        // the same `Mismatch` predicate as the node counterpart, under
        // both spellings (LABS-811: this arm used to overwrite any
        // pre-bound list without reconciling, so the eval walk — pattern
        // comprehensions, pattern predicates, expression shortest paths
        // — silently re-bound what the statement engine pinned).
        let reconciled = if single_hop {
            rel_binding(rp, row).accepts(&trail[0])
        } else {
            rel_list_binding(rp, row).accepts(trail)
        };
        if !reconciled {
            return false;
        }
        v.with_str(|name| bind_rung_name(row, name, value(), rung));
    } else if let Some(seat) = rel_bind_name(rp, rel_idx, named) {
        // The anonymous arm's name through the one selection spelling
        // ([`rel_bind_name`]: the seat iff `named`) — the same derivation
        // the ladder build and the chunk walk read.
        bind_rung_name(row, &seat, value(), rung);
    }
    true
}

/// The one commit step every trail-accepting site reads: reconcile the
/// walked trail against the variable's pre-bound binding and bind it
/// ([`bind_trail`], stamping `rung`, the walk's precomputed schema for
/// this bind), then consume the trail's rels into the walk's used-set —
/// the sequence the eval walk's `walk_rel`, the exec walk's `walk`, and
/// the exact-trail walks each hand-spelled before LABS-811's fix round.
/// `false` means the trail does not reconcile: nothing is bound and
/// nothing is consumed, so the caller skips this trail as an answer
/// (never the walk — a longer pre-bound trail may extend through the
/// rejected prefix).
// The commit step's coordinate set (row, used, pattern, trail, graph,
// index, namedness, rung) — `bind_trail`'s own family plus the used-set
// it consumes into and the rung the bind stamps; a bundled carrier
// would hide which coordinates the commit reads.
#[allow(clippy::too_many_arguments)]
pub(crate) fn commit_trail(
    row: &mut Row,
    used: &mut UsedRels,
    rp: &RelPattern,
    trail: &[RelId],
    graph: &ExecGraph<'_>,
    rel_idx: usize,
    named: bool,
    rung: &RowRung,
) -> bool {
    if !bind_trail(row, rp, trail, graph, rel_idx, named, rung) {
        return false;
    }
    for id in trail {
        used.insert(*id);
    }
    true
}

/// The one node-binding policy, shared by the forward walk, the backward
/// walker, and the cheapest start: reconcile an existing binding of the
/// element's variable (a matching node is kept, anything else — null and
/// non-node bindings included — fails this candidate), else bind the node
/// stub; an anonymous element of a named path binds only the path's
/// first seat (`__node0`), the one seat `bind_named_path` consumes.
/// `false` means this candidate is rejected. The fresh binds append
/// through `rung`, the walk's precomputed schema for this bind; a
/// row-bound variable appends nothing.
///
/// The admission policy lives only in [`Binding::accepts`], spelled once
/// above the match — a row-bound variable accepts only its own id, a
/// null or non-node binding accepts nothing, an absent or nameless
/// variable accepts every candidate — and the match below carries only
/// the binding arms, both bind names derived through the one
/// [`node_bind_name`] spelling the ladder build reads (the helper
/// returns exactly the first seat for a nameless element: `Some` iff
/// `named && idx == 0`), so the precomputed schemas and the runtime
/// binds agree by construction.
///
/// The classification is [`super::binding`]'s, the one vocabulary — but
/// the value an unbound variable binds here is the node STUB, not the
/// full `graph.node_value` the eval walk (`crate::eval_walk::walk_pattern`)
/// binds. The difference is deliberate and preserved: the exec-side
/// walkers (match_node, the backward walker, the cheapest start)
/// re-resolve every later read through the store by id
/// (`eval::expr::prop` reads the store before the value's own property
/// map), so the stub's empty property map is masked, while the full value
/// would pay the property materialization for nothing; and the CIP-257
/// virtual-node fallback `node_value` carries is the visualization side
/// table, which these walkers never see. The eval walk keeps the full
/// value for its own paths — pattern comprehensions and EXISTS over
/// visualization rows. Unifying the two is a behavior decision, not this
/// module's to take.
pub(crate) fn bind_pattern_node(
    graph: &ExecGraph<'_>,
    row: &mut Row,
    np: &NodePattern,
    id: NodeId,
    idx: usize,
    named: bool,
    rung: &RowRung,
) -> bool {
    let class = binding(np, row);
    // The accepts-guard form `bind_trail` already uses: the one
    // admission policy, never re-spelled per arm below.
    if !class.accepts(&id) {
        return false;
    }
    // The binding arms — nothing below admits or rejects, and every bind
    // name derives through the one `node_bind_name` spelling the ladder
    // build reads: an unbound variable's own name, a nameless element's
    // exactly-one seat (`Some` iff `named && idx == 0` — the `__node0`
    // stub `bind_named_path` (exec/expand.rs) reads as the path's
    // first node; seat 0 is the only consumed seat, every later node of
    // the path value is rebuilt from the rel endpoints and named
    // variables, so no `__node{idx > 0}` seat has a reader; writer and
    // reader agree on the seat scheme's width, LABS-811's stellar round).
    match class {
        Binding::Unbound(_) | Binding::Anonymous => {
            if let Some(name) = node_bind_name(np, idx, named) {
                bind_rung_name(row, &name, bind_node_stub(graph, id), rung);
            }
        }
        // Accepted above through `accepts`: a row-bound variable keeps
        // its binding, and `Mismatch` never reaches the match.
        Binding::Bound(_) | Binding::Mismatch => {}
    }
    true
}

/// Bind the pattern's path value under every name it introduces — the
/// outer path variable and each named parenthesized subpath (`p = (q = …)`
/// binds both p and q; the two names name the same value). The one
/// spelling every path-name binding site reads: the GPM walk's
/// complete-state consumers (through gpm's slice-building
/// `bind_path_value`), the frontier family's witness emission (from
/// its materialized chain), the shortest-route row builder, the classic
/// walker's `bind_named_path`, and the eval walker's terminal
/// `bind_eval_path`. The value is built lazily, only when a name
/// consumes it: a pattern that binds no path name — the common anonymous
/// case — pays nothing. The fresh names append through `rung`, the
/// walk's precomputed schema after them (one `Arc` clone for all of
/// them, in the names' own order — the rung was built that way). A name
/// the row already binds replaces its value in place.
pub(crate) fn bind_path_names(
    row: &mut Row,
    path: &PathPattern,
    rung: &RowRung,
    build: impl FnOnce() -> Value,
) {
    if !path.has_path_name() {
        return;
    }
    // Path names ride the IR's representation (cheap copies); the row
    // probes read through the free text bridge and the inserts render.
    let (bound, fresh): (Vec<&SmallName>, Vec<&SmallName>) = path
        .path_names()
        .partition(|name| name.with_str(|text| row.contains_key(text)));
    let value = build();
    for name in bound {
        row.insert(name.to_string(), value.clone());
    }
    if fresh.is_empty() {
        return;
    }
    row.stamp(rung, std::iter::repeat_n(value.clone(), fresh.len()));
}

/// The path-name bind's per-walk rung: the row's schema plus the
/// pattern's fresh path names. What the completion sites — the GPM
/// walk's complete states, the shortest-route trail rows, the frontier
/// witness rows — precompute once per their walk: every candidate binds
/// the same names, so one rung serves them all.
pub(crate) fn path_name_rung(row: &Row, path: &PathPattern) -> RowRung {
    // Map-bridge: the rung's schema names are the String domain; each
    // path name renders once, at this per-walk (memoized) build.
    let names: Vec<String> = path.path_names().map(|name| name.to_string()).collect();
    RowRung::past(row, names.iter().map(|name| Some(name.as_str())))
}

/// The path-name rung through its per-walk memo cell: the first
/// completion with the cell empty builds the rung from its row's schema
/// ([`path_name_rung`]) and parks a clone in the cell; every completion
/// after that finds one parked, takes it, and re-parks a clone — one
/// `Arc` bump per hit, and the rung is built exactly once per walk (a
/// take is always followed by its re-park, so the cell never empties
/// again). The one spelling of that take/re-park dance — the completion
/// sites named above all call this per completed candidate, and none
/// hand-rolls the dance beside it (through `&Cell` a non-`Copy` rung
/// can only be read out by taking it).
pub(crate) fn memoized_path_rung(
    memo: &Cell<Option<RowRung>>,
    row: &Row,
    path: &PathPattern,
) -> RowRung {
    let rung = match memo.take() {
        Some(parked) => parked,
        None => path_name_rung(row, path),
    };
    memo.set(Some(rung.clone()));
    rung
}

/// The relationships one rel binding holds — the one chunk spelling the
/// path-value reconstruction reads: a `Relationship` value is its single
/// rel, a list the relationships of its elements, no binding at all the
/// empty chunk. One policy for every foreign shape: only the absent
/// binding is answered quietly ([`None`] — an anonymous rel of an
/// unnamed pattern binds nothing, so there is no binding to read); a
/// binding that is present but unreadable — null, a scalar, a node, or a
/// list holding a non-relationship — is the write/read contract break
/// this module refuses to paper over, answered loudly through the one
/// [`unreadable_rel_binding`] spelling. It is a contract break because
/// every present binding here is one the walk itself wrote: [`bind_trail`]
/// wrote the stub relationship or the stub list, or the pre-bound list
/// fully decoded through `expand_rel`'s all-or-nothing before the walk
/// bound it in place.
fn rel_chunk(binding: Option<&Value>) -> Vec<Relationship> {
    match binding {
        None => Vec::new(),
        Some(Value::Relationship(r)) => vec![(**r).clone()],
        Some(Value::List(xs)) => xs
            .iter()
            .map(|v| match v {
                Value::Relationship(r) => (**r).clone(),
                other => unreadable_rel_binding(other),
            })
            .collect(),
        Some(other) => unreadable_rel_binding(other),
    }
}

/// The one loud answer to a present-but-unreadable rel binding: the
/// write/read contract break [`rel_chunk`] names in its doc, spelled
/// once so the surviving scalar shape and the foreign list element
/// cannot answer it two opposite ways.
fn unreadable_rel_binding(value: &Value) -> ! {
    panic!("a bound rel binding holds only relationships, found {value:?}")
}

/// The rel half of a path-value reconstruction, spelled once for both
/// path binders (the classic walker's `bind_named_path`, the eval
/// walker's `bind_eval_path`): for each rel element of `path`, in
/// pattern order, the chunk its row binding holds — the bind name
/// through the one [`rel_bind_name`] selection (`row.get` of the
/// variable, else the anonymous seat on a named path — never a third
/// spelling beside the ladder build's and the bind arm's) — walked from
/// the node tail `nodes` ends at: each rel appended in whichever
/// orientation continues the path (the tail's id decides).
///
/// The gone-entity policy, one spelling for the rel and its reached
/// endpoint: both re-read through the graph — the row holds stubs, the
/// path value carries the full records — and an id the graph no longer
/// holds answers from what the row holds of it. The rel's row record is
/// the chunk entry itself (the stub snapshot the bind wrote). The
/// endpoint's is nothing: an endpoint of a walked rel binds a name only
/// at the pattern's own node positions, never at a var-length chunk's
/// interior, so a gone endpoint contributes no entry — the family's
/// established spelling for an invisible endpoint (the GPM walk's slices
/// omit it the same way, gpm/walk.rs) — stated here once, never a silent
/// skip, and never a partial path value the rel's own fallback could not
/// reconstruct.
///
/// `on_empty` runs, at its rel's position in the loop, for every rel
/// whose chunk was empty — the classic walker's fill-end-node addition
/// rides it through the orchestration skeleton; the eval walker passes
/// the no-op.
fn walk_rel_chunks(
    graph: &ExecGraph<'_>,
    path: &PathPattern,
    row: &Row,
    nodes: &mut Vec<Node>,
    rels: &mut Vec<Relationship>,
    mut on_empty: impl FnMut(usize, &mut Vec<Node>),
) {
    for (i, rp) in path.rels.iter().enumerate() {
        let chunk = rel_chunk(
            rel_bind_name(rp, i, path.has_path_name())
                .as_deref()
                .and_then(|name| row.get(name)),
        );
        let empty = chunk.is_empty();
        for r in chunk {
            let next_id = if nodes.last().map(|n| n.id) == Some(r.start) {
                r.end
            } else {
                r.start
            };
            rels.push(graph.rel(r.id).unwrap_or(r));
            if let Some(n) = graph.node(next_id) {
                nodes.push(n);
            }
        }
        if empty {
            on_empty(i, nodes);
        }
    }
}

/// The terminal path bind's orchestration skeleton — the one shape both
/// walkers' terminal binders reduce to (the eval walker's
/// `bind_eval_path`, the classic walker's `bind_named_path`), spelled
/// here beside the chunk walk and the path-name bind it orchestrates:
/// gate on the pattern's namedness, reconstruct the path value from the
/// row's own element bindings — the first node through `first_node`,
/// then [`walk_rel_chunks`] with `on_empty` at each empty chunk, then
/// `last_node` — and bind it under every name the pattern introduces
/// through [`bind_path_names`], stamping `rung`.
///
/// The three deliberate deltas of the two walkers ride in as explicit
/// arguments (each receiving the row, so no closure captures what the
/// skeleton mutates), each a stated policy of the walker it belongs to:
/// `first_node` — the eval walker reads the first node's variable and
/// re-materializes it through the graph, the classic walker reads the
/// variable or the exec-only `__node0` seat from the binding's own
/// snapshot; `on_empty` — the classic walker's fill-end-node addition at
/// each empty chunk, the eval walker's no-op; `last_node` — the classic
/// walker's last-node fallback when no node survived, the eval walker's
/// no-op.
pub(crate) fn bind_reconstructed_path(
    graph: &ExecGraph<'_>,
    path: &PathPattern,
    row: &mut Row,
    rung: &RowRung,
    first_node: impl FnOnce(&Row, &mut Vec<Node>),
    mut on_empty: impl FnMut(usize, &Row, &mut Vec<Node>),
    last_node: impl FnOnce(&Row, &mut Vec<Node>),
) {
    if !path.has_path_name() {
        return;
    }
    let mut nodes = Vec::new();
    let mut rels = Vec::new();
    first_node(row, &mut nodes);
    walk_rel_chunks(graph, path, row, &mut nodes, &mut rels, |i, nodes| {
        on_empty(i, row, nodes)
    });
    last_node(row, &mut nodes);
    bind_path_names(row, path, rung, || Value::from_path(Path { nodes, rels }));
}
