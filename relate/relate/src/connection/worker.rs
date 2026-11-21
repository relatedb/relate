//! The connection's query-worker cluster (ADR-0259, ADR-0312, ADR-0504):
//! the one statement-dispatch entry (worker body, or the caller-stack
//! assertion's fast path — admit inline, then execute inline or escalate
//! to the worker), spawning and reusing the per-connection 64 MiB worker,
//! and the `Drop` join that keeps a busy worker from outliving its
//! connection. Every item but the wasm32 dispatch twin in the parent
//! module is native-only: wasm32 has no threads, so its twin runs the one
//! `run_query` body directly on the caller's own thread (ADR-0312).

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
    /// The statement dispatch entry (ADR-0259, ADR-0504): a connection
    /// whose caller asserted the worker stack takes the fast path, every
    /// other connection the worker body. One entry point so the shared
    /// epilogue (`run_cancellable`) stays mode-free — the shape
    /// `dispatch_plan` already uses.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn dispatch_query(
        &mut self,
        cypher: &str,
        params: BTreeMap<String, Value>,
        token: &CancelToken,
        heartbeat: Option<Heartbeat<'_>>,
    ) -> QueryOutcome {
        if self.caller_stack_asserted {
            self.dispatch_query_or_inline(cypher, params, token, heartbeat)
        } else {
            // The worker-slot refusal and the worker's panic error flow
            // through the one tail every statement path ends in.
            QueryOutcome::from_result(Self::run_on_worker(
                self.worker_slot(),
                || self.run_query(cypher, params, token),
                heartbeat,
            ))
        }
    }

    /// The caller-stack fast path (ADR-0504), for a connection whose
    /// calling thread carries the query worker's own stack budget: the
    /// statement admits on this thread — parse, lowering and planning
    /// recurse proportionally to nesting, and the asserted stack is the
    /// worker's own 64 MiB, so ADR-0259's margin holds unchanged — and
    /// then either executes here (bounded work, no handoff at all) or
    /// escalates to the worker with the admission in hand (the statement
    /// may outlive the keep-alive interval, so the caller must keep
    /// ADR-0312's wait shape and its NOOP ticks).
    ///
    /// ADR-0259's panic containment holds on the whole inline statement,
    /// not only its execution half: the worker loop catches a panic
    /// anywhere in the statement (admission included), so the fast path
    /// guards admission and the inline body with the same
    /// [`catch_inline_panic`] mapping. The escalate arm stays outside the
    /// guard — its wait is the unasserted path's wait, tick panics
    /// included (ADR-0334), and a worker-side panic is already contained
    /// by the worker loop.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn dispatch_query_or_inline(
        &mut self,
        cypher: &str,
        params: BTreeMap<String, Value>,
        token: &CancelToken,
        heartbeat: Option<Heartbeat<'_>>,
    ) -> QueryOutcome {
        // The guarded admission: the one admission sequence (its clock
        // armed inside, ADR-0221) and the route decision, under the fast
        // path's panic guard. The closure returns the ordinary
        // `Result<(Admission, Route)>` — a refusal is a plain `Err`, the
        // same error the worker path's admission refusal produces — and
        // the guard wraps it once; the two failure channels (refusal,
        // caught panic) collapse to the one not-admitted tail at the
        // only consumer.
        let decision = catch_inline_panic(|| {
            let admission = self.admit_statement(
                cypher,
                &params,
                super::cache::CacheUse::Query,
                web_time::Instant::now(),
            )?;
            let route = if plan_is_bounded_work(&admission.logical) {
                Route::Inline
            } else {
                Route::Escalate
            };
            Ok((admission, route))
        });
        match decision {
            // The plain not-admitted outcome — an admission refusal or a
            // caught panic, indistinguishable at this consumer:
            // `run_cancellable`'s epilogue owns failing the open
            // transaction for every non-Ok result, so the fast path does
            // not pre-empt it either.
            Ok(Err(e)) | Err(e) => QueryOutcome::not_admitted(Err(e)),
            Ok(Ok((admission, Route::Inline))) => {
                // Inline: there is no wait, so a heartbeat can never be
                // due — a bounded-work statement finishes far inside any
                // interval a caller arms one for (ADR-0312's own
                // first-tick rule). The heartbeat rides unused to the end
                // of this frame. REL8-1120's optimistic-hit retry rides
                // `run_admitted`, inside the same panic guard.
                QueryOutcome::from_result(catch_inline_panic(|| {
                    self.run_admitted(admission, cypher, &params, token)
                }))
            }
            Ok(Ok((admission, Route::Escalate))) => {
                // Escalate: the post-admission body runs on the 64 MiB
                // worker, the admission travelling with it, the caller
                // waiting with the heartbeat armed exactly as an
                // unasserted connection's statement does. REL8-1120's
                // optimistic-hit retry rides `run_admitted`, the same
                // worker job.
                QueryOutcome::from_result(Self::run_on_worker(
                    self.worker_slot(),
                    || self.run_admitted(admission, cypher, &params, token),
                    heartbeat,
                ))
            }
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

/// The fast path's route for one admitted statement (ADR-0504): inline on
/// the calling thread, or escalated to the query worker. A named route —
/// not a boolean mode — so the dispatch arms read as what they are and a
/// future route cannot be conflated with either.
#[cfg(not(target_arch = "wasm32"))]
enum Route {
    Inline,
    Escalate,
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
