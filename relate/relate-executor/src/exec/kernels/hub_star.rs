//! The hub-star kernel (ADR-0473): two pattern sides that meet at a
//! shared hub, evaluated hub-at-a-time — the candidate intersection the
//! q19/q01/q26 family names, and the bounded cycle q30 is (a shared
//! terminal).
//!
//! Per hub candidate (from the one candidate funnel, admitted by both
//! patterns' hub programs — the walker binds the hub once and reconciles):
//! expand each side hub → terminal with the same checks the chain kernel's
//! hops apply (typed adjacency, liveness, endpoint resolution, lineage
//! uniqueness, the arrival's monomorphic admission program), then combine
//! locally — a shared terminal intersects the sides' terminal sets, an
//! open star needs both non-empty — and emit the surviving cross product
//! with the cross-side relationship-uniqueness check (the two patterns'
//! rels must be disjoint, at most four rels, an inline scan).
//!
//! Why hub-at-a-time instead of enumerating the sides from their terminals
//! and hashing by hub: the sides are O(degree) small per hub, so the
//! per-hub combination is one reused buffer and a linear scan — no hash
//! table, no grouping (the bookkeeping whose cost forced LABS-348's
//! 128-row floor), and existence mode can stop at the first hub whose
//! sides intersect. The cost gate ([`super`]) is what decides this plan is
//! the right one for the shape: it engages only when the hub-driven work
//! estimate beats the terminal-driven alternative, so the queries whose
//! selective terminals make terminal enumeration the better plan (q01's
//! equality-pinned places and tags) keep the walker, which already starts
//! there.
//!
//! The side-A buffer is reused across hubs — hoisted before the hub
//! loop, one clear per hub, no allocation per hub: a `Chunk` per side
//! per hub would allocate per hub, and the q30 acceptance (151k hubs) is
//! exactly the shape that forbids it. Side B needs no buffer at all:
//! rows mode emits each matching pair inside the side-B stream (the
//! rows mode's halt stops the stream when it fills, LABS-990), and
//! existence mode decides inside the stream the same way. The breaker
//! still projects through the chunk type layer, the chain kernel's
//! discipline.

use relate_core::{NodeId, RelId, Result, Value};
use relate_ir::{var_prop_named, Expr};
use relate_names::SmallName;

use relate_core::Error;

use crate::ctx::ExecCtx;
use crate::pattern_walk::{bind_node_stub, bind_rel_stub};
use crate::store::RelTypes;

use super::super::chunk::{Chunk, Column};
use super::super::WalkTallies;
use super::matcher::{Hop, HubStar};
use super::program::{Admission, NodePred};
use super::{KernelMode, KernelOutput};

/// The most rels one side binding can carry (the matcher's 2-hop bound).
const MAX_SIDE_HOPS: usize = 2;

/// One side's binding for one hub: the rels taken and the nodes arrived
/// at, hub → terminal order — the walker's per-pattern bindings, in the
/// side's walk order.
#[derive(Clone, Copy)]
struct SideRow {
    rels: [RelId; MAX_SIDE_HOPS],
    nodes: [NodeId; MAX_SIDE_HOPS],
    /// Hops taken (1 or 2 — the matcher's side bound).
    len: usize,
}

impl SideRow {
    fn terminal(&self) -> NodeId {
        self.nodes[self.len - 1]
    }

    /// Whether any of this row's rels is `other`'s — the cross-side
    /// relationship-uniqueness check (two patterns, one MATCH).
    fn rels_overlap(&self, other: &SideRow) -> bool {
        (0..self.len).any(|i| (0..other.len).any(|j| self.rels[i] == other.rels[j]))
    }
}

/// Run the hub-star contract. The caller has proved the shape, the cost
/// gate, and admitted the hub candidates through both hub programs;
/// `hubs` is the admitted list.
pub(super) fn run(
    ctx: ExecCtx<'_>,
    star: &HubStar<'_>,
    hubs: Vec<NodeId>,
    mode: KernelMode<'_>,
) -> Result<KernelOutput> {
    let graph = ctx.graph;
    // The rows mode's `want` halt (LABS-990, ADR-0524 — the
    // count-exactness contract lives on [`KernelMode::Rows`]): the
    // pairing loop below checks it ahead of each push, so `want == 0`
    // answers zero rows. The side buffers still fill per hub (a capped
    // side could starve the pairing below `want`), their widths the same
    // as an uncapped run's.
    let halt = mode.halt();
    // Reused per-hub side buffers: one clear per hub, no allocation —
    // side B needs no buffer: the rows mode emits each matching pair
    // inside the side-B stream (the halt stops the stream when it fills),
    // and existence decides inside the stream the same way.
    let mut side_a = Vec::new();
    // The breaker's accumulating binding columns, one per named element —
    // the hub is one of them by construction (the family is defined by a
    // shared variable), so a pushed binding fills every column.
    let names = binding_names(star);
    let mut columns: Vec<Vec<Value>> = vec![Vec::new(); names.len()];
    // Complete bindings pushed — the row count's source. The count never
    // rides a column's length: the zero-binding answer is the counter's
    // zero, not an emptiness read off column 0.
    let mut bindings = 0usize;
    let mut hit = false;
    // LABS-805: the whole hub loop's hop/walk counts tally locally and
    // flush on drop at the run's end — never a thread-local access per
    // edge.
    let tallies = WalkTallies::default();
    // LABS-801: each side's hop filters resolve once per run, beside the
    // tallies — never per hub candidate.
    let side_types: [Vec<RelTypes>; 2] = star
        .sides
        .each_ref()
        .map(|hops| hops.iter().map(|hop| graph.rel_types_of(hop.rel)).collect());
    'hubs: for hub in hubs {
        graph.cancel_tick()?;
        // The halt's per-hub guard, the chain kernel's per-row guard's
        // twin: a halt filled at an earlier hub ends the loop before this
        // hub expands either side.
        if halt.is_some_and(|h| bindings >= h) {
            break;
        }
        side_a.clear();
        expand_side(ctx, &star.sides[0], &side_types[0], hub, &tallies, |row| {
            side_a.push(row);
            Ok(true)
        })?;
        if side_a.is_empty() {
            // The open star needs both sides; the walker's AND short-
            // circuits the same way — side B never expands for this hub.
            continue;
        }
        let mut found = false;
        expand_side(ctx, &star.sides[1], &side_types[1], hub, &tallies, |b| {
            // The pairing rule, per B row: a side B row combines with the
            // side A rows whose terminal it lands on (every row, for the
            // open star) and whose rels it does not reuse — the cross-side
            // relationship-uniqueness rule, per pair.
            for a in &side_a {
                if (!star.shared_terminal || a.terminal() == b.terminal()) && !a.rels_overlap(&b) {
                    match mode {
                        KernelMode::Rows { .. } => {
                            // The halt, ahead of the push: `want == 0`
                            // answers zero rows and the checked-ahead form
                            // keeps the count exact for every `want`; a
                            // filled halt also ends this hub's B stream.
                            if halt.is_some_and(|h| bindings >= h) {
                                return Ok(false);
                            }
                            push_binding(ctx, star, hub, a, &b, &names, &mut columns);
                            bindings += 1;
                        }
                        KernelMode::Exists { counted } => {
                            // Existence needs SOME complete binding to
                            // pass, and every pairing of this B row is a
                            // distinct complete binding — a mid node of a
                            // 2-hop side 0, or side 0's terminal in an
                            // open star, varies across them — so the
                            // counted read runs on each pairing until one
                            // passes (the terminal is the only value the
                            // pairings share, and only in the
                            // shared-terminal mode).
                            if counted_nonnull(ctx, star, hub, a, &b, counted) {
                                found = true;
                                return Ok(false);
                            }
                        }
                    }
                }
            }
            Ok(true)
        })?;
        if found {
            hit = true;
            break 'hubs;
        }
    }
    match mode {
        KernelMode::Rows { .. } => {
            let mut rows = Vec::new();
            if bindings > 0 {
                let encoded: Vec<Column> = columns
                    .iter()
                    .map(|column| Column::from_values(column))
                    .collect();
                rows = Chunk::from_columns(&names, encoded).to_rows();
            }
            Ok(KernelOutput::Rows(rows))
        }
        KernelMode::Exists { .. } => Ok(KernelOutput::Exists(hit)),
    }
}

/// One side's walk-invariant state (LABS-805 round 2): the exec context,
/// the side's hops, and the run's walk tallies ride together through the
/// side's recursion, so a new per-walk fact is one field, not a
/// signature edit per frame.
#[derive(Clone, Copy)]
struct SideWalk<'a> {
    ctx: ExecCtx<'a>,
    hops: &'a [Hop<'a>],
    /// The hops' resolved rel-type filters (LABS-801), resolved once per
    /// run beside the hops — never per candidate of the previous hop.
    types: &'a [RelTypes],
    tallies: &'a WalkTallies,
}

/// Expand one side from `hub`, offering every complete binding to
/// `on_row`; its `false` stops the walk (the existence stream's early
/// exit). The depth-first walk applies each hop's monomorphic admission
/// program, the lineage uniqueness rule, and the endpoint resolution, in
/// the walker's own check order.
fn expand_side(
    ctx: ExecCtx<'_>,
    hops: &[Hop<'_>],
    types: &[RelTypes],
    hub: NodeId,
    tallies: &WalkTallies,
    mut on_row: impl FnMut(SideRow) -> Result<bool>,
) -> Result<()> {
    let (mut rels, mut nodes) = (
        [RelId::default(); MAX_SIDE_HOPS],
        [NodeId::default(); MAX_SIDE_HOPS],
    );
    let walk = SideWalk {
        ctx,
        hops,
        types,
        tallies,
    };
    // The caller's stream either ran to completion or stopped itself;
    // both are success here.
    expand_hop_at(&walk, 0, hub, &mut rels, &mut nodes, &mut on_row).map(|_| ())
}

/// The per-hop recursion of [`expand_side`]: one hop's expansion from
/// `from`, carrying the lineage (`rels`) and arrivals (`nodes`) so far.
/// `Ok(false)` means `on_row` stopped the walk; it unwinds unchanged.
fn expand_hop_at(
    walk: &SideWalk<'_>,
    index: usize,
    from: NodeId,
    rels: &mut [RelId; MAX_SIDE_HOPS],
    nodes: &mut [NodeId; MAX_SIDE_HOPS],
    on_row: &mut dyn FnMut(SideRow) -> Result<bool>,
) -> Result<bool> {
    let Some(hop) = walk.hops.get(index) else {
        // The side is complete: offer the binding.
        return on_row(SideRow {
            rels: *rels,
            nodes: *nodes,
            len: index,
        });
    };
    step_hop(
        walk,
        index,
        from,
        rels,
        nodes,
        on_row,
        Admission::of(&hop.target),
    )
}

/// The stream-stop signal through a neighbor loop — the Err-as-break
/// idiom the overlay's own `try_find_neighbor` uses, over a private
/// variant no real failure can produce (a real error stays `Err`).
enum Walk {
    /// `on_row` stopped the stream.
    Stop,
    /// A real failure.
    Err(Error),
}

impl From<Error> for Walk {
    fn from(err: Error) -> Self {
        Walk::Err(err)
    }
}

/// One hop's monomorphic step — every per-candidate check below is a
/// static call, inlined (ADR-0473).
fn step_hop<P: NodePred>(
    walk: &SideWalk<'_>,
    index: usize,
    from: NodeId,
    rels: &mut [RelId; MAX_SIDE_HOPS],
    nodes: &mut [NodeId; MAX_SIDE_HOPS],
    on_row: &mut dyn FnMut(SideRow) -> Result<bool>,
    pred: P,
) -> Result<bool> {
    let SideWalk {
        ctx,
        hops,
        types,
        tallies,
    } = *walk;
    let graph = ctx.graph;
    let hop = &hops[index];
    // The hop's filter arrives resolved from the run (LABS-801); the
    // per-candidate adjacency walk compares interned ids.
    let types = &types[index];
    // The prepared bound fills at the first candidate that reaches the
    // comparison (the admission's own lazy path) — a side with no
    // neighbor never evaluates it, the per-row path's behavior.
    let mut prepared: Option<P::Prepared> = None;
    let stream = graph.try_for_each_hop_resolved(from, hop.traverse, types, |rid, arrival| {
        // The different-relationships rule inside the side: this hop's rel
        // differs from the rels the binding already took.
        if rels[..index].contains(&rid) {
            return Ok(());
        }
        if !pred.admits(&mut prepared, ctx, arrival)? {
            return Ok(());
        }
        tallies.walk();
        tallies.expansion_hop(index);
        rels[index] = rid;
        nodes[index] = arrival;
        // The stream stopped: unwind through the neighbor loop.
        match expand_hop_at(walk, index + 1, arrival, rels, nodes, on_row) {
            Ok(true) => Ok(()),
            Ok(false) => Err(Walk::Stop),
            Err(err) => Err(Walk::Err(err)),
        }
    });
    match stream {
        // The stream stopped: unwind through the neighbor loop.
        Err(Walk::Stop) => Ok(false),
        Err(Walk::Err(err)) => Err(err),
        Ok(()) => Ok(true),
    }
}

/// The binding names the breaker projects: the hub variable, then each
/// side's rel and node variables in hop order — exactly the walker's
/// bindings, pattern order.
fn binding_names(star: &HubStar<'_>) -> Vec<String> {
    // Text render: the binding-name list feeds the row schema (String).
    let mut names = Vec::new();
    if let Some(var) = star.hub[0].pattern.variable.as_ref() {
        names.push(var.to_string());
    }
    for side in &star.sides {
        for hop in side {
            if let Some(var) = hop.rel.variable.as_ref() {
                names.push(var.to_string());
            }
            if let Some(var) = hop.target.pattern.variable.as_ref() {
                names.push(var.to_string());
            }
        }
    }
    names
}

/// Push one complete binding's values onto the breaker's columns.
fn push_binding(
    ctx: ExecCtx<'_>,
    star: &HubStar<'_>,
    hub: NodeId,
    a: &SideRow,
    b: &SideRow,
    names: &[String],
    columns: &mut [Vec<Value>],
) {
    let graph = ctx.graph;
    let mut values = Vec::with_capacity(names.len());
    if star.hub[0].pattern.variable.is_some() {
        values.push(bind_node_stub(graph, hub));
    }
    for (row, side) in [a, b].iter().zip(&star.sides) {
        for (index, hop) in side.iter().enumerate() {
            if hop.rel.variable.is_some() {
                values.push(bind_rel_stub(graph, row.rels[index]));
            }
            if hop.target.pattern.variable.is_some() {
                values.push(bind_node_stub(graph, row.nodes[index]));
            }
        }
    }
    for (slot, value) in columns.iter_mut().zip(values) {
        slot.push(value);
    }
}

/// The existence check for one complete binding: the counted expression
/// is a single `<var>.<key>` read of a variable the patterns bind (the
/// engagement proof), or nothing (an `EXISTS { MATCH }` — every complete
/// binding counts).
fn counted_nonnull(
    ctx: ExecCtx<'_>,
    star: &HubStar<'_>,
    hub: NodeId,
    a: &SideRow,
    b: &SideRow,
    counted: Option<&Expr>,
) -> bool {
    let Some(expr) = counted else {
        return true;
    };
    let (var, key) = var_prop_named(expr)
        .expect("the engagement proof admits only var.prop counted expressions");
    let node = node_of_var(star, hub, a, b, var)
        .expect("the engagement proof checked the counted variable is bound");
    matches!(
        ctx.graph.node_prop_resolved(node, &key),
        Some(v) if !v.is_null()
    )
}

/// The node id the binding gives `var` — the hub, or one side's arrival.
fn node_of_var(
    star: &HubStar<'_>,
    hub: NodeId,
    a: &SideRow,
    b: &SideRow,
    var: &SmallName,
) -> Option<NodeId> {
    if star.hub[0].pattern.variable.as_ref() == Some(var) {
        return Some(hub);
    }
    for (row, side) in [(a, &star.sides[0]), (b, &star.sides[1])] {
        for (index, hop) in side.iter().enumerate() {
            if hop.target.pattern.variable.as_ref() == Some(var) {
                return Some(row.nodes[index]);
            }
        }
    }
    None
}
