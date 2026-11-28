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
use relate_ir::var_prop_named;
use relate_names::{NamedToken, SmallName};

use relate_core::Error;

use crate::ctx::ExecCtx;
use crate::overlay::ExecGraph;
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
    // The star's pattern-order projection, built once: the rows mode
    // accumulates through it and the exists mode resolves its counted
    // variable in it — never a second walk of the same vocabulary.
    let projection = Projection::new(star);
    // The exists mode's counted read, resolved once per run: the parse
    // and the slot lookup are run-invariant, so only the per-pair node
    // read is left inside the stream.
    let landing = match &mode {
        KernelMode::Rows { .. } => Landing::Rows,
        KernelMode::Exists { counted } => Landing::Exists(counted.map(|expr| {
            let (var, key) = var_prop_named(expr)
                .expect("the engagement proof admits only var.prop counted expressions");
            let slot = projection
                .node_slot(var)
                .expect("the engagement proof checked the counted variable is bound");
            (slot, key)
        })),
    };
    // The breaker's accumulating bindings: one column per projected slot
    // — the hub is one of them by construction (the family is defined by
    // a shared variable), so a pushed binding fills every column.
    let mut bindings = Bindings::new(&projection, halt);
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
        if bindings.full() {
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
        // When both sides must meet at a shared terminal and side A is wide
        // (e.g. a hub's 2-hop neighborhood in a triangle/cycle), sort side A
        // in place by terminal id so each side-B arrival binary-searches only
        // its matching slice instead of linearly scanning all of side A.
        const SHARED_TERMINAL_SORT_THRESHOLD: usize = 32;
        let sorted_by_terminal =
            star.shared_terminal && side_a.len() > SHARED_TERMINAL_SORT_THRESHOLD;
        if sorted_by_terminal {
            side_a.sort_unstable_by_key(SideRow::terminal);
        }
        let mut found = false;
        expand_side(ctx, &star.sides[1], &side_types[1], hub, &tallies, |b| {
            // The pairing rule, per B row: a side B row combines with the
            // side A rows whose terminal it lands on (every row, for the
            // open star) and whose rels it does not reuse — the cross-side
            // relationship-uniqueness rule, per pair.
            let candidates: &[SideRow] = if sorted_by_terminal {
                let target = b.terminal();
                let lo = side_a.partition_point(|a| a.terminal() < target);
                let hi = lo + side_a[lo..].partition_point(|a| a.terminal() == target);
                &side_a[lo..hi]
            } else {
                &side_a
            };
            for a in candidates {
                // The pairing loop is the run's quadratic phase; sample the
                // token per pair so a cancelled statement stops here, not
                // after the whole hub has paired.
                graph.cancel_tick()?;
                if (!star.shared_terminal || a.terminal() == b.terminal()) && !a.rels_overlap(&b) {
                    let binding = Binding {
                        hub,
                        sides: [*a, b],
                    };
                    match &landing {
                        Landing::Rows => {
                            // The halt, ahead of the push: `want == 0`
                            // answers zero rows and the checked-ahead form
                            // keeps the count exact for every `want`; a
                            // filled halt also ends this hub's B stream.
                            if bindings.full() {
                                return Ok(false);
                            }
                            bindings.push(graph, &binding);
                        }
                        Landing::Exists(counted_read) => {
                            // Existence needs SOME complete binding to
                            // pass, and every pairing of this B row is a
                            // distinct complete binding — a mid node of a
                            // 2-hop side 0, or side 0's terminal in an
                            // open star, varies across them — so the
                            // counted read runs on each pairing until one
                            // passes (the terminal is the only value the
                            // pairings share, and only in the
                            // shared-terminal mode).
                            if counted_nonnull(ctx, &binding, counted_read) {
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
    match landing {
        Landing::Rows => {
            let (names, columns) = bindings.into_columns();
            Ok(KernelOutput::Rows(
                Chunk::from_columns(&names, columns).to_rows(),
            ))
        }
        Landing::Exists(_) => Ok(KernelOutput::Exists(hit)),
    }
}

/// What one complete pairing does, resolved once per run from the
/// [`KernelMode`] — the pairing loop matches this, never the mode per pair.
enum Landing {
    /// Accumulate the binding into the answer set.
    Rows,
    /// Test the counted read (resolved slot and key) until one pairing
    /// passes; nothing means every complete binding counts.
    Exists(Option<(At, NamedToken)>),
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
        // Sample the token per candidate: one hop step can walk a whole
        // adjacency list, and the recursive side walk beneath it ticks at
        // its own candidates.
        graph.cancel_tick()?;
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
        // A stopped deeper walk becomes `Walk::Stop`, unwinding the stream.
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

/// Where one projected column's value comes from.
#[derive(Clone, Copy)]
enum At {
    /// The shared hub node.
    Hub,
    /// Side `side`'s rel at hop `hop`.
    Rel { side: usize, hop: usize },
    /// Side `side`'s arrival node at hop `hop`.
    Node { side: usize, hop: usize },
}

/// One column of the breaker's projection, in pattern order: the variable
/// it binds and the position holding it.
struct Slot {
    var: SmallName,
    at: At,
}

/// The star's pattern-order projection — the one walk that names a
/// binding's positions. Both modes consult it: rows mode projects each
/// binding through it, existence mode resolves its counted variable in
/// it, so no two of them can disagree.
struct Projection {
    slots: Vec<Slot>,
}

impl Projection {
    /// The hub is always named — the family is defined by one shared
    /// variable — so its absence is a broken compile, not a shape.
    fn new(star: &HubStar<'_>) -> Self {
        let hub = star.hub[0]
            .pattern
            .variable
            .as_ref()
            .expect("the hub-star family is defined by one shared variable");
        let mut slots = vec![Slot {
            var: hub.clone(),
            at: At::Hub,
        }];
        for (side, hops) in star.sides.iter().enumerate() {
            for (hop, h) in hops.iter().enumerate() {
                if let Some(var) = h.rel.variable.as_ref() {
                    slots.push(Slot {
                        var: var.clone(),
                        at: At::Rel { side, hop },
                    });
                }
                if let Some(var) = h.target.pattern.variable.as_ref() {
                    slots.push(Slot {
                        var: var.clone(),
                        at: At::Node { side, hop },
                    });
                }
            }
        }
        Projection { slots }
    }

    /// The column names, in pattern order — the row schema's text render.
    fn names(&self) -> Vec<String> {
        self.slots.iter().map(|slot| slot.var.to_string()).collect()
    }

    /// The node position binding variable `var` — the slot's position
    /// itself, so the node-only proof travels with the value.
    fn node_slot(&self, var: &SmallName) -> Option<At> {
        self.slots
            .iter()
            .find(|slot| slot.var == *var && !matches!(slot.at, At::Rel { .. }))
            .map(|slot| slot.at)
    }
}

/// One complete binding: the hub and both sides' rows.
#[derive(Clone, Copy)]
struct Binding {
    hub: NodeId,
    sides: [SideRow; 2],
}

impl Binding {
    fn node(&self, side: usize, hop: usize) -> NodeId {
        self.sides[side].nodes[hop]
    }

    fn rel(&self, side: usize, hop: usize) -> RelId {
        self.sides[side].rels[hop]
    }

    /// The node at a node position; `None` for a rel position.
    fn node_at(&self, at: At) -> Option<NodeId> {
        match at {
            At::Hub => Some(self.hub),
            At::Node { side, hop } => Some(self.node(side, hop)),
            At::Rel { .. } => None,
        }
    }
}

/// The rows mode's accumulator over the projection: one column of values
/// per slot and the halt. The columns are the row count's one owner —
/// `into_columns` hands them to [`Chunk::from_columns`], which takes the
/// row count from column 0.
struct Bindings<'p> {
    projection: &'p Projection,
    columns: Vec<Vec<Value>>,
    halt: Option<usize>,
}

impl<'p> Bindings<'p> {
    fn new(projection: &'p Projection, halt: Option<usize>) -> Self {
        let columns = vec![Vec::new(); projection.slots.len()];
        Bindings {
            projection,
            columns,
            halt,
        }
    }

    /// Whether the rows mode's halt is filled: `want == 0` is full from
    /// the start, so the pairing loop answers zero rows. The pushed count
    /// is column 0's length, and column 0 always exists — the family
    /// names its hub.
    fn full(&self) -> bool {
        self.halt.is_some_and(|halt| self.columns[0].len() >= halt)
    }

    /// Append one complete binding, projected through the slots.
    fn push(&mut self, graph: &ExecGraph<'_>, binding: &Binding) {
        for (slot, column) in self.projection.slots.iter().zip(&mut self.columns) {
            column.push(match slot.at {
                At::Hub => bind_node_stub(graph, binding.hub),
                At::Rel { side, hop } => bind_rel_stub(graph, binding.rel(side, hop)),
                At::Node { side, hop } => bind_node_stub(graph, binding.node(side, hop)),
            });
        }
    }

    /// The accumulated columns and their names, ready for the chunk
    /// layer. The row construction stays in the caller, whose output type
    /// is the ladder's rather than a row vector (ADR-0555).
    fn into_columns(self) -> (Vec<String>, Vec<Column>) {
        let names = self.projection.names();
        let encoded = self
            .columns
            .iter()
            .map(|column| Column::from_values(column))
            .collect();
        (names, encoded)
    }
}

/// The existence check for one complete binding. `counted` is the run's
/// resolved counted read — the slot holding the variable and its property
/// key, parsed once — or nothing (an `EXISTS { MATCH }`, where every
/// complete binding counts).
fn counted_nonnull(
    ctx: ExecCtx<'_>,
    binding: &Binding,
    counted: &Option<(At, NamedToken)>,
) -> bool {
    let Some((at, key)) = counted else {
        return true;
    };
    let node = binding
        .node_at(*at)
        .expect("the engagement proof checked the counted variable is bound to a node");
    matches!(
        ctx.graph.node_prop_resolved(node, key),
        Some(v) if !v.is_null()
    )
}
