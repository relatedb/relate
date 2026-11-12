//! The contracted-operator layer (ADR-0473): plan-time structural
//! matching over a small family table ([`matcher`]), cost-gated
//! engagement over the planner's own estimate machinery (ADR-0218), and
//! the two traversal kernels — the unrolled fixed-k chain ([`chain`]) and
//! the hub-star candidate intersection ([`hub_star`]) — over the batch
//! substrate's typed column buffers, per-hop predicate programs
//! monomorphized ([`program`]).
//!
//! Engagement is fail-through, never fail-silent: every condition is a
//! proof (the matcher's structural and predicate proofs, the cost gate's
//! estimate), and an unproved clause runs today's per-row walker
//! unchanged — same rows, same errors, so the openCypher TCK is the net
//! either way. The kernels complement the proven machinery, they do not
//! replace it: the aggregate folds (`agg_fold`), the exists probe's
//! depth-first seek, and the scan-filter chain (`exec/batch.rs`) keep
//! every shape the kernels decline; the LABS-348 hub join keeps the
//! multi-row driving tables and the predicate shapes outside the kernels'
//! closed vocabulary (its measured 128-row floor is its own join's
//! grouping cost, which the flat per-hub combination does not pay).
//!
//! The two entry points: [`try_match_kernel`] (the row-returning `MATCH`
//! path, engaged ahead of the per-row walker) and [`try_exists_kernel`]
//! (the existence path, engaged ahead of the exists probe's seek).

pub(crate) mod matcher;
pub(crate) mod program;

// The frontier-NFA compiler (ADR-0478 Decision 1): `compile` is
// production-read by the frontier matcher arm (`frontier_match`) as its
// structural proof engine, and the tables it returns are production-read
// by the BFS runner (`frontier`).
pub(crate) mod nfa;
#[cfg(test)]
mod nfa_tests;

// The frontier BFS core (ADR-0478 Decision 1): the level-synchronous
// walk over the ExecGraph overlay that a compiled `FrontierNfa` drives,
// run by the rider wiring (`frontier_run`) and the witness emission
// (`frontier_emit`).
pub(crate) mod frontier;
#[cfg(test)]
mod frontier_tests;

// The frontier family's table vocabulary (ADR-0478 Decision 6's
// allocation half): the sparse tables one run reads and writes (sized
// by the pairs it touches, never the store's span), the run's
// endpoint/arrival/chain records, and the allocation ceiling that bounds
// them — split from the BFS runner (`frontier`) by the file-size rule;
// the runner and every consumer (the emission, the re-arrival rule)
// read these shapes.
pub(crate) mod frontier_tables;
#[cfg(test)]
mod frontier_tables_tests;

// The sparse node-keyed map under the frontier family's tables and the
// re-arrival rule's buffers: open-addressed, generation-stamped (O(1)
// reset), sized by the keys inserted — never by the store's span.
pub(crate) mod node_map;

// The re-arrival rule (ADR-0478 §3): the frontier BFS's pool-exactness
// seam — the default pool's trail-existence proofs at every accepting
// arrival it cannot already prove a trail for, and the walk pool's
// below-min candidates through the walk-length check. Reached through
// `frontier` and `frontier_emit`; its bounded-search core is the rule's
// own child module (`rearival::search`), never a sibling.
pub(crate) mod rearival;
#[cfg(test)]
mod rearival_tests;

// The frontier family's plan-time matcher arm (ADR-0478 §2): the
// structural and answer-shape proofs plus the cost gate, compiled by the
// rider wiring (`frontier_run`).
pub(crate) mod frontier_match;
#[cfg(test)]
mod frontier_match_tests;

// The witness reconstruction and capped emission (ADR-0478 Decision 1's
// output-sensitive half): the parent+rel tables' reader, binding the row
// shapes the DFS emits for the ≤ cap witnesses — the capped consumer's
// emission, run by the rider wiring.
pub(crate) mod frontier_emit;
#[cfg(test)]
mod frontier_emit_tests;

// The frontier rung's per-push charge pins (LABS-1077): the emission
// points' capped refusal while rows land, mirroring the dispatch layer's
// aggregate-charge pins.
#[cfg(test)]
mod frontier_charge_tests;

// The frontier family's rider wiring (ADR-0478 §2/§6): the executor
// plug-in — the row rung on the MATCH operator ladder and the EXISTS arm
// inside `try_exists_kernel`, the seeds, the PROFILE counters, the
// budget-tick symmetry's engaged half.
#[cfg(test)]
mod frontier_rider_tests;
pub(crate) mod frontier_run;

// The walk-count arm: a non-DISTINCT `count` over a bounded WALK-pool
// quantified pattern with an unfiltered target, answered from per-level
// (node, state) multiplicities and a last-leg degree sum — a MATCH fold,
// tried first in `dispatch::try_match_folds`.
pub(crate) mod frontier_count;
#[cfg(test)]
mod frontier_count_tests;

// The statement-level A/B differential (ADR-0478 Decision 4's test
// half, the differential-suite node): the same whole statements run
// engaged and stood down over the rearival suite's generated
// multigraphs, the fixture matrix, and hand-built cyclic edge cases —
// answer-set equality for distinct-endpoint and EXISTS answers, per-row
// validity for capped paths, identical rows for every decline.
#[cfg(test)]
mod frontier_differential_tests;

// The boundary-pair comparison (`n <> p`) the distinct-endpoint consumer
// admits (LABS-1433): the matcher's decisions and the differential
// against the stood-down walker.
#[cfg(test)]
mod frontier_pair_tests;

// The PROFILE record's plan-time decline reason (LABS-1437).
#[cfg(test)]
mod frontier_decline_tests;

// The diagnostic engine switch's routing pins (ADR-0519): the forced
// rung, the stood-down kernels, and the auto twin — the switch's own
// suite, beside the rider wiring's.
#[cfg(test)]
mod engine_switch_tests;

// The chain gate's pricing of the hops the walk model leaves unpriced
// (LABS-991).
#[cfg(test)]
mod chain_gate_tests;

// The PROFILE rung record's own pins (LABS-1117): two distinct shapes
// (a fixed chain and a var-length pattern) so a constant rung fails, plus
// the walker's positive record.
#[cfg(test)]
mod rung_tests;

mod chain;
mod chain_count;
#[cfg(test)]
mod chain_count_tests;
mod hub_star;

// The chain count fold's entry, lifted the way the aggregate folds lift
// theirs (`agg_fold`): the module stays private to the kernels, the one
// entry is the public path.
pub(crate) use chain_count::try_fold_chain_count;

use relate_core::{NodeId, Result, Value};
use relate_ir::{
    classify_aggregate, AggregateKind, Clause, Expr, MatchMode, PathPattern, Projection,
};
use relate_planner::seed_on_rel_start;
use relate_planner::{endpoint_estimate, seed_rows};

use crate::ctx::{ExecCtx, StartReason, WalkStart};
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use crate::pattern_engine::PatternEngine;
use crate::pattern_walk::node_candidates;
use crate::row::Row;

use super::agg_fold::FoldedReturn;
use super::graph_stats::GraphStats;
use super::pushdown::{flatten_and_owned, match_scope, Residual};
use super::record_walk_start;

use frontier_match::{boundary, boundary_vars};
use matcher::Contract;
use program::{Admission, NodePred, NodeTarget};

/// The one projection item when the suffix is exactly `RETURN count(*)`
/// / `RETURN count(<boundary var>)` — non-DISTINCT, one item, no
/// grouping, ordering, or paging: the answer is the walk count. The
/// RETURN-shape gate both count folds consult (the frontier walk count
/// and the chain count), so it lives beside them in the shared parent.
fn counted_return<'c>(clause: Option<&'c Clause>, pattern: &PathPattern) -> Option<&'c Projection> {
    let Some(Clause::Return {
        distinct: false,
        group_by: None,
        items,
        order,
        skip: None,
        limit: None,
    }) = clause
    else {
        return None;
    };
    let ([item], true) = (items.as_slice(), order.is_empty()) else {
        return None;
    };
    match &item.expr {
        Expr::CountStar => Some(item),
        Expr::Function {
            name,
            distinct: false,
            args,
        } if name.with_str(classify_aggregate) == Some(AggregateKind::Count) => {
            let [Expr::Variable(var)] = args.as_slice() else {
                return None;
            };
            let (start, target) = boundary(pattern)?;
            boundary_vars(start, target)
                .into_iter()
                .flatten()
                .any(|name| name == var)
                .then_some(item)
        }
        _ => None,
    }
}

/// The count folds' one-row answer tail: the `i64` conversion decline,
/// the item's output name, and the single `(name → count)` cell spelled
/// once for both the row and the result's data, so the two cannot drift.
fn count_fold_tail(graph: &ExecGraph<'_>, count: u128, item: &Projection) -> Option<FoldedReturn> {
    let Ok(count) = i64::try_from(count) else {
        return None;
    };
    let name = item.output_name(0);
    let cell = Value::Int(count);
    let mut out = Row::new();
    out.insert(name.clone(), cell.clone());
    Some(FoldedReturn {
        result: super::QueryResult::intermediate(vec![name], vec![vec![cell]], graph.stats()),
        rows: vec![out],
        consumed_after: 1,
    })
}

/// What a caller wants from a contracted kernel.
pub(crate) enum KernelMode<'a> {
    /// Every complete binding, as rows — the clause's output. `want` is
    /// the plain cap's `SKIP+LIMIT` count riding the kernel (LABS-990,
    /// ADR-0524): the kernel's **final collection** — the chain's last
    /// hop, the hub-star's pairing loop — halts after `want` complete
    /// bindings; `None` collects every binding. Only complete bindings
    /// are counted, so the answer is exactly `min(want, total)` —
    /// count-exact by construction, never starved: no intermediate stage
    /// is capped, because the first `want` partials can all die
    /// downstream. Which `want` rows arrive is the kernel's enumeration
    /// order, a contract-free prefix the way ADR-0478 §4 states it for
    /// the frontier's capped emission: LIMIT without ORDER BY promises a
    /// count, not a selection. The count's WHERE-exactness rests on the
    /// kernels' own admissions — a conjunct outside the closed vocabulary
    /// declines the kernel ([`matcher::compile`]), so only kept rows
    /// land and no sink filter runs on this path.
    Rows { want: Option<usize> },
    /// Existence: stop at the first complete binding whose counted
    /// expression is non-null. `None` counts every complete binding (an
    /// `EXISTS { MATCH … }`).
    Exists { counted: Option<&'a Expr> },
}

impl KernelMode<'_> {
    /// The rows mode's halt count — `None` for every other mode. The one
    /// extraction both kernels read their halt from, so the fact is
    /// spelled once beside the enum that owns it.
    pub(crate) fn halt(&self) -> Option<usize> {
        match self {
            Self::Rows { want } => *want,
            Self::Exists { .. } => None,
        }
    }
}

/// A kernel's answer.
pub(crate) enum KernelOutput {
    Rows(Vec<Row>),
    Exists(bool),
}

/// The row-returning entry: one `MATCH` clause's patterns as the batch
/// chain's traversal twin. `Ok(None)` is the decline — the caller runs
/// its per-row path unchanged; `Ok(Some((rows, rung)))` is the clause's
/// output together with the rung that served it (LABS-1117), so the
/// ladder's return value names the kernel without a second lookup.
///
/// The engagement proofs beyond the matcher's: the clause is the
/// statement's first (a single unbound seed row — bindings carried into a
/// mid-statement clause keep the walker's per-row semantics), not
/// `OPTIONAL` (the null-binding arm is the walker's), and no sink
/// projection rides it (its semantics live in the per-row walker's sink,
/// the batch chain's own gate). A **plain** limit cap rides as the rows
/// mode's `want` — it halts the kernel's final collection (LABS-990;
/// ADR-0524), replacing the earlier policy of dropping the cap and
/// letting the trailing projection truncate a fully enumerated space; the
/// ordered and distinct caps still decline (their semantics are the
/// walker's own, the ladder's gate keeps them there).
#[allow(clippy::too_many_arguments)]
pub(crate) fn try_match_kernel(
    ctx: ExecCtx<'_>,
    rows: &[Row],
    optional: bool,
    match_mode: MatchMode,
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    cap_want: Option<usize>,
) -> Result<Option<(Vec<Row>, KernelRung)>> {
    if kernels_stood_down(ctx.graph.base()) {
        return Ok(None);
    }
    if optional || rows.len() != 1 {
        return Ok(None);
    }
    let conjuncts = where_clause.map(flatten_and_owned).unwrap_or_default();
    let Some(contract) = matcher::compile(patterns, match_mode, &rows[0], &conjuncts) else {
        return Ok(None);
    };
    let stats = GraphStats::new(ctx.graph.base(), ctx.params);
    if !engages(&stats, &conjuncts, &contract, false) {
        return Ok(None);
    }
    let rung = contract.rung();
    #[cfg(test)]
    kernel_engaged();
    let residual = Residual {
        conjuncts: &conjuncts,
        scope: &match_scope(&rows[0], patterns),
    };
    let output = run(
        ctx,
        &contract,
        &residual,
        KernelMode::Rows { want: cap_want },
    )?;
    Ok(Some(match output {
        KernelOutput::Rows(rows) => (rows, rung),
        // `run` answers Rows for the rows mode — the two halves of the
        // enum pair up at the two entry points, never across them.
        KernelOutput::Exists(_) => {
            unreachable!("the row entry never asks for existence")
        }
    }))
}

/// The existence entry: the patterns of a plain `MATCH` span (the
/// boolean-count fold's, or an `EXISTS { MATCH … }` subquery's) with the
/// counted expression of the fold, over the one unbound seed row.
/// `Ok(None)` is the decline — the caller keeps the exists probe.
pub(crate) fn try_exists_kernel(
    ctx: ExecCtx<'_>,
    patterns: &[PathPattern],
    wheres: &[Expr],
    row: &Row,
    match_mode: MatchMode,
    counted: Option<&Expr>,
) -> Result<Option<bool>> {
    if kernels_stood_down(ctx.graph.base()) {
        return Ok(None);
    }
    if match_mode != MatchMode::DifferentRelationships || !row.is_empty() {
        return Ok(None);
    }
    // The counted expression must be a single property read of a node
    // variable (the fold's `count(DISTINCT var.key)` family) — anything
    // richer, or a rel variable's property (the probe's row-shaped eval
    // serves it), keeps the probe.
    if let Some(counted) = counted {
        let Some((var, _)) = relate_ir::var_prop(counted) else {
            return Ok(None);
        };
        let is_node_var = patterns
            .iter()
            .flat_map(|path| path.nodes.iter())
            .any(|node| node.variable.as_ref() == Some(var));
        if !is_node_var {
            return Ok(None);
        }
    }
    let conjuncts: Vec<Expr> = wheres.iter().flat_map(flatten_and_owned).collect();
    let stats = GraphStats::new(ctx.graph.base(), ctx.params);
    if let Some(contract) = matcher::compile(patterns, match_mode, row, &conjuncts) {
        if engages(&stats, &conjuncts, &contract, true) {
            #[cfg(test)]
            kernel_engaged();
            let residual = Residual {
                conjuncts: &conjuncts,
                scope: &match_scope(row, patterns),
            };
            let output = run(ctx, &contract, &residual, KernelMode::Exists { counted })?;
            return Ok(Some(match output {
                KernelOutput::Exists(hit) => hit,
                KernelOutput::Rows(_) => {
                    unreachable!("the existence entry never asks for rows")
                }
            }));
        }
    }
    // The frontier family's arm (ADR-0478 Decision 2's first shape): the
    // quantified patterns the structural matcher above declines, as a
    // witness existence answer over the BFS. The nested-evaluation
    // budget tick stays the caller's (`patterns_exist`), after this
    // entry answers — one tick per engaged evaluation on either arm,
    // never on decline.
    if let Some(hit) = frontier_run::try_frontier_exists(
        ctx, patterns, row, match_mode, &conjuncts, counted, &stats,
    )? {
        #[cfg(test)]
        kernel_engaged();
        return Ok(Some(hit));
    }
    Ok(None)
}

/// Run an engaged contract: the scan segment(s), the walk-start fact, and
/// the family's kernel.
fn run(
    ctx: ExecCtx<'_>,
    contract: &Contract<'_>,
    residual: &Residual<'_>,
    mode: KernelMode<'_>,
) -> Result<KernelOutput> {
    match contract {
        Contract::Chain(chain_contract) => {
            // The walk starts at the pattern's first element, forward —
            // the recorded default (the kernel ignores the start ladder;
            // the cost gate is what decided the plan).
            record_walk_start(WalkStart::seed_side_forward());
            let starts = scan_segment(ctx, &chain_contract.start, residual)?;
            chain::run(ctx, chain_contract, starts, mode)
        }
        Contract::HubStar(star) => {
            // The walk-start fact: the hub's element index in pattern 0
            // (the last node when the hub sits at the far end), and
            // whether the side walks run against the written direction.
            record_walk_start(WalkStart {
                element: if star.hub_at_start[0] {
                    0
                } else {
                    star.sides[0].len()
                },
                backward: !star.hub_at_start[0],
                reason: StartReason::IndexZero,
            });
            // The hub candidates: the funnel over pattern 0's hub pattern
            // (its labels drive the index), then both patterns' hub
            // admissions — the walker binds the hub once and reconciles,
            // so both label conjunctions and the hub's one compiled
            // conjunct apply.
            let hubs = scan_segment(ctx, &star.hub[0], residual)?;
            let hubs = if hubs.is_empty() {
                hubs
            } else {
                scan_admit(ctx, &star.hub[1], hubs)?
            };
            hub_star::run(ctx, star, hubs, mode)
        }
    }
}

/// The scan segment: candidates from the one funnel — index seeks, the
/// PROFILE node-read counter, and the seek-phase stamp all keep working —
/// admitted by the target's monomorphic program.
fn scan_segment(
    ctx: ExecCtx<'_>,
    target: &NodeTarget<'_>,
    residual: &Residual<'_>,
) -> Result<Vec<NodeId>> {
    let candidates = node_candidates(
        ctx.graph,
        target.pattern,
        &Row::new(),
        ctx.params,
        residual.conjuncts,
    )?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    scan_admit(ctx, target, candidates)
}

/// Admit a non-empty candidate list through one target's program.
fn scan_admit(
    ctx: ExecCtx<'_>,
    target: &NodeTarget<'_>,
    candidates: Vec<NodeId>,
) -> Result<Vec<NodeId>> {
    admit_each(ctx, Admission::of(target), candidates)
}

/// The monomorphic admission loop over a candidate list — the scan
/// segment's filter half.
fn admit_each<P: NodePred>(
    ctx: ExecCtx<'_>,
    pred: P,
    candidates: Vec<NodeId>,
) -> Result<Vec<NodeId>> {
    let mut prepared: Option<P::Prepared> = None;
    let mut out = Vec::with_capacity(candidates.len());
    for id in candidates {
        ctx.graph.cancel_tick()?;
        if pred.admits(&mut prepared, ctx, id)? {
            out.push(id);
        }
    }
    Ok(out)
}

/// The chain kernel's stage ceiling: the frontier a stage may be
/// estimated to reach before the clause keeps the streaming walker (the
/// walker materializes full rows per pattern, so the ceiling guards the
/// kernel's buffers, not a behavior the walker lacks).
const MAX_ESTIMATED_STAGE_ROWS: u128 = 1 << 20;

/// The existence-mode early-exit allowance: hub-driven existence stops at
/// the first hub whose sides combine, so its work is bounded above by the
/// full estimate and expected far below it — the slop is the factor the
/// bounded-work comparison may be worse than the terminal-driven
/// alternative while the early exit still makes it the right plan.
const EXISTS_EARLY_EXIT_SLOP: u128 = 2;

/// The chain's stage ceiling over the `priced` hops: a count-only run
/// never lands the last stage, so it prices one hop fewer.
fn chain_stages_fit(
    stats: &GraphStats<'_>,
    conjuncts: &[Expr],
    chain: &matcher::Chain<'_>,
    priced: &[matcher::Hop<'_>],
) -> bool {
    let unbound = |_: &str| false;
    let mut rows = seed_rows(endpoint_estimate(
        chain.start.pattern,
        &unbound,
        conjuncts.iter(),
        stats,
    ));
    for hop in priced {
        rows = hop.scaled_from_seed(rows, stats);
        if rows > MAX_ESTIMATED_STAGE_ROWS {
            return false;
        }
    }
    true
}

/// The cost gate (ADR-0473): engage only when the kernel's plan is
/// estimated to pay. All estimates come from the planner's own machinery
/// (ADR-0218's endpoint ladder and degree tails over the live
/// [`GraphStats`]) — the same numbers the compound start selector reads,
/// never a second statistics implementation.
///
/// - The **chain** engages when every stage's estimated width stays under
///   the ceiling — the affordability guard; below it the kernel is the
///   same walk as the planner-oriented one with strictly less per-row
///   machinery, so there is no crossover to gate (the walker's cost is
///   the kernel's plus the row overhead).
/// - The **hub-star** engages when the hub-driven plan's estimated work
///   is no worse than the terminal-driven alternative's — the plan the
///   per-row walker runs (its cheapest-start ladder starts at whichever
///   end estimates cheaper, so a shape whose terminals are the cheaper
///   plan keeps the walker). Existence mode gets the early-exit slop.
/// - The **frontier family** (the quantified-path shapes, ADR-0478) is
///   cost-gated by its own arm — `frontier_match::frontier_engages`
///   inside `frontier_match::compile_frontier`, over this same estimate
///   machinery —
///   because its contract is not run by the chain/hub-star kernels: it
///   engages on an unbounded quantifier or on estimated paths reaching
///   4x the estimated distinct endpoints, and declines keep the GPM DFS.
fn engages(
    stats: &GraphStats<'_>,
    conjuncts: &[Expr],
    contract: &Contract<'_>,
    exists: bool,
) -> bool {
    let unbound = |_: &str| false;
    match contract {
        Contract::Chain(chain) => chain_stages_fit(stats, conjuncts, chain, &chain.hops),
        Contract::HubStar(star) => {
            // Hub-driven work: the hub candidates plus both sides walked
            // hub → terminal.
            let hub_rows = seed_rows(endpoint_estimate(
                star.hub[0].pattern,
                &unbound,
                conjuncts.iter(),
                stats,
            ));
            let mut hub_work = hub_rows;
            for side in &star.sides {
                let mut rows = hub_rows;
                for hop in side {
                    rows = hop.scaled_from_seed(rows, stats);
                    hub_work = hub_work.saturating_add(rows);
                }
            }
            // Terminal-driven work: each side enumerated from its
            // terminal and walked terminal → hub — the alternative plan,
            // and the per-row walker's lower bound.
            let mut term_work = 0u128;
            for (pi, side) in star.sides.iter().enumerate() {
                let terminal = side
                    .last()
                    .expect("every side has at least one hop")
                    .target
                    .pattern;
                let terminal_rows = seed_rows(endpoint_estimate(
                    terminal,
                    &unbound,
                    conjuncts.iter(),
                    stats,
                ));
                term_work = term_work.saturating_add(terminal_rows);
                let mut rows = terminal_rows;
                for hop in side.iter().rev() {
                    // The terminal-side walk rides the rel's other side:
                    // the hub-side flag flipped for a directed hop, and
                    // no side at all for an undirected one.
                    let on_start = seed_on_rel_start(hop.rel, star.hub_at_start[pi]);
                    rows = relate_planner::apply_degree(
                        rows,
                        hop.target.pattern,
                        hop.rel,
                        on_start,
                        stats,
                    );
                    term_work = term_work.saturating_add(rows);
                }
            }
            let allowance = if exists { EXISTS_EARLY_EXIT_SLOP } else { 1 };
            hub_work <= allowance.saturating_mul(term_work)
        }
    }
}

#[cfg(test)]
use std::cell::Cell;

use crate::ctx::KernelRung;

#[cfg(test)]
thread_local! {
    /// Test hook: force the decline, so a differential test can pin the
    /// per-row path's answer on a shape the kernel would take.
    static KERNELS_DISABLED: Cell<bool> = const { Cell::new(false) };
    /// Test counter: how many clauses the kernels took since the last take.
    static KERNEL_ENGAGED: Cell<u32> = const { Cell::new(0) };
    /// Test hook: stand the frontier family down (the GPM DFS), so a
    /// differential test can run the very same statement both ways —
    /// the frontier's decline path is the per-row DFS it replaces, and
    /// the other kernels stay engaged (the family-scoped sibling of
    /// `KERNELS_DISABLED`).
    static FRONTIER_DISABLED: Cell<bool> = const { Cell::new(false) };
    /// Test hook: shrink the frontier family's per-run
    /// table-allocation ceiling, so a decline test pins the
    /// ceiling's run-time fall-through on a small graph instead of
    /// building a run whose tables cross the production constant.
    static FRONTIER_TABLE_BUDGET_OVERRIDE: Cell<Option<u64>> = const { Cell::new(None) };
    /// Test hook: shrink the capped emission's deferred-pass rule work
    /// budget, so a cut test forces the pass's own exhaustion on a
    /// small graph instead of draining the production constant.
    static REARRIVAL_DEFERRED_BUDGET_OVERRIDE: Cell<Option<u64>> = const { Cell::new(None) };
}

/// Whether the test hook forced the per-row path.
#[cfg(test)]
fn kernels_disabled() -> bool {
    KERNELS_DISABLED.with(Cell::get)
}

/// Whether the test hook stood the frontier family down.
#[cfg(test)]
fn frontier_disabled() -> bool {
    FRONTIER_DISABLED.with(Cell::get)
}

/// Whether the chain/hub-star kernels stand down for clauses of this
/// store: the diagnostic switch's `walk` value (ADR-0519 — the
/// env-reachable form of the `KERNELS_DISABLED` differential hook, read
/// from the store's service bundle) or, in test builds, the hook itself.
pub(crate) fn kernels_stood_down(graph: &Graph) -> bool {
    #[cfg(test)]
    let hooked = kernels_disabled();
    #[cfg(not(test))]
    let hooked = false;
    graph.pattern_engine() == PatternEngine::Walk || hooked
}

/// Whether the frontier family stands down for clauses of this store:
/// the diagnostic switch's `walk` value (ADR-0519) or, in test builds,
/// either differential hook — the family-scoped sibling of
/// [`kernels_stood_down`], derived from it (one spelling of the walk
/// check) plus the frontier-scoped hook.
pub(crate) fn frontier_stood_down(graph: &Graph) -> bool {
    #[cfg(test)]
    return kernels_stood_down(graph) || frontier_disabled();
    #[cfg(not(test))]
    kernels_stood_down(graph)
}

/// The test hook's ceiling, when set — the frontier family's
/// table-allocation check reads it in place of the production constant
/// (`frontier_tables::within_table_budget`).
#[cfg(test)]
fn frontier_table_budget_override() -> Option<u64> {
    FRONTIER_TABLE_BUDGET_OVERRIDE.with(Cell::get)
}

/// Test helper: run `f` with the frontier family's table-allocation
/// ceiling held at `bytes`, restoring the production constant
/// afterwards — including on unwind, the shared guards' own spelling.
#[cfg(test)]
pub(crate) fn with_frontier_table_budget<R>(bytes: u64, f: impl FnOnce() -> R) -> R {
    FRONTIER_TABLE_BUDGET_OVERRIDE.with(|cell| cell.set(Some(bytes)));
    /// Restores the ceiling on the way out — including on unwind.
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            FRONTIER_TABLE_BUDGET_OVERRIDE.with(|cell| cell.set(None));
        }
    }
    let _restore = Restore;
    f()
}

/// The test hook's deferred-pass re-arrival budget, when set — the
/// capped emission's deferred pass sizes its own rule instance's work
/// budget with it in place of the production constant
/// (`frontier_emit::Emission::finish`'s `RearivalRule`), so a test can
/// force the pass's cut without touching the runner's instance.
#[cfg(test)]
fn rearival_deferred_budget_override() -> Option<u64> {
    REARRIVAL_DEFERRED_BUDGET_OVERRIDE.with(Cell::get)
}

/// Test helper: run `f` with the capped emission's deferred-pass
/// re-arrival work budget held at `work`, restoring the production
/// constant afterwards — including on unwind, the shared guards' own
/// spelling.
#[cfg(test)]
pub(crate) fn with_rearival_deferred_budget<R>(work: u64, f: impl FnOnce() -> R) -> R {
    REARRIVAL_DEFERRED_BUDGET_OVERRIDE.with(|cell| cell.set(Some(work)));
    /// Restores the budget on the way out — including on unwind.
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            REARRIVAL_DEFERRED_BUDGET_OVERRIDE.with(|cell| cell.set(None));
        }
    }
    let _restore = Restore;
    f()
}

/// Count one engaged kernel — the differential suites' test-only counter.
/// The production rung is the ladder's own return value (LABS-1117), so
/// this counter never feeds PROFILE: a nested evaluation's engagement
/// cannot be attributed to the enclosing clause.
#[cfg(test)]
fn kernel_engaged() {
    KERNEL_ENGAGED.with(|c| c.set(c.get() + 1));
}

/// Test helper: run `f` with the kernels forced off (the per-row path),
/// restoring the flag afterwards — including on unwind, so a panicking
/// assertion inside the closure cannot leave the kernels disabled for
/// every later test on the thread (the shared guard).
#[cfg(test)]
pub(crate) fn with_kernels_disabled<R>(f: impl FnOnce() -> R) -> R {
    super::test_support::with_flag_disabled(&KERNELS_DISABLED, f)
}

/// Test helper: run `f` with the frontier family stood down (the GPM
/// DFS), restoring the flag afterwards — including on unwind, the
/// shared guard's own spelling. The differential suites (the rider
/// tests here, the feature's corpus differential) A/B every
/// frontier-eligible shape through it.
#[cfg(test)]
pub(crate) fn with_frontier_disabled<R>(f: impl FnOnce() -> R) -> R {
    super::test_support::with_flag_disabled(&FRONTIER_DISABLED, f)
}

/// Test helper: drain the engagement counter (0 = the per-row path took
/// every clause since the last take).
#[cfg(test)]
pub(crate) fn take_kernel_engagements() -> u32 {
    KERNEL_ENGAGED.with(|c| c.replace(0))
}
