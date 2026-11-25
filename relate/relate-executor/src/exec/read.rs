//! The read-path statement entries (ADR-0094's pinned admission, ADR-0682's
//! batch consumer): [`execute_read_at`] runs a read-only plan at the
//! caller-pinned timestamp; [`execute_read_streaming_at`] is the same run
//! with the statement's batch consumer attached.

use relate_core::{Error, QueryClockGuard, Result, Value};

use crate::consumer::{BatchFlow, StreamSink, BATCH_ROWS};
use crate::ctx::QueryCtx;
use crate::graph::QueryStats;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::dispatch::{execute_part_read_streamed, streamable_part};
use super::{
    execute_plan_read_seeded, finish_statement, plan_mutates_store, QueryResult, ReadAdmission,
};

/// Execute a read-only plan at the caller-pinned timestamp in `admission`
/// without repinning. The caller must keep its `Graph::pin_now()` guard alive
/// through this call so version GC cannot discard rows visible at
/// `snapshot_ts`, and must capture `schema` alongside that pin while
/// serializing with graph publication.
///
/// There is deliberately no graph-only wrapper that acquires both itself: the
/// publication read guard lives on the embedded database's `Shared` state, not
/// on a bare `Graph`, so such a wrapper could not capture the pin and schema
/// atomically. Admission must happen under that guard at the caller.
pub fn execute_read_at(admission: &ReadAdmission<'_>) -> Result<QueryResult> {
    execute_read_inner(admission, None)
}

/// [`execute_read_at`] with the statement's batch consumer attached
/// (ADR-0682): a streaming statement's terminal `RETURN` hands each bounded
/// batch to the sink as it is produced (see [`streamable_part`]); every
/// other read shape runs materialized as ever and its result replays
/// through the same sink — columns once, then bounded batches — before the
/// call returns. Either way the returned [`QueryResult`] carries the
/// statement's columns, stats and effect but no rows: the consumer has
/// them. A consumer answering [`BatchFlow::Stop`](crate::BatchFlow) ends
/// the walk and cancels the statement (`Error::Cancelled`).
///
/// A `PROFILE` statement (a plan-metrics sink on the request) never
/// engages the consumer: its rows land in the result as ever, so the
/// profiled run's answer is unchanged.
pub fn execute_read_streaming_at(
    admission: &ReadAdmission<'_>,
    sink: &mut StreamSink<'_>,
) -> Result<QueryResult> {
    execute_read_inner(admission, Some(sink))
}

fn execute_read_inner(
    admission: &ReadAdmission<'_>,
    sink: Option<&mut StreamSink<'_>>,
) -> Result<QueryResult> {
    let view = ExecGraph::read_at(admission.graph, admission.snapshot_ts)
        .with_schema(admission.schema)
        .with_cancel(admission.cancel)
        .with_ontology_document(admission.ontology_document);
    view.check_cancelled()?;
    // A read view carries no scratch, so the resets are vacuous and this
    // call is the declared-name seeding — kept through the same one
    // statement-entry operation every write entry takes, never a second
    // spelling of it.
    view.begin_statement(admission.request.plan);
    let _clock = QueryClockGuard::new();
    // A read admission records its write-attempt fact from the same canonical
    // plan walk as every write entry — never a hardcoded "a read cannot
    // write": a mutation operator reaching this route is still an attempt (the
    // read-only scratch refuses to apply it). A read view applies nothing, so
    // its applied count is the zero the admission starts with.
    let attempted = plan_mutates_store(admission.request.plan);
    // ADR-0395: the freshly constructed view carries a zeroed counter —
    // the read admission's statement gets its own budget.
    let qctx =
        QueryCtx::new(admission.request.query_text).with_principal(admission.request.principal);
    let plan = admission.request.plan;
    let params = admission.request.params;
    let seeds = vec![Row::new()];
    let finish = |mut result: QueryResult| {
        result.stats = QueryStats::default();
        finish_statement(result, &view, attempted)
    };
    match sink {
        // A PROFILE run keeps the materialized terminal end to end: its
        // result rows feed the epilogue's plan enrichment, so the consumer
        // stays disengaged and the caller replays them itself.
        Some(sink) if admission.request.plan_metrics.is_none() => {
            let result = match streamable_part(plan)? {
                Some(part) => execute_part_read_streamed(&view, part, params, seeds, qctx, sink)?,
                None => execute_plan_read_seeded(&view, plan, params, seeds, qctx, None)?,
            };
            let mut result = finish(result)?;
            // A shape that did not stream (or a streamed part whose RETURN
            // input could not take the per-batch form) replays its rows.
            if !sink.engaged() {
                replay_result(sink, &mut result)?;
            }
            Ok(result)
        }
        _ => finish(execute_plan_read_seeded(
            &view,
            plan,
            params,
            seeds,
            qctx,
            admission.request.plan_metrics,
        )?),
    }
}

/// The materialized terminal's consumer delivery (ADR-0682): a statement
/// whose shape does not stream (a `UNION` fold, a breaker, a catalog
/// command) still answers its consumer — the columns once, then the result
/// rows in bounded batches — and the result keeps no second copy. A
/// consumer answering `Stop` cancels the finished statement, the same
/// answer the streaming terminal gives.
fn replay_result(sink: &mut StreamSink<'_>, result: &mut QueryResult) -> Result<()> {
    sink.columns(&result.columns)?;
    let rows = std::mem::take(&mut result.rows);
    let mut rows = rows.into_iter();
    loop {
        let batch: Vec<Vec<Value>> = rows.by_ref().take(BATCH_ROWS).collect();
        if batch.is_empty() {
            break;
        }
        if matches!(sink.batch(batch)?, BatchFlow::Stop) {
            return Err(Error::cancelled());
        }
    }
    Ok(())
}
