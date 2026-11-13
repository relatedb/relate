//! Pattern semantic checks: inline property maps and predicates, group
//! variable scoping (QPPs), and the CREATE/MERGE pattern restrictions.

use std::collections::HashSet;

use relate_core::{Error, Result};
use relate_ir::{any_pattern_payload, expr_vars, PathPattern, PatternElement, PatternPayload};
use relate_names::SmallName;

use crate::binding::{Binding, Env, EnvBridge};
use crate::expr_check::{check_expr, check_pred};

pub(crate) fn check_path_exprs(
    env: &Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    p: &PathPattern,
) -> Result<()> {
    for n in &p.nodes {
        if let Some(e) = &n.properties {
            check_expr(env, is_write_procedure, e)?;
        }
    }
    for r in &p.rels {
        if let Some(e) = &r.properties {
            check_expr(env, is_write_procedure, e)?;
        }
    }
    check_gpm_element_exprs(env, is_write_procedure, &p.elements)?;
    if let Some(predicate) = &p.where_clause {
        check_pred(env, is_write_procedure, predicate)?;
    }
    Ok(())
}

fn check_gpm_element_exprs(
    env: &Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    elements: &[PatternElement],
) -> Result<()> {
    for element in elements {
        match element {
            PatternElement::Node(node) => {
                if let Some(expr) = &node.properties {
                    check_expr(env, is_write_procedure, expr)?;
                }
                if let Some(predicate) = &node.where_clause {
                    check_pred(env, is_write_procedure, predicate)?;
                }
            }
            PatternElement::Relationship(rel) => {
                if let Some(expr) = &rel.properties {
                    check_expr(env, is_write_procedure, expr)?;
                }
                if let Some(predicate) = &rel.where_clause {
                    if rel.quantified {
                        let mut local = env.clone();
                        if let Some(name) = &rel.variable {
                            local.insert_name(name.clone(), Binding::Rel);
                        }
                        check_pred(&local, is_write_procedure, predicate)?;
                    } else {
                        check_pred(env, is_write_procedure, predicate)?;
                    }
                }
            }
            PatternElement::Quantified(qpp) => {
                let local_names: std::collections::HashSet<SmallName> = qpp
                    .elements
                    .iter()
                    .filter_map(|inner| match inner {
                        PatternElement::Node(node) => node.variable.clone(),
                        PatternElement::Relationship(rel) => rel.variable.clone(),
                        PatternElement::Quantified(nested) => nested.subpath_variable.clone(),
                    })
                    .chain(qpp.subpath_variable.iter().cloned())
                    .collect();
                let mut predicate_vars = std::collections::HashSet::new();
                if let Some(predicate) = &qpp.where_clause {
                    expr_vars(predicate, &mut predicate_vars);
                }
                for inner in &qpp.elements {
                    match inner {
                        PatternElement::Node(node) => {
                            if let Some(predicate) = &node.where_clause {
                                expr_vars(predicate, &mut predicate_vars);
                            }
                        }
                        PatternElement::Relationship(rel) => {
                            if let Some(predicate) = &rel.where_clause {
                                expr_vars(predicate, &mut predicate_vars);
                            }
                        }
                        PatternElement::Quantified(_) => {}
                    }
                }
                if predicate_vars.iter().any(|name| {
                    // predicate_vars is the expr_vars String sink; the
                    // name-domain membership probes read through the text
                    // bridge.
                    !local_names.contains(&SmallName::from_name(name.as_str()))
                        && env.get_name(&SmallName::from_name(name.as_str()))
                            == Some(&Binding::List)
                }) {
                    return Err(Error::syntax(
                        "NonLocalGroupVariableReference: quantified predicates cannot reference another group variable",
                    ));
                }
                let mut local = env.clone();
                for inner in &qpp.elements {
                    match inner {
                        PatternElement::Node(node) => {
                            if let Some(name) = &node.variable {
                                local.insert_name(name.clone(), Binding::Node);
                            }
                        }
                        PatternElement::Relationship(rel) => {
                            if let Some(name) = &rel.variable {
                                local.insert_name(name.clone(), Binding::Rel);
                            }
                        }
                        PatternElement::Quantified(_) => {}
                    }
                }
                check_gpm_element_exprs(&local, is_write_procedure, &qpp.elements)?;
                if let Some(predicate) = &qpp.where_clause {
                    check_pred(&local, is_write_procedure, predicate)?;
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn check_write_label_exprs(
    env: &Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    p: &PathPattern,
) -> Result<()> {
    fn check_label(
        env: &Env,
        is_write_procedure: &mut impl FnMut(&str) -> bool,
        expr: &relate_ir::LabelExpr,
    ) -> Result<()> {
        match expr {
            relate_ir::LabelExpr::Dynamic { expr, .. } => check_expr(env, is_write_procedure, expr),
            relate_ir::LabelExpr::Not(inner) => check_label(env, is_write_procedure, inner),
            relate_ir::LabelExpr::And(items) | relate_ir::LabelExpr::Or(items) => {
                for item in items {
                    check_label(env, is_write_procedure, item)?;
                }
                Ok(())
            }
            relate_ir::LabelExpr::Label(_) | relate_ir::LabelExpr::Wildcard => Ok(()),
        }
    }
    for node in &p.nodes {
        if let Some(expr) = &node.label_expr {
            check_label(env, is_write_procedure, expr)?;
        }
    }
    for rel in &p.rels {
        if let Some(expr) = &rel.type_expr {
            check_label(env, is_write_procedure, expr)?;
        }
    }
    Ok(())
}

/// Named variables in a pattern predicate / shortestPath must already be bound.
pub(crate) fn check_pattern_pred_vars(
    env: &Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    p: &PathPattern,
) -> Result<()> {
    for name in p.path_names() {
        if !env.contains_name(name) {
            return Err(name.with_str(Error::undefined_variable));
        }
    }
    for n in &p.nodes {
        if let Some(v) = &n.variable {
            if !env.contains_name(v) {
                return Err(v.with_str(Error::undefined_variable));
            }
        }
        if let Some(e) = &n.properties {
            check_expr(env, is_write_procedure, e)?;
        }
    }
    for r in &p.rels {
        if let Some(v) = &r.variable {
            if !env.contains_name(v) {
                return Err(v.with_str(Error::undefined_variable));
            }
        }
        if let Some(e) = &r.properties {
            check_expr(env, is_write_procedure, e)?;
        }
    }
    Ok(())
}

pub(crate) fn check_write_rels(p: &PathPattern, create: bool) -> Result<()> {
    if p.where_clause.is_some()
        || p.elements.iter().any(|element| match element {
            PatternElement::Node(node) => node.where_clause.is_some(),
            PatternElement::Relationship(rel) => rel.where_clause.is_some(),
            PatternElement::Quantified(_) => true,
        })
    {
        return Err(Error::syntax(
            "InvalidNodePatternPredicate: inline pattern predicates are not allowed in CREATE/MERGE",
        ));
    }
    for r in &p.rels {
        if r.var_length {
            return Err(Error::syntax(
                "CreatingVarLength: variable-length relationships cannot be created",
            ));
        }
        if create && r.direction == relate_ir::Direction::Both {
            return Err(Error::syntax(
                "RequiresDirectedRelationship: CREATE/MERGE requires a directed relationship",
            ));
        }
        if r.types.len() != 1
            && !r
                .type_expr
                .as_ref()
                .is_some_and(label_expr_contains_dynamic)
        {
            return Err(Error::syntax(
                "NoSingleRelationshipType: CREATE/MERGE requires exactly one relationship type",
            ));
        }
    }
    Ok(())
}

pub(crate) fn label_expr_contains_dynamic(expr: &relate_ir::LabelExpr) -> bool {
    match expr {
        relate_ir::LabelExpr::Dynamic { .. } => true,
        relate_ir::LabelExpr::Not(inner) => label_expr_contains_dynamic(inner),
        relate_ir::LabelExpr::And(items) | relate_ir::LabelExpr::Or(items) => {
            items.iter().any(label_expr_contains_dynamic)
        }
        relate_ir::LabelExpr::Label(_) | relate_ir::LabelExpr::Wildcard => false,
    }
}

/// Whether a path pattern references a dynamic name operand anywhere:
/// node label expressions and relationship type expressions, visited
/// through the IR's one post-rewrite payload walk — the compact
/// `nodes`/`rels` view authoritative whenever it is populated, the
/// lossless `elements` sequence for quantified/GPM shapes, never both
/// (walking both would double-visit payloads and read pre-fold twins).
/// Relationship where-clauses and property maps are `Expr`s the plan
/// walk in `plan_contains_dynamic_operands` already visits.
pub(crate) fn pattern_contains_dynamic_operands(pattern: &PathPattern) -> bool {
    any_pattern_payload(pattern, &mut |payload| match payload {
        PatternPayload::Node(node) => node
            .label_expr
            .as_ref()
            .is_some_and(label_expr_contains_dynamic),
        PatternPayload::Relationship(rel) => rel
            .type_expr
            .as_ref()
            .is_some_and(label_expr_contains_dynamic),
        // The group itself carries no name operand; the payloads nested
        // in it arrive as their own visits.
        PatternPayload::Quantified(_) => false,
    })
}

pub(crate) fn element_has_qpp(element: &PatternElement) -> bool {
    match element {
        PatternElement::Quantified(_) => true,
        PatternElement::Relationship(rel) => rel.quantified,
        PatternElement::Node(_) => false,
    }
}

/// Whether the element is a parenthesized quantified path pattern —
/// the one spelling that legitimizes adjacent node patterns
/// (juxtaposition, the InvalidNodePatternJuxtaposition gate).
/// Deliberately NARROWER than [`element_has_qpp`]: a postfix
/// quantified rel (`-[r*1..2]->`, `rel.quantified`) is var-length
/// traversal, not a quantified path group, and does not make
/// node-node juxtaposition legal.
fn element_is_quantified_path(element: &PatternElement) -> bool {
    matches!(element, PatternElement::Quantified(_))
}

/// 42N66: a `shortestPath()`/`allShortestPaths()` pattern may not traverse a
/// relationship variable the enclosing scope already binds ("Bound
/// relationships are not allowed in calls to the shortestPath() function").
/// A shadowable non-entity import is *fresh* for the binding, so it does not
/// count as bound — the binder's own answer
/// ([`crate::binding::effectively_bound`]).
pub(crate) fn check_shortest_path_bound_relationships(
    patterns: &[PathPattern],
    env: &crate::binding::Env,
    shadowable: &std::collections::HashSet<String>,
) -> Result<()> {
    for path in patterns {
        if !path.is_shortest_path_function {
            continue;
        }
        let bound = path
            .relationship_variables()
            .find(|name| crate::binding::effectively_bound(env, shadowable, name));
        if let Some(name) = bound {
            return Err(Error::syntax(format!(
                "VariableAlreadyBound: the relationship variable `{name}` is already bound; bound relationships are not allowed in calls to the shortestPath() function"
            )));
        }
    }
    Ok(())
}

/// The InvalidNodePatternJuxtaposition gate, as one named check: a bare
/// path pattern — no mode, no selector, no WHERE — with no parenthesized
/// quantified path and two adjacent node elements is the juxtaposition
/// spelling that requires a quantified connector. Pure PathPattern-shape
/// policy, owned here beside the other pattern-shape predicates; the
/// validate layer calls it per pattern.
pub(crate) fn check_juxtaposition(p: &PathPattern) -> Result<()> {
    if p.mode.is_none()
        && p.selector.is_none()
        && p.where_clause.is_none()
        && !p.elements.iter().any(element_is_quantified_path)
        && p.elements
            .windows(2)
            .any(|pair| matches!(pair, [PatternElement::Node(_), PatternElement::Node(_)]))
    {
        return Err(Error::syntax(
            "InvalidNodePatternJuxtaposition: adjacent node patterns require a quantified connector",
        ));
    }
    Ok(())
}

/// How a quantified element introduces a name: a parenthesized group
/// variable (CIP-28: no reuse anywhere), or the relationship variable of
/// a var-length relationship (the legacy `-[r*]->` family, which lowers
/// to the implicit-boundary quantified element, LABS-661 — its reuse
/// rules are the legacy ones: the same rel variable may repeat across
/// patterns, and a pre-bound list pins the exact trail).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupOrigin {
    Group,
    VarLengthRel,
}

/// Whether `name`, introduced with `origin`, may be bound by this MATCH:
/// a group variable must be fresh (not owned by another group, not
/// pre-bound outside); a var-length rel variable follows the legacy rules
/// (`bind_path`'s list reuse) — a group already owning the name or a
/// non-list pre-binding rejects it. The decision and its one error text
/// (`GroupVariableReuse`) live here, so every introduction site rejects
/// the same violation with the same message.
///
/// `seen` is the set of group-owned names — the only origin that
/// records; `GroupOrigin` selects the rule for this call, it is not a
/// stored fact.
fn name_is_legal(
    name: &SmallName,
    origin: GroupOrigin,
    outer: &Env,
    seen: &mut HashSet<SmallName>,
) -> Result<()> {
    match origin {
        // fresh for a group: no group owns it and nothing pre-binds it
        // outside — the claim is recorded and the name is legal
        GroupOrigin::Group if !seen.contains(name) && !outer.contains_name(name) => {
            seen.insert(name.clone());
            Ok(())
        }
        // legacy reuse for a var-length rel: no group owns it, and any
        // outer pre-binding is absent or a list
        GroupOrigin::VarLengthRel
            if !seen.contains(name)
                && matches!(outer.get_name(name), None | Some(Binding::List)) =>
        {
            Ok(())
        }
        _ => Err(Error::syntax(format!(
            "GroupVariableReuse: `{name}` is already bound"
        ))),
    }
}

pub(crate) fn collect_qpp_group_names(
    elements: &[PatternElement],
    seen: &mut HashSet<SmallName>,
    outer: &Env,
) -> Result<()> {
    for element in elements {
        match element {
            // the postfix quantifier spelling: the rel variable follows
            // the legacy rules
            PatternElement::Relationship(rel) if rel.quantified => {
                if let Some(name) = &rel.variable {
                    name_is_legal(name, GroupOrigin::VarLengthRel, outer, seen)?;
                }
            }
            PatternElement::Quantified(qpp) => {
                // A quantified element that IS a var-length relationship
                // (implicit boundary nodes, the inner rel one-hop — what
                // every `-[r*T*m..n]-` spelling lowers to) introduces its
                // rel variable under the legacy rules; any other
                // quantified shape introduces group variables.
                // the borrowed spelling: the name is only compared below,
                // so the value never needs to leave `qpp`'s borrow
                let var_length_rel = qpp
                    .implicit_var_length_rel()
                    .and_then(|rel| rel.variable.as_ref());
                // The local sweep below IS the whole policy for everything
                // named inside the quantified element: a nested quantified
                // pattern cannot occur (the parser's
                // NestedQuantification rejection refuses it at parse time; the
                // lowering quantifies top level only), so there is nothing
                // left to recurse into.
                // Document order, not per-run HashSet order: the first
                // offending name in source order is always the one the
                // error names. Borrowed — the names are only compared and
                // passed onward as &str, nothing outlives qpp's borrow
                // (the same spelling as var_length_rel).
                let mut local: Vec<&SmallName> = Vec::new();
                // the one push-if-absent: dedup preserving document order,
                // shared by every name-introduction site in the sweep
                fn remember<'a>(local: &mut Vec<&'a SmallName>, name: &'a SmallName) {
                    if !local.contains(&name) {
                        local.push(name);
                    }
                }
                for inner in &qpp.elements {
                    let name = match inner {
                        PatternElement::Node(node) => node.variable.as_ref(),
                        PatternElement::Relationship(rel) => rel.variable.as_ref(),
                        // out of contract, stated as an invariant: the
                        // parser's NestedQuantification rejection refuses a
                        // quantified element inside another, and the
                        // lowering never synthesizes one — a violation
                        // here is a parser/lowering bug, not a query
                        // condition
                        PatternElement::Quantified(_) => unreachable!(
                            "nested quantified pattern: parser rule 4 (NestedQuantification) \
                             rejects it at parse time"
                        ),
                    };
                    if let Some(name) = name {
                        remember(&mut local, name);
                    }
                }
                if let Some(name) = qpp.subpath_variable.as_ref() {
                    remember(&mut local, name);
                }
                for name in local {
                    let origin = if var_length_rel == Some(name) {
                        GroupOrigin::VarLengthRel
                    } else {
                        GroupOrigin::Group
                    };
                    name_is_legal(name, origin, outer, seen)?;
                }
            }
            PatternElement::Relationship(_) | PatternElement::Node(_) => {}
        }
    }
    Ok(())
}
