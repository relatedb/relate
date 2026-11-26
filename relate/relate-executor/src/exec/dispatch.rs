//! Clause dispatchers: `run_clauses` (write path) and
//! `execute_part_read_seeded` (read path).

use std::cell::OnceCell;
use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};
use relate_ir::Clause;
use relate_planner::LogicalPlan;

use crate::consumer::StreamSink;
use crate::ctx::{ExecCtx, QueryCtx};
use crate::graph::QueryStats;
use crate::metrics::{ParkedRecord, PlanMetricsSite};
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::aggregate::Aggregation;
use super::batch_tail::{generator_seeded_tail, BatchTail, BATCH_ROWS};
use super::call::{
    call_subquery_feed, concluding_part_result, exec_call_in_transactions, exec_call_subquery,
    exec_conditional, query_writes, SubquerySpec,
};
use super::catalog::{catalog_result, execute_catalog};
use super::catalog_show::show_indexes_result;
use super::classification::ProjectionClass;
use super::clause_arms::{create_from_stream, exec_delete, exec_merge, exec_remove, exec_set};
use super::ddl::{exec_create_index, exec_drop_index};
use super::feed::{RowStream, StreamedFeed};
use super::foreach::exec_foreach;
use super::load_csv::{exec_load_csv, load_csv_feed, LoadCsvSpec};
use super::match_arm::exec_match_arm;
use super::match_clause::LadderFacts;
use super::procedures::{exec_call_clause, CallClauseOut, CallPipeline, CallSpec};
use super::profile_tick::{finish_parked_feed, profile_tick, ProfileTick};
use super::project::{
    maps_each_row, project_rows, return_result, stream_terminal_return, ProjectionInput, RowSource,
};
use super::scope::{carry_scope_bindings, star_scope_names, ProjectionFields};
use super::stats_short_circuit::stats_short_circuit;
use super::unwind::unwind_feed;
use super::{QueryResult, RowFeed};

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
        Terminal::Land,
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
        Terminal::Land,
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

/// Where a clause loop's terminal `RETURN` delivers (ADR-0682): landed into
/// the part's rows, or handed batch by batch to the statement's consumer.
/// `Stream` is constructed only by [`execute_part_read_streamed`], over a
/// part [`streamable_part`] admitted, with no PROFILE site and no enclosing
/// scope — so the loop's remaining question is the per-run one: whether the
/// RETURN's input can take the per-batch form.
enum Terminal<'s, 'a> {
    Land,
    Stream(&'s mut StreamSink<'a>),
}

#[allow(clippy::too_many_arguments)]
fn run_clauses_mode(
    graph: &ExecGraph<'_>,
    mode: ClauseMode,
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
    rows: Vec<Row>,
    qctx: QueryCtx<'_>,
    profile: Option<PlanMetricsSite<'_>>,
    scope: Option<&Row>,
    mut terminal: Terminal<'_, '_>,
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
    // A generator clause (`UNWIND`, `LOAD CSV`, `CALL { … }`) whose follower
    // is a `WITH`/`RETURN` is not run as a clause of its own: its generator
    // becomes this loop's one stream slot, and the projection pulls rows
    // from it, so the rows are never held. Every projection is such a
    // consumer now — its own classification decides fold against collect —
    // so the dispatcher reads only whether a projection follows, never
    // which shape folds. The stream carries the generator's own statistics
    // and PROFILE position, so each streamed generator reports its own facts
    // once the consumer has drained it. A generator followed by any other
    // clause emits its rows into the inter-clause stream instead.
    //
    // The one value the next projection can also consume is the aggregation
    // a streamed `MATCH` already folded its rows into: not a row stream, so
    // it has its own slot, filled only when the match sink answers with a
    // fold. The `MATCH` consumer's `ORDER BY` plan is borrowed for as long
    // as the fold lives, so the slot sits after the cell that holds it
    // (dropped last).
    let fold_class: OnceCell<ProjectionClass<'_>> = OnceCell::new();
    // The bounded sink's own classification slot. It is separate from the
    // fold's on purpose: a sink that claimed the fold's cell would make a
    // later `MATCH`'s aggregation fold decline to the held path (the
    // folding-consumer-stops-folding class), so the two consumers each own
    // the cell their plan borrows from.
    let sink_class: OnceCell<ProjectionClass<'_>> = OnceCell::new();
    let mut folded: Option<Aggregation<'_>> = None;
    // The one input the clause arms are threaded with (ADR-0660). The held
    // container is the common producer form: each arm drains it through the
    // batch protocol and hands its output back as the next stream — or
    // parks a fused generator in this same slot.
    let mut stream = RowStream::held(rows);
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
        // The clause's parked PROFILE record, built once per iteration
        // beside the tick it shares the site with: the one position value
        // every parked form below reports through, so "this clause is
        // PROFILEd" is spelled once per clause — the feed builders'
        // `profiled` flag and the record derive from the same value.
        let record = profile.map(|site| ParkedRecord::at(site, i));
        // The two MATCH fast paths (the search MATCH and the folded
        // MATCH) consume more than this clause; `skip` carries how many
        // extra clauses they swallowed past `i` (0 for the ordinary
        // one-clause advance). Recording and advancing happen once, at
        // the shared tail.
        let mut skip = 0;
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
        stream = match clause {
            Clause::Catalog(command) => {
                let rows = execute_catalog(graph, command, stream.drain()?, params, qctx)?;
                // Only row-producing catalog commands (the SHOW family and
                // TERMINATE TRANSACTIONS) project their rows — `catalog_result`
                // returns `None` for schema/admin DDL, leaving `last_result`
                // unset so the part finishes with zero rows and zero columns
                // (Neo4j parity, CIP-161).
                if i + 1 == clauses.len() {
                    last_result = catalog_result(graph, &rows, command);
                }
                RowStream::held(rows)
            }
            Clause::Create { patterns } => {
                // The input policy (whole-input hold vs the barrier) lives
                // in the create arm, beside the hoisting that needs it; the
                // arm's own stream reports the clause's rows.
                defer_produced(
                    create_from_stream(graph, stream, patterns, params, record)?,
                    &mut metrics_tick,
                )
            }
            Clause::CreateIndex {
                name,
                if_not_exists,
                label,
                property,
            } => {
                exec_create_index(graph, name.as_deref(), *if_not_exists, label, property)?;
                stream
            }
            Clause::DropIndex { if_exists, target } => {
                exec_drop_index(graph, *if_exists, target)?;
                stream
            }
            Clause::ShowIndexes => {
                last_result = Some(show_indexes_result(graph)?);
                // A terminal that answers the part directly walks no rows:
                // the input stream stops here.
                stream.stop()?;
                stream
            }
            Clause::Finish => {
                last_result = Some(QueryResult::intermediate(
                    Vec::new(),
                    Vec::new(),
                    QueryStats::default(),
                ));
                stream.stop()?;
                stream
            }
            // The same parking as a streamed `UNWIND`: a `LOAD CSV` whose
            // follower is a projection has its generator parked and the
            // consumer pulls one row per record, so neither the records nor
            // the rows are ever held (ADR-0555).
            Clause::LoadCsv {
                with_headers,
                url,
                variable,
                field_terminator,
            } if projection_follows(clauses.get(i + 1)) => {
                let spec = LoadCsvSpec {
                    with_headers: *with_headers,
                    url,
                    variable,
                    field_terminator: field_terminator.as_deref(),
                };
                park_and_defer(
                    load_csv_feed(ctx, stream.drain()?, spec, record.is_some())?,
                    record,
                    &mut metrics_tick,
                )
            }
            Clause::LoadCsv {
                with_headers,
                url,
                variable,
                field_terminator,
            } => RowStream::held(exec_load_csv(
                ctx,
                stream.drain()?,
                LoadCsvSpec {
                    with_headers: *with_headers,
                    url,
                    variable,
                    field_terminator: field_terminator.as_deref(),
                },
            )?),
            // The `CALL { … }` product as the feed spine's one driver
            // (ADR-0555): `IN TRANSACTIONS` owns its batching and commits
            // and emits its batches as its own stream (ADR-0660's `retain`
            // disposition); otherwise a projection follower
            // parks the product — the consumer pulls one merged row at a
            // time, so peak memory is one outer row's body result, not the
            // input count times it — and every other consumer holds it
            // charged (ADR-0463).
            Clause::CallSubquery {
                query,
                optional,
                scope,
                in_transactions,
            } => {
                let spec = SubquerySpec {
                    query,
                    optional: *optional,
                    scope,
                };
                if let Some(tx) = in_transactions.as_ref() {
                    let out =
                        exec_call_in_transactions(graph, stream, spec, tx, params, qctx, record)?;
                    // The stream reports the call's own PROFILE record
                    // once its consumer drains or stops it, never a zero
                    // probe at this tail.
                    defer_produced(out, &mut metrics_tick)
                } else if call_product_parks(clauses.get(i + 1), query) {
                    park_and_defer(
                        call_subquery_feed(ctx, qctx, stream.drain()?, spec, record.is_some())?,
                        record,
                        &mut metrics_tick,
                    )
                } else {
                    RowStream::held(exec_call_subquery(ctx, qctx, stream.drain()?, spec)?)
                }
            }
            Clause::Conditional {
                branches,
                else_query,
            } => {
                let (new_rows, result) = exec_conditional(
                    graph,
                    stream.drain()?,
                    branches,
                    else_query.as_deref(),
                    params,
                    qctx,
                )?;
                last_result = Some(result);
                RowStream::held(new_rows)
            }
            Clause::Foreach {
                variable,
                list,
                clauses,
            } => {
                // ADR-0660's `pass` disposition: the side effects apply one
                // batch at a time and the batch's rows pass through
                // unchanged, so the arm never holds the whole input.
                write_and_defer(
                    stream,
                    record,
                    |rows| {
                        exec_foreach(graph, &rows, variable, list, clauses, params, qctx)?;
                        Ok(rows)
                    },
                    &mut metrics_tick,
                )?
            }
            Clause::Use { .. } => stream,
            Clause::Match { .. } => {
                let mut arm = exec_match_arm(
                    ctx,
                    stream.drain()?,
                    clauses,
                    i,
                    &fold_class,
                    &sink_class,
                    scope,
                    profile,
                )?;
                last_result = arm.last_result.or(last_result);
                skip = arm.skip;
                clause_ladder = arm.ladder;
                if let Some(fold) = arm.folded.take() {
                    // A folded MATCH is always consumed by the very next
                    // clause (a `WITH`/`RETURN`, the only shapes a fold
                    // lands), so the slot is empty here: a fold meeting an
                    // occupied slot is an engine wiring error, never a
                    // silent precedence.
                    if folded.is_some() {
                        return Err(Error::internal(
                            "a folded MATCH met a feed that was never consumed",
                        ));
                    }
                    folded = Some(fold);
                }
                rows_source = arm.rows_source;
                if let Some(tick) = metrics_tick.as_mut() {
                    tick.streamed = arm.streamed;
                }
                RowStream::held(arm.rows)
            }
            Clause::Merge {
                pattern,
                on_create,
                on_match,
            } => write_and_defer(
                stream,
                record,
                |rows| exec_merge(graph, rows, pattern, on_create, on_match, params),
                &mut metrics_tick,
            )?,
            Clause::Delete {
                detach,
                expressions,
            } => {
                // DELETE's rels-before-nodes is a whole-clause invariant: a
                // row's relationship delete may be the one detaching a node
                // a later row deletes (`DELETE r0, n`), so the arm holds its
                // driving input and resolves every row's targets before the
                // first node delete — one delete_values pass, never a
                // per-batch split (ADR-0676; the spec corpus's Set pin). The
                // held stream records through the shared clause-exit tail,
                // the `Match` arm's shape.
                RowStream::held(exec_delete(
                    graph,
                    stream.drain()?,
                    *detach,
                    expressions,
                    params,
                )?)
            }
            Clause::Set { items } => write_and_defer(
                stream,
                record,
                |rows| exec_set(graph, rows, items, params),
                &mut metrics_tick,
            )?,
            Clause::Remove { items } => write_and_defer(
                stream,
                record,
                |rows| exec_remove(graph, rows, items, params),
                &mut metrics_tick,
            )?,
            Clause::Unwind { expression, alias } => {
                let feed = unwind_feed(
                    ctx,
                    &clauses[i + 1..],
                    stream.drain()?,
                    expression,
                    alias,
                    record.is_some(),
                );
                if projection_follows(clauses.get(i + 1)) {
                    park_and_defer(feed, record, &mut metrics_tick)
                } else {
                    // The expanding form: a follower that is not a projection
                    // pulls the generator's rows as it consumes them
                    // (ADR-0660's `expand` disposition) instead of holding the
                    // expansion. The stream carries the generator's facts, so
                    // the clause reports the rows it produced when the
                    // follower drains it — the tail must not record its zero
                    // probe in the meantime.
                    defer_produced(RowStream::expanding(feed, record), &mut metrics_tick)
                }
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
                // F-2-2). A projection follower — the same
                // `projection_follows` guard `UNWIND` and `LOAD CSV` park
                // by — keeps the call's generator instead of its rows: the
                // composed input-rows × procedure-rows product is never
                // held (ADR-0555).
                match exec_call_clause(
                    ctx,
                    stream.drain()?,
                    CallSpec {
                        name,
                        source: args,
                        yield_items,
                        optional: *optional,
                        where_clause: where_clause.as_ref(),
                    },
                    CallPipeline {
                        profiled: record.is_some(),
                        parks: projection_follows(clauses.get(i + 1)),
                        terminal: i + 1 == clauses.len(),
                    },
                    || star_scope_names(&clauses[..i]),
                )? {
                    CallClauseOut::Ran {
                        rows: call_rows,
                        result: call_result,
                    } => {
                        last_result = call_result.or(last_result);
                        RowStream::held(call_rows)
                    }
                    CallClauseOut::Parked(feed) => park_and_defer(feed, record, &mut metrics_tick),
                }
            }
            Clause::With { .. } => {
                let spec = ProjectionFields::of_projection(clause)?;
                let mut rows = project_clause(
                    ctx,
                    stream,
                    &mut folded,
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
                RowStream::held(rows)
            }
            Clause::Return { items, .. } => {
                let spec = ProjectionFields::of_projection(clause)?;
                // ADR-0682: a streamed terminal hands its batches to the
                // caller's consumer — the static shape was admitted once
                // (`streamable_part`); what is left is the run's own input:
                // no folded MATCH aggregation beside it, and an input the
                // per-batch form can take (a landed sink's tail facts have
                // no per-batch reading). Every other RETURN keeps landing
                // into the part's rows.
                let streamed_to = match &mut terminal {
                    Terminal::Stream(sink)
                        if folded.is_none() && !matches!(rows_from, RowSource::SinkLanded(_)) =>
                    {
                        Some(&mut **sink)
                    }
                    _ => None,
                };
                if let Some(sink) = streamed_to {
                    last_result = Some(stream_terminal_return(
                        ctx, stream, sink, &spec, items, clauses, rows_from,
                    )?);
                    // The terminal consumed the stream; the loop tail's
                    // probe reads the empty held container.
                    RowStream::held(Vec::new())
                } else {
                    let mut rows = project_clause(
                        ctx,
                        stream,
                        &mut folded,
                        &spec,
                        true,
                        rows_from,
                        scope,
                        &mut metrics_tick,
                    )?;
                    // `RETURN` moves its cells into the query result below, so
                    // the clause is recorded here, while the values are still in
                    // the rows; the tick is then taken, so the shared tail's
                    // "the tick is still live" is the one fact that says the
                    // clause is unrecorded.
                    if let Some(tick) = metrics_tick.take() {
                        tick.record(i, &rows, clause_ladder);
                    }
                    last_result = Some(return_result(graph, items, clauses, &mut rows)?);
                    RowStream::held(rows)
                }
            }
        };
        // ADR-0344 phase 2 + metric fields + ADR-0463: report one clause
        // operator's metrics — rows produced, store accesses and their read
        // kinds, phase times, expression bytes, row-materialization bytes,
        // elapsed — to the PROFILE sink its tick carries, at the shared
        // clause-exit tail of the dispatch loop. The tick is `None` exactly
        // when no sink is attached (the off-path branch), so the elapsed
        // read, the counter reads, and the row-size walk happen only on the
        // PROFILE path.
        if let Some(tick) = metrics_tick {
            tick.record(i, stream.probe(), clause_ladder);
        }
        clause_i = i + 1 + skip;
    }
    Ok((stream.drain()?, last_result))
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
    let mut feed = load_csv_feed(ctx, rows, head.generator, false)?;
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
        // refused loudly where the empty return relies on it.
        let (_, result) = run_clauses_mode(
            graph,
            mode,
            head.tail,
            params,
            batch,
            qctx,
            None,
            scope,
            Terminal::Land,
        )?;
        if result.is_some() {
            return Err(Error::internal("the admitted tail projected a result"));
        }
    }
    Ok((Vec::new(), None))
}

/// Run one `WITH`/`RETURN` projection over the clause loop's one input —
/// the stream slot, or the `MATCH`-folded aggregation — and close a fused
/// generator it drained. The projection's other input, the `MATCH`-folded
/// aggregation, is the one value that is not a row stream; it is never
/// present beside a non-empty stream.
#[allow(clippy::too_many_arguments)]
fn project_clause<'a>(
    ctx: ExecCtx<'a>,
    stream: RowStream<'a>,
    folded: &mut Option<Aggregation<'a>>,
    spec: &ProjectionFields<'a>,
    inflate: bool,
    rows_from: RowSource,
    scope: Option<&Row>,
    tick: &mut Option<ProfileTick<'a>>,
) -> Result<Vec<Row>> {
    // The fused generator's facts must outlive the stream the projection
    // consumes: the projection's own record excludes the generator's time,
    // and the generator's clause record closes here.
    let facts = stream.facts();
    let input = match folded.take() {
        Some(aggregation) => {
            // The fold consumed every input row, so the stream beside it is
            // the empty held container. Any other form is an engine wiring
            // error that would drop rows silently.
            if !stream.is_empty_held() {
                return Err(Error::internal("a folded MATCH met a non-empty feed"));
            }
            ProjectionInput::Feed(RowFeed::Folded(Box::new(aggregation)))
        }
        // A pre-projected sink owns its rows, so it is taken off the stream;
        // the one refusal of a produced stream lives in `into_held`.
        None => match rows_from {
            RowSource::SinkProjected => ProjectionInput::SinkProjected(stream.into_held()?),
            RowSource::SinkLanded(tail) => {
                ProjectionInput::SinkLanded(tail.into_landed(stream.into_held()?))
            }
            RowSource::Bindings => ProjectionInput::Feed(RowFeed::Held(stream)),
        },
    };
    let rows = project_rows(ctx, input, spec, inflate, scope)?;
    finish_parked_feed(facts, tick.as_mut());
    Ok(rows)
}

/// The one part of a plan whose answer streams from its terminal `RETURN`
/// to a batch consumer (ADR-0682 decision 1). Only [`streamable_part`]
/// constructs it, so holding one is the proof of the shape.
#[derive(Clone, Copy)]
pub(crate) struct StreamablePart<'p> {
    clauses: &'p [Clause],
}

/// The plan's streamable part, when its shape streams (ADR-0682 decision
/// 1): a single stage (no `NEXT`) with a single part (no `UNION`), ending in
/// a `RETURN` whose projection maps each row — no aggregation, `DISTINCT`,
/// `ORDER BY`, `SKIP` or `LIMIT`, so each pulled batch projects
/// independently of every other. `None` keeps the materialized terminal;
/// its result replays through the same consumer after the run.
pub(crate) fn streamable_part(plan: &LogicalPlan) -> Result<Option<StreamablePart<'_>>> {
    let [clauses] = plan.parts.as_slice() else {
        return Ok(None);
    };
    if plan.next.is_some() {
        return Ok(None);
    }
    // A `USE` routes the statement to a namespace the surface learns only
    // from the finished result's header; a streaming surface fixes the
    // namespace before the first row, so such a statement lands.
    if clauses
        .iter()
        .any(|clause| matches!(clause, Clause::Use { .. }))
    {
        return Ok(None);
    }
    let Some(clause @ Clause::Return { .. }) = clauses.last() else {
        return Ok(None);
    };
    let streams = maps_each_row(&ProjectionFields::of_projection(clause)?);
    Ok(streams.then_some(StreamablePart { clauses }))
}

/// Whether the clause after a generator is the `WITH`/`RETURN` that consumes
/// its feed (ADR-0660): every projection takes a generated feed, deciding
/// fold against collect from its own resolved bounds, so the dispatcher
/// reads only whether a projection follows — never which shape folds. The
/// one predicate every generator arm (`UNWIND`, `LOAD CSV`, `CALL { … }`)
/// parks by.
fn projection_follows(next: Option<&Clause>) -> bool {
    next.and_then(ProjectionFields::from_clause).is_some()
}

/// The `CALL { … }` product's park admission — [`projection_follows`],
/// plus this clause's own fact: a writing body never parks (see
/// [`query_writes`]), so its semantics never depend on when the consuming
/// projection evaluates its items.
fn call_product_parks(next: Option<&Clause>, query: &relate_ir::Query) -> bool {
    projection_follows(next) && !query_writes(query)
}

/// Park a clause's generator in the loop's one stream slot for the
/// projection that follows, and close its tick — the one epilogue every
/// parking arm (LOAD CSV, UNWIND, CALL) runs: no arm ran, the consumer's
/// own tick covers the work, and the parked clause reports its facts when
/// that tick closes (the taken tick is what keeps the shared tail from
/// recording it again).
fn park_and_defer<'a>(
    feed: StreamedFeed<'a>,
    record: Option<ParkedRecord<'a>>,
    metrics_tick: &mut Option<ProfileTick<'a>>,
) -> RowStream<'a> {
    defer_produced(RowStream::profiled(feed, record), metrics_tick)
}

/// The write family's counterpart to the parking form: a write clause's
/// rows cross the write barrier (ADR-0660's `pass` disposition — one batch
/// written as it arrives, its rows handed to the consumer as the barrier's
/// own stream), and the produced stream defers its PROFILE facts like
/// every produced form. The one spelling of the two-layer handoff the
/// write arms share.
fn write_and_defer<'a>(
    stream: RowStream<'a>,
    record: Option<ParkedRecord<'a>>,
    write: impl FnMut(Vec<Row>) -> Result<Vec<Row>> + 'a,
    metrics_tick: &mut Option<ProfileTick<'a>>,
) -> Result<RowStream<'a>> {
    Ok(defer_produced(
        stream.write_barrier(write, record)?,
        metrics_tick,
    ))
}

/// The produced-stream epilogue: a stream that carries its own PROFILE
/// facts reports the clause when its consumer drains or stops it, so the
/// arm hands over its tick and the shared tail never records the empty
/// probe. The one spelling of the deferral, beside the parking form.
fn defer_produced<'a>(
    stream: RowStream<'a>,
    metrics_tick: &mut Option<ProfileTick<'a>>,
) -> RowStream<'a> {
    drop(metrics_tick.take());
    stream
}

pub(crate) fn execute_part_read_seeded(
    graph: &ExecGraph<'_>,
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
    rows: Vec<Row>,
    qctx: QueryCtx<'_>,
    profile: Option<PlanMetricsSite<'_>>,
) -> Result<QueryResult> {
    read_part(graph, clauses, params, rows, qctx, profile, Terminal::Land)
}

/// [`execute_part_read_seeded`] for the admitted streamable part (ADR-0682):
/// the terminal `RETURN` hands its batches to `sink`. No PROFILE site — a
/// PROFILE run keeps the materialized terminal — and no enclosing scope.
pub(crate) fn execute_part_read_streamed(
    graph: &ExecGraph<'_>,
    part: StreamablePart<'_>,
    params: &BTreeMap<String, Value>,
    rows: Vec<Row>,
    qctx: QueryCtx<'_>,
    sink: &mut StreamSink<'_>,
) -> Result<QueryResult> {
    read_part(
        graph,
        part.clauses,
        params,
        rows,
        qctx,
        None,
        Terminal::Stream(sink),
    )
}

fn read_part(
    graph: &ExecGraph<'_>,
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
    rows: Vec<Row>,
    qctx: QueryCtx<'_>,
    profile: Option<PlanMetricsSite<'_>>,
    terminal: Terminal<'_, '_>,
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
        terminal,
    )?;
    // The read path reports no write stats; its wrapper resets stats on
    // the way out regardless.
    Ok(concluding_part_result(
        graph,
        clauses,
        &mut rows,
        last_result,
        QueryStats::default(),
    ))
}
