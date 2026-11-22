//! The dispatch outcome vocabulary (ADR-0319, ADR-0259, ADR-0502 §7): the
//! staged outcome types and their per-arm result companions every
//! dispatch path produces — extracted from `dispatch.rs` when that file's
//! growth crossed the 1k-line budget. The dispatch *inputs* (`DispatchCtx`,
//! the token-budget reservation) stayed beside their consumer in
//! `dispatch.rs`; no `Connection` method lives here.

use relate_executor::QueryResult as ExecResult;
use relate_planner::LogicalPlan;

use crate::gate::ResultStamp;
use crate::phase_timing::TimingStamp;
use crate::{Error, QueryResult, Result, ResultHeader};

/// A statement's staged outcome (ADR-0319: `Err` before dispatch, `Ok` with
/// the dispatch outcome after) together with whether it admitted its read
/// before it ran. `admitted` stays true when an admitted read then fails, so
/// its caller can skip the post-run health check for a read that already
/// pinned the last safe snapshot; a read that finished keeps its rows —
/// nothing after execution (the PROFILE estimates read included) may turn a
/// finished result back into an error.
pub(super) struct QueryOutcome {
    pub(super) result: Result<Executed>,
    pub(super) admitted: bool,
}

impl QueryOutcome {
    pub(super) fn not_admitted(result: Result<Executed>) -> Self {
        Self {
            result,
            admitted: false,
        }
    }

    /// The `Result<QueryOutcome>` → `QueryOutcome` tail every statement
    /// path ends in: a worker-spawn refusal, a worker panic (ADR-0259) or
    /// an inline panic (ADR-0504) becomes the not-admitted outcome the
    /// epilogue fails the transaction on. One mapping, shared by
    /// `dispatch_query` and both arms of the fast path.
    /// The staged execution failure, when the statement admitted, ran,
    /// and failed while executing (ADR-0319): the one arm a consumer
    /// reasoning about the run's own error reads — an `Err` result is a
    /// pre-execution refusal or a post-run resource charge, never the
    /// executor's verdict.
    pub(super) fn staged_failure(&self) -> Option<&ExecutionFailure> {
        match &self.result {
            Ok(Executed::Failed(failure)) => Some(failure),
            _ => None,
        }
    }

    /// The `Result<QueryOutcome>` → `QueryOutcome` tail every statement
    /// path ends in: a worker-spawn refusal, a worker panic (ADR-0259) or
    /// an inline panic (ADR-0504) becomes the not-admitted outcome the
    /// epilogue fails the transaction on. One mapping, shared by
    /// `dispatch_query` and both arms of the fast path.
    pub(super) fn from_result(result: Result<QueryOutcome>) -> Self {
        match result {
            Ok(outcome) => outcome,
            Err(error) => QueryOutcome::not_admitted(Err(error)),
        }
    }
}

/// What a running arm produced: the raw executor result with the commit
/// sequence its WAL commit was assigned (ADR-0502 §7). Only the auto-commit
/// write commits at the statement boundary; [`ArmOutput::read`] owns the
/// `None` of every arm that does not.
pub(super) struct ArmOutput {
    pub(super) result: ExecResult,
    pub(super) commit_sequence: Option<u64>,
}

impl ArmOutput {
    /// The output of an arm that commits nothing (read, explicit
    /// transaction): no commit sequence.
    pub(super) fn read(result: ExecResult) -> Self {
        Self {
            result,
            commit_sequence: None,
        }
    }

    /// The output of the auto-commit write: `commit_write`'s result paired
    /// with its commit sequence.
    pub(super) fn committed((result, commit_sequence): (ExecResult, Option<u64>)) -> Self {
        Self {
            result,
            commit_sequence,
        }
    }
}

/// What one dispatch arm produced: either a refused admission (the
/// statement never ran) or the run's own result ([`ArmOutput`]) with whether
/// the run admitted its read first. One type across the transaction, write and read arms, so
/// "refused", "ran admitted" and "ran not admitted" are variants, not a
/// nested `Result` beside a bare flag.
pub(super) enum ArmRun {
    /// The admission was refused (read guard, target, pin): the statement
    /// never ran. The one refusal path of every arm.
    Refused(Error),
    /// The arm admitted its read, then ran; the result may be a failure.
    Admitted(Result<ArmOutput>),
    /// The arm ran without a read admission (the auto-commit write).
    NotAdmitted(Result<ArmOutput>),
}

/// The outcome of a statement that parsed, validated and planned
/// (ADR-0319): it either produced its result or failed while executing.
#[derive(Debug)]
pub enum Executed {
    /// Execution finished; the buffered result.
    Rows(QueryResult),
    /// Execution of the planned statement failed.
    Failed(ExecutionFailure),
}

/// A statement that planned but failed while executing (ADR-0319): what a
/// surface needs to announce the result before it reports the error.
#[derive(Debug)]
pub struct ExecutionFailure {
    /// The planned result metadata — the same header a `QueryResult`
    /// carries: the output `columns`
    /// (`relate_planner::plan_output_names`), their `column_types`
    /// (`ANY` per column, ADR-0224's no-rows stamp), the plan-derived
    /// `query_type` (ADR-0162), the `namespace` (ADR-0117 §7), the
    /// `elapsed` (ADR-0221) and its PARSE / PLAN / EXECUTE phase split
    /// (LABS-832).
    pub header: ResultHeader,
    /// The execution error.
    pub error: Error,
}

impl Executed {
    /// Stage a dispatch-phase failure of `logical`. A planned statement's
    /// output names always derive (the UNION column-agreement check ran in
    /// `plan`), so [`plan_output_names_expect`] holds the one expect for
    /// that invariant — the same policy the planner's `query_type` uses
    /// (ADR-0319, ADR-0332).
    ///
    /// [`plan_output_names_expect`]: relate_planner::plan_output_names_expect
    pub(super) fn staging_failure(
        logical: &LogicalPlan,
        stamp: ResultStamp,
        timing: impl FnOnce() -> TimingStamp,
        error: Error,
    ) -> Executed {
        let columns = relate_planner::plan_output_names_expect(logical);
        Executed::Failed(ExecutionFailure {
            // The no-rows construction (`&[]`): every column stamps `ANY`
            // (ADR-0224), through the same one construction as every other
            // result.
            header: ResultHeader::new(columns, &[], stamp.qtype, stamp.namespace, timing),
            error,
        })
    }

    /// The finished result, or the execution error: the shape of the
    /// non-staged entry points (`query`, `query_with_params`, …).
    pub fn into_result(self) -> Result<QueryResult> {
        match self {
            Executed::Rows(result) => Ok(result),
            Executed::Failed(failure) => Err(failure.error),
        }
    }
}
