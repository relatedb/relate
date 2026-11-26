//! `ORDER BY id(n)` over the whole node population is already satisfied by
//! the scan: a plain `MATCH (n)` walks nodes in ascending id order, so the
//! sort is a no-op that still costs a full scan plus a top-k pass. Dropping
//! it leaves a plain `SKIP`/`LIMIT` page, which the executor pages eagerly
//! and stops early.
//!
//! Deliberately narrow: only ascending `id(n)` (never `elementId(n)`, whose
//! string order puts `n10` before `n2`), only a node scan (a relationship
//! scan walks adjacency, in source-node order, not id order), and only the
//! two-clause `[MATCH, RETURN]` statement whose `MATCH` carries no label,
//! property, predicate, hint or `SEARCH`.

use relate_ir::{is_agg_expr, Clause, Expr, MatchMode, OrderItem, PathPattern, Projection};
use relate_names::SmallName;

/// Clear the `ORDER BY` of a `[MATCH (n), RETURN … ORDER BY id(n)]`
/// statement; every other shape is left untouched.
pub(crate) fn drop_id_order_over_node_scan(clauses: &mut [Clause]) {
    let [matching, Clause::Return {
        distinct: false,
        group_by: None,
        items,
        order,
        ..
    }] = clauses
    else {
        return;
    };
    let Some(var) = whole_population_node_var(matching) else {
        return;
    };
    let [OrderItem {
        expr,
        ascending: true,
    }] = &order[..]
    else {
        return;
    };
    if !is_id_of(expr, var)
        || items.iter().any(|item| is_agg_expr(&item.expr))
        || items
            .iter()
            .enumerate()
            .any(|(index, item)| shadows_var(item, index, var))
    {
        return;
    }
    order.clear();
}

/// The variable of a plain, whole-population, single-node `MATCH`.
fn whole_population_node_var(clause: &Clause) -> Option<&SmallName> {
    let Clause::Match {
        optional: false,
        match_mode: MatchMode::DifferentRelationships,
        patterns,
        where_clause: None,
        using_index,
        using_join,
        search: None,
    } = clause
    else {
        return None;
    };
    if !using_index.is_empty() || !using_join.is_empty() {
        return None;
    }
    let [pattern] = &patterns[..] else {
        return None;
    };
    single_plain_node(pattern)
}

fn single_plain_node(pattern: &PathPattern) -> Option<&SmallName> {
    if pattern.needs_gpm() || pattern.variable.is_some() || !pattern.rels.is_empty() {
        return None;
    }
    let [node] = &pattern.nodes[..] else {
        return None;
    };
    let plain = node.labels.is_empty()
        && node.label_expr.is_none()
        && node.properties.is_none()
        && node.where_clause.is_none()
        && !node.force_scan;
    node.variable.as_ref().filter(|_| plain)
}

/// Whether `expr` is exactly `id(var)`.
fn is_id_of(expr: &Expr, var: &SmallName) -> bool {
    match expr {
        Expr::Function {
            name,
            distinct: false,
            args,
        } if name.with_str(|text| text.eq_ignore_ascii_case("id")) => {
            matches!(&args[..], [Expr::Variable(arg)] if arg == var)
        }
        _ => false,
    }
}

/// Whether a projection item names a column `var` over an expression that
/// is not the scanned node itself, so `ORDER BY id(n)` would read that
/// column and not the node. The column name is the projection's own
/// ([`Projection::output_name`]: an alias, else the expression's canonical
/// name), never re-derived here.
fn shadows_var(item: &Projection, index: usize, var: &SmallName) -> bool {
    item.output_name(index) == var.to_string()
        && !matches!(&item.expr, Expr::Variable(v) if v == var)
}
