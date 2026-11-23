//! The run harness: lower Cypher with the product front-end and execute it at a pinned snapshot.
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::thread::LocalKey;

use relate_core::tokens::{workspace_meter, LedgerGuard, TokenLedger};
use relate_core::{Result, Value};
use relate_front::parse_and_lower;
use relate_ir::Query;
use relate_planner::{plan, plan_with_tokens, LogicalPlan, NoPatternStats};

use crate::graph::Graph;
use crate::metrics::{ClauseMetricsRecord, PlanMetricsSink, StatementMemoRecord};
use crate::overlay::ExecGraph;
use crate::pattern_engine::PatternEngine;
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
    run_planned(g, &plan(q, &mut |_| false)?, cypher, params, None)
}

/// The canonical run scaffold over an already-planned query, with a
/// plan-metrics sink attached — the one spelling [`try_exec_lowered`]
/// reads with `None` and [`profiled`] with its recorder, so the scaffold
/// (pin, schema, admission) is never re-spelled beside it. The caller
/// plans: plain ([`plan`]) or token-resolved ([`plan_with_tokens`]).
fn run_planned(
    g: &Graph,
    p: &LogicalPlan,
    cypher: &str,
    params: &BTreeMap<String, Value>,
    plan_metrics: Option<&dyn PlanMetricsSink>,
) -> Result<QueryResult> {
    let pin = g.pin_now()?;
    let schema = g.schema_snapshot();
    let mut request = ExecRequest::new(p, params, cypher);
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
    let planned = plan_with_tokens(
        parse_and_lower(cypher).unwrap(),
        &mut |_| false,
        &NoPatternStats,
        g,
    )
    .unwrap();
    run_planned(g, &planned, cypher, &BTreeMap::new(), None).unwrap()
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

/// Run `cypher` — PROFILEd when a sink is given — through the one
/// plan-and-run spelling the metered and PROFILEd harnesses share.
fn run_cypher_with(g: &Graph, cypher: &str, sink: Option<&dyn PlanMetricsSink>) -> QueryResult {
    let planned = plan(parse_and_lower(cypher).unwrap(), &mut |_| false).unwrap();
    run_planned(g, &planned, cypher, &BTreeMap::new(), sink).unwrap()
}

/// Run `cypher` PROFILEd, collecting the clause records the dispatch
/// emits — the one PROFILE harness the exec tests read (the frontier
/// rider's engagement pins, the store-read arm's save/restore pin).
pub(crate) fn profiled(g: &Graph, cypher: &str) -> Vec<ClauseMetricsRecord> {
    let recorder = super::ClauseRecords(RefCell::new(Vec::new()));
    run_cypher_with(g, cypher, Some(&recorder));
    recorder.0.take()
}

/// A PROFILE sink that keeps the clause records and the statement-level
/// memo facts (ADR-0654 §3): the harness the memo PROFILE pins read.
struct MemoSink {
    clauses: RefCell<Vec<ClauseMetricsRecord>>,
    statement: Cell<StatementMemoRecord>,
}

impl PlanMetricsSink for MemoSink {
    fn record_clause(&self, record: ClauseMetricsRecord) {
        self.clauses.borrow_mut().push(record);
    }

    fn record_statement_memos(&self, record: StatementMemoRecord) {
        self.statement.set(record);
    }
}

/// Run `cypher` PROFILEd, returning the rows, the clause records, and the
/// statement-level (constructor) memo record the sink received.
pub(crate) fn profiled_memos(
    g: &Graph,
    cypher: &str,
) -> (
    Vec<Vec<Value>>,
    Vec<ClauseMetricsRecord>,
    StatementMemoRecord,
) {
    let sink = MemoSink {
        clauses: RefCell::new(Vec::new()),
        statement: Cell::new(StatementMemoRecord::default()),
    };
    let result = run_cypher_with(g, cypher, Some(&sink));
    (result.rows, sink.clauses.take(), sink.statement.get())
}

/// Run `cypher` under a workspace ledger — PROFILEd when a sink is given —
/// returning the statement's result and the peak workspace bytes charged
/// while it ran: the memory-independence tests' one meter, with and
/// without the PROFILE sink the PROFILE-shape variants read. PROFILE must
/// stream what the plain path streams, so its peak is bounded by the same
/// tests.
fn run_metered_with(
    g: &Graph,
    cypher: &str,
    sink: Option<&dyn PlanMetricsSink>,
) -> (QueryResult, u64) {
    let pool = Arc::new(PeakPoolDouble::new(u64::MAX / 4));
    assert!(relate_core::tokens::PeakPool::try_grow(pool.as_ref(), 1));
    let mut ledger = TokenLedger::new(u64::MAX / 4, 1, pool);
    let _guard = LedgerGuard::install(&mut ledger);
    let result = run_cypher_with(g, cypher, sink);
    let (_, peak, _) = workspace_meter().expect("ledger installed");
    (result, peak)
}

/// Run `cypher` PROFILEd under the same workspace ledger: the result
/// rows and the peak — the PROFILE memory-shape tests' meter.
pub(crate) fn profiled_metered(g: &Graph, cypher: &str) -> (Vec<Vec<Value>>, u64) {
    let recorder = super::ClauseRecords(RefCell::new(Vec::new()));
    let (result, peak) = run_metered_with(g, cypher, Some(&recorder));
    (result.rows, peak)
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

/// Order `rows` by their debug form: `Value` carries no total order, the
/// debug form is one, and equal values print equal. The one spelling of the
/// multiset order — [`same_rows`] compares through it and the engine
/// differentials sort the answer they pin against hard-coded rows with it.
pub(crate) fn sort_rows(rows: &mut [Vec<Value>]) {
    rows.sort_by_cached_key(|row| format!("{row:?}"));
}

/// The scan oracle every shortcut suite shares: the statement's `RETURN`
/// gains a `WITH n` stage, a shape no shortcut recognizer takes, so the
/// rewritten statement's answer is the scan's — the differential the
/// shortcut's answer is compared against. Asserts the rewrite applied
/// and the shortcut declines the oracle.
pub(crate) fn with_n_stage(cypher: &str) -> String {
    let scan = cypher.replacen(") RETURN", ") WITH n RETURN", 1);
    assert_ne!(scan, cypher, "the oracle rewrite must apply: {cypher}");
    scan
}

/// The multiset row comparison (the kernels' own differential idiom): the
/// columns agree and the rows agree as multisets ([`sort_rows`]). The one
/// spelling — the frontier rider suite and the statement differential
/// compare their answer sets through it.
pub(crate) fn same_rows(a: &QueryResult, b: &QueryResult) -> bool {
    if a.columns != b.columns {
        return false;
    }
    let mut a = a.rows.clone();
    let mut b = b.rows.clone();
    sort_rows(&mut a);
    sort_rows(&mut b);
    a == b
}

/// The single-statement assembler: the statement as written (`matched`
/// plus what follows it) — the one base spelling the pair below and any
/// caller needing one statement alone read.
pub(crate) fn written(matched: &str, rest: &str) -> String {
    format!("{matched} {rest}")
}

/// The statement differential's pair: the statement as written (matched
/// plus what follows it) and its held twin — a `WITH *` barrier between
/// the match and the aggregation defeating the fold, so the general path
/// answers. Derived from [`written`]; a barrier elsewhere (between two
/// `MATCH` clauses) is a different shape and stays with its suite.
pub(crate) fn written_and_held(matched: &str, rest: &str) -> (String, String) {
    (written(matched, rest), format!("{matched} WITH * {rest}"))
}

/// A graph with the pattern engine forced to the per-row walker
/// (`RELATE_PATTERN_ENGINE=walk`'s in-process spelling): the differential
/// tests' walker-only side.
pub(crate) fn walk_only(mut graph: Graph) -> Graph {
    graph.configure_pattern_engine(PatternEngine::Walk);
    graph
}

/// The three-way differential the streaming-fold tests share: `written`
/// (the statement as the planner sees it) must answer like `expected`
/// (the oracle statement — typically the same statement with a barrier
/// clause defeating the stream, but any statement answering the same
/// rows serves) on a graph from `fixture`, and on the walker-only twin
/// of it. `side` names what `written` exercised in the failure message.
/// The answer set may be empty — [`same_rows`] compares unconditionally;
/// a caller that needs the fixture-matches guard asserts it at its own
/// call site.
pub(crate) fn assert_written_agrees_with_held(
    fixture: impl Fn() -> Graph,
    side: &str,
    written: &str,
    expected: &str,
) {
    let auto = fixture();
    let walk = walk_only(fixture());
    let oracle = exec_cypher(&auto, expected);
    let got = exec_cypher(&auto, written);
    assert!(same_rows(&got, &oracle), "{written}: {side} vs expected");
    let walked = exec_cypher(&walk, written);
    assert!(same_rows(&walked, &oracle), "{written}: walk vs expected");
}

/// Run `cypher` under a workspace ledger; the result's rows and the peak
/// workspace bytes charged while it ran (the memory-independence tests'
/// one meter).
pub(crate) fn run_metered(g: &Graph, cypher: &str) -> (Vec<Vec<Value>>, u64) {
    let (result, peak) = run_metered_with(g, cypher, None);
    (result.rows, peak)
}

/// The record for one clause position, found by the field rather than by
/// list order — a streamed run emits its stage records before the
/// dispatcher writes the head clause, so position in the list is not the
/// clause.
pub(crate) fn record_for(records: &[ClauseMetricsRecord], clause: usize) -> &ClauseMetricsRecord {
    records
        .iter()
        .find(|record| record.clause == clause)
        .unwrap_or_else(|| panic!("a record for clause {clause} of {}", records.len()))
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
