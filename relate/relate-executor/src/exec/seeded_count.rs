//! The seeded body's names (ADR-0689): a `CALL (x) { … }` body invocation
//! carries its imports in one seed row, so a dynamic label or
//! relationship-type operand (`MATCH (n:$(x))`) is a known string for the
//! whole invocation. [`resolve_seed_names`] rewrites those operands, and
//! the static label tests the `WHERE n:$(x)` spelling folds into, to
//! static names before the clause loop runs. A resolved pattern is the
//! pattern the engine already serves best, in tier order:
//!
//! 1. the maintained counts, when the body is a count-only shape and the
//!    view is exact ([`seeded_count_result`], the ADR-0387 recognizer);
//! 2. otherwise the label index and the typed adjacency, the same scan a
//!    hand-written static label takes, instead of an all-nodes scan with a
//!    per-node label test.
//!
//! No other label-presence structure exists to consult: the label index is
//! an upper-bound set (deleted ids stay until checkpoint GC), so it never
//! answers a count by size. Every operand this module cannot resolve stays
//! dynamic and the scan evaluates it.

use relate_core::Result;
use relate_ir::{Clause, DynamicLabelQuantifier, Expr, LabelExpr, NodePattern, PathPattern};
use relate_names::{unresolved, NamedToken, SmallName};
use std::borrow::Cow;

use crate::ctx::ExecCtx;
use crate::eval::dynamic_label_names;
use crate::row::Row;

use super::count_stats::count_only_stats_result;
use super::shortcut_common::match_return_pattern;
use super::QueryResult;

/// The names a dynamic operand evaluates to under `seed`, or `None` when
/// the operand is not dynamic or its evaluation fails (the scan reproduces
/// that failure only for a row it actually evaluates, so the operand stays
/// dynamic and an empty store keeps answering as it always did).
fn evaluated_names(ctx: ExecCtx<'_>, seed: &Row, expr: &LabelExpr) -> Option<Vec<String>> {
    let LabelExpr::Dynamic { expr, quantifier } = expr else {
        return None;
    };
    // `Any` over several names is a disjunction a conjunction cannot spell.
    let names = dynamic_label_names(ctx.graph, seed, ctx.params, expr).ok()?;
    if *quantifier == DynamicLabelQuantifier::Any && names.len() != 1 {
        return None;
    }
    Some(names)
}

/// The static label expression for resolved `names`: one label, or the
/// conjunction `n:A:B` the front lowers a static multi-label to.
fn static_expr(names: &[String]) -> LabelExpr {
    match names {
        [one] => LabelExpr::Label(SmallName::from_name(one)),
        many => LabelExpr::And(
            many.iter()
                .map(|name| LabelExpr::Label(SmallName::from_name(name)))
                .collect(),
        ),
    }
}

/// Resolve a node's dynamic label operand in place; `true` when changed.
fn resolve_node(ctx: ExecCtx<'_>, seed: &Row, node: &mut NodePattern) -> bool {
    let Some(expr) = &node.label_expr else {
        return false;
    };
    let Some(names) = evaluated_names(ctx, seed, expr) else {
        return false;
    };
    set_labels(node, labels_of(&names), static_expr(&names));
    true
}

fn labels_of(names: &[String]) -> Vec<NamedToken> {
    names
        .iter()
        .map(|name| unresolved(SmallName::from_name(name)))
        .collect()
}

fn set_labels(node: &mut NodePattern, labels: Vec<NamedToken>, expr: LabelExpr) {
    node.labels = labels;
    node.label_expr = Some(expr);
}

/// The labels a static conjunction names, or `None` for any other shape —
/// total over the expression, no fabricated empty list.
fn static_labels(expr: &LabelExpr) -> Option<Vec<NamedToken>> {
    match expr {
        LabelExpr::Label(name) => Some(vec![unresolved(name.clone())]),
        LabelExpr::And(items) => {
            let mut labels = Vec::new();
            for item in items {
                labels.extend(static_labels(item)?);
            }
            (!labels.is_empty()).then_some(labels)
        }
        LabelExpr::Or(_) | LabelExpr::Not(_) | LabelExpr::Wildcard | LabelExpr::Dynamic { .. } => {
            None
        }
    }
}

/// `WHERE n:$(x)` (or its static spelling) over a single unlabeled node
/// folds into the node's own label requirement, the form the label index
/// and the count recognizer serve. Eligible only for the compact pattern
/// shape the name rewrite also visits (`!needs_gpm()`): a GPM-shaped
/// pattern keeps its `WHERE`. Returns whether the clause changed.
fn fold_where_label(
    ctx: ExecCtx<'_>,
    seed: &Row,
    patterns: &mut [PathPattern],
    where_clause: &mut Option<Expr>,
) -> bool {
    let (
        [pattern],
        Some(Expr::LabelPredicate {
            expr, label_expr, ..
        }),
    ) = (patterns, &*where_clause)
    else {
        return false;
    };
    if pattern.needs_gpm() {
        return false;
    }
    let ([node], true, Expr::Variable(var)) =
        (&mut pattern.nodes[..], pattern.rels.is_empty(), &**expr)
    else {
        return false;
    };
    if node.variable.as_ref() != Some(var) || node.label_expr.is_some() || !node.labels.is_empty() {
        return false;
    }
    let (labels, folded) = match label_expr {
        LabelExpr::Dynamic { .. } => {
            let Some(names) = evaluated_names(ctx, seed, label_expr) else {
                return false;
            };
            (labels_of(&names), static_expr(&names))
        }
        static_form => {
            let Some(labels) = static_labels(static_form) else {
                return false;
            };
            (labels, static_form.clone())
        }
    };
    set_labels(node, labels, folded);
    *where_clause = None;
    true
}

/// Whether `clause` holds anything [`resolve_clause`] could rewrite: a
/// top-level dynamic operand on a compact pattern's node or relationship,
/// or a label test in the `WHERE`. The cheap gate that keeps a clause with
/// nothing to resolve from being copied.
fn may_resolve(clause: &Clause) -> bool {
    let Clause::Match {
        patterns,
        where_clause,
        ..
    } = clause
    else {
        return false;
    };
    let dynamic = |expr: &Option<LabelExpr>| matches!(expr, Some(LabelExpr::Dynamic { .. }));
    matches!(where_clause, Some(Expr::LabelPredicate { .. }))
        || patterns.iter().filter(|p| !p.needs_gpm()).any(|p| {
            p.nodes.iter().any(|n| dynamic(&n.label_expr))
                || p.rels.iter().any(|r| dynamic(&r.type_expr))
        })
}

/// One `MATCH` clause with every resolvable dynamic operand replaced, or
/// `None` when nothing in it changed (so the caller never copies a clause
/// it keeps).
fn resolve_clause(ctx: ExecCtx<'_>, seed: &Row, clause: &Clause) -> Option<Clause> {
    if !may_resolve(clause) {
        return None;
    }
    let mut resolved = clause.clone();
    let Clause::Match {
        patterns,
        where_clause,
        ..
    } = &mut resolved
    else {
        return None;
    };
    let mut changed = false;
    for pattern in patterns.iter_mut().filter(|p| !p.needs_gpm()) {
        for node in &mut pattern.nodes {
            changed |= resolve_node(ctx, seed, node);
        }
        for rel in &mut pattern.rels {
            let Some(expr) = &rel.type_expr else { continue };
            // A relationship has one type: only a single name resolves.
            if let Some([name]) = evaluated_names(ctx, seed, expr).as_deref() {
                rel.types = vec![unresolved(SmallName::from_name(name))];
                rel.type_expr = Some(LabelExpr::Label(SmallName::from_name(name)));
                changed = true;
            }
        }
    }
    changed |= fold_where_label(ctx, seed, patterns, where_clause);
    changed.then_some(resolved)
}

/// The statement with every resolvable dynamic node-label and
/// relationship-type operand replaced by its static name under the seed
/// row `seed`, over the leading run of `MATCH` clauses only; borrowed
/// unchanged when nothing resolves. Only plain
/// compact patterns are rewritten: a GPM-shaped pattern (quantified,
/// path-mode, WHERE-bearing) keeps its dynamic operands and its `WHERE`,
/// and the scan evaluates them.
pub(super) fn resolve_seed_names<'a>(
    ctx: ExecCtx<'_>,
    clauses: &'a [Clause],
    seed: &Row,
) -> Cow<'a, [Clause]> {
    let mut out = Cow::Borrowed(clauses);
    for (at, clause) in clauses.iter().enumerate() {
        // The seed row is the body's bindings only as it enters: the first
        // clause that is not a `MATCH` (a `WITH`, `UNWIND`, `CALL`, …) can
        // rebind an imported name, so nothing after it resolves from the
        // seed. A `MATCH` cannot: re-declaring a seed variable in a
        // pattern reads the bound entity.
        if !matches!(clause, Clause::Match { .. }) {
            break;
        }
        if let Some(resolved) = resolve_clause(ctx, seed, clause) {
            out.to_mut()[at] = resolved;
        }
    }
    out
}

/// Answer a count-only body from the statistics under one non-empty seed
/// row whose dynamic names were already resolved (the caller's gate holds
/// the exactness window and the one-row seed). `Ok(None)` declines: the
/// clause loop's label-index scan answers. A pattern variable the seed
/// already binds reads the bound entity, not the population, so it
/// declines too.
pub(super) fn seeded_count_result(
    ctx: ExecCtx<'_>,
    clauses: &[Clause],
    seed: &Row,
) -> Result<Option<(Vec<Row>, QueryResult)>> {
    let Some(pattern) = match_return_pattern(clauses) else {
        return Ok(None);
    };
    let binds_seed = pattern
        .nodes
        .iter()
        .filter_map(|n| n.variable.as_ref())
        .chain(pattern.rels.iter().filter_map(|r| r.variable.as_ref()))
        .any(|v| v.with_str(|name| seed.contains_key(name)));
    if binds_seed {
        return Ok(None);
    }
    count_only_stats_result(ctx.graph, clauses)
}
