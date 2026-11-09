//! The unrolled fixed-k chain kernel (ADR-0473): the scan segment plus
//! 2..=4 fixed single hops as one contracted operator, per-hop predicate
//! programs monomorphized ([`program`]), the frontier carried as typed
//! column buffers.
//!
//! The frontier is structure-of-arrays at fixed arity — the scan origin,
//! one rel-id column and one node-id column per completed hop (the
//! substrate's columnar discipline, ADR-0468). The [`Chunk`] type itself
//! serves the *row-shaped* batches at pipeline boundaries; a hop reads its
//! frontier as ids and writes ids, and routing those through `Value`
//! materialization per read would re-introduce exactly the per-row cost
//! the kernel exists to remove — so the stages stay id-typed end to end
//! and the breaker is where values appear: the surviving bindings project
//! through [`Chunk::from_columns`] → [`Chunk::to_rows`], the scan-filter
//! chain's own breaker discipline (`exec/batch.rs`).
//!
//! Semantics are the walker's, spelled once per check: candidates come
//! from the one funnel ([`node_candidates`] — index seeks, the node-read
//! counter, the seek-phase stamp all keep working); every hop's rel is
//! live, type-filtered and endpoint-resolved through [`ExecGraph::hop`],
//! the same checks `hop_candidate` applies; the rel-uniqueness rule is the
//! lineage check (a hop's rel differs from every rel the same row already
//! took — at most four, an inline scan, never a per-row `HashSet`); and
//! every arrival is admitted by its compiled program (the `node_ok` label
//! arms plus the `WHERE` conjunct, three-valued keep-only-true included).

use relate_core::{NodeId, RelId, Result, Value};
use relate_ir::{var_prop_named, Expr};
use relate_names::SmallName;

use crate::ctx::ExecCtx;
use crate::pattern_walk::{bind_node_stub, bind_rel_stub};
use crate::row::Row;

use relate_core::Error;

use super::super::chunk::{Chunk, Column};
use super::super::WalkTallies;
use super::matcher::{Chain, Hop, MAX_CHAIN_HOPS};
use super::program::{Admission, NodePred};
use super::{KernelMode, KernelOutput};

/// One stage's columnar frontier: structure-of-arrays over the partial
/// bindings that survived the hops so far. `starts` is the scan segment's
/// admitted candidates (shared, never copied per row — the origin column
/// indexes it); `rels[j]` / `nodes[j]` are hop `j`'s rel and arrival
/// columns, live for `j < len`.
pub(super) struct Stage<'s> {
    starts: &'s [NodeId],
    /// Per row: the scan origin the walk left from.
    origin: Vec<u32>,
    /// Per completed hop: the rel id column.
    rels: [Vec<RelId>; MAX_CHAIN_HOPS],
    /// Per completed hop: the arrived-node column (node `j + 1`).
    nodes: [Vec<NodeId>; MAX_CHAIN_HOPS],
    /// Completed hops; `rels`/`nodes` columns `0..len` are live.
    len: usize,
    /// The stage's halt (LABS-990, ADR-0524): the complete-binding count
    /// this stage stops landing rows at — `None` for every uncapped hop,
    /// the rows mode's `want` for the final one. The one spelling of the
    /// halt: [`Stage::full`] is its reader, the hop expansion's two
    /// checks the only places it is read.
    cap: Option<usize>,
}

impl<'s> Stage<'s> {
    /// The stage before any hop: one row per admitted start.
    fn from_starts(starts: &'s [NodeId]) -> Self {
        let origin: Vec<u32> = (0..starts.len() as u32).collect();
        Self {
            starts,
            origin,
            rels: std::array::from_fn(|_| Vec::new()),
            nodes: std::array::from_fn(|_| Vec::new()),
            len: 0,
            cap: None,
        }
    }

    /// The next stage's empty shape: `len + 1` live columns, carrying the
    /// hop's halt (`None` for every uncapped hop).
    fn successor(&self, cap: Option<usize>) -> Stage<'s> {
        Stage {
            starts: self.starts,
            origin: Vec::new(),
            rels: std::array::from_fn(|_| Vec::new()),
            nodes: std::array::from_fn(|_| Vec::new()),
            len: self.len + 1,
            cap,
        }
    }

    /// Rows in the stage.
    fn rows(&self) -> usize {
        self.origin.len()
    }

    /// The halt's reader: whether the stage already holds its cap's
    /// complete-binding count (a capless stage is never full).
    fn full(&self) -> bool {
        self.cap.is_some_and(|at| self.origin.len() >= at)
    }

    /// The node the walk is currently at in row `r` — the last live
    /// arrival column, or the scan start before any hop.
    fn current(&self, r: usize) -> NodeId {
        if self.len == 0 {
            self.starts[self.origin[r] as usize]
        } else {
            self.nodes[self.len - 1][r]
        }
    }

    /// Whether `rid` is already in row `r`'s lineage — the
    /// different-relationships rule inside the pattern, at most four rels.
    fn lineage_contains(&self, r: usize, rid: RelId) -> bool {
        (0..self.len).any(|j| self.rels[j][r] == rid)
    }
}

/// Run the chain contract. The caller has proved the shape and the cost
/// gate; `starts` comes from the scan segment ([`super::scan_segment`]).
///
/// Only the **final** hop carries the rows mode's `want` halt
/// (LABS-990, ADR-0524 — the count-exactness contract and the
/// no-intermediate-cap starvation argument live on
/// [`KernelMode::Rows`]); every earlier hop runs uncapped.
pub(super) fn run(
    ctx: ExecCtx<'_>,
    chain: &Chain<'_>,
    starts: Vec<NodeId>,
    mode: KernelMode<'_>,
) -> Result<KernelOutput> {
    let halt = mode.halt();
    let mut stage = Stage::from_starts(&starts);
    let last = chain.hops.len().saturating_sub(1);
    for (i, hop) in chain.hops.iter().enumerate() {
        if stage.rows() == 0 {
            break;
        }
        let cap = if i == last { halt } else { None };
        stage = hop_stage(ctx, &stage, hop, cap)?;
    }
    match mode {
        KernelMode::Rows { .. } => Ok(KernelOutput::Rows(emit_rows(ctx, chain, &stage))),
        KernelMode::Exists { counted } => {
            Ok(KernelOutput::Exists(exists(ctx, chain, &stage, counted)?))
        }
    }
}

/// One hop as a stage transition: every row's current node expands over
/// the hop's typed adjacency, each candidate rel checked against the
/// lineage, resolved, and admitted by the hop's monomorphic program. The
/// successor stage carries the hop's halt (`cap`), and its `full()`
/// reader ends the expansion once the stage holds that many rows.
fn hop_stage<'s>(
    ctx: ExecCtx<'_>,
    prev: &Stage<'s>,
    hop: &Hop<'_>,
    cap: Option<usize>,
) -> Result<Stage<'s>> {
    let mut next = prev.successor(cap);
    let j = prev.len;
    expand_hop(ctx, prev, hop, j, Admission::of(&hop.target), &mut next)?;
    Ok(next)
}

/// The monomorphic hop expansion — the per-candidate checks below are
/// static calls, inlined (ADR-0473). The stage's own `full()` is the
/// halt's one spelling, derived from the container the landing pushes
/// into — never a second counter: checked ahead of the row and ahead of
/// each candidate, so `want == 0` halts before the first landing and the
/// count is exact for every `want`. The remaining candidates of the
/// halting row cost one comparison each (bounded by that row's degree);
/// later rows never expand.
fn expand_hop<P: NodePred>(
    ctx: ExecCtx<'_>,
    prev: &Stage<'_>,
    hop: &Hop<'_>,
    j: usize,
    pred: P,
    next: &mut Stage<'_>,
) -> Result<()> {
    let graph = ctx.graph;
    // The hop's written type list resolves once per stage hop (LABS-801);
    // the per-candidate adjacency walk compares interned ids.
    let types = graph.rel_types_of(hop.rel);
    // The prepared bound lives outside the row loop: the admission's own
    // lazy path fills it at the first candidate that reaches the
    // comparison, and a stage whose every row has no neighbor never
    // evaluates it — the per-row path's behavior for a candidate set that
    // never binds the target.
    let mut prepared: Option<P::Prepared> = None;
    // LABS-805: the hop/walk counts tally locally through the row loop
    // and flush on drop at the stage's end — never a thread-local
    // access per edge.
    let tallies = WalkTallies::default();
    for r in 0..prev.rows() {
        graph.cancel_tick()?;
        if next.full() {
            break;
        }
        let from = prev.current(r);
        let tallies = &tallies;
        graph.try_for_each_hop_resolved(from, hop.traverse, &types, |rid, arrival| {
            if next.full() {
                return Ok::<(), Error>(());
            }
            if prev.lineage_contains(r, rid) {
                return Ok::<(), Error>(());
            }
            if !pred.admits(&mut prepared, ctx, arrival)? {
                return Ok(());
            }
            tallies.walk();
            tallies.expansion_hop(j);
            next.origin.push(prev.origin[r]);
            for taken in 0..j {
                next.rels[taken].push(prev.rels[taken][r]);
                next.nodes[taken].push(prev.nodes[taken][r]);
            }
            next.rels[j].push(rid);
            next.nodes[j].push(arrival);
            Ok(())
        })?;
    }
    Ok(())
}

/// The binding names the breaker projects: the start variable, then each
/// hop's rel variable and arrival variable, pattern order — named
/// elements only, exactly the walker's bindings.
fn binding_names(chain: &Chain<'_>) -> Vec<String> {
    // Text render: the binding-name list feeds the row schema (String).
    let mut names = Vec::new();
    if let Some(var) = chain.start.pattern.variable.as_ref() {
        names.push(var.to_string());
    }
    for hop in &chain.hops {
        if let Some(var) = hop.rel.variable.as_ref() {
            names.push(var.to_string());
        }
        if let Some(var) = hop.target.pattern.variable.as_ref() {
            names.push(var.to_string());
        }
    }
    names
}

/// The binding values of row `r`, in [`binding_names`] order.
fn binding_values(ctx: ExecCtx<'_>, chain: &Chain<'_>, stage: &Stage<'_>, r: usize) -> Vec<Value> {
    let graph = ctx.graph;
    let mut values = Vec::new();
    if chain.start.pattern.variable.is_some() {
        values.push(bind_node_stub(
            graph,
            stage.starts[stage.origin[r] as usize],
        ));
    }
    for (j, hop) in chain.hops.iter().enumerate() {
        if hop.rel.variable.is_some() {
            values.push(bind_rel_stub(graph, stage.rels[j][r]));
        }
        if hop.target.pattern.variable.is_some() {
            values.push(bind_node_stub(graph, stage.nodes[j][r]));
        }
    }
    values
}

/// The breaker: the surviving bindings project through the chunk type
/// layer and decode to boundary rows — one shared schema, the
/// scan-filter chain's own discipline.
///
/// A pattern that names no element (every node and rel anonymous) binds
/// nothing: the clause's output is one empty row per complete binding,
/// exactly the walker's. The count comes from the stage — a chunk's row
/// count is its first column's length, so the zero-name shape routed
/// through the chunk layer would carry zero columns and answer zero
/// rows, the hole this arm closes.
fn emit_rows(ctx: ExecCtx<'_>, chain: &Chain<'_>, stage: &Stage<'_>) -> Vec<Row> {
    if stage.rows() == 0 {
        return Vec::new();
    }
    let names = binding_names(chain);
    if names.is_empty() {
        return vec![Row::new(); stage.rows()];
    }
    let mut columns: Vec<Vec<Value>> = vec![Vec::with_capacity(stage.rows()); names.len()];
    for r in 0..stage.rows() {
        for (slot, value) in columns.iter_mut().zip(binding_values(ctx, chain, stage, r)) {
            slot.push(value);
        }
    }
    let columns: Vec<Column> = columns.iter().map(|c| Column::from_values(c)).collect();
    Chunk::from_columns(&names, columns).to_rows()
}

/// Existence: the first complete binding whose counted expression is
/// non-null answers `true`; no binding answers `false`. The counted
/// expression is a single `<var>.<key>` read of a node variable the chain
/// binds (the engagement proof's two checks, stated as the invariant
/// here), so the check is the property read the per-row `eval` would
/// make, minus the row it never needs.
fn exists(
    ctx: ExecCtx<'_>,
    chain: &Chain<'_>,
    stage: &Stage<'_>,
    counted: Option<&Expr>,
) -> Result<bool> {
    for r in 0..stage.rows() {
        match counted {
            None => return Ok(true),
            Some(expr) => {
                let (var, key) = var_prop_named(expr)
                    .expect("the engagement proof admits only var.prop counted expressions");
                let node = node_of_var(chain, stage, r, var)
                    .expect("the engagement proof checked the counted variable is bound");
                if matches!(
                    ctx.graph.node_prop_resolved(node, &key),
                    Some(v) if !v.is_null()
                ) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// The node id row `r` binds `var` to. The engagement proof checked the
/// counted expression's variable is one of the chain's, so the miss arm
/// is unreachable — stated as the invariant, never guessed around.
fn node_of_var(chain: &Chain<'_>, stage: &Stage<'_>, r: usize, var: &SmallName) -> Option<NodeId> {
    if chain.start.pattern.variable.as_ref() == Some(var) {
        return Some(stage.starts[stage.origin[r] as usize]);
    }
    for (j, hop) in chain.hops.iter().enumerate() {
        if hop.target.pattern.variable.as_ref() == Some(var) {
            return Some(stage.nodes[j][r]);
        }
    }
    None
}
