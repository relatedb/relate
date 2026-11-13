//! Output-name computation: the column names each query stage exports.
//!
//! Used by semantic validation (UNION column agreement, subquery scope
//! exports) and by the planner to seed a `NEXT` stage's binding environment.

use std::collections::HashSet;

use relate_core::{Error, Result};
use relate_ir::{Clause, Expr, PathPattern, Projection, Query, SingleQuery};
use relate_model::name_list::same_name_multiset;
use relate_names::SmallName;

pub(crate) fn query_output_names(query: &Query) -> Result<Vec<String>> {
    query_output_names_with_seed(query, Vec::new(), &HashSet::new())
}

/// Output names of one stage chain, from a running scope seed.
///
/// `excluded_imports` is the scope-clause import exclusion set: the names a
/// scope-clause `CALL (…) { … }` body imports, which its export set must not
/// contain — neither through a `RETURN *`/`WITH *` star re-expansion nor as
/// an explicitly projected pass-through. An empty set means no exclusion —
/// the same spelling the validate layer uses, so the two layers agree on one
/// meaning. The exclusion has exactly one meaning, decided here at the name
/// computation: an excluded name is simply absent from the returned set.
/// Callers never re-filter the result.
pub(crate) fn query_output_names_with_seed(
    query: &Query,
    seed: Vec<String>,
    excluded_imports: &HashSet<String>,
) -> Result<Vec<String>> {
    let output = parts_output_names(&query.parts, seed, excluded_imports)?;
    if let Some(next) = query.next.as_deref() {
        query_output_names_with_seed(next, output, excluded_imports)
    } else {
        Ok(output)
    }
}

/// Push a synthetic bare-variable `Projection` for each imported name the
/// item list does not already project. Shared by
/// [`query_with_persistent_imports`]'s WITH-item and terminal-RETURN walks.
///
/// Only a *same-name pass-through* keeps an import visible: `WITH a AS b`
/// reads the import `a` but renames it, so `a` still needs the synthetic
/// item (otherwise the following clause sees `a` as undefined).
fn push_missing_imports(items: &mut Vec<Projection>, imported: &[String]) {
    for name in imported {
        // The import list is String (the output-name domain); the
        // synthetic projection's variable is classified into the IR's
        // name domain here, once per import.
        let name = SmallName::from_name(name.as_str());
        if !items.iter().any(|item| {
            matches!(&item.expr, Expr::Variable(variable) if variable == &name)
                && item.alias.as_ref().is_none_or(|alias| alias == &name)
        }) {
            items.push(Projection {
                expr: Expr::Variable(name),
                alias: None,
                source_text: None,
            });
        }
    }
}

/// Rewrite `query` so every imported name survives its `WITH`s: the name is
/// added as a bare-variable projection item where the item list does not
/// already carry it, and the terminal `RETURN` of a `NEXT` stage gets the
/// same treatment. Validation uses the rewrite to admit the imports; the
/// executor's expression subqueries (`EXISTS`/`COUNT`/`COLLECT`) use it so
/// an implicitly imported outer variable stays a grouping key of an
/// aggregating `WITH`. A query transformation, not a validation check: this
/// module already owns the import/export name computation.
pub fn query_with_persistent_imports(query: &Query, imported: &[String]) -> Query {
    let mut query = query.clone();
    // The NEXT-stage rule is a property of the query, not of each part:
    // decide it once outside the part walk.
    let has_next = query.next.is_some();
    for part in &mut query.parts {
        for clause in &mut part.clauses {
            if let Clause::With { items, .. } = clause {
                push_missing_imports(items, imported);
            }
        }
        if has_next {
            if let Some(Clause::Return { items, .. }) = part.clauses.last_mut() {
                push_missing_imports(items, imported);
            }
        }
    }
    if let Some(next) = &mut query.next {
        **next = query_with_persistent_imports(next, imported);
    }
    query
}

/// Output names of a single query stage (its `parts`, ignoring `next`).
pub(crate) fn parts_output_names(
    parts: &[SingleQuery],
    seed: Vec<String>,
    excluded_imports: &HashSet<String>,
) -> Result<Vec<String>> {
    stage_output_names(
        parts.iter().map(|part| part.clauses.as_slice()),
        seed,
        excluded_imports,
    )
}

/// Output names of one stage given each UNION arm's clauses: the shared
/// body of [`parts_output_names`] and the planned-statement form
/// `plan::plan_output_names` (a `LogicalPlan` holds bare clause lists).
pub(crate) fn stage_output_names<'a>(
    parts: impl IntoIterator<Item = &'a [Clause]>,
    seed: Vec<String>,
    excluded_imports: &HashSet<String>,
) -> Result<Vec<String>> {
    let mut expected: Option<Vec<String>> = None;
    for part in parts {
        let names = part_output_names(part, seed.clone(), excluded_imports)?;
        if let Some(expected) = &expected {
            // UNION branches match by column *name*, not order: a branch
            // projecting the same names in a different order is the same
            // output shape (the executor reorders its rows onto the first
            // branch's columns). The first branch's order is what the
            // stage's output names are.
            if !same_name_multiset(expected, &names) {
                return Err(Error::semantic("UNION requires same columns"));
            }
        } else {
            expected = Some(names);
        }
    }
    Ok(expected.unwrap_or_default())
}

/// Output names of one UNION arm, given the scope it starts from: the
/// per-clause scan [`stage_output_names`] folds over the arms. A `RETURN *`
/// expands the running scope, so the result is the arm's *exported* names —
/// which is also what a CALL-scope body's star rule compares against its
/// imports.
///
/// The excluded imports stay in the running scope (a later clause may
/// reference them) but never appear in the returned export set — neither
/// through a star nor as an explicitly projected name.
pub(crate) fn part_output_names(
    clauses: &[Clause],
    mut names: Vec<String>,
    excluded_imports: &HashSet<String>,
) -> Result<Vec<String>> {
    // The names a trailing CALL subquery adds to the scope, captured by the
    // scan below. A stage that concludes with a projecting CALL exports
    // exactly these: the body's own columns (`CALL () { OPTIONAL CALL () {
    // RETURN 1 AS data } }` exports `data`). Exporting the whole accumulated
    // scope instead would re-export a CALL body's imports and trip the
    // outer-scope collision check.
    let mut trailing_call_added: Vec<String> = Vec::new();
    for clause in clauses {
        match clause {
            Clause::With { items, .. } => names = projected_scope(items, names),
            Clause::Return { items, .. } => {
                let mut out = Vec::new();
                for (index, item) in items.iter().enumerate() {
                    if matches!(item.expr, Expr::Star) {
                        for name in names
                            .iter()
                            .filter(|name| !excluded_imports.contains(*name))
                        {
                            push_name(&mut out, name.clone());
                        }
                    } else {
                        let name = item.output_name(index);
                        if !excluded_imports.contains(&name) {
                            push_name(&mut out, name);
                        }
                    }
                }
                return Ok(out);
            }
            Clause::Conditional {
                branches,
                else_query,
            } => {
                return conditional_output_names(
                    branches,
                    else_query.as_deref(),
                    names,
                    excluded_imports,
                )
            }
            Clause::Finish => return Ok(Vec::new()),
            other => {
                // `names` is append-only and duplicate-free (`push_name` is
                // the one mutator `extend_scope` uses), so the names this
                // clause added are exactly the tail `names[start..]` — a
                // CALL subquery's export diff needs no whole-scope
                // snapshot. The value is read only when the stage's *last*
                // clause is a projecting CALL, which overwrites it last.
                let start = names.len();
                extend_scope(other, &mut names)?;
                trailing_call_added = names[start..].to_vec();
            }
        }
    }
    match clauses.last() {
        // A stage that ends in a projecting CALL subquery exports the body's
        // own columns. A unit subquery call with no RETURN leaves the stage
        // columnless, like any other write-only tail.
        Some(Clause::CallSubquery { query, .. }) if query.projects_columns() => {
            Ok(trailing_call_added)
        }
        Some(Clause::Catalog(_)) => Ok(names),
        _ => Ok(Vec::new()),
    }
}

/// The names a `WHEN` clause exports: every branch body carries the same
/// shape (validated separately), so the first one stands in for the whole clause; a
/// clause with no body (no branch, no `ELSE`) exports nothing.
///
/// The seeded form is for the planner's own scope fold
/// ([`part_output_names`]); the executor's declared-columns read uses
/// [`conditional_declared_columns`].
pub(crate) fn conditional_output_names(
    branches: &[(Expr, Query)],
    else_query: Option<&Query>,
    seed: Vec<String>,
    excluded_imports: &HashSet<String>,
) -> Result<Vec<String>> {
    match branches.first().map(|(_, query)| query).or(else_query) {
        Some(query) => query_output_names_with_seed(query, seed, excluded_imports),
        None => Ok(Vec::new()),
    }
}

/// The declared column list of a `WHEN` clause: the same derivation
/// [`part_output_names`] runs, with no enclosing seed and no CALL-scope
/// import exclusion.
///
/// The empty seed is the conditional's own plan-time fact, not a runtime
/// approximation: `validate::check_conditional_clause` derives the clause's
/// exported environment from exactly this seed (`query_output_names`), so
/// the executor's result materialization and the plan agree by construction.
pub fn conditional_declared_columns(
    branches: &[(Expr, Query)],
    else_query: Option<&Query>,
) -> Result<Vec<String>> {
    conditional_output_names(branches, else_query, Vec::new(), &HashSet::new())
}

/// Apply a `WITH` projection to the running scope: a `*` item keeps the
/// incoming names and the explicit items add to them, otherwise the item
/// list replaces the scope.
pub(crate) fn projected_scope(items: &[Projection], names: Vec<String>) -> Vec<String> {
    let mut next = if items.iter().any(|item| matches!(item.expr, Expr::Star)) {
        names
    } else {
        Vec::new()
    };
    for (index, item) in items.iter().enumerate() {
        if !matches!(item.expr, Expr::Star) {
            push_name(&mut next, item.output_name(index));
        }
    }
    next
}

/// The two name lists a `CALL { … }` body carries, derived once from its
/// scope clause: `(seed, imports)`.
///
/// `seed` is what the body's own scope sees, so it is what a body-level
/// `RETURN *` expands: the whole incoming scope for a naked `CALL { … }` and
/// for `CALL (*) { … }`, the named variables for `CALL (a, b) { … }`.
/// `imports` is the scope-clause import exclusion set — the same names for a
/// scope clause, empty for a naked body, which imports the whole row without
/// one. The two coincide for a scope clause and differ for a naked body, so
/// callers pass them on as separate values.
pub(crate) fn call_body_names(
    scope: &relate_ir::CallScope,
    incoming: &[String],
) -> (Vec<String>, HashSet<String>) {
    let seed = match scope {
        // Output names are the String rendering domain; the scope vars
        // ride the IR's name representation and render once, here.
        relate_ir::CallScope::Vars(vars) => vars.iter().map(|name| name.to_string()).collect(),
        relate_ir::CallScope::None | relate_ir::CallScope::Wildcard => incoming.to_vec(),
    };
    // The exclusion set is consumed as a set everywhere it is passed on, so
    // it crosses the boundary in that one shape; the seed stays a `Vec`
    // because the name fold takes it by value in order.
    let imports = if scope.has_scope_clause() {
        seed.iter().cloned().collect()
    } else {
        HashSet::new()
    };
    (seed, imports)
}

/// One clause's variable introductions — the single spelling of "what a
/// clause binds" (`names.rs` owns it; every other consumer collects this
/// enumerator instead of re-matching the `Clause` vocabulary): pattern
/// variables and the CIP-224 score var, `UNWIND`/`LOAD CSV` aliases,
/// `CALL … YIELD` names, subquery exports and reporting status, catalog
/// output columns. Projections (`WITH`/`RETURN`) replace the scope
/// instead of extending it — their drivers own that policy — so they
/// introduce nothing here. The introduced names come back in enumeration
/// order and a name may appear more than once (the pattern walk's
/// path-names prefix and payload walk can both yield it) — dedup is the
/// caller's own policy. `incoming` is the running scope as an ordered
/// name list (the subquery seed consumes it in declaration order — a
/// body-level `RETURN *` expands to exactly it), supplied lazily and
/// materialized inside the subquery arm alone — no other clause pays for
/// a snapshot.
pub(crate) fn clause_bindings(
    clause: &Clause,
    incoming: impl FnOnce() -> Vec<String>,
) -> Result<Vec<String>> {
    let mut introduced = Vec::new();
    let push = &mut |name: String| introduced.push(name);
    match clause {
        Clause::Match {
            patterns, search, ..
        } => {
            for path in patterns {
                pattern_names(path, push);
            }
            // A `<search clause>`'s `SCORE AS` variable is bound by
            // this MATCH like its pattern variables are (the same binding
            // `validate/clause.rs` and `rewrite.rs` record).
            if let Some(score_var) = search.as_ref().and_then(|search| search.score_var.as_ref()) {
                push(score_var.to_string());
            }
        }
        Clause::Create { patterns } => {
            for path in patterns {
                pattern_names(path, push);
            }
        }
        Clause::Merge { pattern, .. } => pattern_names(pattern, push),
        Clause::Unwind { alias, .. }
        | Clause::LoadCsv {
            variable: alias, ..
        } => push(alias.to_string()),
        Clause::Call { yield_items, .. } => {
            for name in yield_items.effective_names() {
                push(name.to_string());
            }
        }
        Clause::CallSubquery {
            query,
            scope,
            in_transactions,
            ..
        } => {
            // The body's two name lists, derived once (seed = what the body
            // sees; imports = the scope-clause import exclusion set).
            let (seed, excluded_imports) = call_body_names(scope, &incoming());
            // The scope clause's import exclusion: the body's export set
            // carries none of the names the scope clause imported, so the
            // star exclusion is part of the UNION-arm agreement itself (a
            // star arm must not disagree with a sibling explicit arm on
            // the import columns).
            // The exclusion is applied by the name computation, so the
            // returned names are the export set verbatim.
            for name in query_output_names_with_seed(query, seed, &excluded_imports)? {
                push(name);
            }
            if let Some(status) = in_transactions
                .as_ref()
                .and_then(|transaction| transaction.report_status.as_ref())
            {
                push(status.to_string());
            }
        }
        Clause::Catalog(command) => {
            for name in catalog_output_names(command) {
                push(name);
            }
        }
        // Projections replace the scope (handled by the drivers); the rest
        // introduce no variable.
        Clause::With { .. }
        | Clause::Return { .. }
        | Clause::Conditional { .. }
        | Clause::Finish
        | Clause::Foreach { .. }
        | Clause::Use { .. }
        | Clause::Delete { .. }
        | Clause::Set { .. }
        | Clause::Remove { .. }
        | Clause::CreateIndex { .. }
        | Clause::DropIndex { .. }
        | Clause::ShowIndexes => {}
    }
    Ok(introduced)
}

/// Extend the running scope with the names one non-projecting clause
/// introduces: pattern variables, `UNWIND`/`LOAD CSV` aliases, `CALL … YIELD`
/// names, `CALL { … }` exports (and a reporting status), and catalog rows.
/// The projecting clauses (`WITH`, `RETURN`, `WHEN`, `FINISH`) are handled by
/// the two scope drivers above, which *replace* the scope.
///
/// Exhaustive over the IR clause enum: a new clause kind cannot be added
/// without deciding what it introduces into a scope.
fn extend_scope(clause: &Clause, names: &mut Vec<String>) -> Result<()> {
    // The lazy scope is materialized inside the subquery arm alone — the
    // immutable borrow ends at the call, so the fold needs no snapshot.
    for name in clause_bindings(clause, || names.clone())? {
        push_name(names, name);
    }
    Ok(())
}

/// Non-star `RETURN` output names of a query's final stage.
pub(crate) fn explicit_return_names(query: &Query) -> Vec<String> {
    query
        .final_stage()
        .parts
        .iter()
        .filter_map(|part| part.clauses.last())
        .filter_map(|clause| match clause {
            Clause::Return { items, .. } => Some(items),
            _ => None,
        })
        .flat_map(|items| {
            items
                .iter()
                .enumerate()
                .filter(|(_, item)| !matches!(item.expr, Expr::Star))
                .map(|(index, item)| item.output_name(index))
        })
        .collect()
}

fn push_name(names: &mut Vec<String>, name: String) {
    if !names.iter().any(|existing| existing == &name) {
        names.push(name);
    }
}

/// Names a catalog (`SHOW …`/`TERMINATE …`) clause introduces into scope.
/// The column tables themselves are owned by `relate-front`
/// (`show_columns`), the single source of truth shared with front
/// validation and executor materialization.
pub(crate) fn catalog_output_names(command: &relate_parser::Clause) -> Vec<String> {
    relate_front::show_columns::catalog_output_columns(command)
}

/// The names a path pattern introduces, in the order it spells them —
/// the IR walk owns the enumeration and its promised declaration order
/// (path names first, then payload variables, quantified subpath before
/// its group); this is the String-rendering seam only. Raw pushes (a
/// name may repeat) — dedup is the caller's own policy.
fn pattern_names(path: &PathPattern, push: &mut impl FnMut(String)) {
    for name in relate_ir::pattern_anchor_variables(path) {
        push(name.to_string());
    }
}
