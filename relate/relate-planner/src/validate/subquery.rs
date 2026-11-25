//! The `CALL { … }` subquery clause: scope-clause imports, the
//! CALL-scope export rules (the star rule the chain driver invokes as its
//! per-stage hook, and the importing-WITH shape rule), and the 42I28 bare
//! redeclaration rule. The `CALL IN TRANSACTIONS` modifier battery lives
//! in [`super::in_transactions`]; the body's shadowing policy is
//! [`crate::expr_check`]'s, shared with every other subquery body.

use std::collections::HashSet;

use relate_core::{Error, Result};
use relate_ir::{CallScope, Clause, Expr, InTransactions, Projection, Query, SingleQuery};
use relate_names::SmallName;

use crate::binding::{Binding, Env, EnvBridge};
use crate::expr_text::expr_text;
use crate::names::{explicit_return_names, query_with_persistent_imports};

use super::chain::{validate_query_with_env, StageSeed};
use super::in_transactions::check_in_transactions_modifiers;
use super::query_contains_in_transactions;
use super::write::ClausePosition;

/// The `CALL { … }` subquery clause checker. The clause's
/// [`ClausePosition`] carries the 42I25 write history reaching it — the
/// chain's incoming fact (an unbatched write in an earlier `NEXT` stage)
/// folded with this part's own earlier unbatched writes, the one answer
/// the clause-validation walk accumulates — plus the part position the
/// `IN TRANSACTIONS` modifier battery reads. The typed variant
/// parameters are the module's convention — the caller destructures the
/// arm it already matched — so the variant is proven once, at the match.
pub(super) fn check_call_subquery_clause(
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    position: ClausePosition<'_>,
    query: &Query,
    scope: &CallScope,
    in_transactions: Option<&InTransactions>,
) -> Result<()> {
    if query_contains_in_transactions(query) {
        return Err(Error::syntax(
            "NestedCallInTransactions: CALL IN TRANSACTIONS cannot be nested",
        ));
    }
    if in_transactions.is_some() && position.write_history.unbatched_write_behind() {
        return Err(Error::syntax(
            "WriteBeforeCallInTransactions: a write cannot precede CALL IN TRANSACTIONS",
        ));
    }
    let imported = match scope {
        // A scope clause declares its imports; the naked body imports the
        // whole row.
        relate_ir::CallScope::Vars(vars) => {
            let mut imported = Env::new();
            for name in vars {
                let Some(binding) = env.get_name(name).copied() else {
                    return Err(name.with_str(Error::undefined_variable));
                };
                if imported.insert_name(name.clone(), binding).is_some() {
                    return Err(name.with_str(Error::duplicate_variable));
                }
            }
            imported
        }
        relate_ir::CallScope::None | relate_ir::CallScope::Wildcard => env.clone(),
    };
    // The body's two name lists — one derivation read everywhere below (the
    // shadowing check, the star rule, validation, and the export-set
    // insertion): the seed is what the body sees (the whole outer scope for
    // a naked `CALL { … }`, whose `RETURN *` expands it), while the CIP-218
    // star-exclusion set is non-empty only for a scope clause.
    let incoming: Vec<String> = imported.keys().cloned().collect();
    let (seed_names, import_names) = crate::names::call_body_names(scope, &incoming);
    // 42N07: the one shadowing policy, shared with every other subquery
    // body (a body projection may not re-alias an imported name). Scoped to
    // a scope clause's declared imports: a naked body's aliasing is the
    // 42I28 rule below.
    let declared_imports: Vec<String> = import_names.iter().cloned().collect();
    crate::expr_check::shadowing::reject_shadowing_projections(query, &declared_imports)?;
    // Only a *naked* CALL body has the legacy importing-WITH rule: a scope
    // clause declares its imports explicitly, so the body's leading WITH is
    // an ordinary projection (`CALL (i) { WITH i AS j RETURN j }` is legal,
    // live-verified). Every UNION arm is a body of its own, so each part's
    // opening clause is checked.
    if !scope.has_scope_clause() {
        let imported_names: HashSet<String> = imported.keys().cloned().collect();
        for part in &query.parts {
            reject_invalid_importing_with(part, &imported_names)?;
        }
    }
    // The rewrite appends one projection per import, so it takes the names
    // in a stable order. For a scope clause the exclusion set is exactly the
    // body seed, which carries the declared order; a naked body has none.
    let persistent_imports: Vec<String> = if scope.has_scope_clause() {
        seed_names.clone()
    } else {
        Vec::new()
    };
    let scoped_query = query_with_persistent_imports(query, &persistent_imports);
    // Validate the body against the imports it may read — the same
    // stage driver the planner runs, without planning anything. The seed
    // is what the body's own `RETURN *` expands (the whole outer scope
    // for a naked body); the import list is the scope-clause import set,
    // so a scope clause's star arms do not re-expand the imports and
    // every UNION arm agrees on the export shape. Its returned names are
    // the body's export set, read again below — the walk is not
    // repeated, and both lists come from the one `call_body_names`
    // derivation.
    let output_names = validate_query_with_env(
        &scoped_query,
        StageSeed::body(scope, imported, seed_names, import_names),
        is_write_procedure,
    )?;
    if let Some(tx) = in_transactions {
        check_in_transactions_modifiers(tx, env, is_write_procedure, position)?;
    }
    // The post-CALL name space, computed once: the body's exports in both
    // spellings (the explicit RETURN names and the computed export set the
    // star arms contribute to) plus `REPORT STATUS`'s variable. One
    // collision check — against the enclosing scope and within the set —
    // runs before anything is inserted, so a body export can neither shadow
    // an enclosing binding nor silently overwrite the status binding.
    let explicit = explicit_return_names(&scoped_query);
    let status_name = in_transactions
        .as_ref()
        .and_then(|tx| tx.report_status.as_ref());
    reject_post_call_collisions(env, &explicit, &output_names, status_name)?;
    // The import exclusion shapes the body's export set: the exclusion is part of
    // the name computation (the walk just run), so the `RETURN *` arms agree
    // with their explicit siblings and no imported name survives in it.
    for name in output_names {
        env.insert(name, Binding::Value);
    }
    if let Some(status) = status_name {
        env.insert(status.to_string(), Binding::Map);
    }

    Ok(())
}

/// The one post-CALL collision policy, over the whole name space the clause
/// inserts: the body's exports in both spellings (`explicit`, then the
/// computed `exports`) and `REPORT STATUS`'s variable.
///
/// Every name must be fresh against the enclosing scope. Within the set,
/// only the status variable is checked: `explicit` is a subset of `exports`
/// by construction, while a status variable that repeats a body export
/// would otherwise be silently overwritten by the export's `Binding::Value`
/// (the round's defect — the status binding must win its name or the clause
/// must be rejected).
fn reject_post_call_collisions(
    env: &Env,
    explicit: &[String],
    exports: &[String],
    status: Option<&SmallName>,
) -> Result<()> {
    if let Some(bound) = explicit
        .iter()
        .chain(exports)
        .find(|name| env.contains_key(name.as_str()))
    {
        return Err(Error::duplicate_variable(bound));
    }
    if let Some(status) = status {
        if status.with_str(|text| exports.iter().any(|name| name == text)) {
            return Err(status.with_str(Error::duplicate_variable));
        }
        if env.contains_name(status) {
            return Err(status.with_str(Error::duplicate_variable));
        }
    }
    Ok(())
}

/// One item of an importing WITH, classified once. `Import` is the legal
/// shape — `*`, or a bare unaliased reference to an outside variable;
/// `Refused` carries the text the 42I28 message quotes.
enum LeadingItem {
    Import,
    Refused(String),
}

fn classify_leading_item(item: &Projection, imported: &HashSet<String>) -> LeadingItem {
    if matches!(item.expr, Expr::Star)
        || item
            .imported_name()
            .is_some_and(|name| name.with_str(|text| imported.contains(text)))
    {
        return LeadingItem::Import;
    }
    let mut text = item
        .source_text
        .clone()
        .unwrap_or_else(|| expr_text(&item.expr));
    if let Some(alias) = &item.alias {
        text = format!("{text} AS {alias}");
    }
    LeadingItem::Refused(text)
}

/// 42I28: a CALL subquery body that opens with an *importing* WITH — one
/// whose items reference the enclosing scope (or pull it in with `*`) — may
/// consist only of direct, unaliased references to outside variables, with
/// no `DISTINCT`/`ORDER BY`/`SKIP`/`LIMIT`/`WHERE` subclause ("Importing
/// WITH can consist only of direct references to outside variables").
/// A WITH that references nothing from the outer scope is an ordinary
/// projection and stays unrestricted (the "non-importing WITH at the start
/// of a subquery" scenarios). Live-verified against
/// neo4j:2026.09-enterprise; a `USE` clause may precede the WITH.
fn reject_invalid_importing_with(part: &SingleQuery, imported: &HashSet<String>) -> Result<()> {
    let Some(with) = part.leading_importing_with() else {
        return Ok(());
    };
    let items = with.items;
    // "Importing" is about *referencing* the enclosing scope anywhere in an
    // item, not only as a bare variable (`WITH n + 1 AS x` is importing).
    // Comprehension-bound identifiers do not count, and a subquery
    // expression's correlated references are internal to its own scope —
    // both are `references_any`'s policy.
    let references_outer = items.iter().any(|item| match &item.expr {
        Expr::Star => true,
        other => relate_ir::references_any(other, imported),
    });
    if !references_outer {
        return Ok(());
    }
    let refuse = |what: &str| {
        Error::syntax(format!(
            "InvalidImportingWith: importing WITH can consist only of direct references to outside variables; '{what}' is not allowed"
        ))
    };
    let refused = items
        .iter()
        .map(|item| classify_leading_item(item, imported))
        .find_map(|item| match item {
            LeadingItem::Refused(text) => Some(text),
            LeadingItem::Import => None,
        });
    if let Some(text) = refused {
        return Err(refuse(&text));
    }
    let modifier = if with.distinct {
        Some("DISTINCT")
    } else if !with.order.is_empty() {
        Some("ORDER BY")
    } else if with.skip.is_some() {
        Some("SKIP")
    } else if with.limit.is_some() {
        Some("LIMIT")
    } else if with.where_clause.is_some() {
        Some("WHERE")
    } else {
        None
    };
    match modifier {
        Some(modifier) => Err(refuse(modifier)),
        None => Ok(()),
    }
}

/// The scope-clause star rule for one stage of a `CALL { … }` body: every
/// UNION arm whose terminal `RETURN` carries a `*` must expand at least
/// one name that is not an import, and no arm may re-list an import as a
/// bare `WITH x`. This is the CALL-scope export policy the body checker
/// owns; the chain driver ([`super::chain`]) invokes it as its per-stage
/// hook, gated by the seed's carried scope-clause fact — so it runs once
/// per stage of the body's chain, never for a planned statement, and the
/// seed names the driver already carries feed it.
pub(super) fn validate_stage_star_rule(
    query: &Query,
    seed_names: &[String],
    excluded: &HashSet<String>,
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
        // counts, not a hand-spelled subset. The star exclusion wants one
        // name that is not an import — an empty export set after the
        // exclusion is that failure.
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
/// CALL-scope body rejects with `VariableAlreadyBound`. A `*` sibling
/// brings the body's own bindings along (`WITH *, x`), so it is not that
/// shape.
fn check_bare_import_redeclaration(clauses: &[Clause], imported: &HashSet<String>) -> Result<()> {
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
