//! The compile-time plan gates (ADR-0116 §2/§3/§4, ADR-0121 §3, ADR-0227 §1,
//! ADR-0229 §1/§2, ADR-0378) and the query-type/write-route
//! classification, extracted from `connection.rs` to keep it under the
//! repository's file-size budget. `Connection::plan_gate_classify` is the
//! one entry point: it plans, enforces the preview gate, validates `CALL`s,
//! enforces the 42N79 `USE`-with-administration-command gate (§2), the
//! namespace and ontology gates, and classifies the statement
//! — all under one read guard, above the EXPLAIN early-return and the
//! dispatch split, so no path can skip a gate.

use std::collections::BTreeMap;

use relate_core::{ce_messages, Namespace};
use relate_executor::{is_ontology_store_op, is_ontology_write_procedure};
use relate_ir::{Clause, Query};
use relate_planner::{
    admin_command_heads_statement, composed_show_databases, composed_show_disallowed_clause,
    is_database_admin_command, is_write, plan_with_tokens, query_type,
    unsupported_system_clause_names, LogicalPlan, QueryType,
};

use crate::connection::Connection;
use crate::procedures::{call_is_write, validate_calls};
use crate::{Error, Result, Value};

/// The metadata stamped onto every `QueryResult` at construction: the
/// namespace the statement ran in (ADR-0117 §7) and its plan-derived query
/// type (ADR-0162), plus the auto-commit write-route decision (ADR-0121:
/// statements calling a `writes`-flagged built-in such as the ontology
/// write procedures must reach the write path even when no data clause
/// classifies them as writes). All are resolved in `plan_gate_classify`
/// above the EXPLAIN/dispatch split and carried into `dispatch` as one
/// bundle.
#[derive(Clone, Copy)]
pub(crate) struct ResultStamp {
    pub(crate) namespace: Namespace,
    pub(crate) qtype: QueryType,
    pub(crate) write_route: bool,
}

/// Every procedure call name in a plan (ADR-0121, ADR-0227, ADR-0378),
/// collected in clause-walk order, plus the ontology-procedure subset of
/// that list. Full descent (`walk_plan_clauses`) so a call nested in a
/// `CALL { … }` body, a `WHEN`/`ELSE` branch, a `FOREACH` body, an
/// expression subquery or a `NEXT` stage is gated and routed like a
/// top-level one.
///
/// Every call-name *gate* on this path derives from this one collection:
/// the preview gate reads [`PlanCallNames::all`], the ontology gate reads
/// both `all` (its other-write predicate) and [`PlanCallNames::ontology`],
/// and the write-route predicate reads `ontology`. Compile-time `CALL`
/// validation keeps its own clause walk by design (it validates each call
/// site, not just names); no gate re-walks the plan for names.
struct PlanCallNames {
    /// Every call name, in walk order (rendered text — the plan-gate
    /// narrative owns String).
    all: Vec<String>,
    /// The ontology-procedure subset of `all`, in walk order.
    ontology: Vec<String>,
}

/// Collect a plan's call names for the gates (ADR-0121, ADR-0227,
/// ADR-0378): the one `CALL` walk on the gate path, with the ontology
/// subset derived here too. Which names are ontology procedures, which
/// preview feature gates a name and which of them write stays with the
/// executor's canonical predicates
/// ([`relate_executor::is_ontology_procedure`],
/// [`relate_executor::procedure_preview_feature`],
/// [`relate_executor::is_ontology_write_procedure`]), so the facade and
/// the executor's `CALL` dispatch agree.
fn plan_call_names(logical: &LogicalPlan) -> PlanCallNames {
    let mut all = Vec::new();
    relate_planner::walk_plan_clauses(logical, &mut |clause| {
        if let Clause::Call { name, .. } = clause {
            // Text render: the collected procedure list is String.
            all.push(name.to_string());
        }
    });
    let ontology = all
        .iter()
        .filter(|name| relate_executor::is_ontology_procedure(name.as_str()))
        .cloned()
        .collect();
    PlanCallNames { all, ontology }
}

impl Connection {
    /// Plan, preview gate, compile-time CALL validation, the namespace gate
    /// and the query-type classification, under one read guard: all of them
    /// read `graph.procedures` through the `GraphCell` deref, whose contract
    /// requires a read guard or a live pin (a follow reload may `replace`
    /// the graph otherwise, ADR-0137). The guard is dropped before
    /// dispatch, so admission semantics are unchanged. The preview gate
    /// precedes compile-time CALL validation and the namespace gates on
    /// every path (ADR-0378); CALL validation in turn precedes the
    /// namespace gates, so a doubly-invalid statement reports the procedure
    /// error. All run exactly once here, above the EXPLAIN early-return and
    /// the normal dispatch, so no future early-return path can skip them.
    /// ADR-0503: the plan step and the gate/classification step are named
    /// separately ([`Self::gates_classify_under_guard`]) because the plan cache's hit
    /// path enters between them — a cached plan skips the planning but
    /// runs every one of these gates, per statement, from the cached plan.
    pub(crate) fn plan_gate_classify(
        &self,
        query: Query,
        params: &BTreeMap<String, Value>,
    ) -> Result<(LogicalPlan, ResultStamp)> {
        let _read = self.inner.read_guard()?;
        let logical = self.plan_under_guard(query, params)?;
        let stamp = self.gates_classify_under_guard(&logical, params)?;
        Ok((logical, stamp))
    }

    /// The planning half of [`Self::plan_gate_classify`], under the
    /// caller's read guard: ADR-0218 — the planner stays pure;
    /// cardinalities come from this store, read under the plan read
    /// guard. A `USE` retarget still plans against the home store
    /// (ADR-0218 Consequences). The one canonical construction
    /// (ADR-0218) every plan read site builds — see GraphPatternStats::new.
    pub(crate) fn plan_under_guard(
        &self,
        query: Query,
        params: &BTreeMap<String, Value>,
    ) -> Result<LogicalPlan> {
        let mut is_write_proc = |name: &str| call_is_write(&self.inner.graph, name);
        let stats = crate::stats::GraphPatternStats::new(&self.inner.graph, params);
        // Same bound-literal window the parameterized parse uses, so a
        // float slice bound fails here the way the original literal does.
        let _auto = relate_core::AutoLiteralGuard::install(params);
        // Deref-coerced down to the store's `Graph`, the token resolver.
        let graph: &relate_executor::Graph = &self.inner.graph;
        plan_with_tokens(query, &mut is_write_proc, &stats, graph)
    }

    /// The gate-and-classification half of [`Self::plan_gate_classify`],
    /// under the caller's read guard — the one sequence every statement's
    /// admission ends with, cached plan or fresh (ADR-0503: the cache
    /// keys the plan, never the metadata). `params` are the statement's
    /// effective parameters: the caller's map plus, on a cache hit, the
    /// extracted `$__AUTO_*__` values.
    pub(crate) fn gates_classify_under_guard(
        &self,
        logical: &LogicalPlan,
        params: &BTreeMap<String, Value>,
    ) -> Result<ResultStamp> {
        // The one call-name collection every gate below reads: each derives
        // its view from it instead of re-walking the plan for `CALL`s.
        let call_names = plan_call_names(logical);
        // ADR-0378: the preview gate runs BEFORE compile-time CALL
        // validation and every later gate (see `preview_gate_error`), so a
        // disabled `db.ontology.*` call reports the authored preview error
        // even when it is also malformed or routed to `system_ontologies`.
        if let Some(e) = self.preview_gate_error(&call_names) {
            return Err(e);
        }
        validate_calls(&self.inner.graph, logical, params)?;
        if let Some(e) = self.transaction_gate_error(logical) {
            return Err(e);
        }
        // ADR-0116 §2 (2026-11-04): the 42N79 auto-routing gate — after
        // CALL validation (a doubly-invalid statement reports the
        // procedure error, the ordering the namespace gates below keep)
        // and before the §4 namespace gates. For a `USE system_ontologies`
        // target both this gate and the §4 not-queryable arm match the
        // same statement (`USE system_ontologies SHOW DATABASES YIELD …
        // RETURN …`); this order is the precedence, deliberate and pinned
        // by use_targeting_non_system_with_administration_command_is_rejected:
        // the explicit-`USE` rejection outranks the routed-namespace gate.
        // `logical` is already a reference: the cache serves the same plan
        // the fresh path plans.
        if let Some(e) = self.admin_use_gate_error(logical) {
            return Err(e);
        }
        // ADR-0116 §3: `USE` routes the statement; the planner resolved
        // the target eagerly, so the resolved target picks the graph and
        // the namespace rules it runs under.
        // ADR-0116 §4: the gates are plan-based (CE raises 42N17/42N18 at
        // semantic-check time), so they apply to EXPLAIN too.
        let namespace = logical.use_target.unwrap_or(self.namespace);
        if let Some(e) = self.namespace_gate_error(logical, namespace) {
            return Err(e);
        }
        // ADR-0121/ADR-0227: the ontology-procedure gates run after the §4
        // namespace gates — after ADR-0229 §1 every procedure routed to
        // `system_ontologies` fails 42N17 there, before the ontology
        // target text, which now fires only on `system`.
        if let Some(e) = self.ontology_gate_error(logical, namespace, &call_names) {
            return Err(e);
        }
        // ADR-0162: the query type is plan-based and total, classified
        // under the same read guard, so EXPLAIN/PROFILE report the
        // wrapped statement's type without executing it. The predicate
        // reads the store's live procedure registry, so a re-registered
        // procedure re-classifies a cached plan's statement on its next
        // run — the metadata is per statement, never cached.
        let mut is_write_proc = |name: &str| call_is_write(&self.inner.graph, name);
        let qtype = query_type(logical, &mut is_write_proc);
        // Auto-commit routing: the write path is also required by a
        // statement whose only write is an ontology write procedure
        // (set/drop/apply push redo; `is_write` alone classifies a
        // bare `CALL` as a read because its procedure predicate is
        // write-blind). Deliberately NOT `plan_calls_write_procedure` with
        // the full predicate: routing user-registered or other
        // writes-flagged procedures to the write path would change their
        // admission semantics for nothing — they push no redo. The names
        // are the ontology subset of the collected call names, filtered by
        // the executor's registry predicate, not a literal list.
        let write_route = is_write(logical)
            || call_names
                .ontology
                .iter()
                .any(|name| is_ontology_write_procedure(name));
        Ok(ResultStamp {
            namespace,
            qtype,
            write_route,
        })
    }

    /// ADR-0570: a statement carrying `CALL { … } IN TRANSACTIONS` owns its
    /// own batch commits, so it is an implicit-transaction-only shape — the
    /// same plan-time rule the ontology store ops follow below (live Neo4j
    /// 2026.10.1 raises GQLSTATUS 25N17 inside an open explicit
    /// transaction).
    fn transaction_gate_error(&self, logical: &LogicalPlan) -> Option<Error> {
        (self.txn.is_some() && relate_planner::plan_contains_in_transactions(logical))
            .then(ce_messages::call_in_transactions_implicit_only)
    }

    /// The preview gate (ADR-0371, ADR-0378), enforced at compile time under
    /// the same read guard as the other gates. `calls` is [`plan_call_names`]'
    /// one collection: [`PlanCallNames::all`], in walk order, so the first
    /// call whose feature is disabled decides (later gated calls never
    /// outrank it) and no gate re-walks the plan for `CALL` clauses.
    /// `plan_gate_classify` calls this before `validate_calls` and the
    /// namespace/ontology gates, the ADR-0378 position: a disabled
    /// `db.ontology.*` call reports the authored preview error even when it
    /// is also malformed or routed to `system_ontologies`.
    fn preview_gate_error(&self, calls: &PlanCallNames) -> Option<Error> {
        let preview = self.inner.graph.preview();
        calls.all.iter().find_map(|name| {
            relate_executor::procedure_preview_feature(name)
                .and_then(|feature| preview.require(feature).err())
        })
    }

    /// The ontology-procedure gates (ADR-0121 §3, ADR-0227 §1), enforced
    /// at compile time — the layer that sees the whole statement — under
    /// the same read guard as the other gates. `calls` is [`plan_call_names`]'
    /// one collection: [`PlanCallNames::ontology`] is its ontology subset,
    /// in walk order, and the other-write predicate below runs over
    /// [`PlanCallNames::all`], so neither check re-walks the plan.
    fn ontology_gate_error(
        &self,
        logical: &LogicalPlan,
        namespace: Namespace,
        calls: &PlanCallNames,
    ) -> Option<Error> {
        if calls.ontology.is_empty() {
            return None;
        }
        // ADR-0227 §1: each procedure acts on the session/USE-target
        // database, which must be `neo4j`. Routed to `system_ontologies`
        // every procedure already failed 42N17 in `namespace_gate_error`
        // (ADR-0229 §1); this arm is what every procedure hits on `system`
        // once the 42N17 standalone-call shape passes.
        if namespace != Namespace::Neo4j {
            return Some(ce_messages::ontology_neo4j_only());
        }
        let set_or_drop = calls
            .ontology
            .iter()
            .find(|name| is_ontology_store_op(name.as_str()))?;
        // ADR-0121 §3: set/drop are store-level commits of their own —
        // implicit (auto-commit) transactions only.
        if self.txn.is_some() {
            return Some(ce_messages::ontology_implicit_transaction_only(set_or_drop));
        }
        // ADR-0121 §3: a statement calling set/drop may not write anything
        // else (Neo4j's 42N03 text). "Anything else" is a data/schema write
        // (`is_write`'s procedure predicate is write-blind, so the set/drop
        // call itself is not counted) or another write procedure — the
        // predicate below excludes the two ontology store ops themselves
        // but counts the in-overlay `apply` (it writes `neo4j`). It runs
        // per name over the collected list rather than a precomputed bool
        // list: `call_is_write` reads the store's live procedure registry.
        let other_write_procedure =
            |name: &str| !is_ontology_store_op(name) && call_is_write(&self.inner.graph, name);
        if is_write(logical)
            || calls
                .all
                .iter()
                .any(|name| other_write_procedure(name.as_str()))
        {
            return Some(ce_messages::cross_database_write());
        }
        None
    }

    /// ADR-0116 §2, amended 2026-11-04: the 42N79 auto-routing gate. A
    /// `USE` clause targeting any non-system namespace on a statement
    /// headed by a database-administration command (the §2 supported set,
    /// [`relate_planner::admin_command_heads_statement`]) is rejected
    /// with CE's `The `USE` clause is not required for administration
    /// commands. …` (GQLSTATUS 42N79, `Neo.ClientError.Statement.
    /// SyntaxError`). The complement is deliberate:
    /// - `USE system` + administration command stays accepted — live CE
    ///   rejects even that spelling for `ALTER DATABASE`, but Relate
    ///   keeps the whole `USE system` + §2 surface runnable (the
    ///   composable-SHOW contract the harness's `USE system SHOW DEFAULT
    ///   DATABASE … SHOW DATABASES …` scenario pins; recorded in ADR-0116
    ///   §2's Later references).
    /// - Statements without a `USE` clause never hit this gate: their
    ///   routing is the session's, so the off-system composed-form
    ///   boundary (Neo4j's 51N28) belongs to `namespace_gate_error`
    ///   below, which knows the namespace they resolved to (ADR-0590).
    fn admin_use_gate_error(&self, logical: &LogicalPlan) -> Option<Error> {
        let target = logical.use_target?;
        if target == Namespace::System || !admin_command_heads_statement(logical) {
            return None;
        }
        Some(ce_messages::use_clause_with_administration_command())
    }

    /// ADR-0116 §4 namespace gates: on a `system` session only SHOW
    /// commands, supported administration commands and standalone procedure
    /// calls run (42N17); on a `system_ontologies` session every statement
    /// fails — reads included — with ADR-0229 §2's 42N17 text (the ADR-0116
    /// §4 42N18 write arm is superseded); and off `system` a composed
    /// administration command is CE's 51N28 (ADR-0590). All three are
    /// plan-based — CE raises them at semantic-check time — so
    /// `plan_gate_classify` enforces this gate once, above the EXPLAIN
    /// early-return and the dispatch split; database-administration
    /// commands are exempt (ADR-0116 §2, and the only thing that still runs
    /// while routed to `system_ontologies`, ADR-0229 §1). The exemption is
    /// computed here from the plan (`is_database_admin_command(logical)`, a
    /// pure function of the `LogicalPlan` the gate already takes).
    fn namespace_gate_error(&self, logical: &LogicalPlan, namespace: Namespace) -> Option<Error> {
        if is_database_admin_command(logical) {
            return None;
        }
        match namespace {
            // ADR-0116 §4: a `WITH` composing a SHOW command is rejected
            // first, with CE's composability text — CE's front-end
            // composability check precedes its plan-level clause whitelist,
            // and live CE reports the combined `… WITH … MATCH …` shape
            // with the composability text too. Every other offender keeps
            // the 42N17 text.
            Namespace::System => composed_show_disallowed_clause(logical)
                .map(ce_messages::clauses_not_allowed_on_system_database)
                .or_else(|| {
                    unsupported_system_clause_names(logical)
                        .map(|clauses| ce_messages::unsupported_system_clauses(&clauses))
                }),
            Namespace::SystemOntologies => Some(ce_messages::system_ontologies_not_queryable()),
            // ADR-0590: off `system`, a composed administration command is
            // CE's 51N28 — the command belongs to the namespace this gate
            // resolved as *not* that one, whether the routing came from a
            // `USE` clause or from the session. (The explicit-`USE` spelling
            // was already answered 42N79 by `admin_use_gate_error` above, and
            // a `system` target is answered by §4's whitelist arm.)
            Namespace::Neo4j => composed_show_databases(logical)
                .map(ce_messages::administration_command_needs_system_database),
        }
    }
}
