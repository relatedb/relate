//! The `CALL { … }` subquery clause: scope-clause imports, CIP-218 export
//! rules, and the 42I28 importing-WITH shape rule. The `CALL IN
//! TRANSACTIONS` modifier battery lives in [`super::in_transactions`]; the
//! body's shadowing policy is [`crate::expr_check`]'s, shared with every
//! other subquery body.

use std::collections::HashSet;

use relate_core::{Error, Result};
use relate_ir::{Clause, Expr, Projection, SingleQuery};
use relate_names::SmallName;

use crate::binding::{Binding, Env, EnvBridge};
use crate::expr_text::expr_text;
use crate::names::{explicit_return_names, query_with_persistent_imports};

use super::in_transactions::check_in_transactions_modifiers;
use super::query_contains_in_transactions;

pub(super) fn check_call_subquery_clause(
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    clause_index: usize,
    part: &SingleQuery,
    clause: &Clause,
) -> Result<()> {
    let Clause::CallSubquery {
        query,
        scope,
        in_transactions,
        ..
    } = clause
    else {
        unreachable!("clause arm");
    };
    if query_contains_in_transactions(query) {
        return Err(Error::syntax(
            "NestedCallInTransactions: CALL IN TRANSACTIONS cannot be nested",
        ));
    }
    if in_transactions.is_some() && write_before(part, clause_index, is_write_procedure) {
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
    // stage driver the planner runs, without planning anything. The seed is
    // what the body's own `RETURN *` expands (the whole outer scope for a
    // naked body); the import list is the scope-clause import set, so a
    // scope clause's star arms do not re-expand the imports and every
    // UNION arm agrees on the export shape. Its returned names are the
    // body's export set, read again below — the walk is not repeated, and
    // both lists come from the one `call_body_names` derivation.
    let output_names = crate::validate::validate_query_with_env(
        &scoped_query,
        &imported,
        &seed_names,
        &import_names,
        is_write_procedure,
    )?;
    if let Some(tx) = in_transactions {
        check_in_transactions_modifiers(tx, env, is_write_procedure, clause_index, part)?;
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

/// Whether any clause *before* `clause_index` writes, which is the whole
/// question `CALL … IN TRANSACTIONS`'s composition rule asks. Derived at
/// the point of use instead of threaded through the clause loop: the
/// enclosing part's write history is a pure function of its prefix, and an
/// `IN TRANSACTIONS` subquery's own writes run in their own transactions,
/// so they do not make the part a writing one.
fn write_before(
    part: &SingleQuery,
    clause_index: usize,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
) -> bool {
    part.clauses[..clause_index].iter().any(|clause| {
        crate::validate::clause_is_write(clause, is_write_procedure)
            && !matches!(
                clause,
                Clause::CallSubquery {
                    in_transactions: Some(_),
                    ..
                }
            )
    })
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
