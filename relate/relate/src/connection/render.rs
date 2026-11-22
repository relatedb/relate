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
use super::outcome::{Executed, QueryOutcome};
use super::Connection;
use crate::explain;
use crate::phase_timing::TimingStamp;
use crate::{QueryResult, Result, Value};
use relate_planner::LogicalPlan;

/// The render stage's read-only view of the admission (REL8-1946): every
/// fact the EXPLAIN/PROFILE render paths consume, as ONE named carrier —
/// the connection module's stage hand-off convention (`DispatchCtx`,
/// `AdmitClocks`, `TxnRun`) — so no render signature grows a loose fact
/// clump or an argument-count suppression, and the stage has one shape
/// end to end. The render mode is deliberately NOT a field: each path is
/// reached under exactly one preamble (`Explain` here, `Profile` in the
/// epilogue), so each names its own constant and the mode cannot arrive
/// wrong. [`RenderFacts::new`] is the one projection body — the
/// destructured parts of a by-value admission — and `From<&Admission>`
/// is its borrowed form.
pub(super) struct RenderFacts<'a> {
    pub(super) logical: &'a LogicalPlan,
    pub(super) admin: bool,
    pub(super) namespace: relate_core::Namespace,
    /// ADR-0221's statement clock and the admission's measured timing —
    /// the EXPLAIN render stamps its result from them.
    pub(super) started: Instant,
    pub(super) timing: crate::phase_timing::AdmissionTiming,
    /// The stamp's plan-derived query type (ADR-0162), for the render's
    /// header.
    pub(super) qtype: crate::QueryType,
}

impl<'a> RenderFacts<'a> {
    /// The one projection body: the admission's destructured parts, as
    /// the by-value destructure in `run_admitted_once` holds them.
    pub(super) fn new(
        logical: &'a LogicalPlan,
        admin: bool,
        stamp: &crate::gate::ResultStamp,
        started: Instant,
        timing: crate::phase_timing::AdmissionTiming,
    ) -> Self {
        Self {
            logical,
            admin,
            namespace: stamp.namespace,
            started,
            timing,
            qtype: stamp.qtype,
        }
    }
}

impl<'a> From<&'a Admission> for RenderFacts<'a> {
    fn from(admission: &'a Admission) -> Self {
        Self::new(
            admission.logical.as_ref(),
            admission.admin,
            &admission.stamp,
            admission.started,
            admission.timing,
        )
    }
}

impl Connection {
    /// The EXPLAIN stage: plan, compile-time validate and render the wrapped
    /// statement without executing it. The notification facts read the same
    /// view the statement would run against (ADR-0363 decision 5) — an
    /// explicit transaction's scratch writes included.
    pub(super) fn explain_run(
        &self,
        cypher: &str,
        params: &BTreeMap<String, Value>,
        facts: &RenderFacts<'_>,
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
        let view = self.notification_view(&_read, facts)?;
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
                facts.started,
                crate::phase_timing::DispatchWindows {
                    execute: execute_started.elapsed(),
                    result_generation: Duration::ZERO,
                },
                facts.timing,
            )
        };
        // The statement-scoped half is fallible (it reads the catalog);
        // EXPLAIN fails with the read's own error rather than silently
        // dropping notes — PROFILE's epilogue settles the same derive with
        // the opposite policy at its own boundary.
        let statement = explain::NotificationInputs {
            view: &view,
            params,
        }
        .derive(facts.logical)?;
        Ok(explain::explain_result(
            facts.logical,
            facts.namespace,
            facts.qtype,
            timing,
            &stats,
            explain::statement_echo(cypher, relate_parser::PreambleMode::Explain),
            statement,
        ))
    }

    /// The PROFILE epilogue: append the profiled plan and its statistics to
    /// a finished result. A failed statement stays failed — PROFILE adds
    /// nothing to a staged execution failure. The epilogue input is
    /// non-optional: `run_admitted_once` calls this only for a PROFILE
    /// statement, so the common case skips the call instead of threading
    /// a `None` through it.
    pub(super) fn profile_epilogue(
        &self,
        outcome: QueryOutcome,
        facts: &RenderFacts<'_>,
        params: &BTreeMap<String, Value>,
        cypher: &str,
        metrics: &crate::profile::ProfileMetrics,
    ) -> QueryOutcome {
        // One signature, one carrier: the admission-derived facts ride as
        // the `RenderFacts` bundle this module defines (REL8-1946) — the
        // stage hand-off convention — and the dependency stays one-way
        // (dispatch imports the carrier from here, never the reverse).
        let QueryOutcome { result, admitted } = outcome;
        let result = match result {
            Ok(Executed::Rows(result)) => Ok(Executed::Rows(
                self.profile_enrich(facts, params, cypher, metrics, result),
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
        facts: &RenderFacts<'_>,
        params: &BTreeMap<String, Value>,
        cypher: &str,
        metrics: &crate::profile::ProfileMetrics,
        result: QueryResult,
    ) -> QueryResult {
        let Some(_read) = self.inner.read_guard().ok() else {
            return result;
        };
        let view = match self.notification_view(&_read, facts) {
            Ok(view) => view,
            Err(_) => return result,
        };
        let statement = match (explain::NotificationInputs {
            view: &view,
            params,
        })
        .derive(facts.logical)
        {
            Ok(statement) => statement,
            Err(_) => return result,
        };
        let stats = crate::stats::GraphPatternStats::new(&self.inner.graph, params);
        explain::profile_result(
            facts.logical,
            metrics,
            result,
            &stats,
            // This path is reached only under the PROFILE epilogue, so its
            // echo strips the PROFILE keyword — the one mode it can run
            // under (the render carrier deliberately holds no mode field;
            // each path names its own constant).
            explain::statement_echo(cypher, relate_parser::PreambleMode::Profile),
            statement,
        )
    }
}
