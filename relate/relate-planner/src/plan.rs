//! Plan types and the `plan` entry point: consume a lowered `Query`,
//! validate it, and build the `LogicalPlan` the executor interprets
//! (ADR-0009), with MATCH rewrites applied after bind (ADR-0024).

use std::collections::HashSet;

use relate_core::Result;
use relate_ir::{Clause, Expr, Query};
use relate_names::TokenResolver;

use crate::binding::{next_stage_scope, Env};
use crate::names::stage_output_names;
use crate::pattern::{label_expr_contains_dynamic, pattern_contains_dynamic_operands};
use crate::rewrite;
use crate::stats::{NoPatternStats, PatternStats, StatsMemo};
use crate::validate::validate_stage_with_env;

/// Logical plan is a pipeline of clauses (plus UNION).
///
/// Built by `plan`, which consumes the lowered `Query`: the plan owns the
/// clause vectors moved out of it (no deep clone), so the rewrite pass
/// mutates them in place.
#[derive(Debug, Clone)]
pub struct LogicalPlan {
    pub parts: Vec<Vec<Clause>>,
    pub union_all: Vec<bool>,
    pub next: Option<Box<LogicalPlan>>,
    /// CIP-150 `CYPHER <version>` preamble of the source statement, lowered
    /// from `Query::cypher_version` (`NEXT` stages inherit it in the
    /// front-end). Read by the executor for version-dependent behavior.
    pub cypher_version: Option<u32>,
    /// The statement's resolved `USE` target (ADR-0116 §3), lifted into the
    /// plan by [`plan`]: `Some(namespace)` when any `USE` clause is present
    /// (all `USE` clauses in one statement must agree), `None` when the
    /// statement carries none and the session namespace applies. Resolution
    /// failures (unknown name 42N00, composite-only selector 42N72, distinct
    /// targets 42NA5, unresolvable element id 42NA7) are `plan` errors.
    /// Always `None` on nested `NEXT`-stage plans; the top-level plan's
    /// resolution walks the whole statement.
    pub use_target: Option<relate_core::Namespace>,
}

impl LogicalPlan {
    /// Every stage of this plan, top-level first: the plan itself, then each
    /// `NEXT` stage. The one place the stage-level recursion lives — the
    /// stage-level walk ([`plan_output_names`]) rides it; part-level walks
    /// ride [`Self::parts`].
    pub(crate) fn stages(&self) -> impl Iterator<Item = &LogicalPlan> {
        std::iter::successors(Some(self), |stage| stage.next.as_deref())
    }

    /// Every part of every stage, top-level first: the one place the
    /// stage→parts flatten lives. The part-level walks ([`walk_plan`],
    /// [`walk_plan_parts`]) and the namespaces classifiers
    /// ([`crate::namespaces::is_show_databases_with_next`]) ride it.
    pub(crate) fn parts(&self) -> impl Iterator<Item = &[Clause]> {
        self.stages()
            .flat_map(|stage| stage.parts.iter().map(Vec::as_slice))
    }
}

/// The output column names of a planned statement (ADR-0319): the names the
/// final stage's `RETURN`/`YIELD`/`SHOW` exports, through the same
/// output-name analysis semantic validation uses. The engine reports them
/// for a statement whose execution failed, so a surface can still announce
/// its columns. A planned statement already passed the UNION
/// column-agreement check, so the `Err` arm is that check's error; callers
/// that hold a `plan`-produced statement share the one expect policy
/// ([`plan_output_names_expect`]).
pub fn plan_output_names(plan: &LogicalPlan) -> Result<Vec<String>> {
    plan.stages().try_fold(Vec::new(), |names, stage| {
        stage_output_names(
            stage.parts.iter().map(Vec::as_slice),
            names,
            &HashSet::new(),
        )
    })
}

/// [`plan_output_names`] for a caller that holds a planned statement: the
/// `Err` arm is the UNION column-agreement check, which `plan` already ran,
/// so a planned statement's output names always derive. The one expect for
/// that invariant, shared with the facade's `ExecutionFailure::staged`
/// (ADR-0319, ADR-0332).
pub fn plan_output_names_expect(plan: &LogicalPlan) -> Vec<String> {
    plan_output_names(plan).expect("a planned statement's UNION columns agree")
}

/// The canonical way to visit every clause of a [`LogicalPlan`]: each
/// part's clause tree via [`relate_ir::walk_clauses`] — UNION arms are
/// parts, and `CALL { … }` subqueries, `WHEN`/`ELSE` branches, `FOREACH`
/// bodies and expression subqueries are all descended into — recursing
/// into `NEXT` stages. Hand-rolling a `plan.parts` + `plan.next` recursion
/// instead of calling this is how coverage of a nesting position gets
/// dropped.
pub fn walk_plan_clauses<'a>(plan: &'a LogicalPlan, on_clause: &mut impl FnMut(&'a Clause)) {
    walk_plan(plan, on_clause, &mut |_| {});
}

/// [`walk_plan_clauses`]' full-descent walk, seeing every expression of
/// the plan (the expression half of the private [`walk_plan`]): the same
/// coverage — UNION arms, `NEXT` stages, subqueries, branches, `FOREACH`
/// bodies and pattern payloads — one expression at a time. The
/// connection-thread fast path's predicate (ADR-0504, `crate::inline`)
/// reads it; clause-granular analyses keep [`walk_plan_clauses`].
pub(crate) fn walk_plan_exprs<'a>(plan: &'a LogicalPlan, on_expr: &mut impl FnMut(&'a Expr)) {
    walk_plan(plan, &mut |_| {}, on_expr);
}

/// [`walk_plan_clauses`]' full-descent walk, seeing every expression of
/// the plan as well as every clause. Rides [`LogicalPlan::parts`], the one
/// stage→parts flatten.
fn walk_plan<'a>(
    plan: &'a LogicalPlan,
    on_clause: &mut impl FnMut(&'a Clause),
    on_expr: &mut impl FnMut(&'a Expr),
) {
    for part in plan.parts() {
        relate_ir::walk_clauses(part, on_clause, on_expr);
    }
}

/// Whether executing `plan` with these parameters would accept a float
/// where the original literal is rejected at plan time (a slice bound or
/// a list index). The plan-cache hit path uses this to fall back to the
/// original statement, which reports the compile error.
pub fn plan_has_bound_float_integer_site(
    plan: &LogicalPlan,
    params: &std::collections::BTreeMap<String, relate_core::Value>,
) -> bool {
    let _auto = relate_core::AutoLiteralGuard::install(params);
    plan_any_expr(plan, &mut |expr| {
        crate::expr_check::float_integer_site(expr).is_some()
    })
}

/// The parameter names `plan` references (`$name`), each once, in the
/// order of first appearance in the full-descent walk ([`walk_plan_clauses`]'
/// coverage: UNION arms, `NEXT` stages, subqueries, branches, `FOREACH`
/// bodies and pattern payloads). The facade checks them against the
/// supplied parameters before dispatch (ADR-0308).
pub fn plan_parameter_names(plan: &LogicalPlan) -> Vec<&str> {
    let mut names: Vec<&str> = Vec::new();
    walk_plan(plan, &mut |_| {}, &mut |expr| {
        if let Expr::Parameter(name) = expr {
            if !names.contains(&name.as_str()) {
                names.push(name);
            }
        }
    });
    names
}

/// The canonical way to visit a [`LogicalPlan`] PART BY PART: each part's
/// TOP-LEVEL clause slice (UNION arms are parts), recursing into `NEXT`
/// stages — with no descent into `CALL { … }` bodies, `WHEN`/`ELSE`
/// branches, `FOREACH` bodies or expression subqueries.
///
/// This is deliberately NOT [`walk_plan_clauses`]'s coverage: analyses that
/// reason per part over top-level clauses only (e.g. the `system`-namespace
/// 42N17 offender collection, where a read MATCH nested in an expression
/// subquery attached to a standalone CALL stays allowed) must use this walk
/// so the part-vs-`NEXT` recursion lives in exactly one place. If your
/// analysis needs the nested positions, use [`walk_plan_clauses`] instead.
pub fn walk_plan_parts<'a>(plan: &'a LogicalPlan, on_part: &mut impl FnMut(&'a [Clause])) {
    for part in plan.parts() {
        on_part(part);
    }
}

/// The canonical way to visit a not-yet-planned [`Query`] PART BY PART —
/// the `ir::Query` analog of [`walk_plan_parts`]: each part's TOP-LEVEL
/// clause slice (UNION arms are parts), recursing into `NEXT` stages —
/// with no descent into `CALL { … }` bodies, `WHEN`/`ELSE` branches,
/// `FOREACH` bodies or expression subqueries.
///
/// Analyses that reason per part over the pre-plan IR (the access fold in
/// [`crate::access`], the validate.rs write / IN TRANSACTIONS sweeps) must
/// use this walk so the part-vs-`NEXT` recursion lives in exactly one
/// place. Queries nested inside a clause (`WHEN`/`ELSE` branch bodies,
/// `CALL { … }`, `FOREACH`) are the CALLER's recursion: the walk yields
/// the clause slices, and the clause handler decides which nested
/// [`Query`]s to descend into (as [`crate::access`]'s `WHEN`/`ELSE`
/// handling does).
pub fn walk_query_parts<'a>(query: &'a Query, on_part: &mut impl FnMut(&'a [Clause])) {
    for part in &query.parts {
        on_part(&part.clauses);
    }
    if let Some(next) = query.next.as_deref() {
        walk_query_parts(next, on_part);
    }
}

/// Whether any clause of `plan` (via [`walk_plan_clauses`]) satisfies
/// `pred` — the one walk-and-accumulate boolean over plan clauses, so
/// classifiers stay one-line predicates instead of hand-rolling
/// `let mut found = false; … found |= …; found`.
pub fn plan_any_clause(plan: &LogicalPlan, pred: &mut impl FnMut(&Clause) -> bool) -> bool {
    let mut found = false;
    walk_plan_clauses(plan, &mut |clause| found |= pred(clause));
    found
}

/// [`plan_any_clause`]' expression half — the one walk-and-accumulate
/// boolean over plan expressions (the full-descent coverage of
/// [`walk_plan_exprs`]), so expression classifiers stay one-line
/// predicates the same way clause classifiers do.
pub(crate) fn plan_any_expr(plan: &LogicalPlan, pred: &mut impl FnMut(&Expr) -> bool) -> bool {
    let mut found = false;
    walk_plan_exprs(plan, &mut |expr| found |= pred(expr));
    found
}

/// The not-yet-planned [`Query`] analog of [`plan_any_clause`], riding
/// [`walk_query_parts`] (top-level clauses per part, `NEXT` stages; the
/// caller's predicate decides which nested queries to descend into).
pub fn query_any_clause(query: &Query, pred: &mut impl FnMut(&Clause) -> bool) -> bool {
    let mut found = false;
    walk_query_parts(query, &mut |part| found |= part.iter().any(&mut *pred));
    found
}

/// ADR-0210: whether the plan carries `CALL { … } IN TRANSACTIONS`
/// anywhere (top level or nested in a `CALL { }` body — the nested form
/// is rejected by [`crate::validate`], so in practice this is the
/// top-level batched subquery). The facade's auto-commit write path uses
/// this to attach the per-batch commit sink.
pub fn plan_contains_in_transactions(plan: &LogicalPlan) -> bool {
    plan_any_clause(plan, &mut crate::clause_kind::clause_is_in_transactions)
}

/// Whether a plan references any **dynamic name operand** — a dynamic
/// label (`LabelExpr::Dynamic`, in a pattern's label expressions or a
/// label predicate) or a dynamic property key (`Expr::DynamicProperty`).
/// ADR-0503's insert gate reads this: the front validates such operands
/// against bound *values* at lowering time (LABS-709 — the check has a
/// content half, an empty or null-byte name, beyond its type half), and a
/// cache hit skips lowering, so a plan carrying one must not be cached —
/// the statement stays uncached, never silently unvalidated.
pub fn plan_contains_dynamic_operands(plan: &LogicalPlan) -> bool {
    // The clause half: MATCH patterns in both representations, one
    // predicate over the canonical clause walk. The expression half: the
    // canonical expression walk — `DynamicProperty`, label predicates,
    // and the pattern-carrying expression positions (`PatternPred`,
    // `PatternComp`, `ShortestPath`) whose inner paths are pattern
    // payloads too.
    plan_any_clause(plan, &mut |clause| {
        matches!(
            clause,
            Clause::Match { patterns, .. }
                if patterns.iter().any(pattern_contains_dynamic_operands)
        )
    }) || plan_any_expr(plan, &mut |expr| match expr {
        Expr::DynamicProperty { .. } => true,
        Expr::LabelPredicate { label_expr, .. } => label_expr_contains_dynamic(label_expr),
        Expr::PatternPred { path }
        | Expr::PatternComp { path, .. }
        | Expr::ShortestPath { path, .. } => pattern_contains_dynamic_operands(path),
        _ => false,
    })
}

/// Bind AST to a logical plan (ADR-0009), then rewrite MATCH (ADR-0024).
///
/// Takes ownership of the query: the clauses are moved into the plan, not
/// cloned.
/// `is_write_procedure` classifies `CALL` targets for the write rules
/// (IN TRANSACTIONS composition, writes in subquery expressions): the
/// procedure registry lives in the executor, so the facade injects the
/// canonical predicate rather than the planner hardcoding names.
pub fn plan(
    query: Query,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
) -> Result<LogicalPlan> {
    plan_with_stats(query, is_write_procedure, &NoPatternStats)
}

/// [`plan`] with cardinality hints for MATCH ordering (ADR-0218).
///
/// `stats` is consulted while rewriting each MATCH, through ADR-0508's
/// per-statement consult memo: the one wrapper built here de-duplicates
/// the greedy order loop's repeated consults for the whole plan call and
/// is dropped at return, which is the memo's entire invalidation story.
/// Missing counts fall back to the fixed endpoint ranks. `NoPatternStats`
/// is what [`plan`] passes, so callers that have no graph keep the
/// rank-only order.
pub fn plan_with_stats(
    query: Query,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    stats: &dyn PatternStats,
) -> Result<LogicalPlan> {
    let memo = StatsMemo::new(stats);
    let mut plan = plan_with_env(query, SeedScope::default(), is_write_procedure, &memo)?;
    // ADR-0116 §3/§10: lift the statement's `USE` target into the plan.
    // Resolution walks the whole statement (UNION arms, CALL subqueries,
    // WHEN branches, expression subqueries, NEXT stages), so it runs once,
    // on the top-level plan.
    plan.use_target = crate::use_target::resolve_use_target(&plan)?;
    Ok(plan)
}

/// [`plan_with_stats`] after resolving the statement's labels, relationship
/// types and property keys to token ids against `resolver` (ADR-0541), once,
/// before any planning step reads them. The ids ride the compiled plan; the
/// caller's statement tree (cloned into `query`) stays id-free.
pub fn plan_with_tokens(
    mut query: Query,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    stats: &dyn PatternStats,
    resolver: &dyn TokenResolver,
) -> Result<LogicalPlan> {
    crate::resolve::resolve_query(&mut query, resolver);
    plan_with_stats(query, is_write_procedure, stats)
}

/// The scope an enclosing context hands one query.
///
/// `env` is what the query may bind and `names` is the order `RETURN *`
/// expands it in (a `NEXT` stage's incoming scope is a set for binding but
/// the previous stage's projection order for `*`).
#[derive(Default)]
pub(crate) struct SeedScope {
    pub env: Env,
    pub names: Vec<String>,
}

/// Bind one stage of a [`Query`] to a plan (ADR-0009) and rewrite its
/// MATCH parts (ADR-0024). `stats` is whatever the caller holds; the one
/// production caller ([`plan_with_stats`]) passes its per-statement
/// [`StatsMemo`] (ADR-0508), which the `NEXT` recursion threads unchanged
/// — the memo's lifetime is the whole statement's plan call, never
/// re-wrapped per stage.
pub(crate) fn plan_with_env(
    query: Query,
    seed: SeedScope,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    stats: &dyn PatternStats,
) -> Result<LogicalPlan> {
    let SeedScope {
        env: seed_env,
        names: seed_names,
    } = seed;
    // The stage's validation (composition rules, per-part checks and the
    // UNION column-agreement check, ADR-0332) is the validate layer's
    // driver, so the planner never spells a validation rule itself. No
    // CIP-218 star exclusion applies here: a scope-clause body is validated
    // by `validate_query_with_env`, never through `plan`'s stage driver.
    let output_names = validate_stage_with_env(
        &query,
        &seed_env,
        &seed_names,
        &HashSet::new(),
        is_write_procedure,
    )?;
    // A NEXT stage's scope is the previous stage's exported columns plus the
    // names this chain inherited from its enclosing scope (a `CALL (a) { … }`
    // / `COLLECT { … }` body's imports, or a `WHEN` branch's outer
    // variables): CIP-213 keeps the imported names visible in *every*
    // statement of the chain (`NextStatementAcceptance` "Should pass constant
    // through NEXT 3"). At the top level the inherited set is empty, so the
    // chain carries exactly the previous stage's columns.
    let next_scope = if query.next.is_some() {
        let (env, names) = next_stage_scope(&seed_env, &seed_names, &output_names);
        SeedScope { env, names }
    } else {
        SeedScope::default()
    };
    let Query {
        parts,
        union_all,
        next,
        cypher_version,
    } = query;
    let mut parts: Vec<Vec<Clause>> = parts.into_iter().map(|part| part.clauses).collect();
    for part in &mut parts {
        rewrite::rewrite_part(part, stats);
    }
    let next = match next {
        Some(next) => Some(Box::new(plan_with_env(
            *next,
            next_scope,
            is_write_procedure,
            stats,
        )?)),
        None => None,
    };
    Ok(LogicalPlan {
        parts,
        union_all,
        next,
        cypher_version,
        use_target: None,
    })
}

#[cfg(test)]
mod next_seed_tests {
    use relate_core::Error;

    use super::*;

    fn lower(cypher: &str) -> Result<LogicalPlan> {
        let query = relate_front::parse_and_lower(cypher)?;
        plan(query, &mut |_| false)
    }

    fn plan_of(cypher: &str) -> Result<LogicalPlan> {
        lower(cypher).map_err(|e| Error::syntax(format!("{cypher}: {e}")))
    }

    /// A NEXT stage sees the chain's inherited names — a `CALL (a) { … }`
    /// body's imports, or a `WHEN` branch's outer variables — as well as the
    /// previous stage's columns (CIP-213 `NextStatementAcceptance` "Should
    /// pass constant through NEXT 3").
    #[test]
    fn next_stage_carries_inherited_names() {
        plan_of(
            "LET a = 1 \
             CALL (a) { WHEN true THEN LET b = 2 RETURN b NEXT RETURN a + b AS x } \
             RETURN a, x",
        )
        .expect("the chain's imports stay visible into the NEXT stage");
    }

    /// An explicit `GROUP BY` moves ORDER BY and WHERE after grouping, so
    /// they may aggregate (Neo4j `GroupByAcceptance` [15] and [19]).
    #[test]
    fn explicit_group_by_allows_order_by_and_where_aggregation() {
        for query in [
            "UNWIND [1, 2] AS a RETURN a, count(*) AS c GROUP BY a ORDER BY sum(a)",
            "UNWIND [1, 2] AS a WITH a, count(*) AS c GROUP BY a WHERE sum(a) > 1 RETURN c",
        ] {
            plan_of(query).unwrap_or_else(|e| panic!("{query}: {e}"));
        }
    }

    /// A mid-chain NEXT stage must not re-project an imported name: that
    /// shadows the enclosing binding the chain keeps visible (CIP-213 42N07,
    /// `NextStatementAcceptance` "Should fail due to redeclaration of
    /// constant 2").
    #[test]
    fn next_stage_reprojecting_an_import_is_rejected() {
        let err = lower(
            "LET a = 1 \
             CALL (a) { LET b = 2 RETURN a, b NEXT RETURN a + b AS x } \
             RETURN a, x",
        )
        .expect_err("redeclaring the import is rejected");
        let text = err.to_string();
        assert!(
            text.contains("VariableShadowing") || text.contains("already declared in outer scope"),
            "{err}"
        );
    }
}
