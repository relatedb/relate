//! `MATCH` clause driver: the operator ladder that picks how a clause
//! runs (batch chain, contracted kernel, or this module's row loop), the
//! row loop itself, the OPTIONAL-miss null binder, and the per-pattern
//! loop. The
//! predicate-filter policy the row loop consults lives one module down,
//! in `predicates`; the one engine fork each per-path dispatch routes
//! through lives in [`super::one_path`]; the LABS-348 hub join the
//! multi-pattern loop tries at its pattern-0→pattern-1 boundary lives
//! in [`super::hub_join`]. The classic walker's terminal named-path
//! binder ([`super::named_path::bind_named_path`]) lives beside its one
//! caller in `named_path`, so this driver's reads stay strictly downward.

use relate_core::Result;
use relate_ir::{Clause, Expr, MatchMode, PathPattern};

use crate::ctx::{ExecCtx, KernelRung};
use crate::eval::candidate_passes;
use crate::overlay::ExecGraph;
use crate::row::{Row, RowRung};
use crate::UsedSet;

use super::batch::try_scan_filter_batch;
use super::hub_join::try_hub_join;
use super::kernels;
use super::one_path::{match_one_path, PathFilters};
use super::pattern_policies::{reduce_row, MatchReduce, MatchTail, ReduceSink, RowFold};
use super::pushdown::{flatten_and_owned, match_scope, MatchCap, Residual};
use super::rider::Rider;
use super::sink::{finish_match_row, SinkProjection};

/// Which operator took one `MATCH` clause down the ladder — the one
/// operator-state vocabulary (LABS-1117). The two facts the dispatcher
/// records project from this single value: [`MatchOperator::BatchChain`]
/// is the ADR-0468 `batched` fact, and [`MatchOperator::Rung`] carries
/// the clause's `KernelRung` (the walker included). The ladder returns
/// it, so a nested evaluation cannot contribute or overwrite one.
#[derive(Clone, Copy)]
pub(crate) enum MatchOperator {
    /// ADR-0468 (LABS-359): the batch scan/filter/project chain over
    /// `Chunk`.
    BatchChain,
    /// The traversal rung that served the clause: a contracted kernel
    /// (ADR-0473), the frontier rung (ADR-0478), or the per-row walker
    /// (every decline's home).
    Rung(KernelRung),
}

impl MatchOperator {
    /// Whether the operator hands the dispatch layer a finished,
    /// **uncharged** container (ADR-0463, LABS-1077): the batch chain and
    /// the contracted chain/hub-star kernels build the whole container
    /// before returning, so the dispatcher charges its rows at the
    /// landing — while the walker (`match_clause.rs`) and the frontier
    /// rung (`frontier_run.rs`) charge every row at their own emission
    /// points and arrive charged. The one spelling of the
    /// charge-ownership fact, spelled exhaustively — a later rung fails
    /// compilation here until its charge owner is decided, never
    /// inheriting a silent default.
    pub(crate) fn hands_dispatch_a_finished_container(&self) -> bool {
        match self {
            MatchOperator::BatchChain
            | MatchOperator::Rung(KernelRung::Chain { .. })
            | MatchOperator::Rung(KernelRung::HubStar) => true,
            // The self-charging rungs: their emission points charge
            // every row before it lands, so their containers arrive
            // already charged.
            MatchOperator::Rung(KernelRung::FrontierNfa)
            | MatchOperator::Rung(KernelRung::Walker) => false,
        }
    }
}

/// Run one `MATCH` clause the folds declined down the operator ladder:
/// the frontier family (ADR-0478) first, then the batch scan/filter/
/// project chain (ADR-0468), then the contracted kernels (ADR-0473),
/// then the per-row walker below — the first operator that takes the
/// clause owns it, and every decline falls through unchanged to the
/// walker. The frontier rung sits first on its own gate: it serves the
/// quantified shapes every rung below structurally declines, and its
/// answer-shape proof reads the statement's suffix (`suffix`) — the
/// capped and distinct-endpoint consumers ride plain caps and sink
/// projections the shared gate below excludes, so it cannot live behind
/// that gate. The two rungs below share one admission gate, computed
/// once: no cap and no sink projection, because a cap's and a sink's
/// semantics live in the walker. The plain limit cap is the gate's one
/// exception: it yields to a specialized operator for exactly the shapes
/// that operator proves, while the distinct and ordered caps never
/// yield. The batch chain takes the cap's want as its halt (LABS-1452):
/// it stops scanning once that many kept rows exist, so a `LIMIT` over a
/// scan reads one batch, not the graph; the contracted kernel takes
/// it as its rows-mode halt (LABS-990, ADR-0524) — the want rides, the
/// kernel's final collection stops at it, and a shape the kernel
/// declines keeps the cap at the walker, whose sink applies it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_match_operator_ladder(
    ctx: ExecCtx<'_>,
    rows: Vec<Row>,
    optional: bool,
    match_mode: MatchMode,
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    cap: &mut Option<MatchCap<'_>>,
    projection: Option<&SinkProjection<'_>>,
    suffix: &[Clause],
    fold: Option<&dyn RowFold>,
) -> Result<(Vec<Row>, MatchOperator)> {
    // The frontier family's rung (ADR-0478): the quantified-path shapes
    // with a pair-shaped answer contract — the capped emission, the
    // distinct-endpoint answer — over the BFS that collapses the trail
    // enumeration. Every decline falls through to the ladder below with
    // the cap and projection untouched.
    if let Some(rows) = kernels::frontier_run::try_frontier_rows(
        ctx,
        &rows,
        optional,
        match_mode,
        patterns,
        where_clause,
        suffix,
        projection,
        fold,
    )? {
        return Ok((rows, MatchOperator::Rung(KernelRung::FrontierNfa)));
    }
    // The one cap classification both rungs below read: the plain cap's
    // want, `Some` only for a plain cap. The gate admits the clause on
    // it and the kernel rung passes it as the rows mode's halt — one
    // read, so the two ends cannot drift (the ordered and distinct caps
    // answer `None` and keep the walker with their semantics intact).
    let plain_want = cap.as_ref().and_then(MatchCap::plain_want);
    let chain_eligible = (cap.is_none() || plain_want.is_some()) && projection.is_none();
    let batched = if chain_eligible {
        try_scan_filter_batch(
            ctx,
            &rows,
            optional,
            match_mode,
            patterns,
            where_clause,
            plain_want,
        )?
    } else {
        None
    };
    // The kernel tries the same gate one rung lower — only once the
    // chain above it declined. A plain cap's want rides as the kernel's
    // rows-mode halt (LABS-990, ADR-0524).
    let kernel_eligible = chain_eligible && batched.is_none();
    let kernaled = if kernel_eligible {
        kernels::try_match_kernel(
            ctx,
            &rows,
            optional,
            match_mode,
            patterns,
            where_clause,
            plain_want,
        )?
    } else {
        None
    };
    // A rung that returns a finished container hands its rows to the fold
    // (when one rides) right here, so the clause still collects nothing.
    let finished = batched
        .map(|rows| (rows, MatchOperator::BatchChain))
        .or(kernaled.map(|(rows, rung)| (rows, MatchOperator::Rung(rung))));
    if let Some((rows, operator)) = finished {
        let Some(fold) = fold else {
            return Ok((rows, operator));
        };
        fold.offer_container(ctx.graph, rows)?;
        return Ok((Vec::new(), operator));
    }
    // Every rung above declined: the walker is this clause's rung, and
    // the ladder's return value carries it (LABS-1117).
    Ok((
        match_clause(
            ctx,
            rows,
            optional,
            match_mode,
            patterns,
            where_clause,
            cap.as_mut(),
            projection,
            fold,
        )?,
        MatchOperator::Rung(KernelRung::Walker),
    ))
}

/// The dispatch layer's finished-container charge (ADR-0463, LABS-781):
/// the one bulk spelling every site that lands a finished container reads
/// — the SEARCH arm (its walker runs are per-candidate, so the
/// clause-level container is only whole — and only chargeable — at this
/// landing, and that arm charges it itself), the dispatcher's ladder arm
/// for the operators whose containers arrive uncharged (the ownership
/// predicate's `true` arms; the walker and the frontier rung charge at
/// their own emission points and arrive charged), and the ladder itself
/// for a rung's container headed to a [`RowFold`] that asks for the charge.
/// The refusal fires at ~the cap with the crossing row's honest figure.
pub(crate) fn charge_finished_container(graph: &ExecGraph<'_>, rows: &[Row]) -> Result<()> {
    let mut retained = graph.retain_bytes(0)?;
    for row in rows {
        retained.add_row(graph, row.values())?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn match_clause(
    ctx: ExecCtx<'_>,
    rows: Vec<Row>,
    optional: bool,
    match_mode: MatchMode,
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    mut cap: Option<&mut MatchCap<'_>>,
    projection: Option<&SinkProjection<'_>>,
    fold: Option<&dyn RowFold>,
) -> Result<Vec<Row>> {
    let ExecCtx {
        graph,
        params,
        cypher_version,
        ..
    } = ctx;
    let mut out = Vec::new();
    // ADR-0463 (LABS-781, OPLANE_REQ-00345609): the walker's output
    // container is a retaining container — each row charges the
    // expression budget through the one `add_row` spelling as it
    // lands, ahead of the landing. The frontier row rung charges its
    // own emission points the same way (frontier_run.rs); the batch
    // chain and the contracted kernels hand the dispatch layer a
    // finished container, which charges there (see dispatch.rs).
    let mut out_retained = graph.retain_bytes(0)?;
    let conjuncts = where_clause.map(flatten_and_owned).unwrap_or_default();
    // The clause filter runs in the walk's sink — once per row, ahead
    // of any cap's count — when the sink projects, and when a cap rides
    // a `WHERE`-bearing walk (LABS-853). The placement is the cap
    // module's one policy (`sink_filters_clause`), not a local boolean:
    // the plain cap's count is exact only because the two ends agree.
    let sink_filters =
        super::match_cap::sink_filters_clause(projection, cap.as_deref(), where_clause);
    // A fold takes the rows after the same checks, so it rides the reduce
    // whether or not the sink projects.
    let reduce = (sink_filters || fold.is_some()).then_some(MatchReduce {
        sink: ReduceSink::select(projection, fold),
        patterns,
        where_clause,
        params,
        cypher_version,
    });
    for row in rows {
        graph.cancel_tick()?;
        if cap.as_ref().is_some_and(|c| c.full()) {
            break;
        }
        let scope = match_scope(&row, patterns);
        let residual = Residual {
            conjuncts: &conjuncts,
            scope: &scope,
        };
        let mut matched = match_paths(
            ctx,
            patterns,
            match_mode,
            row.clone(),
            &residual,
            cap.as_deref_mut(),
            reduce.as_ref(),
        )?;
        // A reduce already applied the inline predicates, the WHERE, and
        // any projection in the sink.
        if reduce.is_none() {
            let mut filtered = Vec::with_capacity(matched.len());
            for candidate in matched {
                if candidate_passes(
                    graph,
                    patterns,
                    where_clause,
                    &candidate,
                    params,
                    cypher_version,
                )? {
                    filtered.push(candidate);
                }
            }
            matched = filtered;
        }
        if matched.is_empty() && optional {
            let mut r = row;
            bind_pattern_nulls(&mut r, patterns);
            if let Some(projection) = projection {
                let projected = finish_match_row(graph, &r, projection, params, cypher_version)?;
                out_retained.add_row(graph, projected.values())?;
                out.push(projected);
            } else {
                out_retained.add_row(graph, r.values())?;
                out.push(r);
            }
        } else {
            for row in matched {
                out_retained.add_row(graph, row.values())?;
                out.push(row);
            }
        }
    }
    Row::share_schemas(&mut out);
    Ok(out)
}

pub(crate) fn bind_pattern_nulls(row: &mut Row, patterns: &[PathPattern]) {
    // The pattern-bound names, collected through the scope walk's
    // callback vocabulary (owned — the callback's reference is
    // higher-ranked, so it cannot escape into a borrowed list) into the
    // one list the rung build reads.
    let mut bound_names: Vec<String> = Vec::new();
    for pattern in patterns {
        super::scope::for_each_pattern_bound_name(pattern, &mut |name| {
            bound_names.push(name.to_string());
        });
    }
    // The null-binding rung: this row's schema plus every pattern-bound
    // name it does not already hold, in the canonical bound-name walk's
    // order — built through the one per-row rung constructor
    // ([`RowRung::past`], its own append accounting, never a hand-built
    // row-keys list), stamped with one null per appended name. A name
    // the row already binds keeps its value.
    let rung = RowRung::past(row, bound_names.iter().map(|name| Some(name.as_str())));
    row.stamp_nulls(&rung);
}

pub(crate) fn match_paths(
    ctx: ExecCtx<'_>,
    patterns: &[PathPattern],
    match_mode: MatchMode,
    row: Row,
    residual: &Residual<'_>,
    mut cap: Option<&mut MatchCap<'_>>,
    reduce: Option<&MatchReduce<'_>>,
) -> Result<Vec<Row>> {
    // LABS-348: the base row, kept for the pattern-0→pattern-1
    // boundary's hub-join attempt below — the join's shape gate reads
    // the incoming row's bindings and its side-B enumeration seeds from
    // it. The join is the two-pattern comma join and declines at its
    // first check unless `patterns.len() == 2`, so that one width gates
    // both this keep-alive clone and the boundary call: a single-pattern
    // match (the common shape) and a three-pattern clause pay no clone
    // for a join that cannot engage.
    let hub_join = patterns.len() == 2;
    let base = hub_join.then(|| row.clone());
    let mut current = vec![(row, UsedSet::default())];
    let account = relate_core::tokens::workspace_accounting();
    let mut batch = relate_core::tokens::WorkspaceCharge::retain_with(|| {
        crate::workspace::binding_bytes(&current)
    })?;
    let last = patterns.len().saturating_sub(1);
    for (pi, p) in patterns.iter().enumerate() {
        // LABS-348: at the pattern-0→pattern-1 boundary, try the hub
        // join — the two-pattern comma join through one shared hub
        // intersects at the hub instead of re-expanding pattern 1 for
        // every pattern-0 row (the nested loop re-applies the side
        // filter per row: the measured adversarial shape paid 25M store
        // touches for 250k result rows). `current` here is this loop's
        // own pattern-0 output — exactly the plain-side rows the join's
        // former private enumeration produced (same base row, same
        // fresh used-set, same residual, no cap or reduce riding a
        // non-last pattern, the set carried) — so a decline falls
        // through to the nested loop below already holding them: the
        // old Seeded/DeclinedWithoutSeed handshake is true by
        // construction. `base` is Some exactly when `hub_join` holds
        // (the guard above), the same fact the join's own first check
        // reads.
        if let (1, Some(base)) = (pi, base.as_ref()) {
            if let Some(rows) = try_hub_join(
                ctx,
                patterns,
                match_mode,
                base,
                &current,
                residual,
                // A fold-only reduce rides the join: its rows take the same
                // reduce below that the nested loop's rows take in the sink.
                cap.is_none() && reduce.is_none_or(|r| matches!(r.sink, ReduceSink::Fold(_))),
            )? {
                let mut out = Vec::with_capacity(rows.len());
                for row in rows {
                    out.extend(reduce_row(ctx.graph, row, reduce)?);
                }
                return Ok(out);
            }
        }
        let mut next = Vec::new();
        let mut next_charge = relate_core::tokens::WorkspaceCharge::empty();
        let mut next_meter = crate::workspace::BindingMeter::default();
        // Draining `current` keeps its allocation until the loop ends.
        // `batch` stays reserved across that overlap and is released when
        // `next_charge` replaces it, after `current` has been dropped.
        let last_pat = pi == last;
        // The tail is pattern-level, not row-level: decided once per
        // pattern, here where its values are decided (F-1-11) — the last
        // pattern finishes its rows (the reduce when the clause sink
        // projects here, the used-rel carry dropped), every earlier
        // pattern carries its used-set forward.
        let tail = if last_pat {
            MatchTail::Finish { reduce }
        } else {
            MatchTail::Carry
        };
        // The shortest-path function's used-set restore is pattern-level,
        // like the tail (F-1-11): only patterns before the last restore
        // the entering row's set, so the predicate is decided once per
        // pattern and the common case clones nothing per row. The carry
        // half reads the tail (F-1-14) — the one spelling of the
        // carry-or-drop decision ([`MatchTail::store_row`]'s), never a
        // second `!last_pat` encoding that must stay hand-in-sync with it.
        let keep_used = p.is_shortest_path_function && matches!(tail, MatchTail::Carry);
        // LABS-801: the pattern's rel-type filters resolve once per
        // pattern, here at the pattern tier beside the tail — never per
        // row of the loop below.
        let filters = PathFilters::resolve(ctx.graph, p, match_mode);
        for (r, used) in current {
            ctx.graph.cancel_tick()?;
            if last_pat && cap.as_ref().is_some_and(|c| c.full()) {
                break;
            }
            let prior_used = keep_used.then(|| used.clone());
            let seed_used =
                if match_mode == MatchMode::RepeatableElements || p.is_shortest_path_function {
                    UsedSet::default()
                } else {
                    used
                };
            let mut matched = {
                // The last pattern owns the cap/reduce and finishes its
                // rows (the used-rel carry dropped); every earlier pattern
                // carries `used` forward. The tail is decided once per
                // pattern (see the hoist above); only the cap's reborrow
                // is per-row.
                let mut rider = if last_pat {
                    cap.as_deref_mut().map(Rider::Cap)
                } else {
                    None
                };
                match_one_path(
                    ctx,
                    p,
                    &filters,
                    r,
                    seed_used,
                    match_mode,
                    residual,
                    rider.as_mut(),
                    tail,
                )?
            };
            if let Some(prior_used) = &prior_used {
                for (_, used) in &mut matched {
                    *used = prior_used.clone();
                }
            }
            if account {
                for (row, used) in &matched {
                    next_meter.push(row, used.len());
                }
                next_charge.resize(next_meter.bytes)?;
                relate_core::tokens::peak_hold("expand");
            }
            next.extend(matched);
        }
        if match_mode == MatchMode::RepeatableElements {
            for (_, used) in &mut next {
                used.clear();
            }
            if account {
                next_charge.resize_with(|| crate::workspace::binding_bytes(&next))?;
                relate_core::tokens::peak_hold("expand");
            }
        }
        current = next;
        batch = next_charge;
        if current.is_empty() {
            return Ok(Vec::new());
        }
    }
    drop(batch);
    Ok(current.into_iter().map(|(r, _)| r).collect())
}
