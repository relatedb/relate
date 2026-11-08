//! The frontier family's rider wiring (ADR-0478 §2, §6): the executor
//! plug-in that turns a compiled [`FrontierContract`] into answers. Two
//! entry points, the two places a statement's answer shape is visible:
//!
//! - [`try_frontier_rows`] — the row rung on the `MATCH` operator ladder
//!   (`match_clause::run_match_operator_ladder`), where the statement's
//!   suffix clauses — the answer-shape gate's input, read by
//!   [`decode_consumer`] — are in scope. Serves the capped consumer
//!   (`RETURN p LIMIT n`, the emission's `capped_rows` with its
//!   per-level early halt) and the distinct-endpoint consumer (one row
//!   per accepting arrival over the boundary variables).
//! - [`try_frontier_exists`] — the EXISTS arm inside
//!   `kernels::try_exists_kernel`, beside the chain/hub-star kernels'
//!   arm: a witness existence answer, one BFS per seed until a hit.
//!
//! Both decline fail-through: every gate is a proof (the structural and
//! answer-shape proofs inside `compile_frontier`), and a declined clause
//! runs the unchanged GPM per-row DFS — same rows, same errors, so the
//! TCK is the net either way. The seeds come from the kernel layer's own
//! scan segment (index seeks and the PROFILE node-read counter keep
//! working), one pinned seed when the entering row binds exactly the
//! seed-side variable (the `CarryingRow` proof's one legal carry).
//!
//! The two RUN-TIME declines ([`crate::ctx::FrontierRunDecline`]:
//! ADR-0516's `RearrivalBudget`, and the sparse tables' allocation
//! ceiling, ADR-0521's `TableBudget`): a run whose re-arrival rule cut a
//! search on its shared work budget carries an unproven endpoint set,
//! and a run whose tables crossed the ceiling an unfinished one — both
//! arms discard every row buffered so far and fall through with the
//! ladder's ordinary decline, so no partial answer ever escapes.

use std::collections::HashSet;

use relate_core::{NodeId, Result, Value};
use relate_ir::{Clause, Expr, MatchMode, PathPattern};

use crate::ctx::{ExecCtx, FrontierCounters, WalkStart};
use crate::eval::{eval, expr_vars};
use crate::graph::Graph;
use crate::row::Row;

use super::super::graph_stats::GraphStats;
use super::super::pattern_policies::{reduce_row, MatchReduce};
use super::super::project::SinkProjection;
use super::super::pushdown::{flatten_and_owned, match_scope, Residual};
use super::super::{record_frontier_decline, record_walk_start};
use super::frontier::{self, FrontierRun, RunOutcome, WalkBudget};
use super::frontier_emit::{capped_rows, CappedRun};
use super::frontier_match::{
    boundary, boundary_vars, compile_frontier, decode_consumer, Consumer, FrontierContract,
};
use super::frontier_tables::{Endpoint, FrontierTables};
use super::nfa;
use super::program::{Admission, NodePred};
use super::rearival::RuleBuffers;

/// The row rung: one `MATCH` clause's patterns against the statement's
/// suffix, engaged when the answer-shape gate proves a pair-shaped
/// consumer and the structural/cost gates prove the pattern. `Ok(None)`
/// is the decline — the ladder falls through to the batch chain, the
/// chain/hub-star kernels, and the per-row walker unchanged; the cap and
/// sink projection the clause carried ride the walker untouched (a
/// clause this rung answers has already applied the projection through
/// the shared `reduce_row` tail, and its cap's `want` is the emission's
/// own bound).
#[allow(clippy::too_many_arguments)]
pub(crate) fn try_frontier_rows(
    ctx: ExecCtx<'_>,
    rows: &[Row],
    optional: bool,
    match_mode: MatchMode,
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    suffix: &[Clause],
    projection: Option<&SinkProjection<'_>>,
) -> Result<Option<Vec<Row>>> {
    // The walker's admission shapes the specialized operators share: not
    // `OPTIONAL` (the null-binding arm is the walker's), one entering row
    // (a wider driving table is per-row semantics; a row binding exactly
    // the seed-side variable is the one legal carry, below).
    if optional || rows.len() != 1 {
        return Ok(None);
    }
    let Some(pattern) = single_quantified(ctx.graph.base(), patterns) else {
        return Ok(None);
    };
    let conjuncts = where_clause.map(flatten_and_owned).unwrap_or_default();
    // The answer-shape gate (ADR-0478 Decision 2): the suffix's consumer
    // as a proof. Every decline keeps the DFS. The decoder answers
    // exactly the two row-shaped consumers; the Exists consumer is the
    // exists entry's own construction, never this rung's (a hand-built
    // IR reaching here keeps the walker).
    let consumer = match decode_consumer(suffix, pattern, ctx.graph, ctx.params, ctx.cypher_version)
    {
        Ok(Consumer::Exists | Consumer::WalkCount) | Err(_) => return Ok(None),
        Ok(consumer) => consumer,
    };
    let stats = GraphStats::new(ctx.graph.base(), ctx.params);
    let Some(Engaged {
        contract,
        mut tables,
        mut bufs,
        seeds,
    }) = engage(
        ctx, patterns, match_mode, &rows[0], &conjuncts, consumer, &stats,
    )?
    else {
        return Ok(None);
    };
    let reduce = projection.map(|projection| MatchReduce {
        projection: Some(projection),
        patterns,
        where_clause,
        params: ctx.params,
        cypher_version: ctx.cypher_version,
    });
    let mut out: Vec<Row> = Vec::new();
    match contract.consumer {
        Consumer::Capped { want } => {
            // The MatchCap rider's semantics (ADR-0475 over ADR-0478):
            // at most `want` accepted rows, the BFS halting at the
            // filling level (`capped_rows`' per-level halt probe) —
            // candidates never count toward nor block the cap, and a
            // filled cap is a contract-free prefix (Decision 4). Each
            // seed's run carries the still-remaining want as the
            // emission's own parameter — the contract itself is shared
            // by every seed, never copied, and so are the clause's
            // recycled tables, handed back per seed.
            for seed in seeds {
                if out.len() >= want {
                    break;
                }
                ctx.graph.cancel_tick()?;
                let answer = capped_rows(
                    ctx,
                    seed,
                    &contract,
                    &rows[0],
                    want - out.len(),
                    &mut tables,
                    &mut bufs,
                )?;
                // The run's facts, recorded either way — the paired
                // answer's own either-arm reader, the outcome reader's
                // spelling. The rows and the run are one fact below,
                // and a decline owns neither half: no row can ride out
                // of a run the rule never proved.
                record_run(answer.facts());
                match answer {
                    CappedRun::Served { matched, run } => {
                        for (row, _used) in matched {
                            if let Some(row) = reduce_row(ctx.graph, &row, reduce.as_ref())? {
                                out.push(row);
                            }
                        }
                        tables = run.reclaim();
                    }
                    CappedRun::Declined { reason, .. } => {
                        record_frontier_decline(reason);
                        return Ok(None);
                    }
                }
            }
        }
        Consumer::DistinctEndpoints => {
            // One row per accepting arrival over the boundary variables,
            // running to fixpoint or `max` — bounded by the pair set,
            // never the path count. The target's admission filters the
            // endpoints; the suffix's distinct/aggregate fold collapses
            // the path multiplicity the frontier never enumerated. The
            // clause's recycled tables pass seed to seed, reclaimed
            // after each run's consumers are done.
            let pool = contract.pool;
            for seed in seeds {
                ctx.graph.cancel_tick()?;
                let outcome = frontier::run(
                    ctx,
                    seed,
                    &contract.nfa,
                    WalkBudget::standard(),
                    pool,
                    &mut tables,
                    &mut bufs,
                )?;
                let Some(walked) = recorded_run(outcome) else {
                    return Ok(None);
                };
                let pass = SeedPass {
                    contract: &contract,
                    entering: &rows[0],
                    seed,
                    walked: &walked,
                    reduce: reduce.as_ref(),
                };
                endpoint_rows(ctx, Admission::of(&contract.target), &pass, &mut out)?;
                tables = walked.reclaim();
            }
        }
        // The consumer gate above already returned: the decoder answers
        // exactly the two row-shaped consumers, and the contract carries
        // the consumer it was compiled with.
        Consumer::Exists | Consumer::WalkCount => {
            unreachable!("the Exists and WalkCount declines above already returned")
        }
    }
    // The test counter's engagement tick (the production rung is the
    // ladder's own return value now, LABS-1117; the exists entry's own
    // arm counts at its caller, `kernels/mod.rs`).
    #[cfg(test)]
    super::kernel_engaged();
    Row::share_schemas(&mut out);
    Ok(Some(out))
}

/// The EXISTS arm (ADR-0478 Decision 2's first shape): a witness
/// existence answer — a pair reaching an accepting state is a witness,
/// the target's admission and the counted expression deciding which.
/// `Ok(None)` is the decline — the exists probe keeps its seek. The
/// nested-evaluation budget tick is the caller's (`patterns_exist`):
/// one tick per engaged evaluation, exactly like the chain/hub-star
/// arm, never on decline.
#[allow(clippy::too_many_arguments)]
pub(crate) fn try_frontier_exists(
    ctx: ExecCtx<'_>,
    patterns: &[PathPattern],
    row: &Row,
    match_mode: MatchMode,
    conjuncts: &[Expr],
    counted: Option<&Expr>,
    stats: &GraphStats<'_>,
) -> Result<Option<bool>> {
    let Some(pattern) = single_quantified(ctx.graph.base(), patterns) else {
        return Ok(None);
    };
    // The counted expression must read only the boundary variables: the
    // frontier binds the boundary pair (a full node value each), and a
    // counted name beyond them reads a binding only a witness carries —
    // the probe's row-shaped evaluation keeps that shape. A pattern
    // without node boundaries is the compile's own `NfaShape` decline.
    if let Some(counted) = counted {
        let Some((start, target)) = boundary(pattern) else {
            return Ok(None);
        };
        let mut vars = HashSet::new();
        expr_vars(counted, &mut vars);
        let boundary = boundary_vars(start, target);
        if !vars.iter().all(|name| {
            boundary
                .into_iter()
                .flatten()
                .any(|var| var == name.as_str())
        }) {
            return Ok(None);
        }
    }
    let Some(Engaged {
        contract,
        mut tables,
        mut bufs,
        seeds,
    }) = engage(
        ctx,
        patterns,
        match_mode,
        row,
        conjuncts,
        Consumer::Exists,
        stats,
    )?
    else {
        return Ok(None);
    };
    let pool = contract.pool;
    for seed in seeds {
        ctx.graph.cancel_tick()?;
        let outcome = frontier::run(
            ctx,
            seed,
            &contract.nfa,
            WalkBudget::standard(),
            pool,
            &mut tables,
            &mut bufs,
        )?;
        let Some(walked) = recorded_run(outcome) else {
            return Ok(None);
        };
        let hit = exists_hit(
            ctx,
            Admission::of(&contract.target),
            &contract,
            seed,
            &walked,
            counted,
        )?;
        if hit {
            return Ok(Some(true));
        }
        tables = walked.reclaim();
    }
    Ok(Some(false))
}

/// The cheapest structural gates both entries share, before any
/// answer-shape decode or matcher proof runs: the family not stood down
/// (the diagnostic switch's `walk` value or a test hook), one pattern
/// (`compile_frontier`'s `MultiplePatterns` decline, spelled where the
/// entries need the one pattern anyway), and exactly one top-level
/// quantified element — the compile's own count fact
/// (`nfa::exactly_one_quantified`), one shared spelling, O(len).
pub(super) fn single_quantified<'p>(
    graph: &Graph,
    patterns: &'p [PathPattern],
) -> Option<&'p PathPattern> {
    if super::frontier_stood_down(graph) {
        return None;
    }
    let [pattern] = patterns else {
        return None;
    };
    nfa::exactly_one_quantified(&pattern.elements).then_some(pattern)
}

/// One engaged clause's run state, the scaffold's yield: the compiled
/// contract, the clause's recycled sparse tables and rule buffers,
/// and the seed list — everything the entries' per-seed loops consume.
struct Engaged<'p> {
    contract: FrontierContract<'p>,
    tables: FrontierTables,
    bufs: RuleBuffers,
    seeds: Vec<NodeId>,
}

/// The engagement scaffold both entries share, past their consumer
/// gates: the matcher's proofs (`compile_frontier`), the clause's
/// sparse tables and buffers (empty until the first arrival — the
/// allocation ceiling is each run's level-boundary check, ADR-0521, not
/// an engagement gate), the seeds, and the walk-start fact — the
/// frontier always starts at the pattern's first element, the seed side,
/// forward, the scan segment's subject (inside an EXISTS probe, nested in
/// the enclosing clause's arm the way the chain/hub-star exists arm
/// records its own). `Ok(None)` is the decline, before any of it ran.
#[allow(clippy::too_many_arguments)]
fn engage<'p>(
    ctx: ExecCtx<'_>,
    patterns: &'p [PathPattern],
    match_mode: MatchMode,
    entering: &Row,
    conjuncts: &[Expr],
    consumer: Consumer,
    stats: &GraphStats<'_>,
) -> Result<Option<Engaged<'p>>> {
    let Ok(contract) = compile_frontier(
        ctx.graph.base(),
        patterns,
        match_mode,
        entering,
        conjuncts,
        consumer,
        stats,
    ) else {
        return Ok(None);
    };
    // The clause's one sparse table set (ADR-0478 Decision 6's
    // allocation half, ADR-0521): nothing allocated until the first
    // arrival, and the allocation ceiling checked by each run at its
    // level boundaries over the exact bytes held — a crossing is the
    // run-time decline (`FrontierRunDecline::TableBudget`), never a
    // span estimate.
    let tables = FrontierTables::new(contract.nfa.states as usize);
    let bufs = RuleBuffers::new();
    let residual = Residual {
        conjuncts,
        scope: &match_scope(entering, patterns),
    };
    let seeds = seeds(ctx, &contract, entering, &residual)?;
    record_walk_start(WalkStart::seed_side_forward());
    Ok(Some(Engaged {
        contract,
        tables,
        bufs,
        seeds,
    }))
}

/// One run's outcome through the rider's two facts, the three consumer
/// arms' shared reader: the counters are recorded either way (a
/// declined run still walked — PROFILE's `[frontier]` counters never
/// change), and the served run is yielded only when every endpoint is
/// proven. `None` is a run-time decline: the rung engaged and ran, but
/// the run's re-arrival rule cut a search on its shared work budget
/// (ADR-0516) or its tables crossed the allocation ceiling (ADR-0521) —
/// the endpoint set is unproven or unfinished, so the caller discards
/// every buffered row of the clause and falls through with the
/// ladder's ordinary decline, the next rung serving the clause
/// unchanged (the DFS answers these shapes exactly). The decline's
/// typed reason rides the counters channel's decline slot
/// (`record_frontier_decline`, the walk-start slot's pattern), the one
/// recording point shared with the capped emission's paired decline —
/// the PROFILE record's fact, never a second behavior beside the
/// fall-through.
fn recorded_run(outcome: RunOutcome) -> Option<FrontierRun> {
    record_run(outcome.facts());
    match outcome {
        RunOutcome::Served(run) => Some(run),
        RunOutcome::Declined { reason, .. } => {
            record_frontier_decline(reason);
            None
        }
    }
}

/// The clause's seed nodes: the scan segment over the seed-side
/// admission, or the one pinned seed when the entering row binds the
/// seed-side variable to a node (the `CarryingRow` proof's single legal
/// carry — the walker's prebound reconciliation, which the admission
/// re-checks exactly as `gpm_start_nodes` does).
pub(super) fn seeds(
    ctx: ExecCtx<'_>,
    contract: &FrontierContract<'_>,
    entering: &Row,
    residual: &Residual<'_>,
) -> Result<Vec<NodeId>> {
    let pinned = contract
        .start
        .pattern
        .variable
        .as_ref()
        .and_then(|name| entering.get_name(name));
    match pinned {
        Some(Value::Node(node)) => Ok(super::scan_admit(ctx, &contract.start, vec![node.id])?),
        // A null binding is the walker's unbound: the scan segment owns
        // the seed. Any other value fails the walker's prebound
        // reconciliation on every start (`gpm_bind_node`'s fallthrough
        // arm) — zero rows, the walker's own answer for this clause.
        Some(Value::Null) | None => super::scan_segment(ctx, &contract.start, residual),
        Some(_) => Ok(Vec::new()),
    }
}

/// One run's accounting: the frontier counters (the `[frontier]`
/// annotation's own trio) and the store-read mix — each edge visit is a
/// neighbor walk, the same read kind the chain kernel's traversals
/// tick, batched once per run. The facts are the outcome's own
/// ([`RunOutcome::facts`]), either arm: a declined run still walked.
/// Shared with the walk-count arm (`frontier_count`), its other caller.
pub(super) fn record_run(facts: FrontierCounters) {
    super::super::count_neighbor_walks(facts.edge_visits);
    super::super::count_frontier(facts);
}

/// One seed's completed run plus the facts its endpoint passes read: the
/// compiled contract, the clause's entering row, the seed the run walked
/// from, the run's tables, and the clause's reduce tail. The two passes
/// (the distinct-endpoint row pass, the EXISTS hit pass) share it — one
/// spelling of the per-run bundle instead of five parallel parameters.
struct SeedPass<'a> {
    contract: &'a FrontierContract<'a>,
    entering: &'a Row,
    seed: NodeId,
    walked: &'a FrontierRun,
    reduce: Option<&'a MatchReduce<'a>>,
}

/// The run's endpoints through the target's admission, one loop both
/// passes share: `f` runs for every admitted endpoint until it answers
/// `true` (a hit), the admission prepared once per run — the
/// distinct-endpoint row pass and the EXISTS hit pass differ only in
/// what they do with an admitted endpoint.
fn for_each_admitted(
    ctx: ExecCtx<'_>,
    pred: Admission<'_>,
    endpoints: &[Endpoint],
    mut f: impl FnMut(ExecCtx<'_>, NodeId) -> Result<bool>,
) -> Result<bool> {
    let mut prepared: Option<<Admission<'_> as NodePred>::Prepared> = None;
    for endpoint in endpoints {
        ctx.graph.cancel_tick()?;
        if !pred.admits(&mut prepared, ctx, endpoint.node)? {
            continue;
        }
        if f(ctx, endpoint.node)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The distinct-endpoint consumer's row pass: every endpoint the run
/// admitted, through the target's admission, bound to its row (the
/// boundary pair's full node values — the values the projection's stub
/// refresh would produce) and the shared `reduce_row` tail — the same
/// tail the cap rider and the materializing walk run, so an emitted row
/// is the row the DFS's pipeline would keep.
fn endpoint_rows(
    ctx: ExecCtx<'_>,
    pred: Admission<'_>,
    pass: &SeedPass<'_>,
    out: &mut Vec<Row>,
) -> Result<()> {
    for_each_admitted(ctx, pred, &pass.walked.endpoints, |ctx, endpoint| {
        let mut row = pass.entering.clone();
        bind_boundary(&mut row, ctx, pass.contract, pass.seed, endpoint);
        if let Some(row) = reduce_row(ctx.graph, &row, pass.reduce)? {
            out.push(row);
        }
        Ok(false)
    })?;
    Ok(())
}

/// The EXISTS arm's endpoint pass: whether any admitted endpoint
/// witnesses existence — the counted expression (when the fold carried
/// one) non-null on the boundary pair's row, else any admitted endpoint.
fn exists_hit(
    ctx: ExecCtx<'_>,
    pred: Admission<'_>,
    contract: &FrontierContract<'_>,
    seed: NodeId,
    walked: &FrontierRun,
    counted: Option<&Expr>,
) -> Result<bool> {
    for_each_admitted(ctx, pred, &walked.endpoints, |ctx, endpoint| {
        let Some(counted) = counted else {
            return Ok(true);
        };
        let mut row = Row::new();
        bind_boundary(&mut row, ctx, contract, seed, endpoint);
        Ok(!eval(ctx.graph, &row, counted, ctx.params, ctx.cypher_version)?.is_null())
    })
}

/// Bind the boundary pair onto `row`: the seed-side and target-side
/// variables' full node values — the values the projection's stub
/// refresh produces, the emission's own binding (`frontier_emit`).
fn bind_boundary(
    row: &mut Row,
    ctx: ExecCtx<'_>,
    contract: &FrontierContract<'_>,
    seed: NodeId,
    endpoint: NodeId,
) {
    for (name, id) in [
        (contract.start.pattern.variable.as_ref(), seed),
        (contract.target.pattern.variable.as_ref(), endpoint),
    ] {
        if let Some(name) = name {
            let node = ctx
                .graph
                .node(id)
                .expect("a walked node is live under the view that walked it");
            row.insert(name.to_string(), Value::from_node(node));
        }
    }
}
