//! Clause dispatchers: `run_clauses` (write path) and
//! `execute_part_read_seeded` (read path).

use std::cell::OnceCell;
use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};
use relate_ir::Clause;

use crate::ctx::{ExecCtx, QueryCtx};
use crate::graph::QueryStats;
use crate::metrics::PlanMetricsSite;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::batch_tail::{generator_seeded_tail, BatchTail, BATCH_ROWS};
use super::call::{concluding_part_result, exec_call_subquery, exec_conditional, SubquerySpec};
use super::catalog::{catalog_result, execute_catalog};
use super::catalog_show::show_indexes_result;
use super::classification::ProjectionClass;
use super::clause_arms::{
    exec_create, exec_delete, exec_merge, exec_remove, exec_set, exec_unwind,
};
use super::ddl::{exec_create_index, exec_drop_index};
use super::feed::{park_generator, take_feed, PendingFeed, StreamedFeed};
use super::foreach::exec_foreach;
use super::load_csv::{exec_load_csv, load_csv_feed};
use super::match_arm::exec_match_arm;
use super::match_clause::LadderFacts;
use super::procedures::{exec_call_clause, CallClauseOut, CallPipeline, CallSpec};
use super::profile_tick::{finish_parked_feed, profile_tick, ProfileTick};
use super::project::{project_rows, return_result, unwind_streams_into, RowSource};
use super::scope::{carry_scope_bindings, star_scope_names, ProjectionFields};
use super::stats_short_circuit::stats_short_circuit;
use super::unwind::unwind_feed;
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
    // The statement's evaluation context, built once before the clause
    // loop (stellar round 2, F-2-21): the authenticated principal
    // (ADR-0273) travels with it, so every clause arm consumes the same
    // context — no per-arm construction.
    let ctx = ExecCtx::new(graph, params).with_principal(qctx.principal);
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
        // ADR-0589: the grouped label aggregates answer from the
        // maintained label-set counts under the same exactness window.
        // The statistics short-circuits, one ladder: the count-only
        // operator (LABS-370), the label-set groups (ADR-0589), and the
        // index-backed aggregates over one labeled property (ADR-0611)
        // — tried in order behind one eligibility gate.
        if let Some((rows, result)) = stats_short_circuit(ctx, clauses, &rows, scope)? {
            return Ok((rows, Some(result)));
        }
        // ADR-0641: a record generator seeding a per-row-independent tail
        // runs batch-major inside this one transaction — the generator's
        // records are pulled in bounded batches and the tail clauses run
        // once per batch over that batch's rows, so the driving rows never
        // exceed one batch (the pokec load's 656 k rows × CSV-record width
        // is the 13.1 GiB boundary ADR-0463 charges). The tail's own
        // containers (a MATCH fan-out) stay charged by ADR-0463, not
        // bounded by the batch. One pre-loop gate, one PROFILE policy: a
        // PROFILE run stays on the ordinary path so its facts describe the
        // plan that ran.
        if let Some(head) = generator_seeded_tail(clauses) {
            return run_generator_batches(graph, mode, head, params, rows, qctx, scope);
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
    // An `UNWIND` whose follower folds rows as they arrive is not run as a
    // clause of its own: its generator is parked here and the consumer pulls
    // rows from it, so the rows are never held. The followers that fold are
    // the ones `unwind_streams_into` admits — the aggregation, and a clause
    // that may take the ordered window; a consumer that cannot fold says so
    // by not being parked. The parked value carries the generator with its
    // own statistics
    // and PROFILE position, so each streamed generator (`UNWIND`, `LOAD
    // CSV`) reports its own facts once the consumer has drained it.
    // The one feed slot the next projection consumes — a parked generator
    // (an `UNWIND` or a `LOAD CSV` fused into it) or the aggregation a
    // streamed `MATCH` folded its rows into, never both: the slot is one
    // value, so "one feed per projection" is the type's own fact. The
    // `MATCH` consumer's `ORDER BY` plan is borrowed for as long as the
    // fold lives, so the slot sits after the cell that holds it (dropped
    // last): one streamed feed per part, any later one takes the held path.
    let fold_class: OnceCell<ProjectionClass<'_>> = OnceCell::new();
    let mut pending: Option<PendingFeed<'_>> = None;
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
        let mut metrics_tick = profile.map(|site| profile_tick(graph, site));
        // The two MATCH fast paths (the search MATCH and the folded
        // MATCH) consume more than this clause; `skip` carries how many
        // extra clauses they swallowed past `i` (0 for the ordinary
        // one-clause advance). Recording and advancing happen once, at
        // the shared tail.
        let mut skip = 0;
        // `RETURN` moves its cells into the query result before this tail
        // runs. Record that clause while the values are still in the rows.
        let mut profile_recorded = false;
        // The operator that served this clause's `MATCH` (LABS-1117) — the
        // single source both PROFILE facts project from (`batched` and the
        // rung). `None` exactly when no `MATCH` operator ran: a non-`MATCH`
        // clause; a `MATCH` a fold answered (`try_match_folds`, e.g. a
        // boolean or walk count); and a `MATCH` inside a nested evaluation
        // (which has its own loop). A `SEARCH` `MATCH` sets `Rung(Walker)`:
        // it runs the per-row walker directly, so its rung is the walker.
        // Carries the ladder's other fact too (LABS-1437): why the frontier
        // family did not serve the clause, returned beside the operator.
        let mut clause_ladder: Option<LadderFacts> = None;
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
                rows = exec_create(graph, rows, patterns, params)?;
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
            // The same parking as a streamed `UNWIND`: a `LOAD CSV` whose
            // follower folds rows as they arrive (the aggregation, or a
            // clause that may take the ordered window — `unwind_streams_into`
            // admits exactly those) has its generator parked and the consumer
            // pulls one row per record, so neither the records nor the rows
            // are ever held (ADR-0555).
            Clause::LoadCsv {
                with_headers,
                url,
                variable,
                field_terminator,
            } if unwind_streams_into(clauses.get(i + 1)) => {
                park_and_defer(
                    load_csv_feed(
                        ctx,
                        std::mem::take(&mut rows),
                        *with_headers,
                        url,
                        variable,
                        field_terminator.as_deref(),
                        profile.is_some(),
                    )?,
                    profile,
                    i,
                    &mut pending,
                    &mut metrics_tick,
                );
            }
            Clause::LoadCsv {
                with_headers,
                url,
                variable,
                field_terminator,
            } => {
                rows = exec_load_csv(
                    ctx,
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
            Clause::Match { .. } => {
                let mut arm = exec_match_arm(
                    ctx,
                    std::mem::take(&mut rows),
                    clauses,
                    i,
                    &fold_class,
                    scope,
                    profile.is_some(),
                )?;
                rows = arm.rows;
                last_result = arm.last_result.or(last_result);
                skip = arm.skip;
                clause_ladder = arm.ladder;
                pending = arm
                    .pending_fold
                    .take()
                    .map(|a| PendingFeed::Folded(Box::new(a)))
                    .or(pending);
                rows_source = arm.rows_source;
                if let (Some(tick), Some(streamed)) = (metrics_tick.as_mut(), arm.streamed_rows) {
                    tick.streamed_rows = Some(streamed);
                }
            }
            Clause::Merge {
                pattern,
                on_create,
                on_match,
            } => {
                rows = exec_merge(graph, rows, pattern, on_create, on_match, params)?;
            }
            Clause::Delete {
                detach,
                expressions,
            } => {
                rows = exec_delete(graph, rows, *detach, expressions, params)?;
            }
            Clause::Set { items } => {
                rows = exec_set(graph, rows, items, params)?;
            }
            Clause::Remove { items } => {
                rows = exec_remove(graph, rows, items, params)?;
            }
            Clause::Unwind { expression, alias } if unwind_streams_into(clauses.get(i + 1)) => {
                park_and_defer(
                    unwind_feed(
                        ctx,
                        &clauses[i + 1..],
                        std::mem::take(&mut rows),
                        expression,
                        alias,
                        profile.is_some(),
                    ),
                    profile,
                    i,
                    &mut pending,
                    &mut metrics_tick,
                );
            }
            Clause::Unwind { expression, alias } => {
                rows = exec_unwind(ctx, &clauses[i + 1..], rows, expression, alias)?;
            }
            Clause::Call {
                name,
                args,
                yield_items,
                optional,
                where_clause,
            } => {
                // The CALL arm's body — the call's one resolution, the
                // parking, the materializing run and the pad's completion —
                // lives in the `procedures` directory module
                // (`procedures::exec_call_clause`, beside `resolve_call`
                // and the classification it reads; stellar round 2,
                // F-2-2). A follower that folds rows as they arrive — the
                // same `unwind_streams_into` admission `UNWIND` and `LOAD
                // CSV` park by — keeps the call's generator instead of its
                // rows: the composed input-rows × procedure-rows product
                // is never held (ADR-0555).
                match exec_call_clause(
                    ctx,
                    std::mem::take(&mut rows),
                    CallSpec {
                        name,
                        source: args,
                        yield_items,
                        optional: *optional,
                        where_clause: where_clause.as_ref(),
                    },
                    CallPipeline {
                        profiled: profile.is_some(),
                        folds: unwind_streams_into(clauses.get(i + 1)),
                        terminal: i + 1 == clauses.len(),
                    },
                    || star_scope_names(&clauses[..i]),
                )? {
                    CallClauseOut::Ran {
                        rows: call_rows,
                        result: call_result,
                    } => {
                        rows = call_rows;
                        last_result = call_result.or(last_result);
                    }
                    CallClauseOut::Parked(feed) => {
                        park_and_defer(feed, profile, i, &mut pending, &mut metrics_tick);
                    }
                }
            }
            Clause::With { .. } => {
                let spec = ProjectionFields::of_projection(clause);
                let mut feeds = ProjectionFeeds {
                    pending: &mut pending,
                    rows: &mut rows,
                };
                project_clause(
                    ctx,
                    &mut feeds,
                    &spec,
                    false,
                    rows_from,
                    scope,
                    &mut metrics_tick,
                )?;
                // An importing subquery's scope bindings survive a `WITH`
                // (Cypher never lets a body projection drop them); re-attach
                // the ones it did drop.
                if let Some(scope) = scope {
                    carry_scope_bindings(&mut rows, scope);
                }
            }
            Clause::Return { items, .. } => {
                let spec = ProjectionFields::of_projection(clause);
                let mut feeds = ProjectionFeeds {
                    pending: &mut pending,
                    rows: &mut rows,
                };
                project_clause(
                    ctx,
                    &mut feeds,
                    &spec,
                    true,
                    rows_from,
                    scope,
                    &mut metrics_tick,
                )?;
                if let Some(tick) = &metrics_tick {
                    tick.record(i, &rows, clause_ladder);
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
            tick.record(i, &rows, clause_ladder);
        }
        clause_i = i + 1 + skip;
    }
    Ok((rows, last_result))
}

/// The ADR-0641 batch-major driver: pull the generator's records in bounded
/// batches and run the tail once per batch over that batch's rows, all on
/// this statement's one scratch — no implicit per-batch commit, the redo is
/// the transaction's own state. Each batch's rows are dropped once the tail
/// consumed them, so the inter-clause container never exceeds one batch.
///
/// The admitted tail's terminal clause is a `CREATE`, which projects
/// nothing, so the statement carries no result and the returned rows are
/// empty: a write statement's answer is its statistics, never its rows.
fn run_generator_batches(
    graph: &ExecGraph<'_>,
    mode: ClauseMode,
    head: BatchTail<'_>,
    params: &BTreeMap<String, Value>,
    rows: Vec<Row>,
    qctx: QueryCtx<'_>,
    scope: Option<&Row>,
) -> Result<(Vec<Row>, Option<QueryResult>)> {
    let ctx = ExecCtx::new(graph, params).with_principal(qctx.principal);
    let mut feed = load_csv_feed(
        ctx,
        rows,
        head.with_headers,
        head.url,
        head.variable,
        head.field_terminator,
        false,
    )?;
    loop {
        graph.check_cancelled()?;
        let batch: Vec<Row> = feed.by_ref().take(BATCH_ROWS).collect::<Result<_>>()?;
        if batch.is_empty() {
            break;
        }
        // The tail's terminal rows are the batch's own working set (a
        // `MATCH* CREATE` passes through 1:1); the statement has no
        // projection, so they are dropped with the batch, never held. The
        // admitted tail projects nothing, so its result is always `None` —
        // asserted where the empty return relies on it.
        let (_, result) =
            run_clauses_mode(graph, mode, head.tail, params, batch, qctx, None, scope)?;
        debug_assert!(result.is_none(), "the admitted tail projects nothing");
    }
    Ok((Vec::new(), None))
}

/// The three places a `WITH`/`RETURN` can find its input: the generator of a
/// streamed `UNWIND`, the aggregation a streamed `MATCH` fed, or the held
/// rows.
struct ProjectionFeeds<'f, 'a> {
    pending: &'f mut Option<PendingFeed<'a>>,
    rows: &'f mut Vec<Row>,
}

/// Run one `WITH`/`RETURN` projection over its feed and close a fused
/// `UNWIND` it drained; the projected rows land in the held rows.
fn project_clause(
    ctx: ExecCtx<'_>,
    feeds: &mut ProjectionFeeds<'_, '_>,
    spec: &ProjectionFields<'_>,
    inflate: bool,
    rows_from: RowSource,
    scope: Option<&Row>,
    tick: &mut Option<ProfileTick<'_>>,
) -> Result<()> {
    let (feed, parked) = take_feed(feeds.pending, feeds.rows);
    *feeds.rows = project_rows(ctx, feed, spec, inflate, rows_from, scope)?;
    finish_parked_feed(parked, tick.as_mut());
    Ok(())
}

/// Park a clause's generator for the folding consumer that follows and
/// close its tick — the one epilogue every parking arm (LOAD CSV, UNWIND,
/// CALL) runs: no arm ran, the consumer's own tick covers the work, and
/// the parked clause reports its facts when that tick closes (the taken
/// tick makes the tail's `profile_recorded` read unreachable on these
/// paths, so the flag is not touched here).
fn park_and_defer<'a>(
    feed: StreamedFeed<'a>,
    profile: Option<PlanMetricsSite<'a>>,
    index: usize,
    pending: &mut Option<PendingFeed<'a>>,
    metrics_tick: &mut Option<ProfileTick<'a>>,
) {
    park_generator(feed, profile.map(|site| (site, index)), pending);
    drop(metrics_tick.take());
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
