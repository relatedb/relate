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
//! The two RUN-TIME declines ([`crate::frontier_decline::FrontierRunDecline`]:
//! ADR-0516's `RearrivalBudget`, and the sparse tables' allocation
//! ceiling, ADR-0521's `TableBudget`): a run whose re-arrival rule cut a
//! search on its shared work budget carries an unproven endpoint set,
//! and a run whose tables crossed the ceiling an unfinished one — both
//! arms discard every row buffered so far and fall through with the
//! ladder's ordinary decline, so no partial answer ever escapes.

use std::collections::HashSet;

use relate_core::{NodeId, Result, Value};
use relate_ir::{Clause, Expr, MatchMode, PathPattern};

use crate::ctx::ExecCtx;
use crate::eval::{eval, expr_vars};
use crate::frontier_decline::{FrontierDeclineReason, FrontierRunDecline};
use crate::graph::Graph;
use crate::metrics::{FrontierCounters, WalkStart};
use crate::overlay::RetainedTotal;
use crate::row::Row;
use crate::workspace;

use super::super::graph_stats::GraphStats;
use super::super::pattern_policies::{reduce_row, MatchReduce, Matched, ReduceSink, RowFold};
use super::super::pushdown::{flatten_and_owned, match_scope, Residual};
use super::super::record_walk_start;
use super::super::sink::SinkProjection;
use super::frontier::{self, FrontierRun, RunOutcome, WalkBudget};
use super::frontier_emit::{capped_rows, CappedRun};
use super::frontier_match::{
    boundary, compile_frontier, decode_consumer, names_boundary_only, BoundaryPair, Consumer,
    FrontierContract,
};
use super::frontier_tables::{Endpoint, FrontierTables};
use super::memo::Memoized;
use super::nfa;
use super::program::{Admission, NodePred};
use super::rearival::RuleBuffers;

/// [`try_frontier_rows`]' verdict: the served rows, or the decline — with
/// the typed reason that made it, plan-time gate or run-time decline
/// (`None` for a clause the family never considered). The reason travels
/// through the ladder's return value, never a thread-local slot, so a
/// nested evaluation's ladder run (or EXISTS arm) cannot leak its decline
/// onto the enclosing clause's record.
pub(crate) enum FrontierRows {
    Served(Vec<Row>),
    Declined(Option<FrontierDeclineReason>),
}

#[cfg(test)]
impl FrontierRows {
    /// The served rows, `None` on any decline — the tests that call the
    /// rung bare and ask only whether it served.
    pub(crate) fn served(self) -> Option<Vec<Row>> {
        match self {
            FrontierRows::Served(rows) => Some(rows),
            FrontierRows::Declined(_) => None,
        }
    }
}

/// The row rung: one `MATCH` clause's patterns against the statement's
/// suffix, engaged when the answer-shape gate proves a pair-shaped
/// consumer and the structural/cost gates prove the pattern.
/// [`FrontierRows::Declined`] is the decline — the ladder falls through to the batch chain, the
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
    fold: Option<&dyn RowFold>,
) -> Result<FrontierRows> {
    // The walker's admission shapes the specialized operators share: not
    // `OPTIONAL` (the null-binding arm is the walker's). Every entering
    // row is served the way the walker serves it — the row's own
    // bindings, its own run — so a wider driving table no longer falls to
    // the trail-enumerating DFS; a row binding exactly the seed-side
    // variable is the one legal carry (`CarryingRow`, proved per row by
    // the compile below).
    if optional || rows.is_empty() {
        return Ok(FrontierRows::Declined(None));
    }
    let Some(pattern) = single_quantified(ctx.graph.base(), patterns) else {
        return Ok(FrontierRows::Declined(None));
    };
    let conjuncts = where_clause.map(flatten_and_owned).unwrap_or_default();
    // The answer-shape gate (ADR-0478 Decision 2): the suffix's consumer
    // as a proof. Every decline keeps the DFS. The decoder answers
    // exactly the two row-shaped consumers; the Exists and WalkCount
    // consumers are other entries' own constructions, never this rung's
    // (the consumer match below keeps the walker for either).
    // The plan-time decline rides the return value (LABS-1437), past the
    // single-quantified gate — a clause the frontier family never
    // considers (no quantified pattern, the stood-down switch) names
    // nothing.
    let consumer = match decode_consumer(suffix, pattern, ctx.graph, ctx.params) {
        Err(reason) => return Ok(FrontierRows::Declined(Some(reason.into()))),
        Ok(consumer) => consumer,
    };
    let stats = GraphStats::new(ctx.graph.base(), ctx.params);
    // The contract is the clause's: one compile serves every entering row
    // — the compile proper reads no row, and the row proofs it runs first
    // read each row once — so nothing here scales with the driving
    // table's width.
    let contract = match compile_frontier(
        ctx.graph.base(),
        patterns,
        match_mode,
        &conjuncts,
        rows,
        consumer,
        &stats,
    ) {
        Ok(contract) => contract,
        Err(reason) => return Ok(FrontierRows::Declined(Some(reason.into()))),
    };
    let reduce = (projection.is_some() || fold.is_some()).then_some(MatchReduce {
        sink: ReduceSink::select(projection, fold),
        patterns,
        where_clause,
        params: ctx.params,
    });
    let mut serving = Serving {
        run: ClauseRun::new(ctx, &contract, &conjuncts, reduce.as_ref()),
        // ADR-0463 (LABS-781, OPLANE_REQ-00345609 item 6): the rung's
        // output container is a retaining container — each emitted row
        // charges the expression budget through the one `add_row`
        // spelling as it lands, ahead of the landing, at the rung's two
        // emission points (the capped consumer's served loop and
        // the distinct-endpoint shapes' `emit_endpoint_row`), so a
        // kernel-served MATCH refuses at ~the cap
        // while the rows land, never after the whole container is built.
        // The operators that still hand the dispatch layer a finished
        // container (the batch chain, the contracted chain/hub-star
        // kernels) keep their charge there.
        out: Vec::new(),
        out_retained: ctx.graph.retain_bytes(0)?,
    };
    // The clause's walk start, recorded once per engagement: the frontier
    // always starts at the pattern's first element, the seed side, forward,
    // the scan segment's subject. The record keeps the clause's first
    // start, so one call is what a per-row call produced.
    record_walk_start(WalkStart::seed_side_forward());
    // The consumer dispatch, once: the consumer is a fact of the contract,
    // never a per-row decision, and each serving shape owns its own row
    // loop — so no clause-level decline and no arm the decoder cannot
    // produce sits at row depth.
    let declined = match contract.consumer {
        Consumer::Capped { want } => serve_capped_rows(&mut serving, rows, want)?,
        Consumer::DistinctEndpoints { .. } => serve_distinct_endpoints(&mut serving, rows)?,
        // Not this rung's consumers (the decoder never yields them): the
        // clause was never the frontier family's, so it names no decline.
        Consumer::Exists | Consumer::WalkCount => return Ok(FrontierRows::Declined(None)),
    };
    if let Some(reason) = declined {
        return Ok(FrontierRows::Declined(Some(reason)));
    }
    // The test counter's engagement tick (the production rung is the
    // ladder's own return value now, LABS-1117; the exists entry's own
    // arm counts at its caller, `kernels/mod.rs`).
    #[cfg(test)]
    super::kernel_engaged();
    let mut out = serving.out;
    Row::share_schemas(&mut out);
    Ok(FrontierRows::Served(out))
}

/// The clause's run scaffold, the one owner of everything a run of the
/// clause needs and nothing an entry adds: the execution context, the
/// compiled contract, the clause's flattened conjuncts and reduce tail,
/// and its one sparse table set and rule buffers (ADR-0478 Decision 6's
/// allocation half, ADR-0521 — nothing allocated until the first arrival,
/// and the allocation ceiling is each run's own level-boundary check over
/// the bytes held, never an engagement gate; a run clears them per seed,
/// `FrontierTables::begin_seed`). Both entries build exactly this and
/// neither re-spells it; the row rung wraps it in [`Serving`] for the
/// output container, the EXISTS probe uses it bare.
struct ClauseRun<'a, 'g> {
    ctx: ExecCtx<'g>,
    contract: &'a FrontierContract<'a>,
    conjuncts: &'a [Expr],
    reduce: Option<&'a MatchReduce<'a>>,
    tables: FrontierTables,
    bufs: RuleBuffers,
}

impl<'a, 'g> ClauseRun<'a, 'g> {
    /// The clause's scaffold for one engagement: the clause's facts and
    /// its one table set and buffer set, sized for the contract's
    /// automaton. A pure construction — the walk-start record is each
    /// entry's own call, so a run built for state alone does not silently
    /// write PROFILE state.
    fn new(
        ctx: ExecCtx<'g>,
        contract: &'a FrontierContract<'a>,
        conjuncts: &'a [Expr],
        reduce: Option<&'a MatchReduce<'a>>,
    ) -> Self {
        Self {
            ctx,
            contract,
            conjuncts,
            reduce,
            tables: FrontierTables::new(contract.nfa.states as usize),
            bufs: RuleBuffers::new(),
        }
    }

    /// Whether the clause's reduce tail is a fold. A fold takes rows
    /// irrevocably, so the distinct-endpoint shape parks its admitted
    /// pairs and replays them once the whole clause is proven. The sink
    /// is the one spelling of that policy — never a second `fold.is_some()`
    /// beside the `MatchReduce` built from the same fold.
    fn defers_fold(&self) -> bool {
        self.reduce.is_some_and(|reduce| reduce.sink.irrevocable())
    }

    /// One entering row's seed list: the clause's pattern and the clause's
    /// flattened conjuncts resolved into the scan segment's residual scope
    /// — the one place the three facts (the contract's pattern, the
    /// conjuncts, the row) are paired, so the entries cannot spell the
    /// scope differently. The clause's sparse tables and buffers are the
    /// run's own; a run clears them per seed
    /// ([`FrontierTables::begin_seed`]), so one set serves every seed of
    /// every row, and nothing here scales with the driving table's width.
    fn row_seeds(&self, entering: &Row) -> Result<Vec<NodeId>> {
        // The clause's one pattern is the contract's: the scope reads it
        // from there rather than from a second path a reader must prove
        // agrees.
        let residual = Residual {
            conjuncts: self.conjuncts,
            scope: &match_scope(
                self.ctx.graph,
                entering,
                std::slice::from_ref(self.contract.pattern),
            ),
        };
        seeds(self.ctx, self.contract, entering, &residual)
    }
}

/// One seed's standard run over the clause's tables and buffers — the
/// cancellation tick, the walk under the clause's pool and the standard
/// budget, and the run's facts, either way. The two serving shapes and the
/// EXISTS probe share it; each caller keeps only its own decline mapping,
/// its pass and the run's reclaim.
fn seed_run(
    ctx: ExecCtx<'_>,
    contract: &FrontierContract<'_>,
    seed: NodeId,
    tables: &mut FrontierTables,
    bufs: &mut RuleBuffers,
) -> Result<std::result::Result<FrontierRun, FrontierRunDecline>> {
    ctx.graph.cancel_tick()?;
    let outcome = frontier::run(
        ctx,
        seed,
        &contract.nfa,
        WalkBudget::standard(),
        contract.pool,
        tables,
        bufs,
    )?;
    Ok(recorded_run(outcome))
}

/// The capped consumer's serving (the MatchCap rider's semantics, ADR-0475
/// over ADR-0478): one run per (row, seed) until the cap fills, the BFS
/// halting at the filling level (`capped_rows`' per-level halt probe) —
/// candidates never count toward nor block the cap, and a filled cap is a
/// contract-free prefix (Decision 4), so no later run, seed or row can
/// contribute a row it would keep. Each seed's run carries the still
/// remaining want as the emission's own parameter — the contract itself is
/// shared by every seed, never copied, and so are the clause's recycled
/// tables, handed back per seed. `Ok(Some(reason))` is the run-time
/// decline; `Ok(None)` is a served clause.
fn serve_capped_rows(
    serving: &mut Serving<'_, '_>,
    rows: &[Row],
    want: usize,
) -> Result<Option<FrontierDeclineReason>> {
    for entering in rows {
        // The same cap read at row depth: a filled cap is a contract-free
        // prefix, so the remaining rows are not even seeded.
        if remaining(want, serving.out.len()).is_none() {
            break;
        }
        let seeds = serving.run.row_seeds(entering)?;
        for seed in seeds {
            // The one cap read: the loop stop and the bound handed to the
            // emission are the same fact, never a `filled` test beside a
            // re-derived `want - out.len()`.
            let Some(left) = remaining(want, serving.out.len()) else {
                break;
            };
            let answer = capped_rows(
                serving.run.ctx,
                seed,
                serving.run.contract,
                entering,
                left,
                &mut serving.run.tables,
                &mut serving.run.bufs,
            )?;
            let (matched, run) = match recorded_capped_run(answer) {
                Ok(served) => served,
                Err(reason) => return Ok(Some(reason.into())),
            };
            for (row, _used) in matched {
                emit_reduced_row(
                    serving.run.ctx,
                    row,
                    serving.run.reduce,
                    &mut serving.out,
                    &mut serving.out_retained,
                )?;
            }
            serving.run.tables = run.reclaim();
        }
    }
    Ok(None)
}

/// The distinct-endpoint consumer's serving: one row per accepting arrival
/// over the boundary variables, running to fixpoint or `max` — bounded by
/// the pair set, never the path count. The target's admission filters the
/// endpoints; the suffix's distinct/aggregate fold collapses the path
/// multiplicity the frontier never enumerated. The admission is
/// stage-fixed — a function of the node alone — so its memo is built once
/// for the whole clause: a node reached from many seeds *and many rows* is
/// checked once (LABS-1478). `Ok(Some(reason))` is the run-time decline;
/// `Ok(None)` is a served clause.
///
/// The clause's fold policy is decided once here, at clause scope, and
/// hands off to the one shape that serves it: a fold takes rows
/// irrevocably, so its shape parks and replays, while a consumer that can
/// take rows back streams them as each run finishes. Neither shape carries
/// the other's state.
fn serve_distinct_endpoints(
    serving: &mut Serving<'_, '_>,
    rows: &[Row],
) -> Result<Option<FrontierDeclineReason>> {
    if serving.run.defers_fold() {
        serve_parked_pairs(serving, rows)
    } else {
        serve_endpoint_rows(serving, rows)
    }
}

/// The distinct-endpoint shape for a consumer that can take rows back:
/// every admitted endpoint of every run is emitted as that run finishes,
/// charged ahead of its landing.
fn serve_endpoint_rows(
    serving: &mut Serving<'_, '_>,
    rows: &[Row],
) -> Result<Option<FrontierDeclineReason>> {
    let Serving {
        run,
        out,
        out_retained,
    } = serving;
    let contract = run.contract;
    let reduce = run.reduce;
    for_each_admitted_endpoint(run, rows, |ctx, entering, seed, endpoint| {
        emit_endpoint_row(
            ctx,
            entering,
            contract,
            reduce,
            seed,
            endpoint,
            out,
            out_retained,
        )
    })
}

/// The distinct-endpoint shape under a fold: a fold takes rows
/// irrevocably and a later run can still decline, so every admitted pair
/// is parked — billed as it lands, to the clause's retaining container, so
/// the per-value cap refuses it — and replayed only once the whole clause
/// is proven. The pairs are output-cardinality memory; the total is
/// scoped to the clause and released with `parked`.
fn serve_parked_pairs(
    serving: &mut Serving<'_, '_>,
    rows: &[Row],
) -> Result<Option<FrontierDeclineReason>> {
    let Serving {
        run,
        out,
        out_retained,
    } = serving;
    // The parked pair carries the entering row itself: `rows` outlives the
    // loop, so a reference needs no index to re-resolve at the replay.
    let mut parked: Vec<(&Row, NodeId, NodeId)> = Vec::new();
    let mut parked_bytes = run.ctx.graph.retain_bytes(0)?;
    if let Some(reason) = for_each_admitted_endpoint(run, rows, |ctx, entering, seed, endpoint| {
        // The charge is the container's capacity growth — header and
        // reservation included — over the stride of the value that is
        // pushed.
        let pair = (entering, seed, endpoint);
        let growth = workspace::buffer_growth(
            parked.len(),
            parked.len() + 1,
            std::mem::size_of_val(&pair) as u64,
        );
        parked_bytes.add_bytes(ctx.graph, growth)?;
        parked.push(pair);
        Ok(())
    })? {
        return Ok(Some(reason));
    }
    // The parked pairs' emit, once every row's every seed has served.
    for (entering, seed, endpoint) in parked {
        run.ctx.graph.cancel_tick()?;
        emit_endpoint_row(
            run.ctx,
            entering,
            run.contract,
            run.reduce,
            seed,
            endpoint,
            out,
            out_retained,
        )?;
    }
    Ok(None)
}

/// How a clause's run loop ended, for the one shape whose body can stop it.
enum RunLoop {
    /// Every row's every seed ran.
    Exhausted,
    /// The body stopped the loop early — the EXISTS probe's hit.
    Stopped,
    /// A run declined at run time; the reason is the caller's to map.
    Declined(FrontierDeclineReason),
}

/// The two serving shapes' loop: their endpoint bodies consume a run and
/// never stop the walk, so the stop is not theirs to ask for — the type
/// says so, rather than handing them a state they cannot produce. The
/// loop itself and its stop contract are
/// [`for_each_admitted_endpoint_until`]'s.
fn for_each_admitted_endpoint<'r>(
    run: &mut ClauseRun<'_, '_>,
    rows: &'r [Row],
    mut f: impl FnMut(ExecCtx<'_>, &'r Row, NodeId, NodeId) -> Result<()>,
) -> Result<Option<FrontierDeclineReason>> {
    let outcome = for_each_admitted_endpoint_until(run, rows, |ctx, entering, seed, endpoint| {
        f(ctx, entering, seed, endpoint)?;
        Ok(false)
    })?;
    match outcome {
        RunLoop::Declined(reason) => Ok(Some(reason)),
        // The wrapper's body never stops; this arm is the enum's totality.
        RunLoop::Exhausted | RunLoop::Stopped => Ok(None),
    }
}

/// The clause's run loop, the one owner of the sequence every shape that
/// walks a clause repeats: the row loop, the row's seed list, the seed's
/// standard run with its decline mapping, the run's reclaim, and the
/// run's endpoints through the target's memoized admission and the
/// clause's boundary-pair comparison. `f` receives each admitted endpoint
/// — the entering row, the seed, the endpoint — and answers `false` to
/// keep going, `true` to stop early: the probe's hit, reported as
/// [`RunLoop::Stopped`] rather than collapsed into a walk that ran to the
/// end, so its caller can tell the two apart.
///
/// A run-time decline is [`RunLoop::Declined`], a value each caller maps
/// itself: the row rung names it on the PROFILE record, the nested probe
/// keeps its seek. This is the entry for the one caller whose body
/// decides whether to stop — the EXISTS probe; the two serving shapes
/// call [`for_each_admitted_endpoint`] instead.
fn for_each_admitted_endpoint_until<'r>(
    run: &mut ClauseRun<'_, '_>,
    rows: &'r [Row],
    mut f: impl FnMut(ExecCtx<'_>, &'r Row, NodeId, NodeId) -> Result<bool>,
) -> Result<RunLoop> {
    let pred = Memoized(Admission::of(&run.contract.target));
    let mut admitted = None;
    // The consumer's boundary pair is a fact of the clause's contract,
    // not of a seed: read once here, never re-derived in the hot loop.
    let pair = run.contract.consumer.pair();
    for entering in rows {
        for seed in run.row_seeds(entering)? {
            let walked =
                match seed_run(run.ctx, run.contract, seed, &mut run.tables, &mut run.bufs)? {
                    Ok(walked) => walked,
                    Err(reason) => return Ok(RunLoop::Declined(reason.into())),
                };
            let stopped = for_each_admitted(
                run.ctx,
                pred,
                &mut admitted,
                pair,
                seed,
                &walked.endpoints,
                |ctx, endpoint| f(ctx, entering, seed, endpoint),
            )?;
            run.tables = walked.reclaim();
            if stopped {
                return Ok(RunLoop::Stopped);
            }
        }
    }
    Ok(RunLoop::Exhausted)
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
        if !names_boundary_only(&vars, start, target) {
            return Ok(None);
        }
    }
    // The exists arm is a nested evaluation of the enclosing clause: its
    // plan-time decline names nothing on the PROFILE record (the clause's
    // own rung is the record's subject). The compile runs the row's own
    // proof first, then the clause's — the row entry's order, so the two
    // entries name the same decline for the same clause.
    let contract = match compile_frontier(
        ctx.graph.base(),
        patterns,
        match_mode,
        conjuncts,
        std::slice::from_ref(row),
        Consumer::Exists,
        stats,
    ) {
        Ok(contract) => contract,
        Err(_) => return Ok(None),
    };
    let mut run = ClauseRun::new(ctx, &contract, conjuncts, None);
    record_walk_start(WalkStart::seed_side_forward());
    // The probe's own endpoint body: any admitted endpoint witnesses
    // existence — the counted expression (when the fold carried one)
    // non-null on the boundary pair's row, else the endpoint itself.
    let outcome = for_each_admitted_endpoint_until(
        &mut run,
        std::slice::from_ref(row),
        |ctx, _entering, seed, endpoint| {
            let Some(counted) = counted else {
                return Ok(true);
            };
            let mut row = Row::new();
            bind_boundary(&mut row, ctx, &contract, seed, endpoint);
            Ok(!eval(ctx.graph, &row, counted, ctx.params)?.is_null())
        },
    )?;
    match outcome {
        RunLoop::Stopped => Ok(Some(true)),
        RunLoop::Exhausted => Ok(Some(false)),
        // The exists arm is a nested evaluation: a run-time decline keeps
        // the probe's seek and names nothing on the PROFILE record.
        RunLoop::Declined(_) => Ok(None),
    }
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

/// The row rung's run state: the clause's [`ClauseRun`] scaffold plus the
/// output container and its charge. Each serving shape takes `&mut` it and
/// adds only its own difference.
struct Serving<'a, 'g> {
    run: ClauseRun<'a, 'g>,
    out: Vec<Row>,
    out_retained: RetainedTotal,
}

/// One run's outcome through the rider's two facts — the reader the
/// distinct-endpoint arm and the EXISTS probe share (the capped arm has
/// its own, [`recorded_capped_run`], whose served half also carries the
/// witness rows): the counters are recorded either way (a
/// declined run still walked — PROFILE's `[frontier]` counters never
/// change), and the served run is yielded only when every endpoint is
/// proven. `Err` is a run-time decline: the rung engaged and ran, but
/// the run's re-arrival rule cut a search on its shared work budget
/// (ADR-0516) or its tables crossed the allocation ceiling (ADR-0521) —
/// the endpoint set is unproven or unfinished, so the caller discards
/// every buffered row of the clause and falls through with the
/// ladder's ordinary decline, the next rung serving the clause
/// unchanged (the DFS answers these shapes exactly). The decline's
/// typed reason is the `Err` payload, which the row entry returns to the
/// ladder for the PROFILE record — the fact, never a second behavior
/// beside the fall-through.
fn recorded_run(outcome: RunOutcome) -> std::result::Result<FrontierRun, FrontierRunDecline> {
    record_run(outcome.facts());
    match outcome {
        RunOutcome::Served(run) => Ok(run),
        RunOutcome::Declined { reason, .. } => Err(reason),
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

/// The run's endpoints through the target's admission and the clause's
/// boundary-pair comparison, one loop every pass shares: `f` runs for
/// every admitted endpoint that answers for `seed` until it answers
/// `true` (a hit), the admission prepared once per run — the
/// distinct-endpoint row pass, its parking arm and the EXISTS hit pass
/// differ only in what they do with an admitted endpoint, and none
/// spells the pair check beside this loop.
fn for_each_admitted<P: NodePred>(
    ctx: ExecCtx<'_>,
    pred: P,
    prepared: &mut Option<P::Prepared>,
    pair: BoundaryPair,
    seed: NodeId,
    endpoints: &[Endpoint],
    mut f: impl FnMut(ExecCtx<'_>, NodeId) -> Result<bool>,
) -> Result<bool> {
    for endpoint in endpoints {
        ctx.graph.cancel_tick()?;
        if !pred.admits(prepared, ctx, endpoint.node)? || !pair.admits(seed, endpoint.node) {
            continue;
        }
        if f(ctx, endpoint.node)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The rung's one row-emission spelling (LABS-1077): reduce the spent
/// source row, drop it, then charge the landed row ahead of the push.
/// The drop comes before the charge for the same reason the
/// projection's output container drops its spent source first
/// (`exec/project.rs`): `row_retained_heap` counts a payload only when
/// every hold is the charged row's own, so a source row still alive
/// lends the landed row its Arcs and the landing charges slot bytes
/// only — the path payload a capped `RETURN p` emission retains would
/// never count. Both the capped consumer's served loop and the
/// distinct-endpoint consumer's row pass land through this one helper,
/// so the drop-before-charge accounting and the charge-ahead-of-landing
/// order have one spelling.
fn emit_reduced_row(
    ctx: ExecCtx<'_>,
    source: Row,
    reduce: Option<&MatchReduce<'_>>,
    out: &mut Vec<Row>,
    out_retained: &mut RetainedTotal,
) -> Result<()> {
    let landed = reduce_row(ctx.graph, source, reduce)?;
    if let Some(row) = landed {
        out_retained.add_row(ctx.graph, row.values())?;
        out.push(row);
    }
    Ok(())
}

/// One admitted endpoint's row: the entering row's own bindings, the
/// boundary pair's full node values (the values the projection's stub
/// refresh would produce) and the shared `reduce_row` tail. The one
/// spelling both distinct-endpoint emissions land through — the per-seed
/// pass and the fold's deferred replay — so a change to the binding or
/// the emission order cannot reach one without the other.
#[allow(clippy::too_many_arguments)]
fn emit_endpoint_row(
    ctx: ExecCtx<'_>,
    entering: &Row,
    contract: &FrontierContract<'_>,
    reduce: Option<&MatchReduce<'_>>,
    seed: NodeId,
    endpoint: NodeId,
    out: &mut Vec<Row>,
    out_retained: &mut RetainedTotal,
) -> Result<()> {
    let mut row = entering.clone();
    bind_boundary(&mut row, ctx, contract, seed, endpoint);
    emit_reduced_row(ctx, row, reduce, out, out_retained)
}

/// The capped emission's one cap read: how many rows the run may still
/// land, or `None` once the cap is filled — a contract-free prefix, no
/// later run, seed or row can contribute a row it would keep. The loop
/// stop and the bound handed to `capped_rows` are this same read, never a
/// `filled` test beside a re-derived `want - emitted`.
///
/// Filled means `emitted >= want`, so an exactly-filled cap is `None`:
/// `want.checked_sub(emitted)` would answer `Some(0)` there and leave the
/// loop running a zero-bounded BFS per remaining seed and row.
fn remaining(want: usize, emitted: usize) -> Option<usize> {
    (emitted < want).then(|| want - emitted)
}

/// One capped run's outcome through the rider's two facts — the capped
/// counterpart of [`recorded_run`]: the counters are recorded either way
/// (a declined run still walked — PROFILE's `[frontier]` counters never
/// change), and the served run yields its witness rows together with the
/// run for the caller's recycle. `Err` is the run-time decline, the
/// caller's fall-through; the rows and the run are one fact, and a
/// decline owns neither half — no row can ride out of a run the rule
/// never proved.
fn recorded_capped_run(
    answer: CappedRun,
) -> std::result::Result<(Matched, FrontierRun), FrontierRunDecline> {
    record_run(answer.facts());
    match answer {
        CappedRun::Served { matched, run } => Ok((matched, run)),
        CappedRun::Declined { reason, .. } => Err(reason),
    }
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
