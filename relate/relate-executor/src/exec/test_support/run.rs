//! The run harness: lower Cypher with the product front-end and execute it at a pinned snapshot.
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::thread::LocalKey;

use relate_core::tokens::{workspace_meter, LedgerGuard, TokenLedger};
use relate_core::{Result, Value};
use relate_front::parse_and_lower;
use relate_ir::Query;
use relate_planner::{plan, plan_with_tokens, NoPatternStats};

use crate::graph::Graph;
use crate::metrics::{ClauseMetricsRecord, PlanMetricsSink};
use crate::overlay::ExecGraph;
use crate::test_support::PeakPoolDouble;

use super::super::{execute, execute_read_at, ExecRequest, QueryResult, ReadAdmission};

/// Parse a statement into the flat clause list the statement-shape
/// recognizers take, against the statement's opening stage (one unbound
/// seed row).
pub(crate) fn clauses_of(cypher: &str) -> Vec<relate_ir::Clause> {
    let q = parse_and_lower(cypher).unwrap();
    q.parts.into_iter().flat_map(|p| p.clauses).collect()
}

/// Run an already-lowered `Query` (tests that hand-build or rewrite IR —
/// e.g. the REPEATABLE ELEMENTS unbounded-cap parity test, whose spellings
/// the parser itself rejects) through the same execute path as
/// [`exec_cypher`].
pub(crate) fn exec_lowered(g: &Graph, q: Query, cypher: &str) -> QueryResult {
    try_exec_lowered(g, q, cypher, &BTreeMap::new()).unwrap()
}

/// The fallible form of [`exec_lowered`]: parse/lowering, planning, and
/// execution errors propagate instead of unwrapping, so tests can assert a
/// query's error message whichever phase raises it.
fn try_exec_lowered(
    g: &Graph,
    q: Query,
    cypher: &str,
    params: &BTreeMap<String, Value>,
) -> Result<QueryResult> {
    try_exec_lowered_with_metrics(g, q, cypher, params, None, false)
}

/// The canonical run scaffold with a plan-metrics sink attached — the
/// one spelling [`try_exec_lowered`] reads with `None` and [`profiled`]
/// with its recorder, so the scaffold (plan, pin, schema, admission) is
/// never re-spelled beside it.
fn try_exec_lowered_with_metrics(
    g: &Graph,
    q: Query,
    cypher: &str,
    params: &BTreeMap<String, Value>,
    plan_metrics: Option<&dyn PlanMetricsSink>,
    resolve_tokens: bool,
) -> Result<QueryResult> {
    let p = if resolve_tokens {
        plan_with_tokens(q, &mut |_| false, &NoPatternStats, g)?
    } else {
        plan(q, &mut |_| false)?
    };
    let pin = g.pin_now()?;
    let schema = g.schema_snapshot();
    let mut request = ExecRequest::new(&p, params, cypher);
    request.plan_metrics = plan_metrics;
    execute_read_at(&ReadAdmission {
        graph: g,
        snapshot_ts: pin.ts(),
        schema: &schema,
        request,
        cancel: None,
        ontology_document: None,
    })
}

pub(crate) fn exec_cypher(g: &Graph, cypher: &str) -> QueryResult {
    exec_lowered(g, parse_and_lower(cypher).unwrap(), cypher)
}

/// [`exec_cypher`] planned with the store's tokens resolved onto the IR
/// (ADR-0541), the way the facade plans — the run the id-consuming reads
/// are tested under.
pub(crate) fn exec_cypher_resolved(g: &Graph, cypher: &str) -> QueryResult {
    try_exec_lowered_with_metrics(
        g,
        parse_and_lower(cypher).unwrap(),
        cypher,
        &BTreeMap::new(),
        None,
        true,
    )
    .unwrap()
}

/// The fallible form of [`exec_cypher`] (ADR-0175's integer-argument-site
/// tests assert errors raised at plan time and at runtime).
pub(crate) fn try_exec_cypher(g: &Graph, cypher: &str) -> Result<QueryResult> {
    try_exec_cypher_params(g, cypher, &BTreeMap::new())
}

/// [`try_exec_cypher`] with statement parameters.
pub(crate) fn try_exec_cypher_params(
    g: &Graph,
    cypher: &str,
    params: &BTreeMap<String, Value>,
) -> Result<QueryResult> {
    try_exec_lowered(g, parse_and_lower(cypher)?, cypher, params)
}

/// The error message of a run the test expects to fail — the one spelling
/// (`QueryResult` is not `Debug`, so `unwrap_err` does not compile): every
/// error-parity differential (the path fold's, the kernels', the batch
/// chain's, the predicate specialization's) extracts the message through
/// it, never a second local closure.
pub(crate) fn error_of(result: Result<QueryResult>) -> String {
    match result {
        Err(err) => err.to_string(),
        Ok(done) => panic!(
            "the query errors, but it returned a result: {:?}",
            done.rows
        ),
    }
}

/// Run `f` with the thread-local disable-flag `flag` forced on, restoring
/// it on the way out — including on unwind, so a panicking assertion
/// inside the closure cannot leave the path disabled for every later test
/// on the thread. The one spelling of the A/B differential hook (audit
/// finding B3): every `with_X_disabled` in the layer delegates here, each
/// site keeping only its own flag declaration and engagement counter.
pub(crate) fn with_flag_disabled<R>(
    flag: &'static LocalKey<Cell<bool>>,
    f: impl FnOnce() -> R,
) -> R {
    /// Restores the flag on the way out — including on unwind.
    struct Restore {
        flag: &'static LocalKey<Cell<bool>>,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            self.flag.with(|cell| cell.set(false));
        }
    }
    flag.with(|cell| cell.set(true));
    let _restore = Restore { flag };
    f()
}

/// Run `cypher` PROFILEd, collecting the clause records the dispatch
/// emits — the one PROFILE harness the exec tests read (the frontier
/// rider's engagement pins, the store-read arm's save/restore pin).
pub(crate) fn profiled(g: &Graph, cypher: &str) -> Vec<ClauseMetricsRecord> {
    struct Recorder(RefCell<Vec<ClauseMetricsRecord>>);
    impl PlanMetricsSink for Recorder {
        fn record_clause(&self, record: ClauseMetricsRecord) {
            self.0.borrow_mut().push(record);
        }
    }
    let recorder = Recorder(RefCell::new(Vec::new()));
    try_exec_lowered_with_metrics(
        g,
        parse_and_lower(cypher).unwrap(),
        cypher,
        &BTreeMap::new(),
        Some(&recorder),
        false,
    )
    .unwrap();
    recorder.0.take()
}

/// Run `cypher` on an existing view through `execute`, so the test can
/// inspect per-view state (e.g. the CSV reader's physical-read count) after
/// the run — the view-bound counterpart of [`exec_cypher`].
pub(crate) fn exec_cypher_on(graph: &ExecGraph<'_>, cypher: &str) -> QueryResult {
    try_exec_cypher_on(graph, cypher).unwrap()
}

/// The fallible form of [`exec_cypher_on`].
pub(crate) fn try_exec_cypher_on(graph: &ExecGraph<'_>, cypher: &str) -> Result<QueryResult> {
    let p = plan(parse_and_lower(cypher)?, &mut |_| false)?;
    execute(graph, &ExecRequest::new(&p, &BTreeMap::new(), cypher))
}

/// The multiset row comparison (the kernels' own differential idiom):
/// `Value` carries no total order, the debug form is one, and equal
/// values print equal. The one spelling — the frontier rider suite and
/// the statement differential compare their answer sets through it.
pub(crate) fn same_rows(a: &QueryResult, b: &QueryResult) -> bool {
    if a.columns != b.columns {
        return false;
    }
    let mut a = a.rows.clone();
    let mut b = b.rows.clone();
    a.sort_by_cached_key(|row| format!("{row:?}"));
    b.sort_by_cached_key(|row| format!("{row:?}"));
    a == b
}

/// Run `cypher` under a workspace ledger; the result's rows and the peak
/// workspace bytes charged while it ran (the memory-independence tests'
/// one meter).
pub(crate) fn run_metered(g: &Graph, cypher: &str) -> (Vec<Vec<Value>>, u64) {
    let pool = Arc::new(PeakPoolDouble::new(u64::MAX / 4));
    assert!(relate_core::tokens::PeakPool::try_grow(pool.as_ref(), 1));
    let mut ledger = TokenLedger::new(u64::MAX / 4, 1, pool);
    let _guard = LedgerGuard::install(&mut ledger);
    let rows = exec_cypher(g, cypher).rows;
    let (_, peak, _) = workspace_meter().expect("ledger installed");
    (rows, peak)
}

/// The one record whose `MATCH` clause a rung served — the ladder's own
/// fact (`kernel` is set exactly when a `MATCH` operator ran), found by
/// the field rather than by clause position. The PROFILE-record suites
/// (the rung, the decline reasons) read through it.
pub(crate) fn served(records: &[ClauseMetricsRecord]) -> &ClauseMetricsRecord {
    records
        .iter()
        .find(|record| record.kernel.is_some())
        .unwrap_or_else(|| panic!("a rung served the MATCH clause ({} records)", records.len()))
}
