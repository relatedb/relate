//! The connection's query-worker cluster (ADR-0259, ADR-0312, ADR-0504,
//! ADR-0663): the one statement-dispatch entry — the guarded admission
//! and route decision, then the one tail every route ends in — with two
//! admission paths: the caller-stack assertion's fast path (ADR-0504,
//! full admission inline, then execute inline or escalate) and the
//! cache-hit inline route (ADR-0663, a raw-stage plan-cache probe; a hit
//! the containment rule admitted executes inline, everything cold or
//! declined is the worker's). Spawns and reuses the per-connection
//! 64 MiB worker, and the `Drop` join that keeps a busy worker from
//! outliving its connection. Every item but the wasm32 dispatch twin in
//! the parent module is native-only: wasm32 has no threads, so its twin
//! runs the one `run_query` body directly on the caller's own thread
//! (ADR-0312).

#[cfg(not(target_arch = "wasm32"))]
use std::collections::BTreeMap;

#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use relate_planner::plan_is_bounded_work;

#[cfg(not(target_arch = "wasm32"))]
use crate::query_worker::{catch_inline_panic, Heartbeat, JobSlot, QueryWorker};
use crate::Connection;
#[cfg(not(target_arch = "wasm32"))]
use crate::Result;
#[cfg(not(target_arch = "wasm32"))]
use crate::{CancelToken, Value};

#[cfg(not(target_arch = "wasm32"))]
use super::QueryOutcome;

impl Connection {
    /// The statement dispatch entry (ADR-0259, ADR-0504, ADR-0663): one
    /// guarded admission-and-route decision, then one tail every route
    /// ends in — the shape `dispatch_plan` already uses, so the shared
    /// epilogue (`run_cancellable`) stays mode-free. The two admission
    /// paths differ only in the decision's closure; every arm below is
    /// theirs alike.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn dispatch_query(
        &mut self,
        cypher: &str,
        params: BTreeMap<String, Value>,
        token: &CancelToken,
        heartbeat: Option<Heartbeat<'_>>,
    ) -> QueryOutcome {
        // The guarded decision: the admission and its route under the
        // fast path's panic guard, so ADR-0259's containment holds on
        // the caller's thread — a refusal and a caught panic collapse to
        // the one not-admitted tail at the only consumer, the same error
        // the worker path's admission refusal produces. The two decision
        // methods share the tail below; each yields its own two of the
        // three routes.
        let decision = catch_inline_panic(|| {
            if self.caller_stack_asserted {
                self.decide_route_asserted(cypher, &params)
            } else {
                self.decide_route_probed(cypher, &params)
            }
        });
        // The one tail. The decision released the connection
        // (`run_on_worker`'s own shape), the arms take it back.
        match decision {
            // The plain not-admitted outcome — an admission refusal or a
            // caught panic, indistinguishable at this consumer:
            // `run_cancellable`'s epilogue owns failing the open
            // transaction for every non-Ok result, so no arm pre-empts
            // it either.
            Ok(Err(e)) | Err(e) => QueryOutcome::not_admitted(Err(e)),
            // Cold: the worker runs the whole statement — admission
            // included, on the 64 MiB stack the parser's nesting cap is
            // tuned for — the caller waiting with the heartbeat armed
            // exactly as an unasserted connection's statement always did.
            Ok(Ok(Route::WorkerCold)) => QueryOutcome::from_result(Self::run_on_worker(
                self.worker_slot(),
                || self.run_query(cypher, params, token),
                heartbeat,
            )),
            // Inline: there is no wait, so a heartbeat can never be due
            // — a bounded-work statement finishes far inside any interval
            // a caller arms one for (ADR-0312's own first-tick rule). The
            // heartbeat rides unused to the end of this frame. REL8-1120's
            // optimistic-hit retry rides `run_admitted`, inside the same
            // panic guard. The probe's containment rule already vetted
            // the entry: a generation-mismatch replan compiles the same
            // depth-bounded carried AST, so the plan that runs is
            // bounded by the same fact (ADR-0663).
            Ok(Ok(Route::Inline(admission))) => {
                // The inline route's two producers need different retry
                // hosts: the asserted fast path's thread carries the
                // worker's own stack (retry in place); the probe's
                // (ADR-0663, an unasserted thread) sends REL8-1120's
                // retry — whose re-admission parses the raw text — to
                // the worker. The flag is exactly the producer: the
                // asserted path requires it, the probe refuses it.
                let retry_host = if self.caller_stack_asserted {
                    super::dispatch::RetryHost::ThisThread
                } else {
                    super::dispatch::RetryHost::Worker
                };
                QueryOutcome::from_result(catch_inline_panic(|| {
                    self.run_admitted(admission, cypher, &params, token, retry_host)
                }))
            }
            // Escalate: the post-admission body runs on the 64 MiB
            // worker, the admission travelling with it, the caller
            // waiting with the heartbeat armed exactly as an unasserted
            // connection's statement does. This arm stays outside the
            // panic guard — its wait is the unasserted path's wait, tick
            // panics included (ADR-0334), and a worker-side panic is
            // already contained by the worker loop.
            Ok(Ok(Route::Escalate(admission))) => QueryOutcome::from_result(Self::run_on_worker(
                self.worker_slot(),
                || {
                    self.run_admitted(
                        admission,
                        cypher,
                        &params,
                        token,
                        super::dispatch::RetryHost::ThisThread,
                    )
                },
                heartbeat,
            )),
        }
    }

    /// ADR-0504, the caller-stack fast path's decision: the calling
    /// thread carries the worker's own stack budget, so the full
    /// admission — parse, lowering and planning, which recurse
    /// proportionally to nesting — runs here, and bounded work executes
    /// here too (no handoff at all) while everything else escalates with
    /// the admission in hand (the statement may outlive the keep-alive
    /// interval, so the caller must keep ADR-0312's wait shape and its
    /// NOOP ticks).
    #[cfg(not(target_arch = "wasm32"))]
    fn decide_route_asserted(
        &mut self,
        cypher: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<Route> {
        let admission = self.admit_statement(
            cypher,
            params,
            super::cache::CacheUse::Query,
            web_time::Instant::now(),
        )?;
        Ok(if plan_is_bounded_work(&admission.logical) {
            Route::Inline(admission)
        } else {
            Route::Escalate(admission)
        })
    }

    /// ADR-0663, the cache-hit inline route's decision: the probe is the
    /// admission prefix with the raw-only cache mode — a map probe that
    /// cannot recurse with nesting — and a hit its containment rule
    /// admitted runs inline, no handoff, the largest single component of
    /// the warm per-statement floor gone. Everything cold or declined is
    /// the worker's: its parse recursion is what the 64 MiB stack exists
    /// for, and its wait keeps ADR-0312's NOOP ticks for statements that
    /// can outlive the interval.
    #[cfg(not(target_arch = "wasm32"))]
    fn decide_route_probed(
        &mut self,
        cypher: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<Route> {
        match self.admit_hit_only(cypher, params, web_time::Instant::now())? {
            Some(admission) => Ok(Route::Inline(admission)),
            None => Ok(Route::WorkerCold),
        }
    }

    /// The one worker-dispatch composition (ADR-0259/ADR-0312): the slot
    /// resolution and the publish-and-wait, one shape for every worker
    /// site. An associated function taking the caller's `worker_slot()`
    /// result — an owned `Result`, evaluated before the `body` closure
    /// re-borrows the connection the slot resolution has just released —
    /// so the body may capture `&mut self` like every worker job does. A
    /// spawn refusal or a worker panic surfaces as the slot's typed
    /// error.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn run_on_worker<T: Send>(
        slot: Result<Arc<JobSlot>>,
        body: impl FnOnce() -> T + Send,
        heartbeat: Option<Heartbeat<'_>>,
    ) -> Result<T> {
        slot.and_then(|slot| slot.run_with_heartbeat(body, heartbeat))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn worker_slot(&mut self) -> Result<Arc<JobSlot>> {
        if self.query_worker.is_none() {
            self.query_worker = Some(QueryWorker::spawn()?);
            #[cfg(test)]
            {
                self.worker_spawns += 1;
            }
        }
        Ok(Arc::clone(
            &self
                .query_worker
                .as_ref()
                .expect("the worker was just spawned above")
                .slot,
        ))
    }

    /// Spawns performed by this connection. Stays at one across later statements.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn query_worker_spawns(&self) -> u32 {
        self.worker_spawns
    }

    /// Statements the cache-hit inline route (ADR-0663) served on the
    /// calling thread.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn hit_inline_served(&self) -> u32 {
        self.hit_inline_statements
    }

    /// Run `f` on this connection's worker. Used to prove a panic leaves the
    /// same worker usable for the next statement.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn drive_query_worker<T: Send>(
        &mut self,
        f: impl FnOnce() -> T + Send,
    ) -> Result<T> {
        Self::run_on_worker(self.worker_slot(), f, None)
    }
}

/// The dispatch route for one statement (ADR-0504, ADR-0663): inline on
/// the calling thread (the admission in hand), escalated to the query
/// worker (the admission in hand), or cold — the worker runs the whole
/// statement, admission included, on its own stack. A named route — not
/// a boolean mode — so the dispatch arms read as what they are and a
/// future route cannot be conflated with either.
#[cfg(not(target_arch = "wasm32"))]
enum Route {
    Inline(super::admission::Admission),
    Escalate(super::admission::Admission),
    WorkerCold,
}

impl Drop for Connection {
    fn drop(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            // Join the worker before releasing the transaction pin. The
            // worker is idle here: a query holds `&mut self` until it returns.
            self.query_worker.take();
        }
        if let Some(txn) = self.txn.take() {
            self.inner.graph.unpin_slot(txn.pin_slot);
        }
    }
}
