//! Per-clause semantic checks. `validate_part_with_env` dispatches one of these.

use relate_core::{Error, Result};
use relate_ir::{is_agg_expr, Clause, Expr, Projection, SingleQuery, YieldItems};

use crate::agg::{
    check_agg_projections, check_post_projection_leaves, check_subclause_agg_leaves,
    is_projected_item, order_key_within_projection, projection_aggregates, reject_agg_in_pred,
    reject_order_by_agg,
};
use crate::binding::{bind_create_path, bind_path, Binding, Env, EnvBridge};
use crate::expr_check::{check_expr, check_nonneg_int_arg, check_pred, is_delete_target};
use crate::names::{catalog_output_names, query_output_names};
use crate::pattern::{
    check_juxtaposition, check_path_exprs, check_write_label_exprs, check_write_rels,
    collect_qpp_group_names, element_has_qpp,
};

use super::{
    check_remove_item, check_set_item, validate_conditional_returns, validate_part_with_env,
};

pub(super) fn check_catalog_clause(
    env: &mut Env,
    _is_write_procedure: &mut impl FnMut(&str) -> bool,
    clause: &Clause,
) -> Result<()> {
    let Clause::Catalog(command) = clause else {
        unreachable!("clause arm");
    };
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
        bind_path(env, p, false, shadowable)?;
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
        // The binding variable itself is excluded by the CIP-224 Rule 2
        // self-reference check in the front end, which runs first.
        for expr in search.spec.exprs() {
            check_expr(env, is_write_procedure, expr)?;
        }
    }
    if let Some(search) = search {
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
    clause: &Clause,
) -> Result<()> {
    let Clause::Create { patterns } = clause else {
        unreachable!("clause arm");
    };
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
    part: &SingleQuery,
    clause: &Clause,
) -> Result<()> {
    let Clause::Call {
        args,
        yield_items,
        where_clause,
        ..
    } = clause
    else {
        unreachable!("clause arm");
    };
    for a in args {
        check_expr(env, is_write_procedure, a)?;
        if is_agg_expr(a) {
            return Err(Error::syntax(
                "InvalidAggregation: aggregations are not allowed in CALL arguments",
            ));
        }
    }
    let star = matches!(yield_items, YieldItems::Star);
    if star && part.clauses.len() > 1 {
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

pub(super) fn check_delete_clause(
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    clause: &Clause,
) -> Result<()> {
    let Clause::Delete { expressions, .. } = clause else {
        unreachable!("clause arm");
    };
    for e in expressions {
        check_expr(env, is_write_procedure, e)?;
        if matches!(e, Expr::LabelPredicate { .. }) {
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
    clause: &Clause,
) -> Result<()> {
    let Clause::Set { items } = clause else {
        unreachable!("clause arm");
    };
    for item in items {
        check_set_item(env, is_write_procedure, item)?;
    }

    Ok(())
}

pub(super) fn check_remove_clause(
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    clause: &Clause,
) -> Result<()> {
    let Clause::Remove { items } = clause else {
        unreachable!("clause arm");
    };
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
        if item.alias.is_none() && !matches!(item.expr, Expr::Variable(_)) {
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
    let effective_imports: crate::agg::Imports = imports
        .iter()
        .filter(|n| {
            (has_star && env.contains_key(*n))
                || items
                    .iter()
                    .enumerate()
                    .any(|(i, item)| item.output_name(i) == **n)
        })
        .cloned()
        .collect();
    check_agg_projections(items, group_by.as_deref(), &effective_imports, env)?;
    let mut next = if has_star { env.clone() } else { Env::new() };
    for (i, item) in items.iter().enumerate() {
        if matches!(item.expr, Expr::Star) {
            continue;
        }
        let name = item.output_name(i);
        let b = match &item.expr {
            Expr::Variable(v) => env.get_name(v).copied().unwrap_or(Binding::Value),
            other => Binding::of_static_expr(other).unwrap_or(Binding::Value),
        };
        next.insert(name, b);
    }
    let mut order_env = env.clone();
    order_env.extend(next.clone());
    reject_order_by_agg(
        items,
        group_by.as_deref(),
        *distinct,
        &effective_imports,
        order,
    )?;
    for o in order {
        // An enclosing scope's import stays a per-row constant for the
        // subclauses even when this projection drops the name: the
        // imported name is not a row of this clause's input at all
        // (AggregationAcceptance "Should find constants in subclause
        // expression in an aggregating projection clause").
        check_aggregating_subclause(
            items,
            group_by.as_deref(),
            imports,
            env,
            &o.expr,
            "ORDER BY",
        )?;
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
        // ADR-0453: after an aggregating (or DISTINCT) WITH, a WHERE
        // predicate may aggregate — HAVING semantics, recomputed per group.
        // Any other WHERE keeps the InvalidAggregation rejection.
        if is_agg_expr(w) {
            if !projection_aggregates(items, group_by.as_deref(), *distinct) {
                reject_agg_in_pred(w)?;
            }
            check_subclause_agg_leaves(
                items,
                group_by.as_deref(),
                &effective_imports,
                w,
                "WHERE aggregation",
            )?;
        } else {
            check_aggregating_subclause(items, group_by.as_deref(), imports, env, w, "WHERE")?;
        }
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
    reject_order_by_agg(items, group_by.as_deref(), *distinct, imports, order)?;
    // A `*` item expands the whole incoming scope, so under DISTINCT the
    // pre-projection names remain orderable (`RETURN DISTINCT * ORDER BY p`).
    // The order-key environment is derived only when something reads it —
    // an ORDER BY key, SKIP or LIMIT — so a plain terminal RETURN pays
    // neither the scope clone nor the per-item output-name inserts.
    if !order.is_empty() || skip.is_some() || limit.is_some() {
        let star_expands = items.iter().any(|item| matches!(item.expr, Expr::Star));
        let mut order_env = if *distinct && !star_expands {
            Env::new()
        } else {
            env.clone()
        };
        for (i, item) in items.iter().enumerate() {
            order_env.insert(item.output_name(i), Binding::Value);
        }
        for o in order {
            check_aggregating_subclause(
                items,
                group_by.as_deref(),
                imports,
                env,
                &o.expr,
                "ORDER BY",
            )?;
            // Under DISTINCT the key resolves against the clause's projected
            // expressions (see `order_key_within_projection`); a key outside
            // them still gets the ordinary projected-name check.
            if *distinct
                && (is_projected_item(items, &o.expr)
                    || order_key_within_projection(&o.expr, items))
            {
                continue;
            }
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
    clause: &Clause,
) -> Result<()> {
    let Clause::Unwind { expression, alias } = clause else {
        unreachable!("clause arm");
    };
    check_expr(env, is_write_procedure, expression)?;
    env.insert_name(alias.clone(), Binding::Value);

    Ok(())
}

pub(super) fn check_load_csv_clause(
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    clause: &Clause,
) -> Result<()> {
    let Clause::LoadCsv { url, variable, .. } = clause else {
        unreachable!("clause arm");
    };
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
    clause: &Clause,
) -> Result<()> {
    let Clause::Foreach {
        variable,
        list,
        clauses,
    } = clause
    else {
        unreachable!("clause arm");
    };
    check_expr(env, is_write_procedure, list)?;
    let mut inner = env.clone();
    inner.insert_name(variable.clone(), Binding::Value);
    // The body is a fresh scope: every name the enclosing scopes bound is
    // shadowable by a CREATE/MERGE declaration there, while the loop variable
    // — declared in the body's own scope, even when an enclosing scope bound
    // the same name — is not.
    let mut body_shadowable: std::collections::HashSet<String> = env.keys().cloned().collect();
    variable.with_str(|text| {
        body_shadowable.remove(text);
    });
    validate_part_with_env(
        &SingleQuery {
            clauses: clauses.clone(),
        },
        super::Scope::foreach_body(inner, body_shadowable),
        is_write_procedure,
    )?;

    Ok(())
}

pub(super) fn check_use_clause(
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    clause_index: usize,
    part: &SingleQuery,
    clause: &Clause,
) -> Result<()> {
    let Clause::Use { graph } = clause else {
        unreachable!("clause arm");
    };
    // Position, not write history: `USE` is valid at the very front (a run
    // of leading USE clauses is itself valid — CIP-146's nested-USE-brace
    // collapse stacks more than one at the front), or immediately after a
    // leading importing WITH. The front's `validate_use_placement` owns the
    // same rule; reading `seen_write` here let a transactions-exempt CALL
    // body slip a later `USE` past this check.
    let all_use_before = part.clauses[..clause_index]
        .iter()
        .all(crate::clause_kind::clause_is_routing);
    let after_importing_with =
        clause_index == 1 && matches!(part.clauses.first(), Some(Clause::With { .. }));
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

/// The aggregating-projection subclause rule (GroupByAcceptance [20], [68],
/// [70], [91], [92]): once a projection aggregates (an aggregate item or an
/// explicit `GROUP BY`), a non-aggregating ORDER BY / WHERE expression may
/// only read grouping keys and projected names. An aggregating subclause
/// expression keeps its own discipline (`reject_order_by_agg` /
/// `check_subclause_agg_leaves`), and a restated projection item is that
/// column, so both are left to their owners.
fn check_aggregating_subclause(
    items: &[Projection],
    group_by: Option<&[Expr]>,
    imports: &crate::agg::Imports,
    outer_names: &crate::binding::Env,
    expr: &Expr,
    context: &str,
) -> Result<()> {
    let aggregating = items.iter().any(|item| is_agg_expr(&item.expr)) || group_by.is_some();
    if !aggregating || is_agg_expr(expr) || is_projected_item(items, expr) {
        return Ok(());
    }
    check_post_projection_leaves(items, group_by, imports, outer_names, expr, context)
}
