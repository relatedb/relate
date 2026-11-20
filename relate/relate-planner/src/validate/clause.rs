//! Per-clause semantic checks. `validate_part_with_env` dispatches one of these.

use relate_core::{Error, Result};
use relate_ir::{
    is_agg_expr, CallArgs, Clause, Expr, PathPattern, RemoveItem, SetItem, YieldItems,
};
use relate_names::SmallName;

use crate::agg::{
    check_agg_projections, check_aggregating_subclause, check_where_subclause, reject_agg_in_pred,
    reject_order_by_agg, SubclauseScope,
};
use crate::binding::{bind_create_path, bind_path, projected_bindings, Binding, Env, EnvBridge};
use crate::expr_check::{check_expr, check_nonneg_int_arg, check_pred, is_delete_target};
use crate::names::{catalog_output_names, query_output_names};
use crate::pattern::{
    check_juxtaposition, check_path_exprs, check_write_label_exprs, check_write_rels,
    collect_qpp_group_names, element_has_qpp,
};

use super::{
    check_remove_item, check_set_item, validate_clauses_with_env, validate_conditional_returns,
    validate_part_with_env,
};

pub(super) fn check_catalog_clause(env: &mut Env, command: &relate_parser::Clause) -> Result<()> {
    for name in catalog_output_names(command) {
        env.insert(name, Binding::Value);
    }

    Ok(())
}

pub(super) fn check_match_clause(
    env: &mut Env,
    shadowable: &std::collections::HashSet<String>,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    clause: &Clause,
) -> Result<()> {
    let Clause::Match {
        patterns,
        where_clause,
        search,
        ..
    } = clause
    else {
        unreachable!("clause arm");
    };
    let mut group_names = std::collections::HashSet::new();
    for pattern in patterns {
        collect_qpp_group_names(&pattern.elements, &mut group_names, env)?;
    }
    crate::pattern::check_shortest_path_bound_relationships(patterns, env, shadowable)?;
    for p in patterns {
        check_juxtaposition(p)?;
        bind_path(env, p, shadowable)?;
    }
    for p in patterns {
        check_path_exprs(env, is_write_procedure, p)?;
    }
    if let Some(search) = search {
        // Every value expression of the (possibly nested, CIP-278) source
        // tree — the control slots and the CIP-240 decomposed predicates'
        // RHS. The source may read any variable this MATCH's own pattern
        // binds (`(seed:Person), (p:Person) SEARCH p IN (… FOR seed.bio …)`),
        // so it is checked after the pattern variables enter scope — the
        // same scope the executor's per-row, pattern-first evaluation sees.
        // The binding variable itself is excluded by the
        // self-reference check in the front end, which runs first.
        for expr in search.spec.exprs() {
            check_expr(env, is_write_procedure, expr)?;
        }
        if let Some(score_var) = &search.score_var {
            env.insert_name(score_var.clone(), Binding::Value);
        }
    }
    if let Some(w) = where_clause {
        let has_qpp = patterns
            .iter()
            .any(|pattern| pattern.elements.iter().any(element_has_qpp));
        // the WHERE policy as one stated table over (has QPP, is an
        // aggregation): aggregation is rejected outright unless a QPP
        // is present, and the predicate is checked unless the QPP
        // aggregation exemption applies
        match (has_qpp, is_agg_expr(w)) {
            // no QPP: aggregations never legal, the predicate checked
            (false, _) => {
                reject_agg_in_pred(w)?;
                check_pred(env, is_write_procedure, w)?;
            }
            // QPP present, plain predicate: checked
            (true, false) => check_pred(env, is_write_procedure, w)?,
            // the QPP aggregation exemption: neither rule applies
            (true, true) => {}
        }
    }

    Ok(())
}

pub(super) fn check_create_clause(
    env: &mut Env,
    shadowable: &std::collections::HashSet<String>,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    patterns: &[PathPattern],
) -> Result<()> {
    // Expressions in one updating clause observe only the scope
    // entering that clause, never entities created alongside
    // them (CIP-156).
    let input_env = env.clone();
    for p in patterns {
        check_write_rels(p, true)?;
        check_write_label_exprs(&input_env, is_write_procedure, p)?;
    }
    for p in patterns {
        bind_create_path(env, p, shadowable)?;
        check_path_exprs(env, is_write_procedure, p)?;
    }

    Ok(())
}

pub(super) fn check_merge_clause(
    env: &mut Env,
    shadowable: &std::collections::HashSet<String>,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    clause: &Clause,
) -> Result<()> {
    let Clause::Merge {
        pattern,
        on_create,
        on_match,
    } = clause
    else {
        unreachable!("clause arm");
    };
    check_write_rels(pattern, false)?;
    let input_env = env.clone();
    check_write_label_exprs(&input_env, is_write_procedure, pattern)?;
    bind_create_path(env, pattern, shadowable)?;
    check_path_exprs(env, is_write_procedure, pattern)?;
    for item in on_create.iter().chain(on_match.iter()) {
        check_set_item(env, is_write_procedure, item)?;
    }

    Ok(())
}

pub(super) fn check_call_clause(
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    clauses: &[Clause],
    args: &CallArgs,
    yield_items: &YieldItems,
    where_clause: Option<&Expr>,
) -> Result<()> {
    for a in args.args() {
        check_expr(env, is_write_procedure, a)?;
        if is_agg_expr(a) {
            return Err(Error::syntax(
                "InvalidAggregation: aggregations are not allowed in CALL arguments",
            ));
        }
    }
    let star = matches!(yield_items, YieldItems::Star);
    if star && clauses.len() > 1 {
        return Err(Error::syntax(
            "UnexpectedSyntax: YIELD * is not allowed in in-query CALL",
        ));
    }
    // Shadow before duplicate, with the same messages as the front's shared
    // yield-binding policy: the front rejects both before lowering, so this
    // is the backstop for a hand-built IR and must not drift from it.
    for name in yield_items.effective_names() {
        if env.contains_name(name) {
            return Err(name.with_str(Error::duplicate_variable));
        }
    }
    let mut seen = std::collections::HashSet::new();
    for name in yield_items.effective_names() {
        if !seen.insert(name.clone()) {
            return Err(name.with_str(Error::duplicate_variable));
        }
        env.insert_name(name.clone(), Binding::Value);
    }
    if let Some(predicate) = where_clause {
        check_pred(env, is_write_procedure, predicate)?;
    }

    Ok(())
}

/// Whether `e` is a label predicate, seen through the `NOT` that lowering
/// wraps around `IS NOT LABELED` (the sibling `IS NOT ...` predicates
/// carry `negated` on their variant; the label form is the one that
/// lowers to `Unary { Not, LabelPredicate }`).
fn is_label_predicate(e: &Expr) -> bool {
    match e {
        Expr::LabelPredicate { .. } => true,
        Expr::Unary {
            op: relate_ir::UnaryOp::Not,
            expr,
        } => is_label_predicate(expr),
        _ => false,
    }
}

pub(super) fn check_delete_clause(
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    expressions: &[Expr],
) -> Result<()> {
    for e in expressions {
        check_expr(env, is_write_procedure, e)?;
        if is_label_predicate(e) {
            return Err(Error::syntax(
                "InvalidDelete: cannot delete a literal or label expression",
            ));
        }
        if !is_delete_target(e) {
            return Err(Error::syntax(
                "InvalidArgumentType: DELETE requires a node, relationship, or path",
            ));
        }
        if let Expr::Variable(v) = e {
            if matches!(env.get_name(v), Some(&Binding::Scalar | &Binding::List)) {
                return Err(Error::syntax("InvalidDelete: cannot delete a non-entity"));
            }
        }
    }

    Ok(())
}

pub(super) fn check_set_clause(
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    items: &[SetItem],
) -> Result<()> {
    for item in items {
        check_set_item(env, is_write_procedure, item)?;
    }

    Ok(())
}

pub(super) fn check_remove_clause(
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    items: &[RemoveItem],
) -> Result<()> {
    for item in items {
        check_remove_item(env, is_write_procedure, item)?;
    }

    Ok(())
}

pub(super) fn check_with_clause(
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    imports: &crate::agg::Imports,
    clause: &Clause,
) -> Result<()> {
    let Clause::With {
        distinct,
        items,
        group_by,
        order,
        skip,
        limit,
        where_clause,
        origin,
        ..
    } = clause
    else {
        unreachable!("clause arm");
    };
    if *origin == relate_ir::WithOrigin::Let {
        for item in items.iter().filter(|item| !matches!(item.expr, Expr::Star)) {
            // A LET item without an alias is the bare-variable form;
            // only an aliased one can rebind a name.
            let Some(name) = item.alias.as_ref() else {
                continue;
            };
            if env.contains_name(name) {
                return Err(Error::syntax(format!(
                    "VariableAlreadyBound: LET cannot rebind `{name}`"
                )));
            }
            if is_agg_expr(&item.expr) {
                return Err(Error::syntax(
                    "InvalidAggregation: aggregations are not allowed in LET",
                ));
            }
        }
    }
    let mut seen = std::collections::HashMap::new();
    for (i, item) in items.iter().enumerate() {
        if matches!(item.expr, Expr::Star) {
            continue;
        }
        if super::is_unaliased_expression(item) {
            return Err(Error::syntax(
                "NoExpressionAlias: WITH expressions must be aliased",
            ));
        }
        let name = item.output_name(i);
        if seen.contains_key(&name) {
            return Err(Error::syntax(format!(
                "ColumnNameConflict: `{name}` is already defined"
            )));
        }
        seen.insert(name, ());
    }
    for item in items {
        check_expr(env, is_write_procedure, &item.expr)?;
    }
    // ADR-0453: an import stays a per-row constant only while this
    // projection still carries it — a `WITH` that drops the name demotes it
    // back to the ordinary grouping-key discipline
    // (CollectListConcat "unprojected outer variable in a CALL subquery").
    let has_star = items.iter().any(|item| matches!(item.expr, Expr::Star));
    // The projection's one scope derivation: the subclause environment is
    // the incoming scope extended with the items' re-bindings, and the
    // post-clause scope is that same item map — under `*` the star carries
    // the incoming scope through, so the subclause environment already is
    // the continuation ([`projected_bindings`], the shared per-item rule).
    // Derived once, above the import filter: the effective imports, the
    // subclause environment and the continuation all read this map.
    let projected = projected_bindings(env, items);
    let effective_imports: crate::agg::Imports = imports
        .iter()
        .filter(|n| (has_star && env.contains_key(*n)) || projected.contains_key(*n))
        .cloned()
        .collect();
    check_agg_projections(items, group_by.as_deref(), &effective_imports, env)?;
    let mut order_env = env.clone();
    order_env.extend(projected.clone());
    let next = if has_star {
        order_env.clone()
    } else {
        projected
    };
    // The clause's one subclause context, built once at the boundary over
    // the clause's own import set (an enclosing scope's import stays a
    // per-row constant for the non-aggregating subclauses even when this
    // projection drops the name — the imported name is not a row of this
    // clause's input at all, AggregationAcceptance "Should find constants
    // in subclause expression in an aggregating projection clause"); the
    // aggregating keys' demoted set (an import stays a per-row constant
    // only while this projection still carries it, CollectListConcat
    // "unprojected outer variable in a CALL subquery") is the explicit
    // import parameter those two checks take.
    let subclauses = SubclauseScope::new(items, group_by.as_deref(), *distinct, imports, env);
    reject_order_by_agg(&subclauses, order, &effective_imports)?;
    for o in order {
        check_aggregating_subclause(&subclauses, &o.expr, "ORDER BY")?;
        check_expr(&order_env, is_write_procedure, &o.expr)?;
    }
    if let Some(s) = skip {
        check_expr(&order_env, is_write_procedure, s)?;
        check_nonneg_int_arg(s)?;
    }
    if let Some(l) = limit {
        check_expr(&order_env, is_write_procedure, l)?;
        check_nonneg_int_arg(l)?;
    }
    if let Some(w) = where_clause {
        // ADR-0453's WHERE twin of the ORDER BY gate: HAVING semantics
        // after an aggregating (or DISTINCT) WITH, the InvalidAggregation
        // rejection otherwise, the demoted import set for the aggregating
        // keys.
        check_where_subclause(&subclauses, &effective_imports, w)?;
        check_pred(&order_env, is_write_procedure, w)?;
    }
    *env = next;

    Ok(())
}

pub(super) fn check_return_clause(
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    imports: &crate::agg::Imports,
    clause: &Clause,
) -> Result<()> {
    let Clause::Return {
        items,
        group_by,
        order,
        skip,
        limit,
        distinct,
        ..
    } = clause
    else {
        unreachable!("clause arm");
    };
    // 42I37: `RETURN *` expands the incoming scope, so it needs one --
    // alone (`RETURN *`) or beside other items (`RETURN *, 1 AS x`).
    if items.iter().any(|item| matches!(item.expr, Expr::Star)) && env.is_empty() {
        return Err(Error::syntax(
            "NoVariablesInScope: RETURN * requires variables in scope",
        ));
    }
    let mut seen = std::collections::HashMap::new();
    for item in items {
        let Some(name) = item.alias.clone() else {
            continue;
        };
        if seen.contains_key(&name) {
            return Err(Error::syntax(format!(
                "ColumnNameConflict: `{name}` is already defined"
            )));
        }
        seen.insert(name, ());
    }
    for item in items {
        check_expr(env, is_write_procedure, &item.expr)?;
    }
    check_agg_projections(items, group_by.as_deref(), imports, env)?;
    // The clause's one subclause context, built once: the aggregating
    // gate, the grouping keys and the output names are derived here and
    // every subclause check below reads them.
    let subclauses = SubclauseScope::new(items, group_by.as_deref(), *distinct, imports, env);
    reject_order_by_agg(&subclauses, order, imports)?;
    // The order-key environment: the incoming scope extended with the
    // projection's re-bindings — the same derivation the WITH twin builds.
    // DISTINCT (and aggregation) restrict which keys the subclause may
    // name through the one leaf discipline ([`check_aggregating_subclause`],
    // REL8-1868); the environment resolves everything it admits.
    // The environment is derived only when something reads it — an ORDER
    // BY key, SKIP or LIMIT — so a plain terminal RETURN pays neither
    // the scope clone nor the per-item output-name inserts.
    if !order.is_empty() || skip.is_some() || limit.is_some() {
        let mut order_env = env.clone();
        order_env.extend(projected_bindings(env, items));
        for o in order {
            check_aggregating_subclause(&subclauses, &o.expr, "ORDER BY")?;
            check_expr(&order_env, is_write_procedure, &o.expr)?;
        }
        if let Some(s) = skip {
            check_expr(&order_env, is_write_procedure, s)?;
            check_nonneg_int_arg(s)?;
        }
        if let Some(l) = limit {
            check_expr(&order_env, is_write_procedure, l)?;
            check_nonneg_int_arg(l)?;
        }
    }

    Ok(())
}

pub(super) fn check_unwind_clause(
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    expression: &Expr,
    alias: &SmallName,
) -> Result<()> {
    check_expr(env, is_write_procedure, expression)?;
    env.insert_name(alias.clone(), Binding::Value);

    Ok(())
}

pub(super) fn check_load_csv_clause(
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    url: &Expr,
    variable: &SmallName,
) -> Result<()> {
    check_expr(env, is_write_procedure, url)?;
    env.insert_name(variable.clone(), Binding::Map);

    Ok(())
}

pub(super) fn check_conditional_clause(
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    clause: &Clause,
) -> Result<()> {
    let Clause::Conditional {
        branches,
        else_query,
    } = clause
    else {
        unreachable!("clause arm");
    };
    // The WHEN branches and the ELSE arm are one policy: each body shares
    // the enclosing scope, and the first body's column list is the shape
    // every later one must match.
    let bodies = branches
        .iter()
        .map(|(predicate, query)| (Some(predicate), query))
        .chain(else_query.as_deref().map(|query| (None, query)));
    let mut expected = None;
    for (predicate, query) in bodies {
        if let Some(predicate) = predicate {
            check_pred(env, is_write_procedure, predicate)?;
        }
        validate_conditional_returns(query)?;
        for part in &query.parts {
            validate_part_with_env(
                part,
                super::Scope::branch_body(env.clone()),
                is_write_procedure,
            )?;
        }
        let names = query_output_names(query)?;
        if expected.as_ref().is_some_and(|e| e != &names) {
            return Err(Error::syntax(
                "ColumnNameConflict: conditional branches must return the same columns",
            ));
        }
        expected = Some(names);
    }
    *env = expected
        .unwrap_or_default()
        .into_iter()
        .map(|name| (name, Binding::Value))
        .collect();

    Ok(())
}

pub(super) fn check_foreach_clause(
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    variable: &SmallName,
    list: &Expr,
    clauses: &[Clause],
) -> Result<()> {
    check_expr(env, is_write_procedure, list)?;
    validate_clauses_with_env(
        clauses,
        super::Scope::foreach_body(env, variable),
        is_write_procedure,
    )?;

    Ok(())
}

pub(super) fn check_use_clause(
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    clause_index: usize,
    clauses: &[Clause],
    graph: &Expr,
) -> Result<()> {
    // Position, not write history: `USE` is valid at the very front (a run
    // of leading USE clauses is itself valid — CIP-146's nested-USE-brace
    // collapse stacks more than one at the front), or immediately after a
    // leading importing WITH. The front's `validate_use_placement` owns the
    // same rule; reading `seen_write` here let a transactions-exempt CALL
    // body slip a later `USE` past this check.
    let all_use_before = clauses[..clause_index]
        .iter()
        .all(crate::clause_kind::clause_is_routing);
    let after_importing_with =
        clause_index == 1 && matches!(clauses.first(), Some(Clause::With { .. }));
    if !all_use_before && !after_importing_with {
        return Err(Error::syntax(
            "InvalidPlacementOfUseClause: USE must be the first clause",
        ));
    }
    // A static graph name (`USE neo4j`, a dotted catalog name
    // like `composite.constituent` that lowers to a property
    // chain, or a string literal) is a selector, not a variable
    // access, so there is nothing to scope-check.
    // `use_target::static_graph_name` is the one static-name
    // test; it also resolves the target itself.
    if crate::use_target::static_graph_name(graph).is_none() {
        // The composite-database `graph.*` selectors are `USE`
        // targets, not row expressions: the call itself is
        // resolved (and rejected, ADR-0116 §3) by
        // `use_target::resolve_use_target`, but its argument
        // variables are still ordinary scoped references
        // (`USE graph.byName(g)` with `g` out of scope is
        // UndefinedVariable, as CIP-141 expects).
        if let Some(args) = crate::use_target::graph_selector_args(graph) {
            for arg in args {
                check_expr(env, is_write_procedure, arg)?;
            }
        } else {
            check_expr(env, is_write_procedure, graph)?;
        }
    }

    Ok(())
}
