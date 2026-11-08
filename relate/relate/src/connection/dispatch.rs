//! Statement dispatch (ADR-0248, ADR-0344/0345 phase 2).
//!
//! The reservation → dispatch epilogue and the outcome vocabulary — the
//! dispatch context (`DispatchCtx`: stamp, start, PROFILE sink), the
//! per-transaction run facts, `QueryOutcome` and its `Executed` variants
//! — live here, beside the methods that consume them. The admission
//! sequence that produces the `Admission` bundle this epilogue reads
//! lives in `admission.rs` (ADR-0503's completion of this file's
//! stages), the cache itself in `cache.rs`, and `Connection`'s lifecycle
//! (transactions, namespace, pull) stays in the parent module.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use web_time::Instant;

use relate_executor::{
    execute_read_at, ExecGraph, ExecRequest, Graph, PlanMetricsSink, ReadAdmission, WriteScratch,
};

use relate_core::Namespace;
use relate_planner::{is_show_databases_with_next, LogicalPlan};

use super::admission::Admission;
use super::cache::CacheUse;
use super::txn::TxnRun;
use super::Connection;
use crate::commit::commit_write;
use crate::gate::ResultStamp;
use crate::phase_timing::TimingStamp;
use crate::{CancelToken, Error, QueryResult, Result, ResultHeader, Value};

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
    pub(super) fn from_result(result: Result<QueryOutcome>) -> Self {
        match result {
            Ok(outcome) => outcome,
            Err(error) => QueryOutcome::not_admitted(Err(error)),
        }
    }
}

/// The statement's token-budget reservation as one owner: the reserved
/// ledger and its observation key as one pair — always `Some` together —
/// plus the consuming half of its lifecycle, `settle` (observe the peak,
/// keep the ledger alive on a finished result). The two ambient steps —
/// charging the retained rows and attaching the receipt — stay the free
/// functions below: they run on the thread-local ledger the admission
/// guard installs, and `LedgerGuard`'s exclusive-borrow lifetime cannot
/// be co-owned with the ledger it guards, so the owner is not borrowable
/// while the guard is armed. Constructed by
/// [`Connection::reserve_statement`].
struct StatementReservation {
    ledger: relate_core::tokens::TokenLedger,
    key: crate::token_budget::PeakKey,
    /// Plan bytes reserved for an uncached run. Dropped with the
    /// reservation, which is the end of the statement.
    plan: Option<crate::token_budget::PlanHold>,
}

impl StatementReservation {
    /// Settle the reservation: observe the ledger's peak against its key,
    /// then hand the ledger to the finished result it charged (its charge
    /// transfers with the rows); any other outcome drops it here. Runs
    /// after the admission guard dropped — the ledger moves once the
    /// window closed.
    fn settle(self, budget: &crate::token_budget::TokenBudget, outcome: &mut QueryOutcome) {
        budget.observe(
            self.key,
            self.ledger
                .receipt(relate_core::TokenOutcome::Success)
                .peak_logical_bytes,
        );
        if let Ok(Executed::Rows(result)) = outcome.result.as_mut() {
            result._retained_ledger = Some(Box::new(self.ledger));
        }
    }
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

impl ExecutionFailure {
    /// Stage a dispatch-phase failure of `logical`. A planned statement's
    /// output names always derive (the UNION column-agreement check ran in
    /// `plan`), so [`plan_output_names_expect`] holds the one expect for
    /// that invariant — the same policy the planner's `query_type` uses
    /// (ADR-0319, ADR-0332).
    ///
    /// [`plan_output_names_expect`]: relate_planner::plan_output_names_expect
    pub(crate) fn staged(
        logical: &LogicalPlan,
        stamp: ResultStamp,
        timing: impl FnOnce() -> TimingStamp,
        error: Error,
    ) -> Executed {
        let columns = relate_planner::plan_output_names_expect(logical);
        Executed::Failed(Self {
            // The no-rows construction (`&[]`): every column stamps `ANY`
            // (ADR-0224), through the same one construction as every other
            // result.
            header: ResultHeader::new(columns, &[], stamp.qtype, stamp.namespace, timing),
            error,
        })
    }
}

impl Executed {
    /// The finished result, or the execution error: the shape of the
    /// non-staged entry points (`query`, `query_with_params`, …).
    pub fn into_result(self) -> Result<QueryResult> {
        match self {
            Executed::Rows(result) => Ok(result),
            Executed::Failed(failure) => Err(failure.error),
        }
    }
}

/// The dispatch-only inputs bundled so `dispatch` stays under
/// `clippy::too_many_arguments`: the stamp; the statement's start
/// instant (ADR-0221), whose elapsed is stamped at result
/// construction; the admission-measured timing facts (LABS-832's
/// PARSE / PLAN durations with LABS-1120's fine sub-split and the
/// plan-cache outcome), which join the dispatch-measured EXECUTE at
/// the same construction — the one site that builds the complete
/// split; the admission-resolved administration fact (ADR-0116 §2)
/// the dispatch target selection reads; and the PROFILE metrics sink
/// (ADR-0344 phase 2) the dispatch's request sites read.
pub(super) struct DispatchCtx<'a> {
    stamp: ResultStamp,
    started: Instant,
    /// The carried admission fact: whether the statement is a
    /// database-administration command (ADR-0116 §2).
    admin: bool,
    /// The admission-measured timing facts (LABS-832 + LABS-1120): PARSE
    /// and PLAN with the fine sub-split and the plan-cache outcome.
    timing: crate::phase_timing::AdmissionTiming,
    /// ADR-0344 phase 2: the PROFILE metrics sink when the statement was
    /// `PROFILE`; `None` on every other run. Rides the ctx like the
    /// stamp: built and coerced to the trait object once in
    /// `run_query`, read at the dispatch's request sites.
    profile_metrics: Option<&'a dyn PlanMetricsSink>,
}

/// Result columns of an auto-parameterized plan name the placeholder
/// (`$__AUTO_INT_0__`). The statement's own return items already carry
/// the column text (`1 + 2`, `(1 + 2)`), so use those instead of the
/// rewritten expression.
fn restore_authored_column_names(cypher: &str, columns: &mut [String]) {
    if !columns.iter().any(|column| column.contains("__AUTO_")) {
        return;
    }
    let Ok(statement) = relate_parser::parser::parse(cypher) else {
        return;
    };
    let Some(items) = final_return_items(&statement) else {
        return;
    };
    if items.len() != columns.len() {
        return;
    }
    for (column, item) in columns.iter_mut().zip(items) {
        if !column.contains("__AUTO_") {
            continue;
        }
        if let Some(alias) = &item.alias {
            *column = alias.to_string();
        } else if let Some(source) = &item.source_text {
            *column = source.clone();
        }
    }
}

/// The return items whose names are the result columns: the final NEXT
/// stage, and within it the last RETURN (a UNION result takes the left
/// arm's names).
fn final_return_items(
    statement: &relate_parser::Statement,
) -> Option<&[relate_parser::ReturnItem]> {
    let mut stage = statement;
    while let Some(next) = &stage.next {
        stage = next;
    }
    stage.clauses.iter().rev().find_map(|clause| match clause {
        relate_parser::Clause::Return(ret) => Some(ret.items.as_slice()),
        _ => None,
    })
}

/// The admission gates between the EXPLAIN early return and dispatch: the
/// ADR-0308 parameter-completeness check and the SHOW DATABASES + NEXT
/// restriction. One named seam, so `run_query` reads as
/// parse → plan → gates → dispatch.
fn post_explain_gates(logical: &LogicalPlan, params: &BTreeMap<String, Value>) -> Result<()> {
    // ADR-0308: a referenced parameter the caller did not supply fails the
    // statement before anything executes (EXPLAIN, above, plans without
    // values, as in Neo4j).
    let missing: Vec<&str> = relate_planner::plan_parameter_names(logical)
        .into_iter()
        .filter(|name| !params.contains_key(*name))
        .collect();
    if !missing.is_empty() {
        return Err(Error::parameters_missing(missing));
    }
    // SHOW DATABASES is a system-database command (ADR-0116 §2); combining
    // it with a NEXT stage is an execution-phase rule, so it does not apply
    // to EXPLAIN. The §2 classification lives in the planner's namespaces
    // module.
    if is_show_databases_with_next(logical) {
        return Err(Error::execution(
            "SystemDatabaseRequired: SHOW DATABASES cannot be combined with NEXT",
        ));
    }
    Ok(())
}

/// Attach the admission receipt to a finished result, refusing the
/// outcome when the token ledger poll fails — and, for a result that
/// carries a plan (EXPLAIN/PROFILE, the render stage's output), stamp
/// the plan root's peak-workspace estimate from the same receipt: one
/// seam reads the receipt, both facts land. Runs on the thread-local
/// ledger the admission guard installed (see [`StatementReservation`]'s
/// doc for why this is not a method on the owner).
fn attach_receipt(outcome: &mut QueryOutcome) {
    if let Err(error) = relate_core::tokens::poll() {
        outcome.result = Err(error);
        return;
    }
    let Some(receipt) = relate_core::tokens::current_receipt(relate_core::TokenOutcome::Success)
    else {
        return;
    };
    let Ok(Executed::Rows(result)) = outcome.result.as_mut() else {
        return;
    };
    result.receipt = Some(receipt);
    if let Some(plan) = result.plan.as_mut() {
        plan.root.token_estimate = Some(crate::TokenEstimate {
            peak_logical_bytes: receipt.peak_logical_bytes,
        });
    }
}

/// Charge a finished statement's retained rows for the workspace they
/// hold. An auto-commit write is exempt: it charged its result before the
/// WAL boundary in `commit_write`, so it cannot report a retryable token
/// failure after committing; explicit transactions charge here because
/// their write has not committed until a later `COMMIT`. Runs on the
/// thread-local ledger the admission guard installed.
fn retain_result_rows(outcome: &QueryOutcome) -> Result<()> {
    let Ok(Executed::Rows(result)) = &outcome.result else {
        return Ok(());
    };
    let charge = relate_core::tokens::WorkspaceCharge::retain_with(|| {
        relate_executor::result_rows_bytes(&result.rows)
    })?;
    charge.transfer_to_ledger();
    Ok(())
}

impl Connection {
    /// The query body, run on the connection's worker thread (ADR-0259).
    /// The five-stage narrative: admit → EXPLAIN → gates → dispatch →
    /// PROFILE epilogue. Each stage is one named call, and the admission
    /// facts ride `Admission`/`DispatchCtx` instead of being re-derived.
    /// The stages after admission live in [`Self::run_admitted`], shared
    /// with the connection-thread fast path (ADR-0504).
    pub(super) fn run_query(
        &mut self,
        cypher: &str,
        params: BTreeMap<String, Value>,
        token: &CancelToken,
    ) -> QueryOutcome {
        let admission = match self.admit_statement(cypher, &params, CacheUse::Query) {
            Ok(admission) => admission,
            // The plain not-admitted outcome: `run_cancellable`'s
            // epilogue owns failing the open transaction for every
            // non-Ok result (with the retryable-refusal classifier), so
            // no statement body pre-empts it.
            Err(e) => return QueryOutcome::not_admitted(Err(e)),
        };
        self.run_admitted(admission, cypher, params, token)
    }

    /// The query body after admission (ADR-0504): the EXPLAIN early
    /// return, the post-EXPLAIN gates, the token reservation, the
    /// dispatch and the PROFILE epilogue. One body shared by every path —
    /// `run_query` (the worker body of an unasserted connection), the
    /// inline fast path of a worker-stack-asserted connection, and that
    /// path's escalation to the worker with the admission already in
    /// hand — so no path can re-admit a statement and no gate can be
    /// skipped between paths.
    pub(super) fn run_admitted(
        &mut self,
        mut admission: Admission,
        cypher: &str,
        params: BTreeMap<String, Value>,
        token: &CancelToken,
    ) -> QueryOutcome {
        // EXPLAIN plans and compile-time validates the wrapped statement,
        // then renders the plan — execution never touches it (no writes, no
        // procedures, no LOAD CSV). The SHOW DATABASES + NEXT restriction is
        // an execution-phase rule, so it does not apply here.
        if admission.preamble.explain() {
            // The plain not-admitted outcome: `run_cancellable`'s
            // epilogue owns failing the open transaction for every
            // non-Ok result, so this body does not pre-empt it.
            return match self.explain_run(cypher, &params, &admission) {
                Ok(result) => QueryOutcome::not_admitted(Ok(Executed::Rows(result))),
                Err(e) => QueryOutcome::not_admitted(Err(e)),
            };
        }
        // ADR-0503: a hit's effective parameter map — the caller's own
        // plus the extracted `$__AUTO_*__` values, built once where the
        // gates ran — rides out with the admission, so this path merges
        // and copies nothing. The token budget's key deliberately stays
        // on the caller's own map: the budget remembers peaks per
        // `(commit_ts, statement, params)` identity, and a cached retry
        // of the same statement must resolve to the same identity the
        // first, uncached run recorded its peak under — the caller's
        // undisturbed map, still owned here, is exactly that identity.
        let budget_params = &params;
        let effective = admission.effective.take();
        let params = effective.as_ref().unwrap_or(budget_params);
        // Taken before the destructure below so the epilogue can still
        // borrow `admission`. `Cached` is the already-handled state.
        let memory = std::mem::replace(&mut admission.memory, super::cache::PlanMemory::Cached);
        // The budget's own key reads the caller's undisturbed map —
        // `budget_params`, never the effective map the shadow names: the
        // PeakKey hashes the whole map, so keying the reservation by the
        // merged map would give a cached retry a different identity than
        // its first, uncached run (found in review).
        // One exhaustive destructure — a new `Admission` field forces a
        // decision here, not a silent `..` drop. The plan borrows (`ref`):
        // the epilogue below carries the whole admission, and every other
        // field is `Copy`.
        let Admission {
            started,
            timing_on,
            preamble,
            admin,
            logical,
            stamp,
            timing,
            effective: _,
            memory: _,
        } = &admission;
        let started = *started;
        let timing_on = *timing_on;
        let profile = preamble.profile();
        let admin = *admin;
        let stamp = *stamp;
        // LABS-1120: the admission's timing facts ride as the one carrier
        // — the same bundle the stamping tail consumes, no re-shaping here.
        let timing = *timing;
        let logical = logical.as_ref();
        // `commit_write` reserves returned rows before it appends to the
        // WAL. Explicit transactions still reserve here: their write has
        // not committed until a later `COMMIT`.
        let auto_commit_write = stamp.write_route && self.txn.is_none();
        // The admission gates between the EXPLAIN early return and dispatch:
        // the ADR-0308 parameter-completeness check and the ADR-0116 §2
        // SHOW DATABASES + NEXT restriction. One named seam.
        if let Err(e) = post_explain_gates(logical, params) {
            return QueryOutcome::not_admitted(Err(e));
        }
        let mut reservation = match self.reserve_statement(cypher, budget_params) {
            Ok(reservation) => reservation,
            Err(error) => return QueryOutcome::not_admitted(Err(error)),
        };
        if let Err(error) = self.charge_plan(memory, &mut reservation) {
            return QueryOutcome::not_admitted(Err(error));
        }
        // The admission guard: installs the reserved ledger on this thread
        // for the statement's execution — a local (and the engaged check
        // below) because its exclusive-borrow lifetime cannot be co-owned
        // with the ledger it guards.
        let guard = reservation
            .as_mut()
            .map(|reservation| relate_core::tokens::LedgerGuard::install(&mut reservation.ledger));
        super::admit_hold();
        let parse_plan_ns = crate::commit::elapsed_nanos(timing_on.then_some(started));
        // PROFILE runs the statement through the normal dispatch, then
        // appends the plan and execution statistics to its result. The
        // result keeps the wrapped statement's query type (ADR-0162).
        let metrics = profile.then(crate::profile::ProfileMetrics::new);
        // The one concrete→dyn coercion (ADR-0344 phase 2): every request
        // site carries the trait object; the concrete local stays here for
        // the PROFILE epilogue.
        let profile_metrics: Option<&dyn PlanMetricsSink> =
            metrics.as_ref().map(|m| m as &dyn PlanMetricsSink);
        let ctx = DispatchCtx {
            stamp,
            started,
            admin,
            timing,
            profile_metrics,
        };
        let mut outcome = self.dispatch(logical, params, parse_plan_ns, cypher, token, ctx);
        // Read and explicit-transaction results are charged here; an
        // auto-commit write charged its result before the WAL boundary in
        // `commit_write` (`retain_result_rows` owns that exemption).
        if guard.is_some() && !auto_commit_write {
            if let Err(error) = retain_result_rows(&outcome) {
                outcome.result = Err(error);
            }
        }
        // The PROFILE epilogue runs only for a PROFILE statement —
        // `metrics` is `Some` exactly then (`profile`'s one use built
        // it), so the carrier decides and the non-PROFILE case skips
        // the call. The admission stays whole inside the epilogue
        // (ADR-0504's shared tail).
        let mut outcome = if let Some(metrics) = metrics {
            self.profile_epilogue(outcome, &admission, params, cypher, &metrics)
        } else {
            outcome
        };
        if guard.is_some() {
            attach_receipt(&mut outcome);
        }
        drop(guard);
        if let Some(reservation) = reservation {
            reservation.settle(&self.inner.resolved.budget, &mut outcome);
        }
        if let Ok(Executed::Rows(result)) = outcome.result.as_mut() {
            restore_authored_column_names(cypher, &mut result.header.columns);
        }
        outcome
    }

    /// Reserve the statement's token budget ([`StatementReservation`]'s
    /// constructor): `None` when the budget is off — the off state's one
    /// representation, and the off path pays no key construction
    /// (`TokenBudget::reserve_keyed` constructs the key only when on) —
    /// the reserved ledger and its observation key otherwise. The
    /// admission guard is armed by the caller — see the owner's doc.
    fn reserve_statement(
        &self,
        cypher: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<Option<StatementReservation>> {
        Ok(self
            .inner
            .resolved
            .budget
            .reserve_keyed(|| self.inner.commit_ts(), cypher, params)
            .transpose()?
            .map(|(ledger, key)| StatementReservation {
                ledger,
                key,
                plan: None,
            }))
    }

    /// Charge the plan that is about to run. A hit is already on the cache
    /// ledger, and the statement reservation above is the workspace check
    /// that sees that charge. An original compile reserves its plan on the
    /// statement. A cacheable miss reserves on the cache ledger only when
    /// the admission ceiling still has room.
    fn charge_plan(
        &self,
        memory: super::cache::PlanMemory,
        reservation: &mut Option<StatementReservation>,
    ) -> Result<()> {
        use super::cache::PlanMemory;
        use crate::token_budget::HoldAttempt;

        let budget = &self.inner.resolved.budget;
        match memory {
            PlanMemory::Cached => Ok(()),
            PlanMemory::Statement(bytes) => match budget.try_hold_admitted(bytes) {
                HoldAttempt::Off => Ok(()),
                HoldAttempt::Held(hold) => {
                    if let Some(reservation) = reservation {
                        reservation.plan = Some(hold);
                    }
                    Ok(())
                }
                HoldAttempt::Refused => Err(budget.plan_refusal(bytes)),
            },
            PlanMemory::Cacheable(pending) => {
                let bytes = pending.bytes;
                match budget.try_hold_admitted(bytes) {
                    HoldAttempt::Off => {
                        self.publish_cache_entry(pending, None);
                        Ok(())
                    }
                    HoldAttempt::Held(hold) => {
                        self.publish_cache_entry(pending, Some(hold));
                        Ok(())
                    }
                    HoldAttempt::Refused => {
                        if let HoldAttempt::Held(hold) = budget.try_hold_grow(bytes) {
                            if let Some(reservation) = reservation {
                                reservation.plan = Some(hold);
                            }
                        }
                        Ok(())
                    }
                }
            }
        }
    }

    /// The normal statement dispatch: explicit-transaction overlay,
    /// auto-commit write, or snapshot read — routed by the statement's
    /// target namespace (ADR-0116 §3, §4, §10; ADR-0117 §2): the resolved
    /// `USE` target, or the session namespace when the statement carries no
    /// `USE`. Only `neo4j` and `system` targets (and administration
    /// commands, which always use the `neo4j` catalog) reach here — every
    /// non-administration statement routed to `system_ontologies` was
    /// rejected by the namespace gate above the dispatch split
    /// (ADR-0229 §1). Each arm produces the raw executor result, its
    /// admission flag, and — for the auto-commit write arm — the commit
    /// sequence the statement's WAL commit was assigned (ADR-0502 §7);
    /// the `stamp` metadata derived above the
    /// EXPLAIN/dispatch split is applied once, at the single tail, so no
    /// arm can return an unstamped `QueryResult`. (`stamp` is bundled to
    /// avoid `clippy::too_many_arguments`.)
    fn dispatch(
        &mut self,
        logical: &LogicalPlan,
        params: &BTreeMap<String, Value>,
        parse_plan_ns: u64,
        cypher: &str,
        token: &CancelToken,
        ctx: DispatchCtx,
    ) -> QueryOutcome {
        // LABS-832: the EXECUTE phase — this whole dispatch stage (target
        // selection, pinning, the run itself), stamped at the single tail
        // every arm funnels through, beside the ADR-0221 total.
        let execute_started = Instant::now();
        let namespace = ctx.stamp.namespace;
        // ADR-0116 §2: the carried admission fact — administration commands
        // run from any namespace against the store's catalog (the `neo4j`
        // graph), so they are exempt from the `system` /
        // `system_ontologies` clause rules (enforced in
        // `plan_gate_classify`, above the EXPLAIN/dispatch split).
        let admin = ctx.admin;
        // The write arm carries its commit sequence (ADR-0502 §7) beside
        // the executor result; every other arm pairs its result with
        // `None` — nothing it runs commits at the statement boundary.
        let (result, admitted) = if let Some(txn) = self.txn.as_ref() {
            // `begin` pinned the transaction's view, and that pin keeps the
            // GraphCell from being replaced for the transaction's lifetime,
            // so deriving the target here is safe without a fresh guard.
            let target = match self.dispatch_target(admin, namespace) {
                Ok(target) => target,
                Err(e) => return QueryOutcome::not_admitted(Err(e)),
            };
            let shared = Arc::clone(&self.inner);
            // `begin` already pinned the transaction's view. Only admission
            // must be serialized with poison publication, not its execution.
            let schema = match shared.read_guard() {
                Ok(_read) => target.schema_snapshot(),
                Err(e) => return QueryOutcome::not_admitted(Err(e)),
            };
            #[cfg(test)]
            shared.pause_admitted_read();
            (
                self.query_in_txn(
                    TxnRun {
                        txn,
                        target,
                        schema: &schema,
                        profile_metrics: ctx.profile_metrics,
                    },
                    logical,
                    params,
                    cypher,
                    token,
                )
                .map(|r| (r, None)),
                true,
            )
        } else if ctx.stamp.write_route {
            // Only reachable when the statement targets the `neo4j` graph:
            // the other namespaces reject writes above, and administration
            // commands target `neo4j`. `write_route` (computed in
            // `plan_gate_classify` under the read guard) is `is_write` plus
            // the ontology write procedures, whose redo lands through the
            // same group-commit pipeline (ADR-0120 §3).
            // ADR-0209 batched-vs-single commit strategy selection lives in
            // commit.rs (`commit_write`).
            (
                commit_write(
                    &self.inner,
                    // The statement's execution facts, the same bundle
                    // `commit_optimistic` executes.
                    &ExecRequest {
                        plan: logical,
                        params,
                        query_text: cypher,
                        principal: self.principal.as_deref(),
                        batch_commit: None,
                        plan_metrics: ctx.profile_metrics,
                    },
                    parse_plan_ns,
                    token,
                ),
                false,
            )
        } else {
            // The target is selected and pinned under the read guard: the
            // `neo4j` arm derefs the GraphCell, whose contract requires the
            // guard (a follow reload may `replace` the graph otherwise,
            // ADR-0137); the pin then keeps the cell alive for `target`'s
            // later use. The (admin, namespace) dispatch decision is made
            // exactly once, here.
            let target: &Graph;
            // The ontology document the statement reads (ADR-0121 §3,
            // ADR-0227): store-level state captured together with the graph
            // pin under the read guard, so an auto-commit statement sees a
            // consistent admission-time document (explicit transactions use
            // the begin-time capture in `query_in_txn`). A shared handle:
            // the capture is an `Arc` pointer clone, not a document copy.
            let ontology_document: Option<Arc<str>>;
            let (pin, schema) = match self.inner.read_guard() {
                Ok(_read) => {
                    // The same target vocabulary every dispatch site
                    // reads (dispatch_target, ADR-0229 §1): the
                    // auto-commit read path routes through it too, its
                    // error mapped to the outcome form.
                    target = match self.dispatch_target(admin, namespace) {
                        Ok(target) => target,
                        Err(e) => return QueryOutcome::not_admitted(Err(e)),
                    };
                    ontology_document = self.inner.ontology.get();
                    match target.pin_now() {
                        Ok(pin) => (pin, target.schema_snapshot()),
                        Err(e) => return QueryOutcome::not_admitted(Err(e)),
                    }
                }
                Err(e) => return QueryOutcome::not_admitted(Err(e)),
            };
            #[cfg(test)]
            self.inner.pause_admitted_read();
            (
                execute_read_at(&ReadAdmission {
                    graph: target,
                    snapshot_ts: pin.ts(),
                    schema: &schema,
                    request: ExecRequest {
                        plan: logical,
                        params,
                        query_text: cypher,
                        principal: self.principal.as_deref(),
                        batch_commit: None,
                        plan_metrics: ctx.profile_metrics,
                    },
                    cancel: Some(token),
                    ontology_document: ontology_document.as_deref(),
                })
                .map(|r| (r, None)),
                true,
            )
        };
        // ADR-0319: a failure here is execution-phase — the statement was
        // planned and dispatched — so it is staged as `Executed::Failed`
        // (a planned statement's output names derive, so the staging keeps
        // the result; the one path that cannot derive them reports the
        // statement's error before any result).
        // The phase split completes here (LABS-832 + LABS-1120): both
        // outcome arms — the finished rows and the staged failure — carry
        // the same admission-measured PARSE / PLAN (with the fine sub-split
        // and the plan-cache outcome) as one timing stamp with the
        // ADR-0221 total. The stamping policy is spelled once, in
        // `TimingStamp::stamped` (phase_timing.rs).
        let result_gen_started = Instant::now();
        // Both EXECUTE windows close at the same instant, inside whichever
        // arm runs — the window IS the arm's construction work (the header
        // build, the staged columns) — so the stamp closure is handed to
        // the constructor, not called here: `ResultHeader::new` evaluates
        // it after its own per-column stamp, so the windows cover the
        // build (LABS-1120). Calling it as an argument would close the
        // window before the build it documents and report a near-zero
        // value. One clock stamp for both durations, so the sub-window
        // containment ADR-0540 documents is structural: EXECUTE spans the
        // dispatch plus the result build, `result_generation` is its tail
        // — `result_gen_started` is at or after `execute_started`, and
        // both durations read the same `tail`, so the fine window can
        // never exceed its coarse phase.
        let stamp_tail = || {
            let tail = Instant::now();
            TimingStamp::stamped(
                ctx.started,
                crate::phase_timing::DispatchWindows {
                    execute: tail.duration_since(execute_started),
                    result_generation: tail.duration_since(result_gen_started),
                },
                ctx.timing,
            )
        };
        let result = match result {
            // The auto-commit write's commit sequence (ADR-0502 §7) stamps
            // the result's bookmark at the same single tail — every other
            // arm's `None` leaves the result bookmark-free.
            Ok((r, commit_sequence)) => Ok(Executed::Rows(
                QueryResult::from_exec(r, ctx.stamp.namespace, ctx.stamp.qtype, stamp_tail)
                    .with_commit_sequence(commit_sequence),
            )),
            // `staged` derives the planned output names (the UNION
            // column-agreement check ran in `plan`), so a planned statement
            // always stages its failure with its columns.
            Err(error) => Ok(ExecutionFailure::staged(
                logical, ctx.stamp, stamp_tail, error,
            )),
        };
        QueryOutcome { result, admitted }
    }

    /// The view a statement executes — and its notification derivation
    /// reads — against (ADR-0363 decision 5): the explicit transaction's
    /// write overlay when the target namespace is writable and a scratch is
    /// supplied, else the committed graph at its commit timestamp. One
    /// constructor for `query_in_txn` and [`Self::notification_view`], so
    /// the two cannot pick differently.
    pub(super) fn statement_view<'a>(
        target: &'a Graph,
        scratch: Option<&'a RefCell<WriteScratch>>,
    ) -> ExecGraph<'a> {
        match scratch {
            Some(scratch) if target.namespace().is_writable() => ExecGraph::write(target, scratch),
            _ => ExecGraph::read_at(target, target.commit_ts()),
        }
    }

    /// The view a statement's notification derivation reads (ADR-0363
    /// decision 5): the same [`Self::statement_view`] `query_in_txn`
    /// executes against, from the transaction's scratch when one is open.
    /// The read guard is a parameter, not an internal acquisition — the
    /// `neo4j` arm of `dispatch_target` below derefs the `GraphCell`,
    /// whose ADR-0137 contract requires a guard or a live pin, so the
    /// caller (the render stage) hands its held guard in and the
    /// precondition is structural: the returned view borrows the guard
    /// (`'a`), so the call cannot be made unguarded and the guard cannot
    /// be dropped while the view lives.
    pub(super) fn notification_view<'a>(
        &'a self,
        _read: &'a crate::shared::ReadGuard<'_>,
        admission: &Admission,
    ) -> Result<ExecGraph<'a>> {
        let target = self.dispatch_target(admission.admin, admission.stamp.namespace)?;
        Ok(Self::statement_view(
            target,
            self.txn.as_ref().map(|txn| &txn.scratch),
        ))
    }

    /// The graph a statement in an explicit transaction runs against
    /// (ADR-0116 §3/§10): administration commands always use the store's
    /// `neo4j` graph from any namespace (ADR-0116 §2); a `system` session
    /// reads the store's empty `system` graph. A `system_ontologies`
    /// statement never reaches here — the namespace gate in
    /// `plan_gate_classify` rejected every non-administration statement
    /// routed there (ADR-0229 §1) — so that arm fails closed instead of
    /// guessing a graph. The `neo4j` arm derefs the GraphCell, so call
    /// only with a read guard held or a live pin (ADR-0137). (The
    /// auto-commit read path selects its target inline in `dispatch`.)
    fn dispatch_target(&self, admin: bool, namespace: Namespace) -> Result<&Graph> {
        match (admin, namespace) {
            (true, _) | (false, Namespace::Neo4j) => Ok(&self.inner.graph),
            (false, Namespace::System) => Ok(&self.inner.system),
            (false, Namespace::SystemOntologies) => {
                Err(Error::internal("system_ontologies is not queryable"))
            }
        }
    }
}
