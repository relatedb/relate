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
    admin_command_heads_statement, composed_show_current_user, composed_show_databases,
    composed_show_disallowed_clause, is_database_admin_command, is_write, plan_with_tokens,
    query_type, unsupported_system_clause_names, LogicalPlan, QueryType,
};

use crate::connection::Connection;
use crate::procedures::{call_is_write, validate_calls};
use crate::{Error, Result, Value};

/// A statement's transaction-mixing class (ADR-0667) — the one
/// three-valued fact behind the mixing rule, shared by the admission
/// classification and the open transaction's history: an ontology write
/// call (`set`, `drop`, `apply`) is a schema update, a data update is
/// the other class, and a read or a schema command (`CREATE CONSTRAINT`,
/// …) is neither. The two non-`None` classes are mutually exclusive by
/// construction: [`Connection::gates_classify_under_guard`] refuses a
/// statement carrying both, so no stamp, history cell or gate arm ever
/// re-spells the exclusivity as a negation guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MixClass {
    /// Neither class: a read or a schema command — a statement that
    /// records nothing into an open transaction's history.
    None,
    /// The statement calls an ontology write procedure (`set`, `drop`,
    /// `apply`) and performs no data update — a schema update in the
    /// transaction-mixing sense.
    OntologySchema,
    /// The statement performs data updates and calls no ontology write —
    /// the other side of the mixing rule. Schema commands are not data
    /// updates (the query type's `s` class excludes them).
    Data,
}

/// The metadata stamped onto every `QueryResult` at construction: the
/// namespace the statement ran in (ADR-0117 §7) and its plan-derived query
/// type (ADR-0162), plus the auto-commit write-route decision (ADR-0121:
/// statements calling a `writes`-flagged built-in such as the ontology
/// write procedures must reach the write path even when no data clause
/// classifies them as writes) and the statement's transaction-mixing
/// class (ADR-0667) — the fact the explicit-transaction arm records into
/// the open transaction's history after the statement succeeds. All are
/// resolved in `plan_gate_classify` above the EXPLAIN/dispatch split and
/// carried into `dispatch` as one bundle.
#[derive(Clone, Copy)]
pub(crate) struct ResultStamp {
    pub(crate) namespace: Namespace,
    pub(crate) qtype: QueryType,
    pub(crate) write_route: bool,
    /// The statement's transaction-mixing class (ADR-0667) — exclusive
    /// by construction (see [`MixClass`]).
    pub(crate) mix_class: MixClass,
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

/// The shared form of the gates' `Option<Error>` verdict: `None` admits,
/// `Some` is the statement's rejection.
fn passes(verdict: Option<Error>) -> Result<()> {
    verdict.map_or(Ok(()), Err)
}

impl Connection {
    /// Plan, preview gate, compile-time CALL validation, the namespace gate
    /// and the query-type classification, under one read guard: all of them
    /// read `graph.procedures` through the `GraphCell` deref, whose contract
    /// requires a read guard or a live pin (a follow reload may `replace`
    /// the graph otherwise, ADR-0137). The guard is dropped before
    /// dispatch, so admission semantics are unchanged. The preview gate
    /// precedes compile-time CALL validation and the namespace gates on
    /// every path (ADR-0378); the namespace visibility gate (ADR-0593)
    /// follows it and precedes CALL validation and every later gate, so a
    /// namespace hidden by the preview set behaves exactly like a name that
    /// never resolved; CALL validation in turn precedes the
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
        self.plan_stamped_under_guard(query, params)
            .map(|(plan, _)| plan)
    }

    /// [`Self::plan_under_guard`] plus the plan cache's second freshness
    /// stamp: the per-key statistics epoch the plan was priced at, or
    /// `None` when planning never consulted the statistics (such a plan
    /// has nothing to go stale there, so the cache never compares it). The
    /// epoch is read BEFORE planning, under the same publication read
    /// guard: a publication that lands while the plan is built moves the
    /// epoch past the stamp, so the entry replans once, never the reverse.
    pub(crate) fn plan_stamped_under_guard(
        &self,
        query: Query,
        params: &BTreeMap<String, Value>,
    ) -> Result<(LogicalPlan, Option<u64>)> {
        let mut is_write_proc = |name: &str| call_is_write(&self.inner.graph, name);
        let stats = crate::stats::GraphPatternStats::new(&self.inner.graph, params);
        // Same bound-literal window the parameterized parse uses, so a
        // float slice bound fails here the way the original literal does.
        let _auto = relate_core::AutoLiteralGuard::install(params);
        // Deref-coerced down to the store's `Graph`, the token resolver.
        let graph: &relate_executor::Graph = &self.inner.graph;
        let epoch = graph.key_stats_epoch();
        #[cfg(test)]
        self.consult_key_stats_for_test(&stats);
        let plan = plan_with_tokens(query, &mut is_write_proc, &stats, graph)?;
        Ok((plan, stats.consulted_key_stats().then_some(epoch)))
    }

    /// Tests only: consult the statistics of the store's configured
    /// `(label, property)` pair, the way a planner rung will.
    #[cfg(test)]
    fn consult_key_stats_for_test(&self, stats: &crate::stats::GraphPatternStats<'_>) {
        use relate_planner::PatternStats;
        let pair = self.inner.test_plan_consult.lock().unwrap().clone();
        if let Some((label, property)) = pair {
            let named =
                |name: &str| relate_names::unresolved(relate_names::SmallName::from_name(name));
            let _ = stats.key_stats(&named(&label), &named(&property));
        }
    }

    /// The plan's token footprint and the per-token counts it was planned
    /// against (ADR-0687), under the caller's read guard: the same
    /// [`crate::stats::GraphPatternStats`] construction (ADR-0218) the
    /// plan was compiled with, so a recorded count is the one the planner
    /// was told. The caller holds the guard across both the plan and this
    /// capture, so no commit can publish between them.
    pub(crate) fn footprint_under_guard(
        &self,
        plan: &LogicalPlan,
        params: &BTreeMap<String, Value>,
    ) -> crate::plan_footprint::PlanFootprint {
        let stats = crate::stats::GraphPatternStats::new(&self.inner.graph, params);
        crate::plan_footprint::plan_footprint(plan, &stats)
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
        passes(self.preview_gate_error(&call_names))?;
        // ADR-0116 §3: `USE` routes the statement; the planner resolved
        // the target eagerly, so the resolved target picks the graph and
        // the namespace rules it runs under. Derived once here — the one
        // spelling every namespace-consuming gate below takes as input
        // (the visibility gate, the §4 gates, the ontology gates and the
        // stamp).
        let namespace = logical.use_target.unwrap_or(self.namespace);
        // ADR-0593: the namespace visibility gate, immediately after the
        // procedures preview gate (ADR-0378's pinned first position stands)
        // and before CALL validation and every later gate — so a namespace
        // hidden by the preview set is indistinguishable from a name that
        // never resolved: the 42N79 admin gate, the §4 namespace gates and
        // dispatch never see it. `require_visible`'s fail-closed form is
        // the ordinary unknown-database rejection (42N00), never the
        // authored preview error — ADR-0371's name-gate rule: an error
        // must not advertise a name the catalog does not list. The
        // element-id `USE` route lands here too, on the resolved
        // namespace.
        namespace.require_visible(self.inner.graph.preview())?;
        validate_calls(&self.inner.graph, logical, params)?;
        passes(self.transaction_gate_error(logical))?;
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
        passes(self.admin_use_gate_error(logical))?;
        // ADR-0116 §4: the gates are plan-based (CE raises 42N17/42N18 at
        // semantic-check time), so they apply to EXPLAIN too.
        passes(self.namespace_gate_error(logical, namespace))?;
        // ADR-0121/ADR-0227: the ontology-procedure gates run after the §4
        // namespace gates — after ADR-0229 §1 every procedure routed to
        // `system_ontologies` fails 42N17 there, before the ontology
        // target text, which now fires only on `system`.
        passes(self.ontology_gate_error(logical, namespace, &call_names))?;
        // ADR-0162: the query type is plan-based and total, classified
        // under the same read guard, so EXPLAIN/PROFILE report the
        // wrapped statement's type without executing it. The predicate
        // reads the store's live procedure registry, so a re-registered
        // procedure re-classifies a cached plan's statement on its next
        // run — the metadata is per statement, never cached.
        let mut is_write_proc = |name: &str| call_is_write(&self.inner.graph, name);
        let qtype = query_type(logical, &mut is_write_proc);
        // The statement's transaction-mixing class (ADR-0667), then the
        // mixing gate that reads it against the open transaction's
        // history at the sequence's tail — the one gate that reads
        // transaction state (see `mixing_gate_error`). `statement_writes`
        // is derived once: the class and the write route both read it.
        let ontology_schema_update = call_names
            .ontology
            .iter()
            .any(|name| is_ontology_write_procedure(name));
        let statement_writes = is_write(logical);
        let mix_class = mixing_class(qtype, ontology_schema_update, statement_writes)?;
        passes(self.mixing_gate_error(mix_class))?;
        // Auto-commit routing: the write path is also required by a
        // statement whose only write is an ontology write procedure
        // (set/drop/apply push redo; `is_write` alone classifies a
        // bare `CALL` as a read because its procedure predicate is
        // write-blind). Deliberately NOT `plan_calls_write_procedure` with
        // the full predicate: routing user-registered or other
        // writes-flagged procedures to the write path would change their
        // admission semantics for nothing — they push no redo.
        let write_route = statement_writes || ontology_schema_update;
        Ok(ResultStamp {
            namespace,
            qtype,
            write_route,
            mix_class,
        })
    }

    /// ADR-0570: a statement carrying `CALL { … } IN TRANSACTIONS` owns its
    /// own batch commits, so it is an implicit-transaction-only shape — the
    /// same plan-time rule the ontology store ops follow below (live Neo4j
    /// 2026.10.1 raises GQLSTATUS 25N17 inside an open explicit
    /// transaction).
    fn transaction_gate_error(&self, logical: &LogicalPlan) -> Option<Error> {
        (self.in_transaction() && relate_planner::plan_contains_in_transactions(logical))
            .then(ce_messages::call_in_transactions_implicit_only)
    }

    /// ADR-0667: the transaction-mixing gate — the one gate that reads
    /// transaction state (its siblings above are plan-only), which is why
    /// it sits at the classification tail in
    /// [`Self::gates_classify_under_guard`], after the plan-only gates:
    /// an ontology write is a schema update, so it refuses a transaction
    /// whose history holds data updates, and a data update refuses one
    /// whose history holds an ontology write. A statement carrying both
    /// classes never reaches here — the classification refuses it with
    /// the schema-after-data text before a class exists.
    fn mixing_gate_error(&self, class: MixClass) -> Option<Error> {
        let txn = self.txn.as_ref()?;
        match (class, txn.mix_class_performed()) {
            (MixClass::OntologySchema, MixClass::Data) => {
                Some(ce_messages::ontology_schema_after_data())
            }
            (MixClass::Data, MixClass::OntologySchema) => {
                Some(ce_messages::ontology_data_after_schema())
            }
            _ => None,
        }
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

    /// The ontology-procedure statement gates (ADR-0121 §3, ADR-0227 §1,
    /// amended by ADR-0667), enforced at compile time — the layer that
    /// sees the whole statement — under the same read guard as the other
    /// gates. `calls` is [`plan_call_names`]'s one collection:
    /// [`PlanCallNames::ontology`] is its ontology subset, in walk order,
    /// and the other-write predicate below runs over
    /// [`PlanCallNames::all`], so neither check re-walks the plan. The
    /// transaction-mixing rule (set/drop/apply as schema updates) is NOT
    /// here: it reads the statement's own classes and the open
    /// transaction's history, so it lives at the classification tail —
    /// the class derivation and [`Self::mixing_gate_error`] in
    /// [`Self::gates_classify_under_guard`].
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
        // The no-mixed-writes arm below applies to a statement calling a
        // store op; a statement whose ontology call is only `apply` (or
        // `get`) runs it only through the other-write predicate's own
        // `apply` count.
        if !calls
            .ontology
            .iter()
            .any(|name| is_ontology_store_op(name.as_str()))
        {
            return None;
        }
        // ADR-0121 §3: a statement calling set/drop may not write anything
        // else (the 42N03 text). "Anything else" is a data/schema write
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
                .or_else(|| composed_show_current_user(logical))
                .map(ce_messages::administration_command_needs_system_database),
        }
    }
}

/// The statement's transaction-mixing class (ADR-0667). The classes are
/// mutually exclusive by construction: the match has no both arm, and a
/// statement whose ontology write call rides beside data updates
/// (`CREATE … CALL db.ontology.apply()`, either clause order, implicit or
/// explicit transaction) is refused here, at the plan gate, with the
/// schema-after-data text the transaction rule uses — the ontology write
/// is the schema update, the data clauses beside it are the data updates
/// it refuses to mix with.
///
/// The data arm masks the write bits the ontology write call itself sets
/// back out (the writers are writes-flagged, so a bare
/// `CALL db.ontology.set` classifies `w` yet is a schema update, not a
/// data update): with the call present, only a clause beside it —
/// `statement_writes`, whose procedure predicate is write-blind — turns
/// the query type's write classes into a data update.
fn mixing_class(
    qtype: QueryType,
    ontology_schema_update: bool,
    statement_writes: bool,
) -> Result<MixClass> {
    let data_update = matches!(qtype, QueryType::Write | QueryType::ReadWrite)
        && (!ontology_schema_update || statement_writes);
    match (ontology_schema_update, data_update) {
        (false, false) => Ok(MixClass::None),
        (true, false) => Ok(MixClass::OntologySchema),
        (false, true) => Ok(MixClass::Data),
        (true, true) => Err(ce_messages::ontology_schema_after_data()),
    }
}
