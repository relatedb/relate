//! Query execution entry points, plan folding, and the `exec/` submodule
//! wiring.

use std::collections::BTreeMap;

use relate_core::{CancelToken, Error, QueryClockGuard, Result, Value};
use relate_ir::{Clause, LabelExpr};
use relate_names::{SmallName, Token};
use relate_planner::{clause_mutates_store, plan_any_clause, LogicalPlan};

use crate::metrics::{PlanMetricsSink, PlanMetricsSite};

use crate::row::Row;

// The instrumentation cluster (LABS-448's extraction) — one private
// module, re-exported here so the walkers' `super::` paths keep working:
// the store-read counters and the walk-start slot. The phase clocks live
// in `crate::ctx` (LABS-668), the lowest module both walk engines share.
mod profiling;
use crate::graph::{Graph, QueryStats, SchemaSnapshot};
use crate::overlay::ExecGraph;
use crate::procedure_registry::call_writes_store;
use crate::StatementEffect;
pub(crate) use profiling::{
    arm_store_profiling, count_frontier, count_neighbor_walks, count_zone_skip, record_memos,
    record_walk_start, restore_store_profiling, take_walk_start, DeltaSnapshot, StageDelta,
    WalkStartSwap, WalkTallies,
};
#[cfg(test)]
pub(crate) use profiling::{
    disarm_store_profiling, memo_counters, take_frontier_counters, take_hop_expands,
    take_neighbor_walks, take_node_reads, take_zone_skips,
};

pub(crate) mod agg_fold;
pub(crate) mod aggregate;
pub(crate) mod batch;
pub(crate) mod batch_tail;
pub(crate) mod bulk;
pub(crate) mod call;
pub(crate) mod candidates;
pub(crate) mod catalog;
pub(crate) mod catalog_schema;
pub(crate) mod catalog_show;
pub(crate) mod create_admission;
// ADR-0468's SoA chunk type layer: the types, the conversion, and the
// operator-serving constructions the batch chain (exec/batch.rs) reads.
pub(crate) mod chunk;
pub(crate) mod classification;
mod clause_arms;
pub(crate) mod compose;
mod conditional_branches;
pub(crate) mod constraint_validation;
pub(crate) mod create;
pub(crate) mod ddl;
pub(crate) mod ddl_notifications;
pub(crate) mod ddl_options;
pub(crate) mod dispatch;
pub(crate) mod exists_probe;
pub(crate) mod expand;
mod expr_safety;
pub(crate) mod far_probe;
pub(crate) mod feed;
pub(crate) mod fulltext;
pub(crate) mod gpm;
pub(crate) mod graph_type_schema;
mod load_csv;
mod match_arm;
// The plan-grade probe (ADR-0687 decision 6): private module, its two
// public types re-exported below (one public path).
mod plan_grade;
mod profile_chain;
mod profile_tick;
mod read;
// The LABS-348 hub join — the whole family (gate, join, row-shape
// helpers, test hooks), invoked by the driver at its pattern-0→
// pattern-1 boundary (`match_clause::match_paths`) over the caller's
// own pattern-0 rows.
pub(crate) mod hub_join;
// ADR-0473 (LABS-644): the contracted traversal kernels — structural
// matcher, cost gate, chain and hub-star operators over the substrate.
pub(crate) mod kernels;
pub(crate) mod match_cap;
pub(crate) mod match_chain;
pub(crate) mod match_clause;
pub(crate) mod match_consumer;
pub(crate) mod match_folds;
pub(crate) mod named_path;
// The one engine fork (`match_one_path`) every per-path consumer routes
// through — the driver's pattern loop (match_clause), the hub join's
// plain-side enumeration, and the exists probe's dispatch — hoisted
// beside `expand` so every consumer imports it downward.
pub(crate) mod one_path;
pub(crate) mod order;
pub(crate) mod pattern_policies;
pub(crate) mod pivot;
pub(crate) mod post_merge;
pub(crate) mod predicate_spec;
pub(crate) mod procedure_rows;
pub(crate) mod procedures;
pub(crate) mod project;
pub(crate) mod property_defaults;
pub(crate) mod pushdown;
pub(crate) mod rel_seek;
pub(crate) mod rider;
mod row_production;
pub(crate) mod scope;
pub(crate) mod search;
pub(crate) mod search_filters;
mod seeded_count;
#[cfg(test)]
mod seeded_count_tests;
pub(crate) mod shortest_eval;
pub(crate) mod sink;
mod stats_short_circuit;
pub(crate) mod subquery_eval;
pub(crate) mod top_k;
pub(crate) mod union;
pub(crate) mod unwind;
pub(crate) mod vector;
pub(crate) mod walk_plans;
pub(crate) mod write;

pub use catalog_show::catalog_create_statements;
pub use plan_grade::GradeProbe;
pub use read::{execute_read_at, execute_read_streaming_at};

/// The read path's one rejection policy: the message a `ClauseMode::Read`
/// clause loop refuses `c` with, or `None` to dispatch it. The dispatcher
/// asks once per clause, before its clause match.
///
/// The table is the planner's published per-clause answer
/// ([`clause_mutates_store`], catalog commands included: the `SHOW` family is
/// a read, a catalog DDL command is a write) plus the two shapes this layer
/// decides: `FOREACH` names itself, and `CALL { … } IN TRANSACTIONS` needs a
/// write transaction. A catalog write reaching a read run is therefore a
/// typed rejection, not a write that only the read-only scratch's panic
/// backstops.
///
/// The name states the policy, not a clause classification: it answers "does
/// the read path reject this clause, and with what message?", which is
/// narrower than the mutation question the composition rules ask
/// ([`mutates_store`]).
pub(crate) fn read_path_rejection(c: &Clause) -> Option<&'static str> {
    match c {
        Clause::Foreach { .. } => Some("internal: FOREACH on read path"),
        Clause::CallSubquery {
            in_transactions: Some(_),
            ..
        } => Some("internal: CALL IN TRANSACTIONS requires a write transaction"),
        other if clause_mutates_store(other) => Some("internal: write clause on read path"),
        _ => None,
    }
}

/// Whether one clause mutates the store — the per-clause form of
/// [`plan_mutates_store`], shared with the composition rules that ask the
/// mutation question rather than the read-admission one. The two answers
/// this layer decides below the planner table live here:
///
/// - `LOAD CSV` is `Write` in the clause table (write-*routed* by the
///   conservative access fold, ADR-0332) but only reads a file;
/// - a `CALL` is the procedure registry's writes flag
///   ([`call_writes_store`]) — a registered procedure is a static row table
///   and cannot mutate the store.
pub(crate) fn mutates_store(clause: &Clause) -> bool {
    match clause {
        Clause::LoadCsv { .. } => false,
        Clause::Call { name, .. } => name.with_str(call_writes_store),
        other => clause_mutates_store(other),
    }
}

/// Whether `plan` holds a mutation operator — the statement's ADR-0402
/// Decision 2 write-attempt fact, recorded once at statement entry (before
/// any rows flow) through the canonical plan walk
/// (`relate_planner::plan_any_clause` over `walk_plan_clauses`): every
/// part, every `NEXT` stage, and every nested body the walk descends into
/// (`CALL { … }`, `WHEN`/`ELSE`, `FOREACH` bodies, expression subqueries). A
/// write inside a body that never dispatches — an empty driving row set, an
/// untaken branch — is therefore still an attempt, which is the same
/// short-circuit the empty top-level pipeline is made of. Both statement
/// boundaries that name an effect record it here — the write path and the
/// read admission — so the fact has one source on every route.
///
/// The planner's [`clause_mutates_store`] answers each clause; the two shapes
/// whose answer lives at this layer are decided by [`mutates_store`], which
/// this walk applies clause by clause.
pub(crate) fn plan_mutates_store(plan: &LogicalPlan) -> bool {
    plan_any_clause(plan, &mut mutates_store)
}

/// Record on the statement's view every name the plan declares — the one
/// per-clause vocabulary ([`scope::clause_declared_names`]) folded
/// cumulatively over the same canonical plan walk
/// ([`plan_mutates_store`]'s `plan_any_clause`), nested bodies (`CALL { … }`,
/// `FOREACH`, expression subqueries) included, so the set is complete
/// before any clause runs and does not depend on which body has run. A
/// name declared only in a nested body is therefore declared
/// statement-wide; no synthetic column can carry it, because every
/// synthetic prefix is chosen past the declared set
/// ([`ExecGraph::uncolliding_prefix`]). Cumulative: a name dropped by a
/// later `WITH` stays declared, since rows between the two still carry it.
/// Called only by [`ExecGraph::begin_statement`], so the reset and this
/// seeding are one operation no entry can split.
pub(crate) fn declare_plan_names(graph: &ExecGraph<'_>, plan: &LogicalPlan) {
    plan_any_clause(plan, &mut |clause| {
        let names = scope::clause_declared_names(clause);
        graph.declare_names(names.iter().map(String::as_str));
        false
    });
}

use constraint_validation::validate_constraints;
use dispatch::run_clauses;
use scope::visible_keys;
pub(crate) use shortest_eval::{eval_shortest, ShortestWant};
pub(crate) use subquery_eval::{eval_collect_query, eval_count_query, eval_exists_query};

use crate::ctx::QueryCtx;

/// The lowering duplicates a static label/type list into a `LabelExpr`
/// (`labels`/`types` carry the same names). A label expression is
/// batch-safe exactly when it is a tree of static `Label` leaves over
/// precisely those names — no wildcards, dynamics, or negation.
/// Hoisted from `exec/bulk.rs` (round 4, F-4-1): the static-mirror
/// discrimination the bulk-write recognizer and the count-only shape
/// recognizer share — one canonical helper per policy.
pub(crate) fn static_label_tree(expr: &LabelExpr, names: &[(SmallName, Token)]) -> bool {
    fn leaves<'a>(expr: &'a LabelExpr, out: &mut Vec<&'a SmallName>) -> bool {
        match expr {
            LabelExpr::Label(name) => {
                out.push(name);
                true
            }
            LabelExpr::And(parts) | LabelExpr::Or(parts) => {
                parts.iter().all(|part| leaves(part, out))
            }
            _ => false,
        }
    }
    let mut found = Vec::new();
    leaves(expr, &mut found)
        && found.len() == names.len()
        && found
            .iter()
            .all(|name| names.iter().any(|(n, _)| n == *name))
}

/// Whether an optional label expression is absent or static-matches
/// `names` — the one gate every label-expression-accepting recognizer
/// consults: gate on static-vs-dynamic, never on presence (round 4,
/// F-4-1/F-4-2: the raw front-lowered shape carries the static mirror,
/// and a presence gate would decline real pipeline shapes).
pub(crate) fn label_expr_ok(expr: &Option<LabelExpr>, names: &[(SmallName, Token)]) -> bool {
    expr.as_ref().is_none_or(|e| static_label_tree(e, names))
}

pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub stats: QueryStats,
    /// The engine's truthful write effect for this statement (ADR-0402
    /// Decision 2), named once at the statement boundary from two execution
    /// facts: whether the statement's plan holds a mutation operator (the one
    /// plan walk, [`plan_mutates_store`]) and whether any change reached the
    /// write redo since the statement's entry reset. A read therefore carries
    /// [`StatementEffect::None`]. Results built *inside* a statement (a
    /// clause's rows, a nested body's output) go through the crate-private
    /// `QueryResult::intermediate` and never spell this field: the boundary is
    /// the one place that names it. See [`StatementEffect`] for why neither
    /// the plan's query type nor the mutation counters can answer this.
    pub effect: StatementEffect,
}

impl QueryResult {
    /// A result a statement boundary finished, its effect named by the
    /// execution that produced it — the construction for a crate outside this
    /// one (the ADR-0209 batched bulk write is its own statement boundary and
    /// runs no clause loop). The effect always comes from
    /// [`StatementEffect`]'s own constructors, so a caller never spells a
    /// zero state at the construction site.
    pub fn finished(
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
        stats: QueryStats,
        effect: StatementEffect,
    ) -> Self {
        Self {
            columns,
            rows,
            stats,
            effect,
        }
    }

    /// A result built *inside* a statement — a clause's rows, a nested
    /// body's output, a finishing projection — before the boundary names its
    /// effect.
    ///
    /// The effect starts as the unstamped read state; the statement boundary
    /// ([`execute`] on the write path, `execute_read_at` on the read path) is
    /// the one place that names it from the execution's own facts. Routing
    /// every intermediate construction through this constructor is what keeps
    /// a placeholder out of the construction sites: no clause can hand a
    /// surface an effect that claims a write applied nothing.
    pub(crate) fn intermediate(
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
        stats: QueryStats,
    ) -> Self {
        Self::finished(columns, rows, stats, StatementEffect::None)
    }

    /// CIP-159's empty unit result — no columns, no rows, default stats —
    /// the one spelling of the terminal-void answer.
    pub(crate) fn unit() -> Self {
        Self::intermediate(Vec::new(), Vec::new(), QueryStats::default())
    }
}

/// The plan, parameters, source text, and authenticated principal shared by
/// every execution entry point. `query_text` is the source text of the
/// statement (exposed as `SHOW TRANSACTIONS`' `currentQuery` column); the
/// statement's CIP-150 version preamble rides on the plan. `principal` is the
/// connection's authenticated principal (ADR-0273), read by
/// `dbms.showCurrentUser`; `None` reports the documented placeholder.
/// `Copy`: every field is a reference (or an optional reference) — the
/// commit path reuses the caller's request with its own batch-commit
/// sink attached, without rebuilding the bundle field by field.
#[derive(Clone, Copy)]
pub struct ExecRequest<'a> {
    pub plan: &'a LogicalPlan,
    pub params: &'a BTreeMap<String, Value>,
    pub query_text: &'a str,
    pub principal: Option<&'a str>,
    /// ADR-0210: per-batch durable-commit sink for `CALL { … } IN
    /// TRANSACTIONS`; `None` keeps the accumulate-to-statement-end
    /// behavior.
    pub batch_commit: Option<&'a dyn crate::ctx::BatchCommit>,
    /// ADR-0344 phase 2: the PROFILE metrics sink for the statement's own
    /// clauses; `None` on every non-PROFILE run. The dispatch loop
    /// records one entry per clause operator; nested execution strips it.
    pub plan_metrics: Option<&'a dyn crate::metrics::PlanMetricsSink>,
}

impl<'a> ExecRequest<'a> {
    /// The None-default request: no principal, no batch-commit sink, no
    /// PROFILE sink — the statement's own execution with none of the
    /// optional carriers. The one construction site for the defaults, so
    /// a new optional field lands here, not at every caller.
    pub fn new(
        plan: &'a LogicalPlan,
        params: &'a BTreeMap<String, Value>,
        query_text: &'a str,
    ) -> Self {
        Self {
            plan,
            params,
            query_text,
            principal: None,
            batch_commit: None,
            plan_metrics: None,
        }
    }
}

/// A read admitted at a caller-pinned timestamp: the `graph`, the pinned
/// `snapshot_ts`, and the `schema` captured together with that pin under the
/// publication read guard, plus the `request` to run and its optional
/// cooperative cancellation token (ADR-0111; `None` is never cancelled).
pub struct ReadAdmission<'a> {
    pub graph: &'a Graph,
    pub snapshot_ts: u64,
    pub schema: &'a SchemaSnapshot,
    pub request: ExecRequest<'a>,
    pub cancel: Option<&'a CancelToken>,
    /// The store's ontology document (verbatim) captured at admission
    /// (ADR-0121 §3, ADR-0227): attached to the read view like `cancel`
    /// and read by `db.ontology.get` / `db.ontology.apply`; `None` when
    /// the store holds no ontology.
    pub ontology_document: Option<&'a str>,
}

/// The write boundary a batch of writes closes with, in order: the graph
/// type's property default values, then the constraint validation. One
/// home beside the write pipeline — the statement boundary
/// ([`execute`]) and the `CALL { … } IN TRANSACTIONS` batch attempt both
/// call it, so the two cannot drift.
pub(crate) fn close_write_boundary(graph: &ExecGraph<'_>) -> Result<()> {
    property_defaults::apply_property_defaults(graph)?;
    validate_constraints(graph)
}

/// Execute a write-capable plan, then validate the constraints against what
/// it wrote (ADR-0172). Each call starts a fresh CSV memo even when `graph`
/// is reused across statements.
pub fn execute(graph: &ExecGraph<'_>, request: &ExecRequest<'_>) -> Result<QueryResult> {
    let (result, attempted) = run_statement(graph, request)?;
    // CIP-262: the statement is this engine's transaction, so the graph
    // type's property default values land at its boundary -- after the
    // writes (the statement's own RETURN already saw null, CE1), before
    // the post-statement constraint validation sees the defaulted
    // properties like any other write. They are writes, so they land
    // before the effect's applied fact is read (below).
    close_write_boundary(graph)?;
    finish_statement(result, graph, attempted)
}

/// [`execute`] without the constraint validation: the statement's writes
/// stay in the redo slice the next `validate_constraints` checks. The
/// executor's own tests drive this entry (`exec::test_support`); production
/// always takes the validated one.
///
/// It is the same statement boundary, so it names the effect from its own two
/// facts rather than handing back a placeholder: a caller that stops here
/// still cannot read a read state off a statement that wrote.
#[cfg(test)]
pub(crate) fn execute_unvalidated(
    graph: &ExecGraph<'_>,
    request: &ExecRequest<'_>,
) -> Result<QueryResult> {
    let (result, attempted) = run_statement(graph, request)?;
    finish_statement(result, graph, attempted)
}

/// The private un-finished runner every write statement entry shares: the
/// per-statement resets, the entry checks, the entry facts, and the plan's
/// execution. It never names the effect and never polls the token ledger —
/// [`finish_statement`] does both, exactly once, so `execute` and
/// `execute_unvalidated` cannot stamp a statement twice.
///
/// Returns the un-finished result and the statement's write-attempt fact
/// ([`plan_mutates_store`]), recorded here before any rows flow and handed to
/// [`finish_statement`] unchanged.
fn run_statement(graph: &ExecGraph<'_>, request: &ExecRequest<'_>) -> Result<(QueryResult, bool)> {
    let entry = begin_statement_entry(graph, request)?;
    let result = walk_statement(
        graph,
        request.plan,
        vec![entry.root],
        request.plan_metrics,
        |part_clauses, part_seeds, site| {
            let (mut rows, last) = run_clauses(
                graph,
                part_clauses,
                request.params,
                part_seeds,
                entry.ctx,
                site,
            )?;
            Ok(call::concluding_part_result(
                graph,
                part_clauses,
                &mut rows,
                last,
                graph.stats(),
            ))
        },
    )?;
    Ok((result, entry.attempted))
}

/// The statement-entry prelude's one product: the statement's single root
/// seed row, its write-attempt fact ([`plan_mutates_store`], walked before
/// any rows flow), the [`QueryCtx`] its clauses run under, and the clock
/// guard that pins the statement's clock. The guard's contract — it must
/// outlive the run — is a field of this value, so an entry that keeps the
/// entry alive for the run keeps the clock pinned; no caller has to remember
/// a tuple slot.
struct StatementEntry<'q> {
    root: Row,
    attempted: bool,
    ctx: QueryCtx<'q>,
    /// Pins the statement's clock; never read, dropped with the entry.
    _clock: QueryClockGuard,
}

/// The prelude every statement entry runs, in one place: the one
/// statement-entry operation ([`ExecGraph::begin_statement`]) with the single
/// root row it mints, the cancellation check, the statement clock, and the
/// entry facts. Both entries pass the same [`ExecRequest`] bundle, so the
/// request's own fields are the one source of the plan, the query text, the
/// principal and the batch-commit sink — and the clock is the graph's own
/// ([`ExecGraph::transaction_clock`], `None` on a read view, its transaction
/// clock on a write view), never a parameter beside the same fact.
fn begin_statement_entry<'q>(
    graph: &ExecGraph<'_>,
    request: &'q ExecRequest<'q>,
) -> Result<StatementEntry<'q>> {
    let root = graph.begin_statement(request.plan);
    graph.check_cancelled()?;
    let clock = QueryClockGuard::within(graph.transaction_clock());
    let attempted = plan_mutates_store(request.plan);
    let ctx = QueryCtx::new(request.query_text)
        .with_principal(request.principal)
        .with_batch_commit(request.batch_commit);
    Ok(StatementEntry {
        root,
        attempted,
        ctx,
        _clock: clock,
    })
}

/// Name the statement's effect and finish the result — the one assignment to
/// [`QueryResult::effect`], and the one token-ledger poll. Every entry into a
/// statement (the write path, the unvalidated path, a read admission) runs
/// the un-finished runner and ends here exactly once, so no entry can return
/// a fabricated effect for a statement that wrote, and none stamps twice.
///
/// The two facts are taken where they are known: `attempted` from the plan
/// walk at statement entry, `applied` from the statement's own change count —
/// zeroed by the same entry reset as the stats and grown by every redo push,
/// so no baseline has to travel beside it.
fn finish_statement(
    mut result: QueryResult,
    graph: &ExecGraph<'_>,
    attempted: bool,
) -> Result<QueryResult> {
    result.effect = StatementEffect::from_facts(attempted, graph.applied_ops() > 0);
    let result = finish_tokens(result)?;
    // ADR-0691: last, so the effect stamp and the token poll stay the
    // boundary's exactly-once steps and a refusal is only ever the cap's.
    graph.check_result_expansion(&result.rows)?;
    Ok(result)
}

fn finish_tokens(result: QueryResult) -> Result<QueryResult> {
    relate_core::tokens::poll()?;
    Ok(result)
}

pub(crate) fn result_seed_rows(result: &QueryResult) -> Vec<Row> {
    result
        .rows
        .iter()
        .map(|values| {
            result
                .columns
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    (
                        name.clone(),
                        values
                            .get(i)
                            .cloned()
                            .expect("QueryResult rows are rectangular with respect to its columns"),
                    )
                })
                .collect()
        })
        .collect()
}

/// Result of the rows' visible keys (sorted). This clones: the clause-exit
/// PROFILE tick still reads these rows. [`materialize_rows`] is the move
/// path, used once a tick has already recorded the clause.
pub(crate) fn rows_as_result(graph: &ExecGraph<'_>, rows: &[Row]) -> QueryResult {
    let mut columns = rows
        .first()
        .map(|row| visible_keys(graph, row))
        .unwrap_or_default();
    columns.sort();
    let data = rows
        .iter()
        .map(|row| {
            read_declared_columns(row, &columns, Missing::Null)
                .expect("padding a missing result column with null is infallible")
        })
        .collect();
    QueryResult::intermediate(columns, data, graph.stats())
}

/// How the one materialization loop treats a declared column the row does
/// not bind.
#[derive(Clone, Copy)]
pub(crate) enum Missing {
    /// Pad the cell with null (the CALL export policy).
    Null,
    /// A miss is the engine contract break it is — every declared column is
    /// bound by the same rule that declared it.
    Error,
}

/// One cell of a declared column under the one miss policy — the single
/// spelling both halves of the cell loop read, so the borrowed and the
/// moving read cannot disagree on what a miss means.
fn declared_cell(value: Option<Value>, column: &str, missing: Missing) -> Result<Value> {
    match (value, missing) {
        (Some(value), _) => Ok(value),
        (None, Missing::Null) => Ok(Value::Null),
        (None, Missing::Error) => Err(missing_column_error(column)),
    }
}

/// The one miss message — for [`declared_cell`] and for the arms that read a
/// column without going through the cell loop (the borrowed single-column
/// DISTINCT key), so a miss reads the same wherever it is reported.
pub(crate) fn missing_column_error(column: &str) -> Error {
    Error::internal(format!("row is missing declared result column {column:?}"))
}

/// One row's declared columns, moved out — for a caller that owns its rows
/// and does not need them again. A miss under [`Missing::Error`] is the
/// engine contract break it is.
pub(crate) fn take_declared_columns(
    row: &mut Row,
    columns: &[String],
    missing: Missing,
) -> Result<Vec<Value>> {
    columns
        .iter()
        .map(|column| declared_cell(row.take_value(column), column, missing))
        .collect()
}

/// One row's declared columns, cloned — the borrowed half of the same
/// loop, for a caller that still needs the row (a clause-exit PROFILE tick
/// has read it, a catalog row is read twice).
pub(crate) fn read_declared_columns(
    row: &Row,
    columns: &[String],
    missing: Missing,
) -> Result<Vec<Value>> {
    columns
        .iter()
        .map(|column| declared_cell(row.get(column).cloned(), column, missing))
        .collect()
}

/// The one materialization loop: move each cell of `rows` onto `columns`.
/// The source slots are left null; callers that still need the values must
/// clone (or record their metrics) before this.
fn materialize_rows_with(
    rows: &mut [Row],
    columns: &[String],
    missing: Missing,
) -> Result<Vec<Vec<Value>>> {
    rows.iter_mut()
        .map(|row| take_declared_columns(row, columns, missing))
        .collect()
}

/// Materialize `rows` onto `columns`, moving each cell and padding misses
/// with null. See [`materialize_rows_with`] for the move semantics.
pub(crate) fn materialize_rows(rows: &mut [Row], columns: &[String]) -> Vec<Vec<Value>> {
    materialize_rows_with(rows, columns, Missing::Null)
        .expect("padding a missing result column with null is infallible")
}

/// Materialize `rows` onto `columns`, moving each cell and treating a miss
/// as the engine contract break it is. The projection result path's policy.
pub(crate) fn materialize_rows_strict(
    rows: &mut [Row],
    columns: &[String],
) -> Result<Vec<Vec<Value>>> {
    materialize_rows_with(rows, columns, Missing::Error)
}

/// The rows a projection consumes: the one inter-clause stream — a held
/// container or a generator clause fused into the projection (ADR-0660) —
/// or the aggregation a producer already folded the rows into. The stream
/// is the only row-bearing form; a `Folded` feed holds no rows, so its
/// consumer finishes it (`aggregate_rows`) instead of walking it. The
/// projection pipeline's input shape lives here — the seam the dispatcher
/// (which builds one), the generator producers (which feed one) and the
/// projection and aggregation consumers (which read one) all already
/// reach; the stream machinery itself lives in [`feed`], beside this seam.
pub(crate) enum RowFeed<'a> {
    /// The one inter-clause stream: a held container, or a generator the
    /// projection is fused with.
    Held(feed::RowStream<'a>),
    /// Rows already folded into an [`aggregate::Aggregation`] by the
    /// producer: the projection only finishes it. Built by the streaming
    /// match sink.
    Folded(Box<aggregate::Aggregation<'a>>),
}

impl<'a> RowFeed<'a> {
    /// The rows already in hand, for the decisions that read the row shape
    /// before the walk starts. A generated stream has none yet.
    pub(crate) fn probe(&self) -> &[Row] {
        match self {
            RowFeed::Held(stream) => stream.probe(),
            RowFeed::Folded(_) => &[],
        }
    }
}

/// One statement's stage walk — the one spelling both statement entries run
/// after their prelude (ADR-0344 phase 2 review): the walk starts at the
/// plan's root stage over the entry's seeds, folds each stage's parts
/// through the entry's per-part runner, and recurses into the NEXT stage with
/// the shared seeding policy (ADR-0287) and the stage count (the tree's stage
/// numbering: the root is stage 1, every NEXT link one later). Each part's
/// PROFILE site is derived here, where the `(stage, part)` key is known — the
/// sink the admission attached and the part's own numbers, one fact — and
/// the statement's constructor-memo facts are reported once the walk
/// finished. The entries supply only the per-part runner: the write path's
/// clause loop with its concluding result, the read path's read part.
fn walk_statement(
    graph: &ExecGraph<'_>,
    plan: &LogicalPlan,
    seeds: Vec<Row>,
    sink: Option<&dyn PlanMetricsSink>,
    run_part: impl Fn(&[Clause], Vec<Row>, Option<PlanMetricsSite<'_>>) -> Result<QueryResult>,
) -> Result<QueryResult> {
    fn walk_stages(
        plan: &LogicalPlan,
        stage: usize,
        seeds: Vec<Row>,
        sink: Option<&dyn PlanMetricsSink>,
        run_part: &impl Fn(&[Clause], Vec<Row>, Option<PlanMetricsSite<'_>>) -> Result<QueryResult>,
    ) -> Result<QueryResult> {
        let site = |part: usize| sink.map(|sink| PlanMetricsSite::at(sink, stage, part));
        // A stage with one part consumes the seeds. Later parts of a UNION
        // each need the same bag, so those plans still clone it.
        let result = if plan.parts.len() == 1 {
            run_part(plan.parts[0].as_slice(), seeds, site(0))?
        } else {
            union::fold_parts(plan, |part, part_clauses| {
                run_part(part_clauses, seeds.clone(), site(part))
            })?
        };
        match &plan.next {
            Some(next) => walk_stages(
                next,
                stage + 1,
                next_seeds(&result, plan.parts.iter().map(|p| p.as_slice())),
                sink,
                run_part,
            ),
            None => Ok(result),
        }
    }
    let result = walk_stages(plan, 1, seeds, sink, &run_part)?;
    report_statement_memos(graph, sink);
    Ok(result)
}

/// Hand the statement's constructor-memo facts (ADR-0654 §3) to the
/// PROFILE sink once the statement finished: they serve any expression, so
/// they belong to the statement, not to an operator. A non-PROFILE
/// statement has no sink and reads nothing.
fn report_statement_memos(graph: &ExecGraph<'_>, sink: Option<&dyn PlanMetricsSink>) {
    if let Some(sink) = sink {
        sink.record_statement_memos(graph.construct_memo_record());
    }
}

/// The NEXT-stage seeding policy (one spelling, every NEXT-chain walker
/// reads it — both pass their parts iterator, plan-level or subquery):
/// the stage's result rows, or — for a stage whose unit output is a
/// single row (ADR-0287) — one empty seed so the next stage still runs.
pub(crate) fn next_seeds<'p>(
    result: &QueryResult,
    parts: impl Iterator<Item = &'p [Clause]>,
) -> Vec<Row> {
    let mut seeds = result_seed_rows(result);
    if seeds.is_empty() && row_production::parts_have_unit_output(parts) {
        seeds.push(Row::new());
    }
    seeds
}

#[cfg(test)]
mod aggregate_charge_tests;
#[cfg(test)]
mod aggregate_footprint_tests;
#[cfg(test)]
mod aggregate_percentile_tests;
#[cfg(test)]
mod aggregate_pool_tests;
#[cfg(test)]
mod aggregate_tests;
#[cfg(test)]
mod batch_limit_tests;
#[cfg(test)]
mod batch_tests;
#[cfg(test)]
mod breaker_stream_tests;
#[cfg(test)]
mod chunk_tests;
#[cfg(test)]
mod conditional_tests;
mod count_stats;
#[cfg(test)]
mod count_stats_tests;
#[cfg(test)]
mod direct_fold_tests;
#[cfg(test)]
mod effect_tests;
#[cfg(test)]
mod exists_probe_tests;
#[cfg(test)]
mod expand_tests;
#[cfg(test)]
mod far_node_tests;
mod foreach;
#[cfg(test)]
mod foreach_tests;
pub mod graph_stats;
#[cfg(test)]
mod graph_stats_key_tests;
#[cfg(test)]
mod graph_stats_tests;
#[cfg(test)]
mod hub_join_gate_tests;
#[cfg(test)]
mod id_seek_tests;
mod index_aggregates;
#[cfg(test)]
mod index_aggregates_tests;
#[cfg(test)]
mod integer_arg_tests;
#[cfg(test)]
mod kernels_tests;
mod label_set_groups;
#[cfg(test)]
mod label_set_groups_tests;
#[cfg(test)]
mod limit_where_tests;
#[cfg(test)]
mod match_chain_tests;
#[cfg(test)]
mod match_fold_tests;
#[cfg(test)]
mod match_memory_tests;
#[cfg(test)]
mod nested_eval_tests;
#[cfg(test)]
mod one_hop_fold_tests;
#[cfg(test)]
mod path_fold_lookup_tests;
#[cfg(test)]
mod path_fold_tests;
#[cfg(test)]
mod pivot_tests;
#[cfg(test)]
mod plan_grade_tests;
#[cfg(test)]
mod predicate_spec_tests;
#[cfg(test)]
mod procedures_tests;
#[cfg(test)]
mod profiling_tests;
#[cfg(test)]
mod project_tests;
#[cfg(test)]
mod pushdown_tests;
#[cfg(test)]
mod range_start_tests;
#[cfg(test)]
mod rel_list_binding_tests;
#[cfg(test)]
mod rel_seek_tests;
#[cfg(test)]
mod seek_readiness_tests;
#[cfg(test)]
mod set_labels_function_tests;
mod shortcut_common;
#[cfg(test)]
mod shortest_expression_tests;
#[cfg(test)]
mod shortest_path_tests;
#[cfg(test)]
mod stream_agg_tests;
#[cfg(test)]
mod subquery_eval_tests;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod text_seek_tests;
#[cfg(test)]
mod walk_plans_tests;
#[cfg(test)]
mod write_tests;
