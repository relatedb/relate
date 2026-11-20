//! The subclause-expression policy family: which leaves a projection's
//! ORDER BY / WHERE subclauses may read once the projection aggregates —
//! an aggregate item, an explicit `GROUP BY`, or `DISTINCT` (whose items
//! are the grouping keys, ADR-0453) — judged by the one
//! [`projection_aggregates`](super::projection_aggregates) predicate.
//! Split from the grouping-key core (the LABS-311 size gate): the two
//! families share the leaf vocabulary, which stays in the parent. The
//! deep containment a subquery-bearing key needs is the IR's own
//! [`Expr::contains_deep`](relate_ir::Expr::contains_deep).

use relate_core::{Error, Result};
use relate_ir::{is_agg_call, is_agg_expr, Expr, OrderItem, Projection};

use super::{
    collect_nested_scope_leaves, collect_non_agg_leaves, grouping_keys, leaf_allowed_as_group,
    leaf_root_name, projection_aggregates, Imports,
};
use crate::binding::Env;

/// One clause's aggregating-projection facts, built once at the clause
/// boundary and threaded through the subclause checks: the projection's
/// items and explicit keys, the CALL-subquery imports, the incoming
/// scope, and the DISTINCT flag — with the derived grouping keys and
/// output names computed once in the constructor and the aggregating
/// gate ([`projection_aggregates`]) carried beside them, so no check
/// re-derives a clause-level fact.
pub(crate) struct SubclauseScope<'a> {
    items: &'a [Projection],
    group_by: Option<&'a [Expr]>,
    imports: &'a Imports,
    outer_names: &'a Env,
    /// Whether the clause aggregates — an aggregate item, an explicit
    /// `GROUP BY`, or `DISTINCT` (whose items are the grouping keys,
    /// ADR-0453) — the one gate, decided once.
    aggregating: bool,
    /// The clause's grouping keys, derived once.
    group_keys: Vec<&'a Expr>,
    /// The projection's output names, derived once.
    names: Vec<String>,
}

impl<'a> SubclauseScope<'a> {
    pub(crate) fn new(
        items: &'a [Projection],
        group_by: Option<&'a [Expr]>,
        distinct: bool,
        imports: &'a Imports,
        outer_names: &'a Env,
    ) -> Self {
        Self {
            items,
            group_by,
            imports,
            outer_names,
            aggregating: projection_aggregates(items, group_by, distinct),
            group_keys: grouping_keys(items, group_by),
            names: output_names(items),
        }
    }

    /// The one aggregating gate, decided once in the constructor.
    pub(crate) fn aggregating(&self) -> bool {
        self.aggregating
    }
}

/// The one aggregating-context gate a projection's subclauses pass
/// through (GroupByAcceptance [20], [68], [70], [91], [92]; ADR-0453):
/// once a projection aggregates — an aggregate item, an explicit
/// `GROUP BY`, or `DISTINCT`, judged by the one [`projection_aggregates`]
/// predicate (a DISTINCT projection's items are its grouping keys) — a
/// non-aggregating ORDER BY / WHERE expression may only read grouping
/// keys and projected names. An aggregating subclause expression keeps
/// its own discipline ([`reject_order_by_agg`] /
/// [`check_subclause_agg_leaves`]), and a restated projection item is
/// that column ([`is_projected_item`]), so both are left to their
/// owners.
pub(crate) fn check_aggregating_subclause(
    scope: &SubclauseScope<'_>,
    expr: &Expr,
    context: &str,
) -> Result<()> {
    if !scope.aggregating || is_agg_expr(expr) || is_projected_item(scope.items, expr) {
        return Ok(());
    }
    check_post_projection_leaves(scope, expr, context)
}

/// The post-projection leaf discipline an aggregating projection's ORDER BY
/// and WHERE apply (GroupByAcceptance [20], [68], [70], [91], [92]): every
/// variable/property leaf outside an aggregate call must be
///
/// - verbatim a grouping-key expression (`ORDER BY a.p` under
///   `GROUP BY a.p`, [12]-[14]),
/// - a property of a bare-variable grouping key (`a.p` under `GROUP BY a`,
///   [18]/[22]/[37]), or
/// - a projected output name (an alias, [17]/[76]/[90]).
///
/// Any other reference is inaccessible: Neo4j reports 42N44 for `ORDER BY
/// a.q` beside `GROUP BY a.p`, and for the variables of a complex key
/// (`a.p + 0`, `toInteger(b)`) used outside the key expression itself
/// ([68]/[70]). Under implicit grouping (no `GROUP BY` clause) a complex
/// projected key stays referenceable as a sub-expression of itself —
/// deprecated, but accepted ([67]/[69]).
pub(crate) fn check_post_projection_leaves(
    scope: &SubclauseScope<'_>,
    expr: &Expr,
    context: &str,
) -> Result<()> {
    let mut leaves = Vec::new();
    collect_nested_scope_leaves(expr, &mut leaves);
    for leaf in &leaves {
        // A name the subclause's own nested scopes bind (`z` in `EXISTS {
        // WITH x + 1 AS z RETURN z }`) is not a read of this clause's row.
        if leaf_root_name(leaf)
            .is_some_and(|name| !name.with_str(|text| scope.outer_names.contains_key(text)))
        {
            continue;
        }
        if leaf_allowed_in_subclause(leaf, &scope.group_keys, &scope.names, scope.imports) {
            continue;
        }
        // Implicit grouping keeps the deprecated sub-expression resolution:
        // a leaf inside a *complex* key is the key's own value. The key may
        // contain a subquery expression (ProjectionSubclauseDeprecation
        // [51]/[61]), so containment descends into subquery bodies — but a
        // bare subquery-expression key is not a "complex projection item
        // expression" and stays unrecognized ([34]/[48]/[53]).
        if scope.group_by.is_none()
            && scope
                .group_keys
                .iter()
                .filter(|key| key.subquery_body().is_none())
                .any(|key| key.contains_deep(leaf) && expr.contains_deep(key))
        {
            continue;
        }
        return Err(Error::syntax(format!(
            "UndefinedVariable: {context} refers to a non-projecting variable"
        )));
    }
    Ok(())
}

/// Whether `haystack` contains `needle` as itself or as a subexpression —
/// [`Expr::any`] tests the node itself before its descendants, so one
/// pre-order call is the whole answer. Deliberately *not*
/// [`Expr::contains_deep`]: the projected-item decomposition below is a
/// same-scope fact — a projection item's expression and the subclause
/// stand in one clause scope, while a subquery body carries its own — so
/// an inner body's equal sub-expression must not satisfy an outer-scope
/// question it cannot answer.
fn subexpr_contains(haystack: &Expr, needle: &Expr) -> bool {
    haystack.any(&mut |e| e == needle)
}

pub(crate) fn reject_agg_in_pred(expr: &Expr) -> Result<()> {
    if is_agg_expr(expr) {
        return Err(Error::syntax(
            "InvalidAggregation: aggregations are not allowed in WHERE",
        ));
    }
    Ok(())
}

/// The projection's output names, in item order — the one derivation the
/// leaf checks and the filtered name lists read, beside
/// [`is_projected_item`]. The index stays the item's own, so a list
/// filtered over these names keeps every synthetic `column_<idx>` name
/// stable.
pub(crate) fn output_names(items: &[Projection]) -> Vec<String> {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| item.output_name(i))
        .collect()
}

/// ADR-0453: the one leaf-discipline check an aggregating subclause
/// expression passes through. Aggregate calls recompute over the group, so
/// anything inside them is the aggregation's own input; every leaf a
/// subclause expression references *outside* its aggregate calls must be a
/// grouping key or a projected output name (an aggregating item's alias
/// included — `ORDER BY s + sum(a.p)` beside `sum(a.p) AS s` reads the
/// projected `s`). `imports` is the caller's import set — the demoted one
/// for a `WITH`'s aggregating keys (an import stays a per-row constant
/// only while the projection still carries it; the non-aggregating
/// subclauses read the clause's own set off the scope). `context` names
/// the subclause in the rejection.
pub(crate) fn check_subclause_agg_leaves(
    scope: &SubclauseScope<'_>,
    expr: &Expr,
    imports: &Imports,
    context: &str,
) -> Result<()> {
    // ADR-0453 ([30]/[32] `sum(c) + y` beside `1 AS y`): a reference to a
    // name the same clause declares as a CONSTANT is the rejected
    // same-clause reference. An item derived from outer variables (`me.age
    // AS age`, ReturnOrderBy6 [2]; `a.p AS a`, ProjectionSubclauseDeprecation
    // [77]/[78]) introduces a grouping key, and its alias stays referenceable.
    // An aggregating item's alias names the projected aggregate — also never
    // a same-clause variable reference ([68]/[75] `ORDER BY s + sum(a.p)`
    // beside `sum(a.p) AS s`).
    // `any` tests the item itself before its descendants, so it already
    // covers a bare `Expr::Variable` item: a pass-through (`a`, `a AS a`)
    // re-binds nothing.
    let reintroduced: Vec<String> = scope
        .items
        .iter()
        .zip(&scope.names)
        .filter(|(item, _)| {
            !is_agg_expr(&item.expr) && !item.expr.any(&mut |e| matches!(e, Expr::Variable(_)))
        })
        .map(|(_, name)| name.clone())
        .collect();
    let mut leaves = Vec::new();
    collect_non_agg_leaves(expr, &mut leaves);
    for leaf in &leaves {
        if matches!(leaf, Expr::Variable(v) if v.with_str(|t| reintroduced.iter().any(|r| r == t)))
        {
            return Err(same_clause_ref(context));
        }
        // The one leaf predicate ([`leaf_allowed_in_subclause`]: the
        // grouping-key answer, an output name, a star-covered property).
        if !leaf_allowed_in_subclause(leaf, &scope.group_keys, &scope.names, imports)
            // A leaf inside a projection item's *expression* is part of that
            // grouping key: `RETURN me.age + you.age … ORDER BY me.age +
            // you.age + count(*)` (ReturnOrderBy6 [5]) decomposes the
            // projected expression, which Neo4j allows.
            && !scope
                .items
                .iter()
                .any(|item| subexpr_contains(&item.expr, leaf) && subexpr_contains(expr, &item.expr))
        {
            return Err(Error::syntax(format!(
                "UndefinedVariable: {context} refers to a non-projected expression"
            )));
        }
    }
    check_same_clause_refs(scope.items, expr, &reintroduced, context)?;
    Ok(())
}

/// The aggregate calls anywhere in `expr`, in pre-order — the one
/// collection through [`is_agg_call`], so the same-clause rules never
/// re-spell the vocabulary match. Aggregate aliases are canonical to their
/// lowered name before the planner sees them
/// (`relate-front`'s `lowered_alias_name`), so the canonical vocabulary is
/// the complete one here.
fn aggregate_calls(expr: &Expr) -> Vec<&Expr> {
    fn walk<'a>(expr: &'a Expr, calls: &mut Vec<&'a Expr>) {
        if is_agg_call(expr) {
            calls.push(expr);
        }
        expr.each_child(&mut |child| walk(child, calls));
    }
    let mut calls = Vec::new();
    walk(expr, &mut calls);
    calls
}

/// The one aggregate-call reference rejection the same-clause rules share
/// (ADR-0453): a subclause expression that is verbatim a projection item
/// is that item, not a same-clause reference; otherwise the first
/// aggregate call whose variables reference one of `names` raises
/// `error()`. Each caller derives its own name list and error text.
fn reject_call_referencing(
    items: &[Projection],
    expr: &Expr,
    names: &[String],
    error: impl Fn() -> Error,
) -> Result<()> {
    // A subclause expression that *is* a projection item is that item, not
    // a subclause reference ([20]-[23]: `ORDER BY count(a)` beside
    // `count(a) AS cnt` with `1 AS a` re-introducing `a`).
    if is_projected_item(items, expr) {
        return Ok(());
    }
    for call in aggregate_calls(expr) {
        if call.any(&mut |node| {
            matches!(node, Expr::Variable(v)
                if v.with_str(|text| names.iter().any(|name| name == text)))
        }) {
            return Err(error());
        }
    }
    Ok(())
}

/// The same-clause-reference rejection ([`check_same_clause_refs`] and its
/// leaf loop). One construction: both sites spell the same policy.
fn same_clause_ref(context: &str) -> Error {
    Error::syntax(format!(
        "InvalidAggregation: {context} cannot reference variables declared in the same clause"
    ))
}

/// ADR-0453 (AggregationsInSubclauses [24]-[33]): an aggregation
/// sub-expression of a subclause may not reference a variable the SAME
/// clause re-introduces — `WITH 1 AS a, … ORDER BY sum(a)` is ambiguous
/// between the incoming `a` and the new one. A pass-through item (`a`,
/// `a AS a`) re-binds nothing, and an aggregation call that is verbatim a
/// projection item is that item, not a subclause reference ([20]-[23]).
fn check_same_clause_refs(
    items: &[Projection],
    expr: &Expr,
    reintroduced: &[String],
    context: &str,
) -> Result<()> {
    reject_call_referencing(items, expr, reintroduced, || same_clause_ref(context))
}

/// Whether `expr` is verbatim one of the clause's projected items — the
/// restated-projection exemption the ORDER BY checks share (a subclause may
/// restate a projected expression, and under DISTINCT that restatement
/// resolves against the projected expressions). The `RETURN *` / `WITH *`
/// item lowers to `Expr::Star`, a marker that no value expression being
/// checked can equal, so the star needs no separate arm here.
pub(crate) fn is_projected_item(items: &[Projection], expr: &Expr) -> bool {
    items.iter().any(|item| item.expr == *expr)
}

/// ADR-0453's ORDER BY aggregation gate: aggregation in an ORDER BY key
/// is legal only when the clause aggregates ([`SubclauseScope`]'s one
/// gate), and each aggregating key then passes the same-clause shadowing
/// check and the leaf discipline — `imports` is the caller's set: the
/// demoted one for a `WITH` (an import stays a per-row constant only
/// while the projection still carries it), the clause's own for a
/// `RETURN`.
pub(crate) fn reject_order_by_agg(
    scope: &SubclauseScope<'_>,
    order: &[OrderItem],
    imports: &Imports,
) -> Result<()> {
    let order_has_agg = order.iter().any(|o| is_agg_expr(&o.expr));
    if order_has_agg && !scope.aggregating {
        return Err(Error::syntax(
            "InvalidAggregation: aggregation functions are not allowed in ORDER BY when clause has no aggregation",
        ));
    }
    for o in order {
        if !is_agg_expr(&o.expr) {
            continue;
        }
        reject_shadowed_aggregate_argument(scope, &o.expr)?;
        check_subclause_agg_leaves(scope, &o.expr, imports, "ORDER BY aggregation")?;
    }
    Ok(())
}

/// ADR-0453's WHERE twin of [`reject_order_by_agg`]: after an aggregating
/// (or DISTINCT) `WITH`, a WHERE predicate may aggregate — HAVING
/// semantics, recomputed per group; any other WHERE keeps the
/// InvalidAggregation rejection. An aggregating predicate reads the
/// demoted import set; a non-aggregating one passes the scope's leaf
/// discipline over its own import set (an enclosing import stays a
/// per-row constant for the non-aggregating subclauses even when this
/// projection drops the name).
pub(crate) fn check_where_subclause(
    scope: &SubclauseScope<'_>,
    demoted_imports: &Imports,
    w: &Expr,
) -> Result<()> {
    if is_agg_expr(w) {
        if !scope.aggregating() {
            return reject_agg_in_pred(w);
        }
        check_subclause_agg_leaves(scope, w, demoted_imports, "WHERE aggregation")
    } else {
        check_aggregating_subclause(scope, w, "WHERE")
    }
}

/// An ORDER BY aggregation whose argument names another item's *aggregate*
/// output (`RETURN sum(a) AS a … ORDER BY sum(a) + 1`) reads a projected
/// aggregate as its own input — Neo4j rejects the same-clause shadowing with
/// 42001 SyntaxError. Only variables inside the aggregate calls are tested:
/// a projected aggregate referenced elsewhere in the key (`ORDER BY a + 1`)
/// is an ordinary read of that column.
fn reject_shadowed_aggregate_argument(scope: &SubclauseScope<'_>, expr: &Expr) -> Result<()> {
    // A subclause expression that *is* a projection item is that item, not a
    // same-clause reference (`RETURN sum(a) AS a ORDER BY sum(a)` is legal;
    // live 2026.10 rejects only an aggregate embedded in a larger key).
    let aggregate_aliases: Vec<String> = scope
        .items
        .iter()
        .zip(&scope.names)
        .filter(|(item, _)| is_agg_expr(&item.expr))
        .map(|(_, name)| name.clone())
        .collect();
    reject_call_referencing(scope.items, expr, &aggregate_aliases, || {
        Error::syntax(
            "InvalidAggregation: ORDER BY aggregation references an aggregate \
             projected in the same clause",
        )
    })
}

/// The subclause leaf predicate — [`leaf_allowed_as_group`]'s grouping-key
/// answer (an exact key, a `*` key over a variable leaf, a CALL-subquery
/// import, a property of a bare-variable key) plus the two deltas a
/// subclause's own row adds, stated once here: a leaf resolves through a
/// projected output name (an alias — GroupByAcceptance [17]/[76]/[90]),
/// and a `*` key covers property leaves too, because the star carried the
/// whole variable through the projection (ADR-0198 ruling 1) — the
/// item-grouping walk keeps its variables-only star, its own doc states
/// why. A leaf whose shape names no resolvable base (a chained property
/// `a.p.q`) stays admitted only by the exact-key arm — it fails closed,
/// never by default.
fn leaf_allowed_in_subclause(
    leaf: &Expr,
    group_keys: &[&Expr],
    output_names: &[String],
    imports: &Imports,
) -> bool {
    if leaf_allowed_as_group(leaf, group_keys, imports) {
        return true;
    }
    let Some(base) = leaf_root_name(leaf) else {
        return false;
    };
    base.with_str(|text| output_names.iter().any(|name| name == text))
        || (matches!(leaf, Expr::Property { .. } | Expr::DynamicProperty { .. })
            && group_keys.iter().any(|key| matches!(key, Expr::Star)))
}
