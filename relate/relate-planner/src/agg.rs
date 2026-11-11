//! Aggregation checks: grouping-key discipline for projections and the
//! ORDER BY / WHERE aggregation restrictions.

use std::collections::HashSet;

use relate_core::{Error, Result};
use relate_names::SmallName;
// The aggregate-name set is owned by `relate-ir` and re-exported by
// `relate-front::agg` (which adds the CIP-128 alias spellings). The
// planner binds against this single definition instead of keeping a local
// copy.
use relate_front::agg::is_aggregate_name as is_agg_fn;
use relate_ir::{
    is_agg_call, is_agg_expr, walk_scope_children, ChildScope, Expr, MapProjectionItem, OrderItem,
    Projection, Query,
};

/// ADR-0453: the names a CALL subquery imports (`CALL (tax) { ... }`), for
/// which the grouping-key discipline is relaxed — an import is a per-row
/// constant in the subquery's scope, so an aggregation expression may
/// reference it without it being a grouping key ("should not group by a
/// value imported into a CALL subquery"). Empty at every other scope.
pub(crate) type Imports = std::collections::HashSet<String>;

// `outer_names` (every aggregation-check fn below) is the incoming
// scope's binding environment: the checks read its keys as the scope's
// name set (membership, length, iteration), never a materialized
// per-clause clone of them.
pub(crate) fn check_agg_projections(
    items: &[Projection],
    group_by: Option<&[Expr]>,
    imports: &Imports,
    outer_names: &crate::binding::Env,
) -> Result<()> {
    // A `*` projection item is only a grouping key under implicit grouping
    // or `GROUP BY ALL` — the latter lowers the star into the key list,
    // where the executor expands it to the carried variables. With an
    // explicit key list (`GROUP BY k, ...`) or the empty grouping set
    // (`GROUP BY ()`) the star is not among the keys, and its carried
    // non-key variables would silently project first-row-of-group values
    // (ADR-0198 *Later references*). This check must run before the
    // no-aggregates early return below: an explicit GROUP BY without
    // aggregate items still takes the aggregation path (it acts as
    // DISTINCT), so `WITH *, n GROUP BY n` is the same wrong-answer hole.
    //
    // An *aggregating* projection whose explicit key list names exactly the
    // variables `*` expands to is the same grouping, so the star stays legal
    // there (GroupByAcceptance [52] `RETURN *, count(*) AS cnt GROUP BY a, b`);
    // a key list missing one of them, or built from expressions, is not. The
    // zero-aggregate form stays rejected: it takes the DISTINCT path and
    // Relate pins the rejection (CollectListConcat s23 `WITH *, n GROUP
    // BY n`).
    if let Some(keys) = group_by {
        let star_item = items.iter().any(|i| matches!(i.expr, Expr::Star));
        if star_item && !keys.iter().any(|k| matches!(k, Expr::Star)) {
            let key_names: Option<std::collections::HashSet<String>> = keys
                .iter()
                .map(|k| match k {
                    Expr::Variable(v) => Some(v.to_string()),
                    _ => None,
                })
                .collect();
            let covers_scope = items.iter().any(|i| is_agg_expr(&i.expr))
                && !keys.is_empty()
                && key_names.is_some_and(|names| {
                    names.len() == outer_names.len()
                        && outer_names.keys().all(|s| names.contains(s))
                });
            if !covers_scope {
                return Err(Error::syntax(
                    "AmbiguousAggregationExpression: `*` projection is not allowed with an explicit GROUP BY",
                ));
            }
        }
    }
    // A projection is an aggregating context when it carries an aggregate
    // item or an explicit `GROUP BY` (which aggregates even with no
    // aggregate items: it acts as DISTINCT). A plain projection carries no
    // grouping discipline.
    if !items.iter().any(|i| is_agg_expr(&i.expr)) && group_by.is_none() {
        return Ok(());
    }
    let group_keys = grouping_keys(items, group_by);
    for item in items {
        // Aggregating items carry the discipline everywhere; a
        // non-aggregating item only needs it when an explicit key list
        // governs the clause. Under implicit grouping and `GROUP BY ALL`
        // the non-aggregating items *are* the keys, and an item that is
        // verbatim one of the keys is that key, however complex
        // (GroupByAcceptance [32]).
        if !is_agg_expr(&item.expr)
            && (group_by.is_none()
                // A subquery expression's body has its own scope (imports,
                // shadowing bindings); its grouping discipline is the
                // subquery's own, not this projection's (GroupByAcceptance
                // [28]-[31]).
                || contains_subquery_expression(&item.expr)
                || group_keys
                    .iter()
                    .any(|key| **key == item.expr || key_is_property_of(key, &item.expr)))
        {
            continue;
        }
        check_item_grouping(&item.expr, &group_keys, imports, outer_names)?;
    }
    Ok(())
}

/// Whether the expression contains an expression-embedded subquery
/// (`COUNT`/`COLLECT`/`EXISTS { … }`) at any position.
fn contains_subquery_expression(expr: &Expr) -> bool {
    expr.any(&mut |node| {
        matches!(
            node,
            Expr::CountSubquery { .. } | Expr::CollectSubquery { .. } | Expr::ExistsSubquery { .. }
        )
    })
}

/// GBN1.5 alias resolution: the aliased projection item becomes the base of
/// the resolved grouping key (`a.p AS a` with `GROUP BY a.p` lowers to the
/// key `(a.p).p`), so the item is that key's own sub-expression and stays
/// projectable — GroupByAcceptance [46] evaluates the key and reports the
/// type error, not a grouping-key rejection.
fn key_is_property_of(key: &Expr, item: &Expr) -> bool {
    matches!(key, Expr::Property { expr, .. } if **expr == *item)
}

/// The grouping-key expressions of a projection clause: the explicit key
/// list when the clause has one (`GROUP BY ALL` lowers its non-aggregating
/// items into that list; `GROUP BY ()` is an empty list), else the inferred
/// non-aggregating items.
fn grouping_keys<'a>(items: &'a [Projection], group_by: Option<&'a [Expr]>) -> Vec<&'a Expr> {
    match group_by {
        Some(keys) => keys.iter().collect(),
        None => items
            .iter()
            .filter(|i| !is_agg_expr(&i.expr))
            .map(|i| &i.expr)
            .collect(),
    }
}

/// One projection item's grouping discipline: every variable/property leaf
/// it reads outside its aggregate calls must be a grouping key (or an
/// allowed property of one), including leaves read inside a nested
/// per-element or subquery body.
fn check_item_grouping(
    expr: &Expr,
    group_keys: &[&Expr],
    imports: &Imports,
    outer_names: &crate::binding::Env,
) -> Result<()> {
    let mut leaves = Vec::new();
    collect_non_agg_leaves(expr, false, &mut leaves);
    let mut offending: Vec<String> = Vec::new();
    // A rejected leaf whose shape names no variable (a chained property
    // `a.p.q`, a parameter property `$p.x`, map-projection shorthand on a
    // non-variable target) must still raise: the rejection is decided by the
    // leaf, the *name* in its message is best-effort. Gating on the name
    // list instead let those leaves pass silently.
    let mut rejected = false;
    for leaf in &leaves {
        if !leaf_allowed_as_group(leaf, group_keys, imports) {
            rejected = true;
            push_offending_name(leaf, &mut offending);
        }
    }
    if rejected {
        return Err(non_grouping_ref(&offending));
    }
    check_nested_scope_refs(expr, group_keys, imports, outer_names)
}

/// Record the variable a rejected leaf reads from, once — the description
/// names each offending variable, mirroring Neo4j's 42I18 text.
fn push_offending_name(leaf: &Expr, out: &mut Vec<String>) {
    let name = match leaf {
        Expr::Variable(v) => Some(v.to_string()),
        Expr::Property { expr, .. } => match expr.as_ref() {
            Expr::Variable(v) => Some(v.to_string()),
            _ => None,
        },
        _ => None,
    };
    if let Some(name) = name {
        if !out.contains(&name) {
            out.push(name);
        }
    }
}

/// The non-grouping-reference rejection the item and nested-scope walks
/// raise. One construction, so both channels spell the same policy; the
/// offending variables are named when the walk knows them.
fn non_grouping_ref(offending: &[String]) -> Error {
    let named = match offending {
        [] => String::new(),
        [one] => format!(": the expression contains a non-grouping sub-expression `{one}`"),
        [rest @ .., last] => format!(
            ": the expression contains non-grouping sub-expressions {} and `{last}`",
            rest.iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    Error::syntax(format!(
        "AmbiguousAggregationExpression: non-aggregated expression must be a grouping key{named}"
    ))
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
    items: &[Projection],
    group_by: Option<&[Expr]>,
    imports: &Imports,
    outer_names: &crate::binding::Env,
    expr: &Expr,
    context: &str,
) -> Result<()> {
    let group_keys = grouping_keys(items, group_by);
    let output_names: Vec<String> = items
        .iter()
        .enumerate()
        .map(|(i, item)| item.output_name(i))
        .collect();
    let mut leaves = Vec::new();
    collect_nested_scope_leaves(expr, &mut Vec::new(), &mut leaves);
    for leaf in &leaves {
        // A name the subclause's own nested scopes bind (`z` in `EXISTS {
        // WITH x + 1 AS z RETURN z }`) is not a read of this clause's row.
        if leaf_root_name(leaf)
            .is_some_and(|name| !name.with_str(|text| outer_names.contains_key(text)))
        {
            continue;
        }
        if leaf_allowed_in_aggregating_scope(leaf, &group_keys, &output_names, imports) {
            continue;
        }
        // Implicit grouping keeps the deprecated sub-expression resolution:
        // a leaf inside a *complex* key is the key's own value. The key may
        // contain a subquery expression (ProjectionSubclauseDeprecation
        // [51]/[61]), so containment descends into subquery bodies — but a
        // bare subquery-expression key is not a "complex projection item
        // expression" and stays unrecognized ([34]/[48]/[53]).
        if group_by.is_none()
            && group_keys
                .iter()
                .filter(|key| !is_subquery_expression(key))
                .any(|key| expr_contains_deep(key, leaf) && expr_contains_deep(expr, key))
        {
            continue;
        }
        return Err(Error::syntax(format!(
            "UndefinedVariable: {context} refers to a non-projecting variable"
        )));
    }
    Ok(())
}

/// Whether the expression is itself an embedded subquery
/// (`COUNT`/`COLLECT`/`EXISTS { … }`).
fn is_subquery_expression(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::CountSubquery { .. } | Expr::CollectSubquery { .. } | Expr::ExistsSubquery { .. }
    )
}

/// Whether `haystack` contains `needle` as itself or as a subexpression,
/// descending into expression-embedded subquery bodies — the containment
/// [`Expr::children`] alone cannot see (`COUNT { RETURN a.p + 1 }` holds
/// the read of `a`).
fn expr_contains_deep(haystack: &Expr, needle: &Expr) -> bool {
    // One worklist pass over the expression tree and each reachable subquery
    // body, every node visited once (identity-keyed, so a shared node is not
    // re-expanded). The recursive form re-expanded a nested subquery body
    // once per ancestor node, which is exponential in the nesting depth of an
    // attacker-supplied statement — reachable during validation, where no
    // cancel token is polled.
    let mut exprs: Vec<&Expr> = vec![haystack];
    let mut queries: Vec<&Query> = Vec::new();
    let mut seen_exprs: HashSet<*const Expr> = HashSet::new();
    let mut seen_queries: HashSet<*const Query> = HashSet::new();
    loop {
        while let Some(expr) = exprs.pop() {
            if expr == needle {
                return true;
            }
            if !seen_exprs.insert(expr as *const Expr) {
                continue;
            }
            expr.each_child(&mut |child| exprs.push(child));
            if let Expr::CountSubquery { query }
            | Expr::CollectSubquery { query }
            | Expr::ExistsSubquery { query } = expr
            {
                queries.push(query);
            }
        }
        let Some(query) = queries.pop() else {
            return false;
        };
        if !seen_queries.insert(query as *const Query) {
            continue;
        }
        relate_ir::walk_query(query, &mut |_| {}, &mut |expr| exprs.push(expr));
    }
}

/// The leaf predicate of [`check_post_projection_leaves`]: an exact key, a
/// property of a bare-variable key, a projected output name, or a
/// CALL-subquery import. A `*` key carries every variable (ADR-0198 ruling
/// 1).
fn leaf_allowed_in_aggregating_scope(
    leaf: &Expr,
    group_keys: &[&Expr],
    output_names: &[String],
    imports: &Imports,
) -> bool {
    if group_keys.iter().any(|key| **key == *leaf) {
        return true;
    }
    let base_var = match leaf {
        Expr::Variable(v) => v.clone(),
        Expr::Property { expr, .. } => match expr.as_ref() {
            Expr::Variable(v) => v.clone(),
            _ => return false,
        },
        _ => return true,
    };
    group_keys
        .iter()
        .any(|key| matches!(key, Expr::Variable(k) if k == &base_var) || matches!(key, Expr::Star))
        || base_var.with_str(|text| output_names.iter().any(|name| name == text))
        || base_var.with_str(|text| imports.contains(text))
}

/// The same-clause-reference rejection ([`check_same_clause_refs`] and its
/// leaf loop). One construction: both sites spell the same policy.
fn same_clause_ref(context: &str) -> Error {
    Error::syntax(format!(
        "InvalidAggregation: {context} cannot reference variables declared in the same clause"
    ))
}

/// ADR-0453 (AggregationAcceptance "reference to non-grouping
/// sub-expression", ExpressionAcceptance [3]): a name an aggregating item
/// reads inside a nested scope — a pattern predicate's pattern, a
/// comprehension/quantifier/reduce body, or an `EXISTS`/`COUNT`/`COLLECT`
/// body — is read against the enclosing row once per element, so it must be
/// a grouping key exactly like a direct leaf. `MATCH (a) RETURN a.p, COUNT {
/// RETURN a.p + 1 + count(a) } + count(a)` is a non-grouping reference while
/// the same body beside a projected `a` is not, and
/// `WITH o, size([x IN collect(e) WHERE exists((s)-[:OTHER]->(o))])` reads
/// the ungrouped `s`. Names the scopes bind themselves are not row reads;
/// aggregate calls are the aggregate's own inputs, not leaves.
///
/// `outer_names` filters the reads collected from subquery bodies, whose
/// [walk](relate_ir::walk_query) reports every name they mention —
/// inner bindings included.
fn check_nested_scope_refs(
    item: &Expr,
    group_keys: &[&Expr],
    imports: &Imports,
    outer_names: &crate::binding::Env,
) -> Result<()> {
    let mut leaves = Vec::new();
    collect_nested_scope_leaves(item, &mut Vec::new(), &mut leaves);
    for leaf in &leaves {
        let root = leaf_root_name(leaf);
        if root.is_none_or(|name| name.with_str(|text| outer_names.contains_key(text)))
            && !leaf_allowed_as_group(leaf, group_keys, imports)
        {
            let mut offending = Vec::new();
            push_offending_name(leaf, &mut offending);
            return Err(non_grouping_ref(&offending));
        }
    }
    Ok(())
}

/// The variable name a leaf reads from (a bare variable, or a property's
/// base variable) — the name the `outer_names` filter of
/// [`check_nested_scope_refs`] tests.
fn leaf_root_name(leaf: &Expr) -> Option<&SmallName> {
    match leaf {
        Expr::Variable(v) => Some(v),
        Expr::Property { expr, .. } => match expr.as_ref() {
            Expr::Variable(v) => Some(v),
            _ => None,
        },
        _ => None,
    }
}

/// The leaves of [`check_nested_scope_refs`]: every variable or property
/// read `expr` makes outside its aggregate calls, in the same shape
/// [`collect_non_agg_leaves`] produces, with the identifiers its own nested
/// scopes bind subtracted from the scopes they govern.
fn collect_nested_scope_leaves(expr: &Expr, bound: &mut Vec<SmallName>, out: &mut Vec<Expr>) {
    // Aggregate calls fold the group; their arguments are the aggregation's
    // input, not enclosing-row reads.
    if is_agg_call(expr) {
        return;
    }
    fn read(name: &SmallName, bound: &[SmallName], out: &mut Vec<Expr>) {
        if !bound.iter().any(|b| b == name) {
            out.push(Expr::Variable(name.clone()));
        }
    }
    match expr {
        Expr::Variable(v) => {
            read(v, bound, out);
            return;
        }
        // Inner-scope leaves keep the direct walker's shape: a property
        // leaf is the property expression, so it resolves against a
        // property grouping key (`{foo: a.name = 'Andres', kids:
        // collect(..)}` beside a projected `a.name`, TCK Return6 [6]).
        Expr::Property { expr: target, .. } => {
            if let Expr::Variable(v) = target.as_ref() {
                if !bound.iter().any(|b| b == v) {
                    out.push(expr.clone());
                }
                return;
            }
        }
        // A pattern predicate correlates the pattern against already-bound
        // names; its element variables are reads, never bindings here.
        Expr::PatternPred { path } | Expr::ShortestPath { path, .. } => {
            for name in path.path_names().chain(relate_ir::path_pattern_vars(path)) {
                read(name, bound, out);
            }
            return;
        }
        // A pattern comprehension's pattern variables are reads when the
        // enclosing scope already binds them (`size([(a)-->() | 1])` reads
        // the grouped `a`), and fresh element bindings otherwise; the
        // caller's `outer_names` filter drops the fresh ones. The predicate
        // and projection evaluate with every pattern name bound.
        Expr::PatternComp {
            path,
            predicate,
            projection,
        } => {
            let names: Vec<&SmallName> = path
                .path_names()
                .chain(relate_ir::path_pattern_vars(path))
                .collect();
            for name in names.iter().copied() {
                read(name, bound, out);
            }
            let mark = bound.len();
            bound.extend(names.into_iter().cloned());
            if let Some(predicate) = predicate {
                collect_nested_scope_leaves(predicate, bound, out);
            }
            collect_nested_scope_leaves(projection, bound, out);
            bound.truncate(mark);
            return;
        }
        // A subquery body's own bindings are not enumerable here; the
        // caller's `outer_names` filter drops them.
        Expr::ExistsSubquery { query }
        | Expr::CountSubquery { query }
        | Expr::CollectSubquery { query } => {
            // The two walker callbacks cannot share one collection, so the
            // body is walked twice: pattern bindings, then expression
            // variables.
            let mut names: Vec<&SmallName> = Vec::new();
            relate_ir::walk_query(
                query,
                &mut |clause| {
                    let relate_ir::Clause::Match { patterns, .. } = clause else {
                        return;
                    };
                    for pattern in patterns {
                        names.extend(
                            pattern
                                .path_names()
                                .chain(relate_ir::path_pattern_vars(pattern)),
                        );
                    }
                },
                &mut |_| {},
            );
            relate_ir::walk_query(query, &mut |_| {}, &mut |e| {
                if let Expr::Variable(v) = e {
                    names.push(v);
                }
            });
            for name in names {
                read(name, bound, out);
            }
            return;
        }
        _ => {}
    }
    // Per-element bodies scope the identifiers their variant binds, so the
    // shared walk pushes them for the body's descent and drops them after.
    walk_scope_children(expr, &mut |child, scope| match scope {
        ChildScope::Enclosing => collect_nested_scope_leaves(child, bound, out),
        ChildScope::Element(names) => {
            let mark = bound.len();
            bound.extend(names.iter().cloned());
            collect_nested_scope_leaves(child, bound, out);
            bound.truncate(mark);
        }
    });
}

fn collect_non_agg_leaves(expr: &Expr, inside_agg: bool, out: &mut Vec<Expr>) {
    if matches!(expr, Expr::CountStar) {
        return;
    }
    let inside_agg =
        inside_agg || matches!(expr, Expr::Function { name, .. } if name.with_str(is_agg_fn));
    if matches!(expr, Expr::Variable(_) | Expr::Property { .. }) && !inside_agg {
        out.push(expr.clone());
        return;
    }
    // Comprehension/reduce variants bind their own identifiers; leaves inside
    // them are not grouping-key candidates, so do not descend (same coverage
    // as the historical walker).
    if expr.binds_identifiers() {
        return;
    }
    if let Expr::MapProjection {
        expr: target,
        items,
    } = expr
    {
        if !inside_agg {
            // The variable/property shorthand items are leaves with the same
            // shape the pre-typed pseudo-call produced.
            for item in items {
                match item {
                    MapProjectionItem::Variable { name } => {
                        out.push(Expr::Variable(name.clone()));
                    }
                    MapProjectionItem::Property { key } => {
                        out.push(Expr::property((**target).clone(), key.clone()));
                    }
                    MapProjectionItem::KeyValue { .. } | MapProjectionItem::AllProperties => {}
                }
            }
        }
    }
    expr.each_child(&mut |child| collect_non_agg_leaves(child, inside_agg, out));
}

fn leaf_allowed_as_group(leaf: &Expr, group_keys: &[&Expr], imports: &Imports) -> bool {
    if group_keys.contains(&leaf) {
        return true;
    }
    // ADR-0198 ruling 1: a `*` projection item carries every in-scope
    // variable as a grouping key, so any variable leaf resolves against it.
    // Property leaves are NOT covered: `*` projects the variables, not
    // their property expressions, so the unprojected-property-leaf
    // rejection (battery i03) stands under `*` too.
    if matches!(leaf, Expr::Variable(_)) && group_keys.iter().any(|g| matches!(g, Expr::Star)) {
        return true;
    }
    // ADR-0453: a CALL-subquery import is a per-row constant, not a
    // grouping-key candidate — a variable leaf naming one (or a property
    // leaf on one) resolves without grouping.
    if let Expr::Variable(v) = leaf {
        if v.with_str(|text| imports.contains(text)) {
            return true;
        }
    }
    if let Expr::Property { expr, .. } = leaf {
        if let Expr::Variable(v) = expr.as_ref() {
            if v.with_str(|text| imports.contains(text)) {
                return true;
            }
            return group_keys
                .iter()
                .any(|g| matches!(g, Expr::Variable(k) if k == v));
        }
    }
    false
}

/// Whether `haystack` contains `needle` as itself or as a subexpression.
fn subexpr_contains(haystack: &Expr, needle: &Expr) -> bool {
    haystack == needle || haystack.any(&mut |e| e == needle)
}

/// A `RETURN DISTINCT` (or aggregating) `ORDER BY` key resolves against the
/// clause's *projected expressions*, not the pre-projection variables: the
/// key may decompose any expression the clause projects
/// (`RETURN DISTINCT {p: 1} AS pa0, pa0.p AS pa1, x AS y ORDER BY -1 = pa0.p + x`
/// uses the projected `pa0.p` and `x`), while a variable/property leaf no
/// projected expression contains (`ORDER BY a.age` beside `RETURN DISTINCT
/// a.name`) stays undefined. Every leaf of `expr` must therefore lie inside
/// a projection item's expression; keys with no variable leaves (constants,
/// parameters) pass.
pub(crate) fn order_key_within_projection(expr: &Expr, items: &[Projection]) -> bool {
    let mut leaves = Vec::new();
    collect_non_agg_leaves(expr, false, &mut leaves);
    leaves
        .iter()
        .all(|leaf| items.iter().any(|item| subexpr_contains(&item.expr, leaf)))
}

pub(crate) fn reject_agg_in_pred(expr: &Expr) -> Result<()> {
    if is_agg_expr(expr) {
        return Err(Error::syntax(
            "InvalidAggregation: aggregations are not allowed in WHERE",
        ));
    }
    Ok(())
}

/// ADR-0453: whether a projection clause establishes the aggregating
/// context its subclauses (ORDER BY, a following WHERE) may aggregate in —
/// an aggregate item, an explicit `GROUP BY` (which aggregates even with
/// no aggregate items), or `DISTINCT` (whose items are the grouping keys).
pub(crate) fn projection_aggregates(
    items: &[Projection],
    group_by: Option<&[Expr]>,
    distinct: bool,
) -> bool {
    items.iter().any(|i| is_agg_expr(&i.expr)) || group_by.is_some() || distinct
}

/// ADR-0453: the one leaf-discipline check an aggregating subclause
/// expression passes through. Aggregate calls recompute over the group, so
/// anything inside them is the aggregation's own input; every leaf a
/// subclause expression references *outside* its aggregate calls must be a
/// grouping key or a projected output name (an aggregating item's alias
/// included — `ORDER BY s + sum(a.p)` beside `sum(a.p) AS s` reads the
/// projected `s`). `context` names the subclause in the rejection.
pub(crate) fn check_subclause_agg_leaves(
    items: &[Projection],
    group_by: Option<&[Expr]>,
    imports: &Imports,
    expr: &Expr,
    context: &str,
) -> Result<()> {
    let group_keys = grouping_keys(items, group_by);
    let output_names: Vec<String> = items
        .iter()
        .enumerate()
        .map(|(i, item)| item.output_name(i))
        .collect();
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
    let reintroduced: Vec<String> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            !is_agg_expr(&item.expr) && !item.expr.any(&mut |e| matches!(e, Expr::Variable(_)))
        })
        .map(|(i, item)| item.output_name(i))
        .collect();
    let mut leaves = Vec::new();
    collect_non_agg_leaves(expr, false, &mut leaves);
    for leaf in &leaves {
        if matches!(leaf, Expr::Variable(v) if v.with_str(|t| reintroduced.iter().any(|r| r == t)))
        {
            return Err(same_clause_ref(context));
        }
        if !leaf_allowed_as_group(leaf, &group_keys, imports)
            && !matches!(leaf, Expr::Variable(v)
                if v.with_str(|t| output_names.iter().any(|n| n == t)))
            // A leaf inside a projection item's *expression* is part of that
            // grouping key: `RETURN me.age + you.age … ORDER BY me.age +
            // you.age + count(*)` (ReturnOrderBy6 [5]) decomposes the
            // projected expression, which Neo4j allows.
            && !items
                .iter()
                .any(|item| subexpr_contains(&item.expr, leaf) && subexpr_contains(expr, &item.expr))
        {
            return Err(Error::syntax(format!(
                "UndefinedVariable: {context} refers to a non-projected expression"
            )));
        }
    }
    check_same_clause_refs(items, expr, &reintroduced, context)?;
    Ok(())
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
    // The whole subclause expression verbatim matching a projection item is
    // that item, not a subclause reference ([20]-[23]: `ORDER BY count(a)`
    // beside `count(a) AS cnt` with `1 AS a` re-introducing `a`).
    if is_projected_item(items, expr) {
        return Ok(());
    }
    let mut calls = Vec::new();
    expr.visit(&mut |e| {
        if matches!(e, Expr::CountStar)
            || matches!(e, Expr::Function { name, .. } if name.with_str(is_agg_fn))
        {
            calls.push(e.clone());
        }
    });
    for call in calls {
        let mut vars = Vec::new();
        call.visit(&mut |e| {
            if let Expr::Variable(v) = e {
                vars.push(v.clone());
            }
        });
        if vars
            .iter()
            .any(|v| v.with_str(|t| reintroduced.iter().any(|r| r == t)))
        {
            return Err(same_clause_ref(context));
        }
    }
    Ok(())
}

pub(crate) fn reject_order_by_agg(
    items: &[Projection],
    group_by: Option<&[Expr]>,
    distinct: bool,
    imports: &Imports,
    order: &[OrderItem],
) -> Result<()> {
    let order_has_agg = order.iter().any(|o| is_agg_expr(&o.expr));
    if order_has_agg && !projection_aggregates(items, group_by, distinct) {
        return Err(Error::syntax(
            "InvalidAggregation: aggregation functions are not allowed in ORDER BY when clause has no aggregation",
        ));
    }
    for o in order {
        if !is_agg_expr(&o.expr) {
            continue;
        }
        reject_shadowed_aggregate_argument(items, &o.expr)?;
        check_subclause_agg_leaves(items, group_by, imports, &o.expr, "ORDER BY aggregation")?;
    }
    Ok(())
}

/// An ORDER BY aggregation whose argument names another item's *aggregate*
/// output (`RETURN sum(a) AS a … ORDER BY sum(a) + 1`) reads a projected
/// aggregate as its own input — Neo4j rejects the same-clause shadowing with
/// 42001 SyntaxError. Only variables inside the aggregate calls are tested:
/// a projected aggregate referenced elsewhere in the key (`ORDER BY a + 1`)
/// is an ordinary read of that column.
fn reject_shadowed_aggregate_argument(items: &[Projection], expr: &Expr) -> Result<()> {
    // A subclause expression that *is* a projection item is that item, not a
    // same-clause reference (`RETURN sum(a) AS a ORDER BY sum(a)` is legal;
    // live 2026.10 rejects only an aggregate embedded in a larger key).
    if is_projected_item(items, expr) {
        return Ok(());
    }
    let aggregate_aliases: Vec<String> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| is_agg_expr(&item.expr))
        .map(|(index, item)| item.output_name(index))
        .collect();
    if aggregate_aliases.is_empty() {
        return Ok(());
    }
    let mut calls = Vec::new();
    expr.visit(&mut |e| {
        if matches!(e, Expr::CountStar)
            || matches!(e, Expr::Function { name, .. } if name.with_str(is_agg_fn))
        {
            calls.push(e.clone());
        }
    });
    for call in calls {
        let mut shadowed = false;
        call.visit(&mut |e| {
            if let Expr::Variable(v) = e {
                shadowed |= v.with_str(|t| aggregate_aliases.iter().any(|a| a == t));
            }
        });
        if shadowed {
            return Err(Error::syntax(
                "InvalidAggregation: ORDER BY aggregation references an aggregate \
                 projected in the same clause",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
