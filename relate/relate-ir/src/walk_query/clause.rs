//! The per-clause classification vocabulary: the patterns a clause
//! carries and the names it reads from the incoming row. Split from the
//! clause-tree walk (the LABS-311 size gate) as one self-contained family;
//! `super` re-exports both items.

use std::collections::HashSet;

use relate_names::SmallName;

use super::{path_pattern_mentions, walk_remove_item, walk_set_item};
use crate::{CallScope, Clause, Expr, PathPattern};

/// The patterns one clause carries — the one per-clause selection of the
/// pattern vocabulary, exhaustive over [`Clause`] so a new pattern-carrying
/// variant is a compile error here instead of a silent miss at every
/// consumer (the executor's read-set walk and the MATCH trim both read it).
pub(crate) fn clause_patterns(clause: &Clause) -> &[PathPattern] {
    match clause {
        Clause::Match { patterns, .. } | Clause::Create { patterns } => patterns,
        Clause::Merge { pattern, .. } => std::slice::from_ref(pattern),
        Clause::Catalog(_)
        | Clause::Call { .. }
        | Clause::Delete { .. }
        | Clause::Set { .. }
        | Clause::Remove { .. }
        | Clause::With { .. }
        | Clause::Unwind { .. }
        | Clause::Return { .. }
        | Clause::CreateIndex { .. }
        | Clause::DropIndex { .. }
        | Clause::ShowIndexes
        | Clause::Finish
        | Clause::LoadCsv { .. }
        | Clause::CallSubquery { .. }
        | Clause::Conditional { .. }
        | Clause::Foreach { .. }
        | Clause::Use { .. } => &[],
    }
}

/// Every name one clause reads from the incoming row, without descending
/// into a nested body: every pattern the clause carries (through
/// [`clause_patterns`] and the mention walk) and its own read slots. The
/// one per-clause read enumeration — the executor's MATCH trim reads it
/// instead of re-listing slots. A projection (`WITH`/`RETURN`, the
/// caller's position policy), a conditional body and a `FOREACH`/`CALL { }`
/// body are the caller's to enter.
pub fn clause_read_vars(clause: &Clause, out: &mut HashSet<String>) {
    for path in clause_patterns(clause) {
        path_pattern_mentions(path, out);
    }
    let mut no_clause = |_: &Clause, _: bool| {};
    let read_item = |item: Option<&Expr>, out: &mut HashSet<String>| {
        if let Some(expr) = item {
            crate::expr_vars(expr, out);
        }
    };
    match clause {
        Clause::Match {
            where_clause,
            search,
            ..
        } => {
            read_item(where_clause.as_ref(), out);
            if let Some(search) = search {
                for expr in search.spec.exprs() {
                    crate::expr_vars(expr, out);
                }
                // The window split's score half rides the SEARCH clause
                // (ADR-0678): its reads join every other expression's
                // here, so a variable the score predicate names is as
                // visible as one the entity half names.
                read_item(search.post_where.as_ref(), out);
            }
        }
        Clause::Merge {
            on_create,
            on_match,
            ..
        } => {
            for item in on_create.iter().chain(on_match) {
                walk_set_item(item, &mut no_clause, &mut |expr, _| {
                    crate::expr_vars(expr, out)
                });
            }
        }
        Clause::Call {
            args, where_clause, ..
        } => {
            for arg in args.args() {
                crate::expr_vars(arg, out);
            }
            read_item(where_clause.as_ref(), out);
        }
        Clause::Delete { expressions, .. } => {
            for expr in expressions {
                crate::expr_vars(expr, out);
            }
        }
        Clause::Set { items } => {
            for item in items {
                walk_set_item(item, &mut no_clause, &mut |expr, _| {
                    crate::expr_vars(expr, out)
                });
            }
        }
        Clause::Remove { items } => {
            for item in items {
                walk_remove_item(item, &mut no_clause, &mut |expr, _| {
                    crate::expr_vars(expr, out)
                });
            }
        }
        Clause::Unwind { expression, .. } => crate::expr_vars(expression, out),
        Clause::LoadCsv { url, .. } => crate::expr_vars(url, out),
        // The import list is the clause's own read of the enclosing row; the
        // body is not entered here.
        Clause::CallSubquery {
            in_transactions,
            scope,
            ..
        } => {
            if let CallScope::Vars(vars) = scope {
                out.extend(vars.iter().map(SmallName::to_string));
            }
            if let Some(tx) = in_transactions {
                for expr in tx.exprs() {
                    crate::expr_vars(expr, out);
                }
            }
        }
        // The branch predicates and the list evaluate before their bodies.
        Clause::Conditional { branches, .. } => {
            for (predicate, _) in branches {
                crate::expr_vars(predicate, out);
            }
        }
        Clause::Foreach { list, .. } => crate::expr_vars(list, out),
        Clause::Use { graph } => crate::expr_vars(graph, out),
        Clause::Create { .. }
        | Clause::With { .. }
        | Clause::Return { .. }
        | Clause::Catalog(_)
        | Clause::CreateIndex { .. }
        | Clause::DropIndex { .. }
        | Clause::ShowIndexes
        | Clause::Finish => {}
    }
}
