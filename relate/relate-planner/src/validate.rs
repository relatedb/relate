//! Clause-level semantic validation: the per-clause scope and composition
//! rules checked against the binding environment before a plan is built.

use relate_core::{Error, Result};
use relate_ir::{Clause, Expr, Query, SingleQuery};

use crate::binding::Env;
use crate::expr_check::check_expr;

mod clause;
mod in_transactions;
mod subquery;

use clause::*;
use subquery::*;

/// The scope one validation pass runs over. The three parts travel
/// together because every boundary derives them from *one* fact — what the
/// body may see — and passing them as three parallel arguments let a call
/// site pair the wrong import or shadowable set with an environment. Each
/// boundary has a named constructor.
pub(crate) struct Scope {
    /// The bindings the body starts from.
    pub(crate) env: Env,
    /// The names the body's aggregation expressions may read without
    /// grouping by them.
    pub(crate) imports: crate::agg::Imports,
    /// The non-entity imports the body's own pattern may rebind.
    pub(crate) shadowable: std::collections::HashSet<String>,
}

impl Scope {
    /// A plain stage: its seed bindings are its imports, and nothing is
    /// shadowable (a same-scope rebinding is a type conflict).
    pub(crate) fn stage(env: Env) -> Self {
        let imports = env.keys().cloned().collect();
        Self {
            env,
            imports,
            shadowable: std::collections::HashSet::new(),
        }
    }

    /// An expression-subquery body (`EXISTS`/`COUNT`/`COLLECT`): every outer
    /// name is an import, and the non-entity ones are shadowable.
    pub(crate) fn expression_body(env: Env) -> Self {
        let imports = env.keys().cloned().collect();
        let shadowable = crate::binding::shadowable_imports(&env);
        Self {
            env,
            imports,
            shadowable,
        }
    }

    /// A `WHEN`/`ELSE` body: it shares the enclosing scope but is a fresh
    /// branch — nothing is an aggregation import and nothing is shadowable
    /// (a rebinding there is a type conflict like any other same-scope
    /// rebinding).
    pub(crate) fn branch_body(env: Env) -> Self {
        Self {
            env,
            imports: crate::agg::Imports::new(),
            shadowable: std::collections::HashSet::new(),
        }
    }

    /// A FOREACH body: it shares the enclosing scope, so nothing is an
    /// aggregation import; the enclosing names are shadowable by the body's
    /// own CREATE/MERGE declarations (the loop variable excluded by the
    /// caller).
    pub(crate) fn foreach_body(env: Env, shadowable: std::collections::HashSet<String>) -> Self {
        Self {
            env,
            imports: crate::agg::Imports::new(),
            shadowable,
        }
    }
}

pub(crate) fn validate_part_with_env(
    part: &SingleQuery,
    scope: Scope,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
) -> Result<()> {
    let Scope {
        mut env,
        imports,
        shadowable,
    } = scope;
    if part.clauses.is_empty() {
        // Live Neo4j reports an empty/whitespace-only statement as a
        // compile-time SyntaxError (42N45, unexpected end of input).
        return Err(Error::syntax("empty query part"));
    }
    // A grammar-ending DDL command is standalone whichever AST carries
    // it (the family is `clause_kind::clause_is_ddl_command`'s — schema
    // DDL plus `ALTER DATABASE`): every continuation (`WITH`, `SET`,
    // `RETURN`, `FINISH`) is a syntax error on live 2026.10.1 (probed
    // 2026-11-08). `USE` is routing, not a continuation (ADR-0116: an
    // administration command runs from any namespace), so it never
    // counts against the command's standaloneness.
    let substantive = part
        .clauses
        .iter()
        .filter(|c| !crate::clause_kind::clause_is_routing(c))
        .count();
    if part
        .clauses
        .iter()
        .any(crate::clause_kind::clause_is_ddl_command)
        && substantive != 1
    {
        return Err(Error::syntax(
            "InvalidClauseComposition: schema and database-alteration commands must be standalone clauses",
        ));
    }
    for (clause_index, c) in part.clauses.iter().enumerate() {
        // The terminal clauses are part-level composition facts, not
        // per-clause checks: a `RETURN` or `FINISH` must be its part's
        // last *substantive* clause — trailing routing `USE`s do not
        // count (the `USE` placement rule owns rejecting one there),
        // and anything else following is rejected here, once, for both
        // spellings.
        if matches!(c, Clause::Return { .. } | Clause::Finish)
            && part.clauses[clause_index + 1..]
                .iter()
                .any(|c| !crate::clause_kind::clause_is_routing(c))
        {
            return Err(Error::syntax(format!(
                "InvalidClauseComposition: {} must terminate its query",
                crate::clause_kind::ir_clause_keyword(c)
            )));
        }
        match c {
            Clause::Catalog(_) => check_catalog_clause(&mut env, is_write_procedure, c)?,
            Clause::Match { .. } => {
                check_match_clause(&mut env, &shadowable, is_write_procedure, c)?
            }
            Clause::Create { .. } => {
                check_create_clause(&mut env, &shadowable, is_write_procedure, c)?
            }
            Clause::Merge { .. } => {
                check_merge_clause(&mut env, &shadowable, is_write_procedure, c)?
            }
            Clause::Call { .. } => check_call_clause(&mut env, is_write_procedure, part, c)?,
            Clause::Delete { .. } => check_delete_clause(&mut env, is_write_procedure, c)?,
            Clause::Set { .. } => check_set_clause(&mut env, is_write_procedure, c)?,
            Clause::Remove { .. } => check_remove_clause(&mut env, is_write_procedure, c)?,
            Clause::With { .. } => check_with_clause(&mut env, is_write_procedure, &imports, c)?,
            Clause::Return { .. } => {
                check_return_clause(&mut env, is_write_procedure, &imports, c)?
            }
            Clause::Unwind { .. } => check_unwind_clause(&mut env, is_write_procedure, c)?,
            Clause::CreateIndex { .. } | Clause::DropIndex { .. } | Clause::ShowIndexes => {}
            // `FINISH` owns no per-clause rule: its terminality is the
            // part-level check above, and what may precede it is the
            // DDL-standalone rule's answer.
            Clause::Finish => {}
            Clause::LoadCsv { .. } => check_load_csv_clause(&mut env, is_write_procedure, c)?,
            Clause::CallSubquery { .. } => {
                check_call_subquery_clause(&mut env, is_write_procedure, clause_index, part, c)?
            }
            Clause::Conditional { .. } => {
                check_conditional_clause(&mut env, is_write_procedure, c)?
            }
            Clause::Foreach { .. } => check_foreach_clause(&mut env, is_write_procedure, c)?,
            Clause::Use { .. } => {
                check_use_clause(&mut env, is_write_procedure, clause_index, part, c)?
            }
        }
    }
    Ok(())
}

/// The one spelling of the UNION joiner-uniformity rule: a query's part
/// boundaries are all `UNION` or all `UNION ALL`, never mixed. The stage
/// validator applies it to every planned stage; the executor's UNION fold
/// applies it again to bodies that are never stage-validated (`COUNT {}`
/// / `COLLECT {}` and conditional branch bodies), so a mixed chain there
/// fails with this typed error instead of reaching the fold.
pub fn check_union_joiners(union_all: &[bool]) -> Result<()> {
    match union_all.first() {
        Some(first) if union_all.iter().any(|a| a != first) => Err(Error::syntax(
            "InvalidClauseComposition: mixing UNION and UNION ALL",
        )),
        _ => Ok(()),
    }
}

/// The stage-level validation driver: the structural composition rules one
/// planned stage must satisfy, its per-part semantic checks, and its output
/// columns (the UNION column-agreement check). `plan_with_env` calls it for
/// each stage before assembling that stage's plan;
/// [`validate_query_with_env`] chains it over a whole `NEXT` chain.
pub(crate) fn validate_stage_with_env(
    query: &Query,
    seed_env: &Env,
    seed_names: &[String],
    excluded_imports: &std::collections::HashSet<String>,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
) -> Result<Vec<String>> {
    if query.parts.is_empty() {
        return Err(Error::semantic("empty query"));
    }
    if !query.union_all.is_empty() {
        check_union_joiners(&query.union_all)?;
        if let Some(offender) = query
            .parts
            .iter()
            .flat_map(|p| p.clauses.iter())
            .find(|c| crate::clause_kind::clause_cannot_join_union(c))
        {
            return Err(Error::syntax(format!(
                "InvalidClauseComposition: {} cannot be used with UNION",
                crate::clause_kind::ir_clause_keyword(offender)
            )));
        }
    }
    for part in &query.parts {
        validate_part_with_env(part, Scope::stage(seed_env.clone()), is_write_procedure)?;
        if query.next.is_some() {
            if let Some(Clause::Return { items, .. }) = part.clauses.last() {
                if items.iter().any(|item| {
                    item.alias.is_none() && !matches!(item.expr, Expr::Variable(_) | Expr::Star)
                }) {
                    return Err(Error::syntax(
                        "NoExpressionAlias: expression in RETURN followed by NEXT must use AS",
                    ));
                }
            }
        }
        if matches!(
            part.clauses.last(),
            Some(
                Clause::Match { .. }
                    | Clause::With { .. }
                    | Clause::Unwind { .. }
                    | Clause::Use { .. }
            )
        ) {
            return Err(Error::syntax(
                "InvalidClauseComposition: query must conclude with RETURN, FINISH, an update, or a unit subquery",
            ));
        }
    }
    // This stage's output columns, computed from the unrewritten parts. The
    // stage's own incoming scope seeds the computation: a bare `RETURN *`
    // stage projects exactly the variables passed through the previous
    // `NEXT` (CIP-213), so an empty seed here would drop them. The
    // exclusion set is CIP-218 Rule 10: a scope-clause body's imports are in
    // scope but a star must not re-expand them (and every UNION arm must
    // agree on that shape).
    crate::names::parts_output_names(&query.parts, seed_names.to_vec(), excluded_imports)
}

/// [`validate_stage_with_env`] over a whole `NEXT` chain — each stage
/// validated with the scope its predecessor exports (CIP-213), the same
/// seeding the planner's stage recursion does. This is what a caller that
/// holds a query it will not plan (today: a `CALL { … }` body during
/// validation) uses, so validation never re-enters the planner. Returns the
/// final stage's output columns — the query's export set.
///
/// The two name lists are different facts and stay separate parameters:
/// `seed_names` is what the body's own scope sees, so it is what a
/// body-level `RETURN *` expands (the whole outer scope for a naked
/// `CALL { … }`); `excluded_imports` is the CIP-218 Rule 10 exclusion set
/// (non-empty only for a scope clause, whose star must not re-expand the
/// imports). They coincide for a scope clause and differ for a naked body.
/// `call_body_names` derives the exclusion set once; this entry threads it
/// through the `NEXT` chain, whose later stages still see the same imports.
pub(crate) fn validate_query_with_env(
    query: &Query,
    seed_env: &Env,
    seed_names: &[String],
    excluded_imports: &std::collections::HashSet<String>,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
) -> Result<Vec<String>> {
    validate_query_chain(
        query,
        seed_env,
        seed_names,
        excluded_imports,
        is_write_procedure,
    )
}

/// The recursive half of [`validate_query_with_env`]: one stage, then the
/// same exclusion set threaded into the `NEXT` stage's scope.
fn validate_query_chain(
    query: &Query,
    seed_env: &Env,
    seed_names: &[String],
    excluded_imports: &std::collections::HashSet<String>,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
) -> Result<Vec<String>> {
    let output_names = validate_stage_with_env(
        query,
        seed_env,
        seed_names,
        excluded_imports,
        is_write_procedure,
    )?;
    // CIP-218 Rule 10's star rule for this stage, folded into the one chain
    // walk: a scope-clause body's star arm must expand at least one name the
    // body itself introduces. Running it here (rather than in a second
    // recursion over the same chain) walks each stage once and reuses the
    // seed names this walk already carries.
    if !excluded_imports.is_empty() {
        validate_stage_star_rule(query, seed_names, excluded_imports)?;
    }
    match query.next.as_deref() {
        Some(next) => {
            let (env, names) =
                crate::binding::next_stage_scope(seed_env, seed_names, &output_names);
            validate_query_chain(next, &env, &names, excluded_imports, is_write_procedure)
        }
        None => Ok(output_names),
    }
}

/// CIP-218 Rule 10's star rule for one stage: every UNION arm whose terminal
/// `RETURN` carries a `*` must expand at least one name that is not an
/// import, and no arm may re-list an import as a bare `WITH x`. The stage's
/// seed names come from the enclosing chain walk (`validate_query_chain`),
/// which also threads the `NEXT` stage's scope — so this is one pass, not a
/// second recursion that recomputes each stage's output names.
fn validate_stage_star_rule(
    query: &Query,
    seed_names: &[String],
    excluded: &std::collections::HashSet<String>,
) -> Result<()> {
    for part in &query.parts {
        let Some(Clause::Return { items, .. }) = part.clauses.last() else {
            continue;
        };
        if !items.iter().any(|item| matches!(item.expr, Expr::Star)) {
            continue;
        }
        check_bare_import_redeclaration(&part.clauses, excluded)?;
        // The star expands through the output-name fold, so every clause
        // kind a CALL-scope body can export (a `CALL … YIELD` included)
        // counts, not a hand-spelled subset. Rule 10 wants one name that is
        // not an import — an empty export set after the exclusion is that
        // failure.
        let expanded =
            crate::names::part_output_names(&part.clauses, seed_names.to_vec(), excluded)?;
        if expanded.is_empty() {
            return Err(Error::syntax(
                "NoVariablesInScope: RETURN * has no non-imported variables",
            ));
        }
    }
    Ok(())
}

/// Re-listing an imported variable as a bare `WITH x` — no other exported
/// name beside it and no `*` sibling — is the import-only redeclaration the
/// CALL-scope body rejects with `VariableAlreadyBound`. A `*` sibling brings
/// the body's own bindings along (`WITH *, x`, SubqueryAcceptance "Return
/// all should not return imported variable #1"), so it is not that shape.
fn check_bare_import_redeclaration(
    clauses: &[Clause],
    imported: &std::collections::HashSet<String>,
) -> Result<()> {
    for clause in clauses {
        let Clause::With { items, .. } = clause else {
            continue;
        };
        let has_star = items.iter().any(|item| matches!(item.expr, Expr::Star));
        let has_non_imported = items.iter().enumerate().any(|(index, item)| {
            !matches!(item.expr, Expr::Star) && !imported.contains(&item.output_name(index))
        });
        for (index, item) in items.iter().enumerate() {
            let name = item.output_name(index);
            if imported.contains(&name)
                && matches!(&item.expr, Expr::Variable(variable) if variable == name.as_str())
                && !has_non_imported
                && !has_star
            {
                return Err(Error::duplicate_variable(&name));
            }
        }
    }
    Ok(())
}

fn validate_conditional_returns(query: &Query) -> Result<()> {
    for part in &query.parts {
        if let Some(Clause::Return { items, .. }) = part.clauses.last() {
            for item in items {
                if item.alias.is_none() && !matches!(item.expr, Expr::Variable(_) | Expr::Star) {
                    return Err(Error::syntax(
                        "NoExpressionAlias: expressions returned by conditional clauses must use AS",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn check_set_item(
    env: &Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    item: &relate_ir::SetItem,
) -> Result<()> {
    match item {
        relate_ir::SetItem::Property { target, value }
        | relate_ir::SetItem::ReplaceAll { target, value }
        | relate_ir::SetItem::MergeMap { target, value } => {
            check_expr(env, is_write_procedure, target)?;
            check_expr(env, is_write_procedure, value)?;
        }
        relate_ir::SetItem::Labels { target, .. } => check_expr(env, is_write_procedure, target)?,
        relate_ir::SetItem::LabelsFunction { target, value, .. } => {
            check_expr(env, is_write_procedure, target)?;
            check_expr(env, is_write_procedure, value)?;
        }
    }
    Ok(())
}

fn check_remove_item(
    env: &Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    item: &relate_ir::RemoveItem,
) -> Result<()> {
    match item {
        relate_ir::RemoveItem::Property { target }
        | relate_ir::RemoveItem::Labels { target, .. } => {
            check_expr(env, is_write_procedure, target)?;
        }
    }
    Ok(())
}

/// `is_write_procedure` classifies `CALL` targets: the canonical
/// registry-backed predicate, injected by the facade — the procedure
/// registry lives in the executor, which the planner cannot depend on.
/// Every write gate (the `IN TRANSACTIONS` composition rule, the
/// subquery-expression rejection, the §4 namespace gate in
/// `namespaces.rs`) classifies procedures through this one predicate, so
/// no literal procedure name is hardcoded here.
///
/// The clause half is the published per-clause answer
/// ([`crate::clause_mutates_store`]); this function only adds the answers
/// that live below the clause itself — a `CALL { … }` body, a `WHEN`/`ELSE`
/// body, a writes-flagged procedure — which is what the composition rules
/// need. (A nested body is walked here rather than classified outright: a
/// read-only `CALL { … }` body makes no write.)
pub(crate) fn clause_is_write(
    c: &Clause,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
) -> bool {
    crate::clause_mutates_store(c)
        || matches!(c, Clause::CallSubquery { query, .. } if query_contains_write(query, is_write_procedure))
        || matches!(c, Clause::Call { name, .. }
            if name.with_str(|text| is_write_procedure(text)))
        || matches!(c, Clause::Conditional { branches, else_query }
            if branches.iter().any(|(_, query)| query_contains_write(query, is_write_procedure))
                || else_query.as_deref().is_some_and(|q| query_contains_write(q, is_write_procedure)))
}

fn query_contains_write(query: &Query, is_write_procedure: &mut impl FnMut(&str) -> bool) -> bool {
    crate::query_any_clause(query, &mut |c| clause_is_write(c, is_write_procedure))
}

fn query_contains_in_transactions(query: &Query) -> bool {
    crate::query_any_clause(query, &mut |clause| {
        crate::clause_kind::clause_is_in_transactions(clause)
            || matches!(
                clause,
                Clause::CallSubquery { query, .. } if query_contains_in_transactions(query)
            )
    })
}

#[cfg(test)]
mod composition_tests;
#[cfg(test)]
mod subquery_scope_tests;
