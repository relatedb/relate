//! Statement dispatch (ADR-0248, ADR-0344/0345 phase 2).
//!
//! The reservation → dispatch epilogue lives here, beside the methods
//! that consume it; the outcome vocabulary it produces (`QueryOutcome`,
//! `Executed`, the per-arm companions) lives in `outcome.rs`, extracted
//! when this file crossed the 1k-line budget. The admission sequence
//! that produces the `Admission` bundle this epilogue reads lives in
//! `admission.rs` (ADR-0503's completion of this file's stages), the
//! cache itself in `cache.rs`, and `Connection`'s lifecycle
//! (transactions, namespace, pull) stays in the parent module.

use std::cell::RefCell;
use std::collections::BTreeMap;

use web_time::Instant;

use relate_executor::{
    execute_read_at, ExecGraph, ExecRequest, Graph, PlanMetricsSink, ReadAdmission, WriteScratch,
};

use relate_core::Namespace;
use relate_error::{error_info, TYPE_ERROR_CODE};
use relate_parser::PreambleMode;
use relate_planner::{is_show_databases_with_next, LogicalPlan};

use super::admission::Admission;
use super::cache::CacheUse;
use super::outcome::{ArmOutput, ArmRun, Executed, QueryOutcome};
use super::render::RenderFacts;
use super::txn::{OpenTxn, TxnRun};
use super::Connection;
use crate::commit::commit_write;
use crate::gate::ResultStamp;
use crate::phase_timing::TimingStamp;
use crate::{CancelToken, Error, QueryResult, Result, Value};

/// Where REL8-1120's optimistic-hit retry leg runs its uncached
/// re-admission — the parse-bearing tail: on this thread (the worker
/// body, the escalation arm, or the asserted fast path's inline arm,
/// whose stacks carry the parser's nesting-proportional recursion), or
/// on the query worker (the cache-hit inline route's arm, ADR-0663,
/// whose thread never asserted that budget).
#[derive(Clone, Copy)]
pub(super) enum RetryHost {
    ThisThread,
    /// The query-worker host — native only: wasm32 has no worker, so
    /// the enum itself carries no `Worker` variant there and the retry
    /// below runs in place on the caller (the platform's only thread).
    #[cfg(not(target_arch = "wasm32"))]
    Worker,
}

/// The statement's token-budget reservation as one owner: the reserved
/// ledger and its observation key as one pair — always `Some` together —
/// plus the consuming half of its lifecycle, `settle` (observe the peak,
/// keep the ledger alive on a finished result). The two ambient steps —
/// charging the retained rows and attaching the receipt — stay the free
/// functions in `dispatch.rs`: they run on the thread-local ledger the
/// admission guard installs, and `LedgerGuard`'s exclusive-borrow lifetime
/// cannot be co-owned with the ledger it guards, so the owner is not
/// borrowable while the guard is armed. Constructed by
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

/// The admission gates between the EXPLAIN early return and dispatch: the
/// ADR-0308 parameter-completeness check and the SHOW DATABASES + NEXT
/// restriction. One named seam, so `run_query` reads as
/// parse → plan → gates → dispatch.
fn post_explain_gates(logical: &LogicalPlan, params: &BTreeMap<String, Value>) -> Result<()> {
    // ADR-0308: a referenced parameter the caller did not supply fails the
    // statement before anything executes (EXPLAIN, above, plans without
    // values, as in Neo4j). The predicate is the planner's one spelling —
    // the same call the `01N60` notice makes, so the refusal and the notice
    // cannot disagree.
    let missing = relate_planner::plan_missing_parameters(logical, params);
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
        let admission = match self.admit_statement(cypher, &params, CacheUse::Query, Instant::now())
        {
            Ok(admission) => admission,
            // The plain not-admitted outcome: `run_cancellable`'s
            // epilogue owns failing the open transaction for every
            // non-Ok result (with the retryable-refusal classifier), so
            // no statement body pre-empts it.
            Err(e) => return QueryOutcome::not_admitted(Err(e)),
        };
        self.run_admitted(admission, cypher, &params, token, RetryHost::ThisThread)
    }

    /// The query body after admission (ADR-0504): the EXPLAIN early
    /// return, the post-EXPLAIN gates, the token reservation, the
    /// dispatch and the PROFILE epilogue. One body shared by every path —
    /// `run_query` (the worker body of an unasserted connection), the
    /// inline fast path of a worker-stack-asserted connection, and that
    /// path's escalation to the worker with the admission already in
    /// hand — so no path can skip a step between paths.
    ///
    /// REL8-1120 wraps the body with the optimistic-hit retry leg: a
    /// cache-served statement (the admission's outcome
    /// [`PlanCacheOutcome::served_from_cache`]) runs optimistically — the
    /// hit no longer re-runs the front with this statement's values, so a
    /// value-dependent compile verdict (a type analysis that read a bound
    /// list's element types) can surface first at execution, in the
    /// runtime error class. The compile's own verdict wins: on a
    /// type-error-class execution failure the statement runs once more
    /// uncached, and that outcome — the compile error, or a clean run —
    /// is what the caller sees. Read-ness is the insert gate's own
    /// guarantee (`cache_insert_eligible` refuses every write-routed
    /// plan, and the hit's stamp re-derives from that plan), so the only
    /// eligibility conjuncts are the cache service itself and being
    /// outside an explicit transaction (the failed attempt already
    /// failed it, so a re-admission would report the transaction
    /// refusal instead of the statement's own verdict). The type-error
    /// class is the one where compile and runtime
    /// verdicts can diverge — a type error raised by runtime *data* also
    /// re-runs once, the accepted cost of a wasted compile on a statement
    /// that was already failing; every other error keeps its outcome, so
    /// no query executes twice for any other reason. The retry re-runs
    /// the body directly (its `Bypass` admission is never cache-served),
    /// so the leg cannot recurse.
    ///
    /// [`PlanCacheOutcome::served_from_cache`]:
    /// crate::PlanCacheOutcome::served_from_cache
    pub(super) fn run_admitted(
        &mut self,
        admission: Admission,
        cypher: &str,
        params: &BTreeMap<String, Value>,
        token: &CancelToken,
        retry_host: RetryHost,
    ) -> QueryOutcome {
        // REL8-1120 retry eligibility, captured before the admission is
        // consumed — the doc above states the contract; read-ness needs
        // no conjunct because `cache_insert_eligible` already refuses
        // every write-routed plan, and the hit's stamp re-derives from
        // that same plan.
        let started = admission.started;
        let retryable_hit = admission.timing.plan_cache.served_from_cache() && self.txn.is_none();
        let outcome = self.run_admitted_once(admission, cypher, params, token);
        // The run's own error is the staged failure (ADR-0319): every
        // executor type error rides `Ok(Executed::Failed(..))`, while an
        // `Err` is a pre-execution refusal or a post-run resource charge —
        // never this retry's class. The type-error family is read from
        // the one classification table (`error_info`'s `TYPE_ERROR_CODE`
        // grouping), never re-derived as a variant list here.
        let retryable = retryable_hit
            && outcome
                .staged_failure()
                .is_some_and(|failure| error_info(&failure.error).neo4j_code == TYPE_ERROR_CODE);
        if !retryable {
            return outcome;
        }
        // ADR-0221: the caller's one measurement spans both attempts —
        // the retry's admission is built on the first attempt's clock.
        // The host: the retry re-admits with the bypass, whose uncached
        // tail parses the raw text — recursion with the statement's
        // nesting that only the query worker's stack (or a caller that
        // asserted it) carries. The worker body, the escalation arm and
        // the asserted fast path run it in place; the cache-hit inline
        // route's arm (ADR-0663, an unasserted thread) sends it to the
        // worker, the re-run itself bounded by the same statement that
        // just ran inline.
        #[cfg(not(target_arch = "wasm32"))]
        if matches!(retry_host, RetryHost::Worker) {
            return QueryOutcome::from_result(Self::run_on_worker(
                self.worker_slot(),
                || match self.admit_statement(cypher, params, CacheUse::Bypass, started) {
                    Ok(admission) => self.run_admitted_once(admission, cypher, params, token),
                    Err(e) => QueryOutcome::not_admitted(Err(e)),
                },
                // No heartbeat: the retry is one more run of a statement
                // that just finished inline, far inside any interval a
                // caller arms one for (the inline arm's own arithmetic).
                None,
            ));
        }
        match self.admit_statement(cypher, params, CacheUse::Bypass, started) {
            Ok(admission) => self.run_admitted_once(admission, cypher, params, token),
            Err(e) => QueryOutcome::not_admitted(Err(e)),
        }
    }

    /// [`Self::run_admitted`]'s body: everything after admission, run
    /// exactly once per attempt.
    fn run_admitted_once(
        &mut self,
        admission: Admission,
        cypher: &str,
        params: &BTreeMap<String, Value>,
        token: &CancelToken,
    ) -> QueryOutcome {
        // EXPLAIN plans and compile-time validates the wrapped statement,
        // then renders the plan — execution never touches it (no writes, no
        // procedures, no LOAD CSV). The SHOW DATABASES + NEXT restriction is
        // an execution-phase rule, so it does not apply here.
        if admission.preamble == PreambleMode::Explain {
            // The plain not-admitted outcome: `run_cancellable`'s
            // epilogue owns failing the open transaction for every
            // non-Ok result, so this body does not pre-empt it.
            return match self.explain_run(cypher, params, &RenderFacts::from(&admission)) {
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
        // One exhaustive by-value destructure — a new `Admission` field
        // forces a decision here, not a silent `..` drop, and the bundle
        // is never mutated past `stage_admission` (REL8-1946): the
        // consumed payloads (`effective`, `memory`) move out here instead
        // of being `take()`n or sentinel-swapped in place. The budget's
        // own key reads the caller's undisturbed map — `budget_params`,
        // never the effective map the shadow names: the PeakKey hashes
        // the whole map, so keying the reservation by the merged map
        // would give a cached retry a different identity than its first,
        // uncached run (found in review).
        let budget_params = params;
        let Admission {
            started,
            timing_on,
            preamble,
            admin,
            logical,
            stamp,
            timing,
            effective,
            memory,
            preview,
        } = admission;
        let params = effective.as_ref().unwrap_or(budget_params);
        // LABS-1120: the admission's timing facts ride as the one carrier
        // — the same bundle the stamping tail consumes, no re-shaping here.
        let logical = logical.as_ref();
        // The admission gates between the EXPLAIN early return and dispatch:
        // the ADR-0308 parameter-completeness check and the ADR-0116 §2
        // SHOW DATABASES + NEXT restriction. One named seam.
        if let Err(e) = post_explain_gates(logical, params) {
            return QueryOutcome::not_admitted(Err(e));
        }
        // The statement's effect on the pipelined write batch (ADR-0642) is
        // settled before the statement's ledger exists: ending a batch runs
        // its commit, whose encoded record would otherwise be charged to the
        // joining statement's reservation and its failure framed as that
        // statement's admission — an identity that did not produce it.
        if let Err(error) = self.write_batch_policy(logical) {
            return QueryOutcome::not_admitted(Err(error));
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
        let parse_plan_ns = crate::commit_timing::elapsed_nanos(timing_on.then_some(started));
        // PROFILE runs the statement through the normal dispatch, then
        // appends the plan and execution statistics to its result. The
        // result keeps the wrapped statement's query type (ADR-0162).
        let metrics = (preamble == PreambleMode::Profile).then(crate::profile::ProfileMetrics::new);
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
        // `commit_write` reserves returned rows before it appends to the
        // WAL, so an auto-commit write's result is exempt from the charge
        // below. Read after dispatch, never before it: the dispatch's own
        // pipelined-batch policy opens or ends the batch, so a statement
        // that ends one runs as the auto-commit write it then is, and one
        // that opens one is charged like the batched write it became.
        let auto_commit_write = self.auto_commit_write(stamp.write_route);
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
        // the call. The epilogue's inputs ride as one `RenderFacts`
        // bundle — the facts it consumes, never the whole admission
        // (REL8-1946; ADR-0504's shared tail).
        let mut outcome = if let Some(metrics) = metrics {
            let facts = RenderFacts::new(logical, admin, &stamp, started, timing);
            self.profile_epilogue(outcome, &facts, params, cypher, &metrics)
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
        if let Ok(executed) = outcome.result.as_mut() {
            // The admission's own preview copy (read once under the
            // publication guard, ADR-0378): the restore's re-parse runs
            // under the same options the statement's admission parsed
            // with, so the two can never disagree on the grammar. The
            // header both `Executed` variants carry (`Failed` included —
            // its columns are user-visible too), read through the one
            // accessor.
            super::columns::restore_authored_column_names(
                logical,
                cypher,
                &mut executed.header_mut().columns,
                preview,
            );
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
        // Park a hold on the statement reservation. A hold exists only
        // when the budget is on, and the budget is on exactly when
        // `reserve_statement` produced a reservation — so a held plan
        // without one is a broken invariant, not a case to drop.
        let park = |reservation: &mut Option<StatementReservation>, hold| {
            reservation
                .as_mut()
                .expect("a plan hold implies an enabled budget, hence a statement reservation")
                .plan = Some(hold);
        };
        match memory {
            PlanMemory::Cached => Ok(()),
            PlanMemory::Statement(bytes) => match budget.try_hold_admitted(bytes) {
                HoldAttempt::Off => Ok(()),
                HoldAttempt::Held(hold) => {
                    park(reservation, hold);
                    Ok(())
                }
                HoldAttempt::Refused => Err(budget.plan_refusal(bytes)),
            },
            PlanMemory::Cacheable(pending) => {
                let bytes = pending.bytes;
                match budget.try_hold_admitted(bytes) {
                    HoldAttempt::Off => self.publish_cache_entry(pending, None),
                    HoldAttempt::Held(hold) => self.publish_cache_entry(pending, Some(hold)),
                    HoldAttempt::Refused => {
                        // The cache ceiling refused the entry: the plan is
                        // not published, and the statement carries it with
                        // running-query growth when the pool allows. `Off`
                        // cannot follow a `Refused` (same budget); a refused
                        // grow leaves the plan uncharged by design — the
                        // statement already holds its own reservation.
                        if let HoldAttempt::Held(hold) = budget.try_hold_grow(bytes) {
                            park(reservation, hold);
                        }
                    }
                }
                Ok(())
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
        // The write arm carries its commit sequence (ADR-0502 §7) beside
        // the executor result (`ArmOutput`); every other arm's output has
        // none — nothing it runs commits at the statement boundary.
        let arm = if let Some(txn) = self.txn.as_ref() {
            self.txn_arm(txn, &ctx, logical, params, cypher, token)
        } else if self.auto_commit_write(ctx.stamp.write_route) {
            // Only reachable when the statement targets the `neo4j` graph:
            // the other namespaces reject writes above, and administration
            // commands target `neo4j`. `write_route` (computed in
            // `plan_gate_classify` under the read guard) is `is_write` plus
            // the ontology write procedures, whose redo lands through the
            // same group-commit pipeline (ADR-0120 §3).
            // ADR-0209 batched-vs-single commit strategy selection lives in
            // commit.rs (`commit_write`).
            ArmRun::NotAdmitted(
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
                )
                .map(ArmOutput::committed),
            )
        } else {
            // A refused admission (read guard, target, pin) is not-admitted;
            // the run's own result pairs with `admitted: true`.
            self.query_read(&ctx, logical, params, cypher, token)
        };
        let (result, admitted) = match arm {
            ArmRun::Refused(e) => return QueryOutcome::not_admitted(Err(e)),
            ArmRun::Admitted(result) => (result, true),
            ArmRun::NotAdmitted(result) => (result, false),
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
        // Dispatch is total: every arm yields rows or a staged failure —
        // never an `Err` — so the `Ok` wraps the match once, structurally.
        let result = Ok(match result {
            // The auto-commit write's commit sequence (ADR-0502 §7) stamps
            // the result's bookmark at the same single tail — every other
            // arm's `None` leaves the result bookmark-free.
            Ok(ArmOutput {
                result,
                commit_sequence,
            }) => Executed::Rows(
                QueryResult::from_exec(result, ctx.stamp.namespace, ctx.stamp.qtype, stamp_tail)
                    .with_commit_sequence(commit_sequence),
            ),
            // `staged` derives the planned output names (the UNION
            // column-agreement check ran in `plan`), so a planned statement
            // always stages its failure with its columns.
            Err(error) => Executed::staging_failure(logical, ctx.stamp, stamp_tail, error),
        });
        QueryOutcome { result, admitted }
    }

    /// The auto-commit read arm: the admission protocol (read guard, target
    /// dispatch, ontology capture, pin), then the snapshot read — the
    /// read-side twin of the explicit-transaction arm's `query_in_txn`.
    /// [`ArmRun::Refused`] is a refused admission (the statement never
    /// ran); [`ArmRun::Admitted`] carries the run's own result.
    fn query_read(
        &self,
        ctx: &DispatchCtx<'_>,
        logical: &LogicalPlan,
        params: &BTreeMap<String, Value>,
        cypher: &str,
        token: &CancelToken,
    ) -> ArmRun {
        let (admin, namespace) = (ctx.admin, ctx.stamp.namespace);
        // The target is selected and pinned under the read guard: the
        // `neo4j` arm derefs the GraphCell, whose contract requires the
        // guard (a follow reload may `replace` the graph otherwise,
        // ADR-0137); the pin then keeps the cell alive for `target`'s
        // later use. The (admin, namespace) dispatch decision is made
        // exactly once, here (dispatch_target, ADR-0229 §1).
        // The ontology document the statement reads (ADR-0121 §3,
        // ADR-0227): store-level state captured together with the graph
        // pin under the read guard, so an auto-commit statement sees a
        // consistent admission-time document (explicit transactions use
        // the begin-time capture in `query_in_txn`). A shared handle:
        // the capture is an `Arc` pointer clone, not a document copy.
        let admit = || -> Result<_> {
            let _read = self.inner.read_guard()?;
            let target = self.dispatch_target(admin, namespace)?;
            let ontology_document = self.inner.ontology.get();
            let pin = target.pin_now()?;
            Ok((target, ontology_document, pin, target.schema_snapshot()))
        };
        let (target, ontology_document, pin, schema) = match admit() {
            Ok(admitted) => admitted,
            Err(e) => return ArmRun::Refused(e),
        };
        #[cfg(test)]
        self.inner.pause_admitted_read();
        ArmRun::Admitted(
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
            .map(ArmOutput::read),
        )
    }

    /// The explicit-transaction arm: derive the target and admit the read
    /// under the read guard, then run in the transaction's overlay. A
    /// refused admission is [`ArmRun::Refused`], the read arm's one refusal
    /// path.
    fn txn_arm(
        &self,
        txn: &OpenTxn,
        ctx: &DispatchCtx<'_>,
        logical: &LogicalPlan,
        params: &BTreeMap<String, Value>,
        cypher: &str,
        token: &CancelToken,
    ) -> ArmRun {
        // `begin` pinned the transaction's view, and that pin keeps the
        // GraphCell from being replaced for the transaction's lifetime,
        // so deriving the target here is safe without a fresh guard.
        let target = match self.dispatch_target(ctx.admin, ctx.stamp.namespace) {
            Ok(target) => target,
            Err(e) => return ArmRun::Refused(e),
        };
        let shared = &self.inner;
        // `begin` already pinned the transaction's view. Only admission
        // must be serialized with poison publication, not its execution.
        let schema = match shared.read_guard() {
            Ok(_read) => target.schema_snapshot(),
            Err(e) => return ArmRun::Refused(e),
        };
        #[cfg(test)]
        shared.pause_admitted_read();
        ArmRun::Admitted(
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
            .map(ArmOutput::read),
        )
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
        facts: &RenderFacts<'_>,
    ) -> Result<ExecGraph<'a>> {
        let target = self.dispatch_target(facts.admin, facts.namespace)?;
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
    /// auto-commit read path selects its target in `query_read`.)
    fn dispatch_target(
        &self,
        admin: bool,
        namespace: Namespace,
    ) -> Result<&crate::engines::LiveQuery> {
        match (admin, namespace) {
            (true, _) | (false, Namespace::Neo4j) => Ok(&self.inner.graph),
            (false, Namespace::System) => Ok(&self.inner.system),
            (false, Namespace::SystemOntologies) => {
                Err(Error::internal("system_ontologies is not queryable"))
            }
        }
    }
}
