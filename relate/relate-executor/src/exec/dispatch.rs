//! Clause dispatchers: `run_clauses` (write path) and
//! `execute_part_read_seeded` (read path).

use std::collections::BTreeMap;
// `web_time` on wasm32: `std::time::Instant::now()` traps on
// `wasm32-unknown-unknown`, and PROFILE is a user-reachable path in the
// browser build — the clock must read, not panic (the facade uses the
// same shim).
use web_time::Instant;

use relate_core::{Error, Result, Value};
use relate_ir::{Clause, MatchMode};

use crate::ctx::{ClauseMetricsRecord, ExecCtx, PlanMetricsSite, QueryCtx};
use crate::eval::rows_logical_bytes;
use crate::graph::QueryStats;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::call::{
    concluding_part_result, exec_call_subquery, exec_conditional, exec_load_csv, SubquerySpec,
};
use super::catalog::{catalog_result, execute_catalog};
use super::catalog_show::show_indexes_result;
use super::clause_arms::{
    exec_create, exec_delete, exec_merge, exec_remove, exec_set, exec_unwind,
};
use super::count_stats::count_only_stats_result;
use super::ddl::{exec_create_index, exec_drop_index};
use super::exists_probe::try_fold_boolean_count;
use super::foreach::exec_foreach;
use super::match_clause::{run_match_operator_ladder, MatchOperator};
use super::procedures::{exec_call, CallSpec};
use super::project::{project_rows, return_result, sinkable_projection, ProjectSpec, RowSource};
use super::pushdown::{
    live_from, match_limit_cap, trim_rows, try_fold_one_hop_aggs, try_fold_path_aggs, FoldedReturn,
};
use super::scope::{carry_scope_bindings, star_scope_names};
use super::search::search_match_clause;
use super::QueryResult;

/// Read/write mode for the unified clause dispatcher. Both clause loops —
/// the write-capable [`run_clauses`] and the read-only
/// [`execute_part_read_seeded`] — are the same loop over the same `Clause`
/// arms; the mode only decides whether write clauses run or are rejected and
/// which stats a `RETURN` reports. Clause-independent concerns (the loop-top
/// cancellation check, per-clause dispatch) live in exactly one place:
/// [`run_clauses_mode`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum ClauseMode {
    Read,
    Write,
}

impl ClauseMode {
    fn is_read(self) -> bool {
        matches!(self, ClauseMode::Read)
    }
}

pub(crate) fn run_clauses(
    graph: &ExecGraph<'_>,
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
    rows: Vec<Row>,
    qctx: QueryCtx<'_>,
    profile: Option<PlanMetricsSite<'_>>,
) -> Result<(Vec<Row>, Option<QueryResult>)> {
    run_clauses_mode(
        graph,
        ClauseMode::Write,
        clauses,
        params,
        rows,
        qctx,
        profile,
        None,
    )
}

/// The subquery-body variant of [`run_clauses`]: `scope` carries the
/// bindings an importing subquery (`CALL (vars) { … }`, `CALL(*) { … }`,
/// an expression subquery) keeps visible for the whole body. Cypher does
/// not let a body `WITH` drop them, and the value is constant across the
/// body's rows, so the clause loop re-attaches a missing scope binding
/// after every `WITH` projection (`carry_scope_bindings`). Threading the
/// items through the `WITH` instead would make an injected import a
/// grouping key, which loses the keyless aggregation's identity row over
/// an emptied input.
pub(crate) fn run_nested_clauses_scoped(
    graph: &ExecGraph<'_>,
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
    rows: Vec<Row>,
    qctx: QueryCtx<'_>,
    scope: &Row,
) -> Result<(Vec<Row>, Option<QueryResult>)> {
    graph.nested_evals().tick()?;
    run_clauses_mode(
        graph,
        ClauseMode::Write,
        clauses,
        params,
        rows,
        qctx,
        None,
        Some(scope),
    )
}

/// One nested clause-engine pass (ADR-0395): tick the statement's budget
/// counter, then run the clause engine. The direct nested entries — the
/// EXISTS/COUNT clause-engine paths (`subquery_eval`) and the FOREACH arm
/// — all enter through here; the seeded statement engine
/// (`execute_query_seeded`) ticks its own entry. The nested boundary is
/// the explicit `None` profile site — no PROFILE facts cross it.
pub(crate) fn run_nested_clauses(
    graph: &ExecGraph<'_>,
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
    rows: Vec<Row>,
    qctx: QueryCtx<'_>,
) -> Result<(Vec<Row>, Option<QueryResult>)> {
    graph.nested_evals().tick()?;
    run_clauses(graph, clauses, params, rows, qctx, None)
}

#[allow(clippy::too_many_arguments)]
fn run_clauses_mode(
    graph: &ExecGraph<'_>,
    mode: ClauseMode,
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
    mut rows: Vec<Row>,
    qctx: QueryCtx<'_>,
    profile: Option<PlanMetricsSite<'_>>,
    scope: Option<&Row>,
) -> Result<(Vec<Row>, Option<QueryResult>)> {
    let mut last_result: Option<QueryResult> = None;
    // LABS-370 (v1): the count-only short-circuit — a whole-population
    // count answers from the maintained statistics (the label counts, the
    // rel-type counts, the (label, type, end-label) pair counts) instead
    // of scanning its population. Only without the PROFILE site
    // attached: a PROFILE run must observe the plan it describes, and
    // the count operator's self-describing plan node + its 0-db-hit
    // facts are the follow-up phase.
    if profile.is_none() {
        // The incoming seed rows ride along (round 3, F-1-3): the
        // short-circuit answers a whole-population count — one unbound
        // seed row. A stage seeded with the previous stage's rows (a
        // NEXT/WHEN/ELSE body) must answer N × population, so the
        // recognizer declines on anything but that one row.
        if let Some((rows, result)) = count_only_stats_result(graph, clauses, &rows) {
            return Ok((rows, Some(result)));
        }
    }
    let mut clause_i = 0;
    // Set to `SinkProjected` when the clause just run left the next
    // projection's columns already evaluated in its rows (the match sink,
    // which only ever feeds the clause immediately after it) and read by that
    // projection one iteration later. The rows carry no marker of their own,
    // so a user alias can never forge this state; the dispatcher is its one
    // authority.
    let mut rows_source = RowSource::Bindings;
    while clause_i < clauses.len() {
        let i = clause_i;
        let clause = &clauses[i];
        graph.check_cancelled()?;
        let rows_from = std::mem::replace(&mut rows_source, RowSource::Bindings);
        // The bundled PROFILE fact (ADR-0344 phase 2 + metric fields):
        // the site's presence decides the per-clause tick — the sink it
        // carries, its position, and the arm-start snapshots (clock, store
        // reads, phase times, expression bytes) — through one branch, never
        // an `Instant` or a counter read on the off-path. The site bundles the sink with
        // its position, so both half-states (a tick with no sink, a sink
        // with no position) are unrepresentable.
        let metrics_tick = profile.map(|site| profile_tick(graph, site));
        // The two MATCH fast paths (the search MATCH and the folded
        // MATCH) consume more than this clause; `skip` carries how many
        // extra clauses they swallowed past `i` (0 for the ordinary
        // one-clause advance). Recording and advancing happen once, at
        // the shared tail.
        let mut skip = 0;
        // `RETURN` moves its cells into the query result before this tail
        // runs. Record that clause while the values are still in the rows.
        let mut profile_recorded = false;
        // ADR-0468 (LABS-359): whether this clause ran the batch
        // scan/filter/project chain — a PROFILE fact the metric tick
        // reports, beside the rows it produced. Set only by the `MATCH`
        // arm that took the chain; `false` is every other clause's value.
        let mut clause_batched = false;
        // The read path's one admission rule, asked once per clause before
        // any clause arm runs: the policy names the message, the dispatcher
        // only reports it. The clause match below therefore holds clause arms
        // and nothing else.
        if mode.is_read() {
            if let Some(reason) = super::read_path_rejection(clause) {
                return Err(Error::semantic(reason));
            }
        }
        match clause {
            Clause::Catalog(command) => {
                rows = execute_catalog(graph, command, rows, params, qctx)?;
                // Only row-producing catalog commands (the SHOW family and
                // TERMINATE TRANSACTIONS) project their rows — `catalog_result`
                // returns `None` for schema/admin DDL, leaving `last_result`
                // unset so the part finishes with zero rows and zero columns
                // (Neo4j parity, CIP-161).
                if i + 1 == clauses.len() {
                    last_result = catalog_result(&rows, command);
                }
            }
            Clause::Create { patterns } => {
                rows = exec_create(graph, rows, patterns, params, qctx)?;
            }
            Clause::CreateIndex {
                name,
                if_not_exists,
                label,
                property,
            } => {
                exec_create_index(graph, name.as_deref(), *if_not_exists, label, property)?;
            }
            Clause::DropIndex { if_exists, target } => {
                exec_drop_index(graph, *if_exists, target)?;
            }
            Clause::ShowIndexes => {
                last_result = Some(show_indexes_result(graph)?);
            }
            Clause::Finish => {
                last_result = Some(QueryResult::intermediate(
                    Vec::new(),
                    Vec::new(),
                    QueryStats::default(),
                ));
            }
            Clause::LoadCsv {
                with_headers,
                url,
                variable,
                field_terminator,
            } => {
                rows = exec_load_csv(
                    ExecCtx::new(graph, params, qctx.cypher_version),
                    rows,
                    *with_headers,
                    url,
                    variable,
                    field_terminator.as_deref(),
                )?;
            }
            Clause::CallSubquery {
                query,
                optional,
                scope,
                in_transactions,
            } => {
                rows = exec_call_subquery(
                    graph,
                    rows,
                    SubquerySpec {
                        query,
                        optional: *optional,
                        scope,
                        in_transactions: in_transactions.as_ref(),
                    },
                    params,
                    qctx,
                )?;
            }
            Clause::Conditional {
                branches,
                else_query,
            } => {
                let (new_rows, result) =
                    exec_conditional(graph, rows, branches, else_query.as_deref(), params, qctx)?;
                rows = new_rows;
                last_result = Some(result);
            }
            Clause::Foreach {
                variable,
                list,
                clauses,
            } => {
                exec_foreach(graph, &rows, variable, list, clauses, params, qctx)?;
            }
            Clause::Use { .. } => {}
            Clause::Match {
                optional,
                match_mode,
                patterns,
                where_clause,
                search,
                using_index: _,
                using_join: _,
            } => {
                if let Some(search) = search {
                    rows = search_match_clause(
                        ExecCtx::new(graph, params, qctx.cypher_version),
                        rows,
                        *optional,
                        *match_mode,
                        patterns,
                        where_clause.as_ref(),
                        search,
                    )?;
                    if let Some(live) = live_from(&clauses[i + 1..]) {
                        trim_rows(&mut rows, &live);
                    }
                    // The same ADR-0463 (LABS-781) MATCH-container charge
                    // as the ladder landing below: the SEARCH arm's rows
                    // land at this dispatch point too.
                    let mut match_retained = graph.retain_bytes(0)?;
                    for row in &rows {
                        match_retained.add_row(graph, row.values())?;
                    }
                } else {
                    let folded = if !*optional {
                        try_match_folds(
                            graph,
                            &rows,
                            patterns,
                            where_clause.as_ref(),
                            &clauses[i..],
                            params,
                            qctx.cypher_version,
                        )?
                    } else {
                        None
                    };
                    match folded {
                        Some(folded) => {
                            rows = folded.rows;
                            last_result = Some(folded.result);
                            skip = folded.consumed_after;
                        }
                        None => {
                            // The MATCH's own WHERE no longer disarms the
                            // plain LIMIT pull-in (LABS-853):
                            // `match_limit_cap` states the WHERE policy,
                            // and `match_clause` filters in the sink ahead
                            // of the count.
                            let mut cap = if *optional {
                                None
                            } else {
                                match_limit_cap(
                                    &clauses[i + 1..],
                                    patterns,
                                    where_clause.as_ref(),
                                    graph,
                                    params,
                                    qctx.cypher_version,
                                    rows.len(),
                                )?
                            };
                            let projection = clauses.get(i + 1).and_then(sinkable_projection);
                            // The operator ladder — batch chain
                            // (ADR-0468), contracted kernel (ADR-0473),
                            // per-row walker, first taker owns the clause
                            // — lives in `match_clause.rs`, one place with
                            // one admission gate.
                            let (ladder_rows, operator) = run_match_operator_ladder(
                                ExecCtx::new(graph, params, qctx.cypher_version),
                                rows,
                                *optional,
                                *match_mode,
                                patterns,
                                where_clause.as_ref(),
                                &mut cap,
                                projection.as_ref(),
                                &clauses[i + 1..],
                            )?;
                            rows = ladder_rows;
                            clause_batched = matches!(operator, MatchOperator::BatchChain);
                            if projection.is_some() {
                                rows_source = RowSource::SinkProjected;
                            } else if let Some(live) = live_from(&clauses[i + 1..]) {
                                // A sink projection already dropped the
                                // bindings the next clause does not name.
                                // Trimming here would keep the pre-projection
                                // variables and discard the output columns.
                                trim_rows(&mut rows, &live);
                            }
                            // ADR-0463 (LABS-781,
                            // OPLANE_REQ-00345609): the MATCH output
                            // container is a retaining container. The
                            // walker charges its rows as they land inside
                            // its own emission loop (match_clause.rs);
                            // the kernel and batch operators hand this
                            // layer a finished container, whose rows
                            // charge here — #763 owns their per-push
                            // form. The refusal fires at ~the cap with
                            // the crossing row's honest figure.
                            if !matches!(operator, MatchOperator::Walker) {
                                let mut match_retained = graph.retain_bytes(0)?;
                                for row in &rows {
                                    match_retained.add_row(graph, row.values())?;
                                }
                            }
                        }
                    }
                }
            }
            Clause::Merge {
                pattern,
                on_create,
                on_match,
            } => {
                rows = exec_merge(graph, rows, pattern, on_create, on_match, params, qctx)?;
            }
            Clause::Delete {
                detach,
                expressions,
            } => {
                rows = exec_delete(graph, rows, *detach, expressions, params, qctx)?;
            }
            Clause::Set { items } => {
                rows = exec_set(graph, rows, items, params, qctx)?;
            }
            Clause::Remove { items } => {
                rows = exec_remove(graph, rows, items, params, qctx)?;
            }
            Clause::Unwind { expression, alias } => {
                rows = exec_unwind(
                    graph,
                    &clauses[i + 1..],
                    rows,
                    expression,
                    alias,
                    params,
                    qctx,
                )?;
            }
            Clause::Call {
                name,
                args,
                explicit,
                yield_items,
                optional,
                where_clause,
            } => {
                let was_empty = rows.is_empty();
                // Text render: the procedure dispatcher matches on name
                // text (String vocabulary), once per clause.
                let call_name = name.to_string();
                let (call_rows, yields_unit) = exec_call(
                    ExecCtx::new(graph, params, qctx.cypher_version).with_principal(qctx.principal),
                    rows,
                    CallSpec {
                        name: &call_name,
                        args,
                        explicit: *explicit,
                        yield_items,
                        optional: *optional,
                        where_clause: where_clause.as_ref(),
                    },
                )?;
                rows = call_rows;
                // A void optional call over a zero-row driving table yields
                // one unit row (CIP-159 [12]); Neo4j reads every variable
                // in scope as null on it, so bind the scope names
                // explicitly (ADR-0335) instead of leaving a following
                // projection to read an unbound reference.
                if was_empty && rows.len() == 1 && rows[0].is_empty() {
                    for name in star_scope_names(&clauses[..i]) {
                        rows[0].insert(name, Value::Null);
                    }
                }
                // A terminal void call yields an empty unit result (CIP-159):
                // the call path reported the fact on the resolution it
                // already made.
                if i + 1 == clauses.len() && yields_unit {
                    last_result = Some(QueryResult::intermediate(
                        Vec::new(),
                        Vec::new(),
                        QueryStats::default(),
                    ));
                }
            }
            Clause::With {
                distinct,
                group_by,
                items,
                order,
                skip,
                limit,
                where_clause,
                ..
            } => {
                rows = project_rows(
                    ExecCtx::new(graph, params, qctx.cypher_version),
                    rows,
                    &ProjectSpec {
                        items,
                        distinct: *distinct,
                        group_by: group_by.as_deref(),
                        order,
                        skip,
                        limit,
                        where_clause: where_clause.as_ref(),
                    },
                    false,
                    rows_from,
                    scope,
                )?;
                // An importing subquery's scope bindings survive a `WITH`
                // (Cypher never lets a body projection drop them); re-attach
                // the ones it did drop.
                if let Some(scope) = scope {
                    carry_scope_bindings(&mut rows, scope);
                }
            }
            Clause::Return {
                distinct,
                group_by,
                items,
                order,
                skip,
                limit,
            } => {
                rows = project_rows(
                    ExecCtx::new(graph, params, qctx.cypher_version),
                    rows,
                    &ProjectSpec {
                        items,
                        distinct: *distinct,
                        group_by: group_by.as_deref(),
                        order,
                        skip,
                        limit,
                        where_clause: None,
                    },
                    true,
                    rows_from,
                    scope,
                )?;
                if let Some(tick) = &metrics_tick {
                    tick.record(i, &rows, clause_batched);
                    profile_recorded = true;
                }
                last_result = Some(return_result(graph, items, clauses, &mut rows)?);
            }
        }
        // ADR-0344 phase 2 + metric fields + ADR-0463: report one clause
        // operator's metrics — rows produced, store accesses and their read
        // kinds, phase times, expression bytes, row-materialization bytes,
        // elapsed — to the PROFILE sink its tick carries, at the shared
        // clause-exit tail of the dispatch loop. The tick is `None` exactly
        // when no sink is attached (the off-path branch), so the elapsed
        // read, the counter reads, and the row-size walk happen only on the
        // PROFILE path.
        if let Some(tick) = metrics_tick.filter(|_| !profile_recorded) {
            tick.record(i, &rows, clause_batched);
        }
        clause_i = i + 1 + skip;
    }
    Ok((rows, last_result))
}

/// The expression-built meter's typed per-clause snapshot (ADR-0463): the
/// same delta protocol as `StoreReads` and `PhaseTimes` — a value taken at
/// the arm's start and a loud `since` delta, never a bare u64 pair.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ExpressionBytes(u64);

impl ExpressionBytes {
    fn now(graph: &ExecGraph<'_>) -> Self {
        Self(graph.expression_built_bytes())
    }

    fn since(self, start: Self) -> u64 {
        self.0.saturating_sub(start.0)
    }
}

/// The PROFILE path's per-clause snapshot: the site's sink and its
/// `(stage, part)` position, plus the arm-start facts the tick owns — the
/// `Instant`, the store-read and phase-time snapshots, and the view whose
/// expression-bytes meter it samples. Owning the site makes the half-states
/// — a tick with no sink to report to, a sink with no position to key it —
/// unrepresentable. Each metric is the delta the clause's arm moved its
/// counter, so the facts ride the executor's existing counters with no
/// second counting mechanism.
struct ProfileTick<'a> {
    /// The statement's PROFILE site, held whole — the sink↔position
    /// pairing stays the site type's invariant, never re-spelled as
    /// parallel flat fields here.
    site: PlanMetricsSite<'a>,
    t0: Instant,
    /// The store-read arm flag's prior value (LABS-805), captured at the
    /// arm and restored by the drop — a pre-armed thread (the test
    /// builds' default) is never disarmed by a profiled clause.
    store_profiling_was_armed: bool,
    /// The read-kind snapshot at the arm's start (LABS-369): the tick's
    /// `reads` is the delta against it, three thread-local reads per
    /// clause, never a per-row cost — and the one snapshot, since the
    /// store-access total is the kinds' sum.
    reads: crate::ctx::StoreReads,
    /// The phase-time snapshot at the arm's start (LABS-448): the tick's
    /// `phases` is the delta against it — and arming the phase stamps is
    /// this tick's first act, disarming its last, so the stamps never run
    /// outside a profiled statement's arm.
    phases: crate::ctx::PhaseTimes,
    /// The zone-skip counter snapshot at the arm's start (LABS-645): the
    /// tick's `zone_skips` is the delta against it — one thread-local
    /// read per clause, the store-access snapshot's pattern.
    zone_skips: u64,
    /// The frontier family's counter snapshot at the arm's start
    /// (ADR-0478): the tick's `frontier` is the delta against it — one
    /// thread-local read per clause, the zone-skip snapshot's pattern.
    frontier: crate::ctx::FrontierCounters,
    /// The view whose expression-built meter the tick samples at the arm's
    /// start and reads again in `record` (ADR-0463) — the third counter
    /// family it owns, beside `reads` and `phases`.
    graph: &'a ExecGraph<'a>,
    /// The meter's typed snapshot when the arm started; `record` reports
    /// `ExpressionBytes::now(..).since(start)`, the same delta protocol as
    /// `StoreReads` and `PhaseTimes`.
    expression_built_bytes_start: ExpressionBytes,
}

impl Drop for ProfileTick<'_> {
    /// Disarm the phase stamps (LABS-448) and restore the store-read
    /// counters' prior arm state (LABS-805): the arm is over, so the
    /// next clause's stamps and counter flushes arm with its own tick —
    /// and a non-PROFILEd statement following never pays an `Instant` or
    /// moves a counter. The restore, never an unconditional disarm: a
    /// pre-armed thread (the test builds' default) keeps its state.
    fn drop(&mut self) {
        crate::ctx::disarm_phase_profiling();
        super::restore_store_profiling(self.store_profiling_was_armed);
    }
}

fn profile_tick<'a>(graph: &'a ExecGraph<'a>, site: PlanMetricsSite<'a>) -> ProfileTick<'a> {
    // The clause's clearing boundary (LABS-369): a non-PROFILE statement
    // records walk starts without ever taking one, so the slot must be
    // emptied here, before the clause's arm runs — otherwise the next
    // PROFILE statement's first walking clause would drop its own fresh
    // start and report the earlier statement's walk. The frontier
    // rung's decline slot clears the same way (ADR-0516).
    let _ = super::take_walk_start();
    let _ = super::take_frontier_decline();
    // LABS-448: arm the phase stamps for this arm — the only site that
    // does, so the stamps' `Instant` reads happen exclusively inside a
    // profiled clause arm. LABS-805: arm the store-read counters beside
    // them — the walk tallies' per-run flushes land only while armed —
    // keeping the flag's prior value for the drop's restore.
    crate::ctx::arm_phase_profiling();
    let store_profiling_was_armed = super::arm_store_profiling();
    ProfileTick {
        site,
        t0: Instant::now(),
        store_profiling_was_armed,
        reads: super::store_reads(),
        phases: crate::ctx::phase_times(),
        zone_skips: super::zone_skips(),
        frontier: super::frontier_counters(),
        graph,
        expression_built_bytes_start: ExpressionBytes::now(graph),
    }
}

impl ProfileTick<'_> {
    /// Report one clause operator's metrics — rows produced, store
    /// accesses, row-materialization bytes, elapsed — to the sink the
    /// tick carries, at the position the statement-level walker keyed.
    /// `batched` (ADR-0468, LABS-359) says the clause ran the batch
    /// scan/filter/project chain; `zone_skips` (LABS-645) counts the
    /// conjunct-batch evaluations its zone metadata provably replaced
    /// with the correct empty survivor set — the delta of the counter
    /// snapshot the tick took, which only ever increases (the
    /// store-access subtraction's invariant, never a clamp). `frontier`
    /// (ADR-0478) carries the frontier family's run counters — the
    /// delta of the counter snapshot the tick took, the same
    /// subtraction's shape.
    fn record(&self, clause: usize, rows: &[Row], batched: bool) {
        let reads = super::store_reads().since(self.reads);
        self.site.sink.record_clause(ClauseMetricsRecord {
            stage: self.site.stage,
            part: self.site.part,
            clause,
            rows: rows.len() as u64,
            memory_bytes: rows_logical_bytes(rows),
            expression_built_bytes: ExpressionBytes::now(self.graph)
                .since(self.expression_built_bytes_start),
            time_ns: self.t0.elapsed().as_nanos(),
            reads,
            phases: crate::ctx::phase_times().since(self.phases),
            start: super::take_walk_start(),
            batched,
            zone_skips: super::zone_skips() - self.zone_skips,
            frontier: super::frontier_counters().since(self.frontier),
            frontier_decline: super::take_frontier_decline(),
        });
    }
}

fn try_match_folds(
    graph: &ExecGraph<'_>,
    rows: &[Row],
    patterns: &[relate_ir::PathPattern],
    where_clause: Option<&relate_ir::Expr>,
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Option<FoldedReturn>> {
    if let Some(folded) = try_fold_boolean_count(graph, rows, clauses, params, cypher_version)? {
        return Ok(Some(folded));
    }
    // The walk-count arm (`kernels::frontier_count`): a bounded
    // REPEATABLE ELEMENTS quantified pattern under a plain `count`, ahead
    // of the multi-hop REPEATABLE decline below.
    if let Some(folded) = super::kernels::frontier_count::try_fold_walk_count(
        graph,
        rows,
        clauses,
        params,
        cypher_version,
    )? {
        return Ok(Some(folded));
    }
    // `skip_rel` keeps one relationship out of the next hop, which is the
    // DIFFERENT RELATIONSHIPS rule. A single hop has nothing to skip, so
    // REPEATABLE ELEMENTS still uses the folds. Wider patterns decline and the
    // ordinary matcher applies the mode.
    let single_hop = patterns.len() == 1 && patterns[0].rels.len() == 1;
    if !single_hop
        && matches!(
            clauses.first(),
            Some(Clause::Match {
                match_mode: MatchMode::RepeatableElements,
                ..
            })
        )
    {
        return Ok(None);
    }
    // The path fold first: it compiles the full hop plan once and serves the
    // widest coverage. The one-hop fold follows and serves exactly the
    // queries the path fold declined — no coverage gate between them, so
    // no query compiles the plan twice.
    if let Some(folded) = try_fold_path_aggs(
        graph,
        rows,
        patterns,
        where_clause,
        &clauses[1..],
        params,
        cypher_version,
    )? {
        return Ok(Some(folded));
    }
    try_fold_one_hop_aggs(
        graph,
        rows,
        patterns,
        where_clause,
        &clauses[1..],
        params,
        cypher_version,
    )
}

pub(crate) fn execute_part_read_seeded(
    graph: &ExecGraph<'_>,
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
    rows: Vec<Row>,
    qctx: QueryCtx<'_>,
    profile: Option<PlanMetricsSite<'_>>,
) -> Result<QueryResult> {
    let (mut rows, last_result) = run_clauses_mode(
        graph,
        ClauseMode::Read,
        clauses,
        params,
        rows,
        qctx,
        profile,
        None,
    )?;
    // The read path reports no write stats; its wrapper resets stats on
    // the way out regardless.
    Ok(concluding_part_result(
        clauses,
        &mut rows,
        last_result,
        QueryStats::default(),
    ))
}
