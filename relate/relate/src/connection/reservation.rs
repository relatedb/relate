//! The statement's token-budget reservation and its charge policies
//! (ADR-0452, ADR-0503), extracted from `dispatch.rs` to keep that file
//! under the repository's file-size budget: the retry host vocabulary the
//! query body threads, the reservation owner and its settle, and the two
//! constructors — [`Connection::reserve_statement`] and
//! [`Connection::charge_plan`] — the dispatch epilogue calls before it runs
//! the statement.

use std::collections::BTreeMap;

use super::cache::PlanMemory;
use super::Connection;
use crate::token_budget::{HoldAttempt, PlanHold, TokenBudget};
use crate::{Result, Value};

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
pub(super) struct StatementReservation {
    pub(super) ledger: relate_core::tokens::TokenLedger,
    key: crate::token_budget::PeakKey,
    /// Plan bytes reserved for an uncached run. Dropped with the
    /// reservation, which is the end of the statement.
    plan: Option<PlanHold>,
}

impl StatementReservation {
    /// Settle the reservation: observe the ledger's peak against its key,
    /// then hand the ledger to the finished result it charged (its charge
    /// transfers with the rows); any other outcome drops it here. Runs
    /// after the admission guard dropped — the ledger moves once the
    /// window closed.
    pub(super) fn settle(self, budget: &TokenBudget, outcome: &mut super::outcome::QueryOutcome) {
        budget.observe(
            self.key,
            self.ledger
                .receipt(relate_core::TokenOutcome::Success)
                .peak_logical_bytes,
        );
        if let Ok(super::outcome::Executed::Rows(result)) = outcome.result.as_mut() {
            result._retained_ledger = Some(Box::new(self.ledger));
        }
    }
}

impl Connection {
    /// Reserve the statement's token budget ([`StatementReservation`]'s
    /// constructor): `None` when the budget is off — the off state's one
    /// representation, and the off path pays no key construction
    /// (`TokenBudget::reserve_keyed` constructs the key only when on) —
    /// the reserved ledger and its observation key otherwise. The
    /// admission guard is armed by the caller — see the owner's doc.
    pub(super) fn reserve_statement(
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
    pub(super) fn charge_plan(
        &self,
        memory: PlanMemory,
        reservation: &mut Option<StatementReservation>,
    ) -> Result<()> {
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
                let pending = *pending;
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
}
