//! The EXPLAIN/PROFILE render stage of a statement (ADR-0344/0345 phase
//! 2, ADR-0359): the result builders that turn an admitted plan into a
//! rendered EXPLAIN result or a PROFILE-enriched one — extracted from
//! `dispatch.rs` so that module keeps the admission → reservation →
//! dispatch path its doc describes, and this one owns the render
//! lifecycle beside the `crate::explain`/`crate::profile` machinery it
//! drives.

use std::collections::BTreeMap;
use std::time::Duration;
use web_time::Instant;

use super::admission::Admission;
use super::dispatch::{Executed, QueryOutcome};
use super::Connection;
use crate::explain;
use crate::phase_timing::TimingStamp;
use crate::{QueryResult, Result, Value};

impl Connection {
    /// The EXPLAIN stage: plan, compile-time validate and render the wrapped
    /// statement without executing it. The notification facts read the same
    /// view the statement would run against (ADR-0363 decision 5) — an
    /// explicit transaction's scratch writes included.
    pub(super) fn explain_run(
        &self,
        cypher: &str,
        params: &BTreeMap<String, Value>,
        admission: &Admission,
    ) -> Result<QueryResult> {
        // LABS-832: the EXECUTE phase of an EXPLAIN run — the plan render,
        // this path's execute-equivalent stage (the wrapped statement never
        // executes). The window opens here; the stamp itself is taken by
        // the builder after the render (its header construction, below the
        // tree and notification derivation), so the render's cost lands in
        // the EXECUTE arm and the ADR-0221 total — never between phases.
        // The stamping policy itself is spelled once, in
        // `TimingStamp::stamped` (phase_timing.rs).
        let execute_started = Instant::now();
        // The estimates (ADR-0218) read the store under a read guard, like
        // the plan's own admission above.
        let _read = self.inner.read_guard()?;
        let view = self.notification_view(&_read, admission)?;
        let stats = crate::stats::GraphPatternStats::new(&self.inner.graph, params);
        let timing = || {
            // LABS-1120: the EXPLAIN path carves nothing out of its
            // execute-equivalent stage — the render IS the result
            // generation (the tree, the notifications and the header all
            // build inside it) — so the result-generation sub-window is
            // the true zero, and the fine parse sub-split and the
            // plan-cache outcome ride from the admission like every
            // other path's.
            TimingStamp::stamped(
                admission.started,
                crate::phase_timing::DispatchWindows {
                    execute: execute_started.elapsed(),
                    result_generation: Duration::ZERO,
                },
                admission.timing,
            )
        };
        explain::explain_result(
            &admission.logical,
            admission.stamp.namespace,
            admission.stamp.qtype,
            timing,
            &stats,
            explain::statement_echo(cypher, "EXPLAIN"),
            explain::NotificationInputs {
                view: &view,
                params,
            },
        )
    }

    /// The PROFILE epilogue: append the profiled plan and its statistics to
    /// a finished result. A failed statement stays failed — PROFILE adds
    /// nothing to a staged execution failure. The epilogue input is
    /// non-optional: `run_admitted` calls this only for a PROFILE
    /// statement, so the common case skips the call instead of threading
    /// a `None` through it.
    pub(super) fn profile_epilogue(
        &self,
        outcome: QueryOutcome,
        admission: &Admission,
        params: &BTreeMap<String, Value>,
        cypher: &str,
        metrics: &crate::profile::ProfileMetrics,
    ) -> QueryOutcome {
        // One signature, no intermediate bundle: the enrich reads the
        // admission for its own facts (the plan, the admin flag, the
        // stamp's namespace), and the module dependency stays one-way
        // (dispatch imports nothing from render's internals).
        let QueryOutcome { result, admitted } = outcome;
        let result = match result {
            Ok(Executed::Rows(result)) => Ok(Executed::Rows(
                self.profile_enrich(admission, params, cypher, metrics, result),
            )),
            failed => failed,
        };
        QueryOutcome { result, admitted }
    }

    /// Enrich one finished PROFILE result with the estimate tree. It reads
    /// the store for the tree's estimates (ADR-0218) under a fresh read
    /// guard — execution has finished, so this guard cannot nest a dispatch
    /// one.
    ///
    /// The enrichment is a **rendering detail and never discards the
    /// finished result** (ADR-0319: the statement admitted its read and
    /// its rows came off its own pinned snapshot): a refused guard, an
    /// unresolved namespace or a failed notification derivation keeps
    /// the rows un-enriched — one policy, stated here, at its three
    /// fallback sites.
    fn profile_enrich(
        &self,
        admission: &Admission,
        params: &BTreeMap<String, Value>,
        cypher: &str,
        metrics: &crate::profile::ProfileMetrics,
        result: QueryResult,
    ) -> QueryResult {
        let Some(_read) = self.inner.read_guard().ok() else {
            return result;
        };
        let view = match self.notification_view(&_read, admission) {
            Ok(view) => view,
            Err(_) => return result,
        };
        let statement = match (explain::NotificationInputs {
            view: &view,
            params,
        })
        .derive(&admission.logical)
        {
            Ok(statement) => statement,
            Err(_) => return result,
        };
        let stats = crate::stats::GraphPatternStats::new(&self.inner.graph, params);
        explain::profile_result(
            &admission.logical,
            metrics,
            result,
            &stats,
            explain::statement_echo(cypher, "PROFILE"),
            statement,
        )
    }
}
