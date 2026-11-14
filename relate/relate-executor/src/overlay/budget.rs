//! The per-statement budget mechanisms (ADR-0395 nested evaluation and
//! ADR-0463 expression allocation), extracted from the overlay hub: the
//! constants, their counters, and the [`ExecGraph`] charge/limit seam.

use std::cell::Cell;

use relate_core::{Error, Result, Value};

use super::ExecGraph;

/// ADR-0395: the fixed per-top-level-statement budget on nested
/// clause-engine part passes — every `execute_query_seeded` part
/// iteration (a CALL body per imported row, an import-invariant CALL's
/// shared execution, a conditional's taken branch bodies, the
/// COUNT/COLLECT expression-subquery engines, the NEXT-stage recursion;
/// a UNION body ticks once per part, so a wide body costs its part
/// count) plus the direct part passes of the EXISTS/COUNT paths and
/// the FOREACH arm (one `run_nested_clauses` call each). The runtime
/// twin of the compile-side `MAX_CLONED_NODES` (ADR-0382), at the same
/// number: a fixed constant, no knob — the remedy for legitimate work
/// that needs more is a new ADR, never ambient configuration.
pub const NESTED_EVAL_BUDGET: u64 = 100_000;

/// The per-top-level-statement nested-evaluation counter (ADR-0395): one
/// shared cell per statement, owned by the execution view beside
/// `cancel_ticks` — the one per-statement state container both the
/// statement engine and the expression layer reach — so nesting never
/// resets it: every nested entry ticks [`ExecGraph::nested_evals`], never
/// a context-local cell. A `Cell` suffices for the same reason the
/// cancellation window's does: a statement's executor access is
/// single-threaded.
#[derive(Debug, Default)]
pub(crate) struct NestedEvalCounter(Cell<u64>);

impl NestedEvalCounter {
    /// Zero the counter: the per-statement reset a reused view runs at
    /// statement entry (freshly constructed views start at zero
    /// already).
    pub(crate) fn reset(&self) {
        self.0.set(0);
    }

    /// The passes counted so far — the test-side read of the budget's
    /// counter, for asserting the counting rule's exact tick counts.
    #[cfg(test)]
    pub(crate) fn count(&self) -> u64 {
        self.0.get()
    }

    /// Count one nested statement-engine pass; the authored typed error
    /// when the pass would exceed [`NESTED_EVAL_BUDGET`]. The message's
    /// `Kind:` prefix is the token the error channel classifies
    /// (`ErrorKind::NestedEvaluationBudgetExceeded`, ADR-0395).
    pub(crate) fn tick(&self) -> Result<()> {
        let passes = self.0.get() + 1;
        if passes > NESTED_EVAL_BUDGET {
            return Err(Error::execution(format!(
                "NestedEvaluationBudgetExceeded: nested evaluation exceeds \
                 {NESTED_EVAL_BUDGET} passes per statement; nested subqueries \
                 compound per level (security budget)."
            )));
        }
        self.0.set(passes);
        Ok(())
    }
}

/// The per-top-level-statement expression allocation counter (ADR-0463):
/// the ADR-0395 twin, owning its cell and the statement-entry reset. The
/// constant lives at module level and the typed refusal on [`ExecGraph`]
/// (`check_expression_allocation`), which this counter backs with the
/// construction-work telemetry.
#[derive(Debug, Default)]
pub(crate) struct ExpressionBudgetCounter(Cell<u64>);

impl ExpressionBudgetCounter {
    /// Zero the counter: the per-statement reset a reused view runs at
    /// statement entry (freshly constructed views start at zero already).
    pub(crate) fn reset(&self) {
        self.0.set(0);
    }

    /// The bytes charged so far this statement.
    pub(crate) fn bytes(&self) -> u64 {
        self.0.get()
    }

    /// Record `bytes` of construction work. Telemetry only: enforcement is
    /// per allocation at the check sites, never a cumulative total, because
    /// a running sum ignores values the statement dropped (ADR-0463).
    pub(crate) fn record(&self, bytes: u64) {
        self.0.set(self.0.get().saturating_add(bytes));
    }
}

impl<'a> ExecGraph<'a> {
    /// Refuse one expression-built allocation larger than the view's cap
    /// (ADR-0463). The size is the allocation about to be made, so the
    /// check is exact and stateless: no running total, no refunds, and a
    /// long streaming statement with many small allocations never trips it.
    /// `None` is the explicit opt-out.
    #[inline]
    pub(crate) fn check_expression_allocation(&self, bytes: u64) -> Result<()> {
        if let Some(limit) = self.expression_limit {
            if bytes > limit {
                return Err(Error::capacity_expression(bytes, limit));
            }
        }
        Ok(())
    }

    /// Refuse the allocation, then record it as construction-work telemetry
    /// (the PROFILE `Expression Built (Bytes)` figure, never enforcement).
    #[inline]
    pub(crate) fn charge_expression(&self, bytes: u64) -> Result<()> {
        self.check_expression_allocation(bytes)?;
        self.expression_budget.record(bytes);
        Ok(())
    }

    /// Enforce the cap against a **retaining container's running total**
    /// (ADR-0463). A retaining container (`collect`, a comprehension output,
    /// UNWIND's materialized rows) is one logical allocation that grows:
    /// nothing in it is dropped before the container drops, so its running
    /// retained size is exact and cannot over-count. `total` is the
    /// construction's running size, reset when the construction completes;
    /// `added` is the buffer slot/header or pushed payload just appended.
    /// Transient per-allocation charges keep using
    /// [`Self::charge_expression`]; this is not a statement-wide meter.
    #[inline]
    pub(crate) fn charge_retained(&self, total: &mut u64, added: u64) -> Result<()> {
        *total = total.saturating_add(added);
        self.check_expression_allocation(*total)
    }

    /// The statement's shared live-memo-bytes counter (LABS-1478).
    pub(crate) fn memo_live(&self) -> crate::exec::kernels::memo::MemoLive {
        self.memo_live.clone()
    }

    /// The configured per-allocation cap (`None` = no cap).
    #[cfg(test)]
    pub(crate) fn expression_limit(&self) -> Option<u64> {
        self.expression_limit
    }

    /// The expression bytes charged so far this statement — the cumulative
    /// fact PROFILE snapshots at clause boundaries (ADR-0463). The counter
    /// resets at [`Self::begin_statement`].
    pub(crate) fn expression_built_bytes(&self) -> u64 {
        self.expression_budget.bytes()
    }

    /// Charge one `Vec<Value>` of `len` elements through the workspace's
    /// one value-buffer figure ([`crate::workspace::value_buffer_bytes`],
    /// built on the saturating `buffer_bytes_saturating`). An extreme `len`
    /// (e.g. `range(1, 10^18)`) saturates to `u64::MAX`, which the
    /// per-allocation check refuses as a typed `Error::Capacity` instead of
    /// wrapping to a small charge and letting `Vec::with_capacity` abort.
    #[inline]
    pub(crate) fn charge_value_buffer(&self, len: usize) -> Result<()> {
        self.charge_expression(crate::workspace::value_buffer_bytes(len))
    }

    /// Start a retaining container of `slots` value slots: charge the slot
    /// buffer (derived once) and hold its running retained total.
    pub(crate) fn retain_slots(&self, slots: usize) -> Result<RetainedTotal> {
        self.retain_bytes(crate::workspace::value_buffer_bytes(slots))
    }

    /// Start a retaining string/byte container from `bytes` already built
    /// (0 for an empty accumulator).
    pub(crate) fn retain_bytes(&self, bytes: u64) -> Result<RetainedTotal> {
        self.charge_expression(bytes)?;
        Ok(RetainedTotal { total: bytes })
    }
}

/// One retaining container's running retained total (ADR-0463). A
/// `collect`/comprehension/UNWIND container holds every element until it
/// drops, so its total is exact and is the enforced quantity; the guard owns
/// the running sum and the check, so no site hand-spells the protocol.
#[derive(Clone)]
pub(crate) struct RetainedTotal {
    total: u64,
}

impl RetainedTotal {
    /// Grow by a payload already charged where it was built (a shared clone
    /// or a value whose own construction was charged). Shared/borrowed
    /// payloads contribute nothing via [`crate::workspace::owned_heap`].
    #[inline]
    pub(crate) fn add_owned(&mut self, graph: &ExecGraph<'_>, value: &Value) -> Result<()> {
        self.add_bytes(graph, crate::workspace::owned_heap(value))
    }

    /// Grow by raw retained bytes (a key, an entry, a batch figure).
    #[inline]
    pub(crate) fn add_bytes(&mut self, graph: &ExecGraph<'_>, bytes: u64) -> Result<()> {
        graph.charge_retained(&mut self.total, bytes)
    }

    /// One materialized row landing in the container: the row's
    /// retained payload — a flat payload duplicated across the row's
    /// slots counts once when the row solely holds it, and a payload
    /// held beyond the row (the store, a parameter list, the group
    /// container a seed was cloned from) is borrowed and adds nothing —
    /// then the row's value buffer and its slot in the container's row
    /// vector. The one spelling of the row-landing charge — the
    /// aggregate group map's population and the projection output
    /// containers call it, charged ahead of the landing so a refusal
    /// never leaves the row retained.
    pub(crate) fn add_row(&mut self, graph: &ExecGraph<'_>, values: &[Value]) -> Result<()> {
        self.add_bytes(graph, crate::workspace::row_retained_heap(values))?;
        self.add_bytes(
            graph,
            crate::workspace::retained_rows_bytes(1, values.len()),
        )
    }

    /// One group-table landing in a streaming fold: the map entry's
    /// slot growth plus the key's slots and owned payload (the workspace
    /// group-entry delta — the meter's own figures, which count each key
    /// payload exactly once), then the key-half values' slot buffer. The
    /// values' own payloads add nothing by construction: the key shares
    /// every payload's `Arc` from `CachedKey::from_vals`, so `owned_heap`
    /// sees a share and the entry delta's `key_account` is the one count
    /// each payload gets. The one spelling of the group-landing charge —
    /// both folds' vacant arms and the keyless seeds call it, charged
    /// ahead of the insert so a refusal never leaves the group landed.
    pub(crate) fn add_group_entry(
        &mut self,
        graph: &ExecGraph<'_>,
        map_slots: usize,
        key: &[crate::group_key::GroupKey],
        vals_len: usize,
    ) -> Result<()> {
        self.add_bytes(
            graph,
            crate::workspace::group_entry_insert_delta(map_slots, key),
        )?;
        self.add_bytes(graph, crate::workspace::value_buffer_bytes(vals_len))
    }

    /// One just-built allocation that is also retained: record it as
    /// construction-work telemetry, check it per allocation, and grow the
    /// container total.
    #[inline]
    pub(crate) fn add_allocation(&mut self, graph: &ExecGraph<'_>, bytes: u64) -> Result<()> {
        graph.charge_expression(bytes)?;
        graph.charge_retained(&mut self.total, bytes)
    }

    /// The retained total so far (the test-side read).
    #[cfg(test)]
    pub(crate) fn total(&self) -> u64 {
        self.total
    }
}
