//! The streamed `CALL { … }` forms (ADR-0555): the outer-rows × body-rows
//! product as a [`FeedSource`] — the same feed spine `UNWIND`, `LOAD CSV`
//! and the `CALL` composition park by. A projection follower pulls one
//! merged row at a time and the product is never held, so peak memory is
//! bounded by **one** outer row's body result instead of the input-row
//! count times it.
//!
//! Each outer row's body result is itself materialized — the nested clause
//! boundary (`run_clauses: Vec<Row>`) is ADR-0555's recorded residual — but
//! only the open row's is alive at a time: the source stages it at `open`
//! and hands it out at `pull`, and the spine's materializing form charges
//! each staged result to the running total before its first row is built
//! (ADR-0463). The shared-execution fast path (LABS-198) parks the same
//! way: its one body result is held once (bounded by the body, never by
//! the input rows) and each outer row's merged view of it is staged at
//! `open` and handed out row by row — bounded by that one body result,
//! never by the input rows.

use std::collections::HashSet;

use relate_core::Result;

use crate::ctx::{ExecCtx, QueryCtx};
use crate::exec::feed::{FeedSource, StreamedFeed};
use crate::exec::QueryResult;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::shared::{import_invariant_body, shared_execution_plan, SharedExecution};
use super::{call_subquery_row, empty_body_row, merge_body_export, SubquerySpec};

#[cfg(test)]
thread_local! {
    /// Test seam: how many times a shared-execution body actually ran —
    /// the shared fast path runs the body once for every outer row; the
    /// per-row path never initializes a shared body. Per-thread (the
    /// graph's shared take-and-reset counter shape, LABS-805), so the
    /// parallel test threads each count their own statement.
    static SHARED_BODY_RUNS: crate::graph::BuildCounter = const {
        crate::graph::BuildCounter::new()
    };
}

/// The seam's read-and-reset (test-only, see [`SHARED_BODY_RUNS`]).
#[cfg(test)]
pub(crate) fn take_shared_body_runs() -> u64 {
    SHARED_BODY_RUNS.with(crate::graph::BuildCounter::take)
}

/// Count one shared body execution — test-only (see
/// [`SHARED_BODY_RUNS`]); the production spelling is a no-op.
#[cfg(test)]
fn count_shared_body_run() {
    SHARED_BODY_RUNS.with(crate::graph::BuildCounter::bump);
}

/// The production spelling of [`count_shared_body_run`]: nothing to
/// count, nothing to pay.
#[cfg(not(test))]
fn count_shared_body_run() {}
/// The `CALL { … }` product as a feed source: per outer row, the body's
/// produced rows staged at `open` and handed out one at a time at `pull`.
/// The execution mode is one field — the per-row body run, or the
/// shared-execution fast path whose one body result is computed lazily on
/// the first open and reused for every outer row.
pub(crate) struct SubquerySource<'a> {
    query: &'a relate_ir::Query,
    optional: bool,
    scope: &'a relate_ir::CallScope,
    qctx: QueryCtx<'a>,
    mode: Mode,
    /// The open outer row's staged product — its body result merged, or
    /// the `OPTIONAL CALL` null pad. Empty when the last open contributed
    /// nothing.
    staged: std::vec::IntoIter<Row>,
}

/// The one execution mode of a `CALL { … }` source: the mode carries its
/// own facts, so a shared result cannot exist beside per-row execution.
enum Mode {
    /// Run the body per outer row.
    PerRow,
    /// The shared-execution fast path (LABS-198): the plan decided at
    /// construction, the one body result computed on the first open and
    /// reused for every outer row.
    Shared(SharedBody),
}

/// The shared mode's whole payload: the plan, the merge exclusion, and
/// the one lazily computed body result. `rows_for` is the entire
/// protocol — init-once through `Option::insert`, the outer row's merged
/// view — so `open`'s shared arm delegates and owns only the tail it
/// shares with the per-row path.
struct SharedBody {
    plan: SharedExecution,
    /// The merge's exclusion set — the plan's checked names (the seed's
    /// keys), derived once at construction (the admission test already
    /// needed it).
    check: HashSet<String>,
    /// The one body result, boxed: it dwarfs the rest. `None` until the
    /// first open computes it.
    body: Option<Box<QueryResult>>,
}

impl SharedBody {
    /// The outer row's merged rows. The one shared body result is
    /// computed on the first call — `Option::insert` hands back the
    /// placed reference, so init-once is one expression — and every
    /// outer row's merged view of it is built under the check-set
    /// exclusion.
    fn rows_for(
        &mut self,
        ctx: ExecCtx<'_>,
        row: &Row,
        query: &relate_ir::Query,
        scope: &relate_ir::CallScope,
        qctx: QueryCtx<'_>,
    ) -> Result<Vec<Row>> {
        let body: &QueryResult = match self.body.as_deref() {
            Some(existing) => existing,
            None => {
                count_shared_body_run();
                // The set the plan reduced at construction is the one the
                // body form and the arm strip both read.
                let body = super::call_body(scope.has_scope_clause().then_some(&self.check));
                let executed = super::execute_query_scoped(
                    ctx.graph,
                    query,
                    ctx.params,
                    self.plan.seed.clone(),
                    body,
                    qctx,
                )?;
                self.body.insert(Box::new(executed))
            }
        };
        // The map of an empty body result is an empty Vec — the pad tail
        // in `open` is the one empty-case decision.
        Ok(body
            .rows
            .iter()
            .map(|values| merge_body_export(row, &body.columns, values, &self.check))
            .collect())
    }
}

impl<'a> SubquerySource<'a> {
    /// Decide the execution mode once (the shared plan needs the whole
    /// input), then hand the source to the feed spine.
    fn new(
        graph: &ExecGraph<'_>,
        spec: SubquerySpec<'a>,
        rows: &[Row],
        qctx: QueryCtx<'a>,
    ) -> Result<Self> {
        let mode = match shared_execution_plan(graph, spec.query, spec.scope, rows)? {
            Some(plan) => {
                let check = plan.check();
                if import_invariant_body(spec.query, &check) {
                    Mode::Shared(SharedBody {
                        plan,
                        check,
                        body: None,
                    })
                } else {
                    Mode::PerRow
                }
            }
            None => Mode::PerRow,
        };
        Ok(Self {
            query: spec.query,
            optional: spec.optional,
            scope: spec.scope,
            qctx,
            mode,
            staged: Vec::new().into_iter(),
        })
    }
}

impl FeedSource for SubquerySource<'_> {
    fn open(&mut self, ctx: ExecCtx<'_>, row: Row) -> Result<bool> {
        // Cleared first: a body run that errors returns with nothing
        // staged, so an empty staging keeps its one meaning (the
        // degenerate open) and never the previous row's leftovers.
        self.staged = Vec::new().into_iter();
        // Both execution paths agree on the one tail: a body that produced
        // nothing stages the `OPTIONAL CALL` null pad (or nothing, for the
        // plain form); a producing body stages its rows.
        let produced: Vec<Row> = match &mut self.mode {
            Mode::PerRow => call_subquery_row(
                ctx.graph, &row, self.query, self.scope, ctx.params, self.qctx,
            )?,
            Mode::Shared(shared) => {
                shared.rows_for(ctx, &row, self.query, self.scope, self.qctx)?
            }
        };
        self.staged = if produced.is_empty() {
            pad_rows(ctx.graph, row, self.optional, self.query, self.scope)
        } else {
            produced.into_iter()
        };
        Ok(!self.staged.as_slice().is_empty())
    }

    fn charge_facts(&self) -> Result<(usize, usize)> {
        // An empty staging (the degenerate open) bills nothing — there is
        // no row to width.
        let staged = self.staged.as_slice();
        Ok((staged.len(), staged.first().map_or(0, Row::len)))
    }

    fn pull(&mut self, ctx: ExecCtx<'_>) -> Result<Option<Row>> {
        ctx.graph.cancel_tick()?;
        Ok(self.staged.next())
    }
}

/// The `OPTIONAL CALL` empty-body pad as staged rows: one null-padded row
/// for the optional form, nothing for the plain one.
fn pad_rows(
    graph: &ExecGraph<'_>,
    row: Row,
    optional: bool,
    query: &relate_ir::Query,
    scope: &relate_ir::CallScope,
) -> std::vec::IntoIter<Row> {
    empty_body_row(graph, row, optional, query, scope)
        .into_iter()
        .collect::<Vec<_>>()
        .into_iter()
}

/// One `CALL { … }` clause's product as the feed the caller consumes:
/// park it for a folding consumer (the projection pulls one merged row at
/// a time), or hold it charged otherwise (`StreamedFeed::materialize`,
/// ADR-0463). The `IN TRANSACTIONS` form never takes this path — it owns
/// its own batching and commits, and stays materializing.
pub(crate) fn call_subquery_feed<'a>(
    ctx: ExecCtx<'a>,
    qctx: QueryCtx<'a>,
    rows: Vec<Row>,
    spec: SubquerySpec<'a>,
    profiled: bool,
) -> Result<StreamedFeed<'a>> {
    let source = SubquerySource::new(ctx.graph, spec, &rows, qctx)?;
    Ok(StreamedFeed::new(ctx, rows, Box::new(source), profiled))
}

/// The clause's materializing form: the feed spine's `materialize` for a
/// consumer the product cannot be parked for (`exec_load_csv`'s shape) —
/// every follower but a projection, and a writing body whatever follows.
pub(crate) fn exec_call_subquery<'a>(
    ctx: ExecCtx<'a>,
    qctx: QueryCtx<'a>,
    rows: Vec<Row>,
    spec: SubquerySpec<'a>,
) -> Result<Vec<Row>> {
    call_subquery_feed(ctx, qctx, rows, spec, false)?.materialize()
}
