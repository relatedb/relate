//! Clause-level semantic validation: the per-clause scope and composition
//! rules checked against the binding environment before a plan is built.
//! The `NEXT`-chain driver and its seed live in [`chain`]; the 42I25
//! write-classification family in [`write`].

use relate_core::{Error, Result};
use relate_ir::{Clause, Query, SingleQuery};
use relate_names::SmallName;

use crate::binding::{Binding, Env, EnvBridge};
use crate::expr_check::check_expr;

pub(crate) mod chain;
mod clause;
mod in_transactions;
mod subquery;
pub(crate) mod write;

use clause::*;
use subquery::*;
use write::{ClausePosition, WriteHistory};

/// The scope one validation pass runs over. The three parts travel
/// together because every boundary derives them from *one* fact — what
/// the body may see — and passing them as three parallel arguments let a
/// call site pair the wrong import or shadowable set with an environment.
/// Each boundary has a named constructor.
///
/// Walk state is not scope state: the 42I25 write history a validation
/// walk accumulates is the walk's own ([`WriteHistory`], threaded by the
/// clause and stage loops), never a field here.
#[derive(Clone)]
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

    /// [`Self::expression_body`] for the body ADR-0685's comprehension
    /// lowering synthesizes: its path variable is an assignment, not a
    /// join, so it shadows an outer name of the same spelling whatever
    /// that name's kind — the name is therefore not an import at all
    /// (the plan-time twin of the runtime seed's own filter), and the
    /// body's MATCH binds it freely.
    pub(crate) fn comprehension_body(mut env: Env, path_shadow: Option<&SmallName>) -> Self {
        if let Some(name) = path_shadow {
            env.remove_name(name);
        }
        Self::expression_body(env)
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

    /// A FOREACH body: it shares the enclosing scope (`outer`), so nothing is
    /// an aggregation import; the enclosing names are shadowable by the
    /// body's own CREATE/MERGE declarations, while the loop variable —
    /// declared in the body's own scope, even when an enclosing scope bound
    /// the same name — is not.
    pub(crate) fn foreach_body(outer: &Env, loop_variable: &SmallName) -> Self {
        let mut shadowable: std::collections::HashSet<String> = outer.keys().cloned().collect();
        loop_variable.with_str(|text| {
            shadowable.remove(text);
        });
        let mut env = outer.clone();
        env.insert_name(loop_variable.clone(), Binding::Value);
        Self {
            env,
            imports: crate::agg::Imports::new(),
            shadowable,
        }
    }
}

/// The clause-slice core of the stage driver's part validation
/// ([`chain::validate_part_with_env`]): a FOREACH body validates its
/// borrowed clauses through it without cloning them into an owned
/// `SingleQuery`. Returns the walk's 42I25 answer — the incoming
/// history folded with this list's own unbatched writes.
pub(crate) fn validate_clauses_with_env(
    clauses: &[Clause],
    scope: Scope,
    mut write_history: WriteHistory,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
) -> Result<WriteHistory> {
    let Scope {
        mut env,
        imports,
        shadowable,
    } = scope;
    if clauses.is_empty() {
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
    let substantive = clauses
        .iter()
        .filter(|c| !crate::clause_kind::clause_is_routing(c))
        .count();
    if clauses
        .iter()
        .any(crate::clause_kind::clause_is_ddl_command)
        && substantive != 1
    {
        return Err(Error::syntax(
            "InvalidClauseComposition: schema and database-alteration commands must be standalone clauses",
        ));
    }
    for (clause_index, c) in clauses.iter().enumerate() {
        // The one position value the checks below read: this clause's
        // index, its part, and the 42I25 history the walk has
        // accumulated to this point.
        let position = ClausePosition {
            clauses,
            index: clause_index,
            write_history,
        };
        // The terminal clauses are part-level composition facts, not
        // per-clause checks: a `RETURN` or `FINISH` must be its part's
        // last *substantive* clause — the one terminality fact
        // ([`ClausePosition::is_terminal`]; trailing routing `USE`s do
        // not count, the `USE` placement rule owns rejecting one there),
        // and anything else following is rejected here, once, for both
        // spellings.
        if matches!(c, Clause::Return { .. } | Clause::Finish) && !position.is_terminal() {
            return Err(Error::syntax(format!(
                "InvalidClauseComposition: {} must terminate its query",
                crate::clause_kind::ir_clause_keyword(c)
            )));
        }
        match c {
            Clause::Catalog(command) => check_catalog_clause(&mut env, command)?,
            Clause::Match { .. } => {
                check_match_clause(&mut env, &shadowable, is_write_procedure, c)?
            }
            Clause::Create { patterns } => {
                check_create_clause(&mut env, &shadowable, is_write_procedure, patterns)?
            }
            Clause::Merge { .. } => {
                check_merge_clause(&mut env, &shadowable, is_write_procedure, c)?
            }
            Clause::Call {
                args,
                yield_items,
                where_clause,
                ..
            } => check_call_clause(
                &mut env,
                is_write_procedure,
                clauses,
                args,
                yield_items,
                where_clause.as_ref(),
            )?,
            Clause::Delete { expressions, .. } => {
                check_delete_clause(&mut env, is_write_procedure, expressions)?
            }
            Clause::Set { items } => check_set_clause(&mut env, is_write_procedure, items)?,
            Clause::Remove { items } => check_remove_clause(&mut env, is_write_procedure, items)?,
            Clause::With { .. } => check_with_clause(&mut env, is_write_procedure, &imports, c)?,
            Clause::Return { .. } => {
                check_return_clause(&mut env, is_write_procedure, &imports, c)?
            }
            Clause::Unwind { expression, alias } => {
                check_unwind_clause(&mut env, is_write_procedure, expression, alias)?
            }
            Clause::CreateIndex { .. } | Clause::DropIndex { .. } | Clause::ShowIndexes => {}
            // `FINISH` owns no per-clause rule: its terminality is the
            // part-level check above, and what may precede it is the
            // DDL-standalone rule's answer.
            Clause::Finish => {}
            Clause::LoadCsv { url, variable, .. } => {
                check_load_csv_clause(&mut env, is_write_procedure, url, variable)?
            }
            Clause::CallSubquery {
                query,
                scope,
                in_transactions,
                ..
            } => check_call_subquery_clause(
                &mut env,
                is_write_procedure,
                position,
                query,
                scope,
                in_transactions.as_ref(),
            )?,
            Clause::Conditional { .. } => {
                check_conditional_clause(&mut env, is_write_procedure, c)?
            }
            Clause::Foreach {
                variable,
                list,
                clauses: body,
            } => check_foreach_clause(&mut env, is_write_procedure, variable, list, body)?,
            Clause::Use { graph } => {
                check_use_clause(&mut env, is_write_procedure, position, graph)?
            }
        }
        // The 42I25 history accumulates as the part is walked: this
        // clause's unbatched writes join for every later clause's
        // batched-subquery check (a `CALL { … } IN TRANSACTIONS` clause
        // contributes nothing — its writes run in its batches' own
        // transactions).
        write_history = write_history.after(c, is_write_procedure);
    }
    Ok(write_history)
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

/// The one spelling of the projection alias rule the `NEXT` tail, a
/// conditional branch and a `WITH` share: an item with no `AS` must be
/// canonically named ([`relate_ir::canonical_name`]: a bare variable,
/// `*`, or a map projection over a variable — the shapes that name
/// their own column, so an unaliased `WITH n {.*}` passes exactly like
/// `WITH n`); any other expression needs its alias. Each site keeps its
/// own text.
fn is_unaliased_expression(item: &relate_ir::Projection) -> bool {
    item.alias.is_none() && relate_ir::canonical_name(&item.expr).is_none()
}

/// Whether the part's terminal `RETURN` carries an unaliased expression.
fn last_return_has_unaliased_expression(part: &SingleQuery) -> bool {
    matches!(
        part.clauses.last(),
        Some(Clause::Return { items, .. }) if items.iter().any(is_unaliased_expression)
    )
}

/// The terminal-RETURN alias rule, one spelling over a query's parts:
/// every arm's terminal `RETURN` must name its expression columns
/// ([`is_unaliased_expression`]'s policy — the shared predicate; each
/// caller keeps its own refusal text, the pattern the predicate's own
/// doc states). The `NEXT` tail and the conditional branches are the
/// two sites.
fn refuse_unaliased_terminal_returns(query: &Query, text: &str) -> Result<()> {
    if query.parts.iter().any(last_return_has_unaliased_expression) {
        return Err(Error::syntax(text));
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
