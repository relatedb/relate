//! Aggregation checks: the grouping-key discipline for projection items
//! and the ORDER BY / WHERE aggregation restrictions. The subclause
//! family — which leaves a projection's ORDER BY/WHERE subclauses may
//! read once it aggregates (an aggregate item, an explicit `GROUP BY`,
//! or `DISTINCT`) — lives in [`subclause`]; this module keeps the
//! grouping-key core and the shared leaf vocabulary.

use relate_core::{Error, Result};
use relate_ir::{
    is_agg_call, is_agg_expr, walk_scope_children, ChildScope, Expr, MapProjectionItem, Projection,
};
use relate_names::SmallName;

mod subclause;

pub(crate) use subclause::{
    check_aggregating_subclause, check_where_subclause, reject_agg_in_pred, reject_order_by_agg,
    SubclauseScope,
};

/// ADR-0453: the names a CALL subquery imports (`CALL (tax) { ... }`), for
/// which the grouping-key discipline is relaxed — an import is a per-row
/// constant in the subquery's scope, so an aggregation expression may
/// reference it without it being a grouping key ("should not group by a
/// value imported into a CALL subquery"). Empty at every other scope.
pub(crate) type Imports = std::collections::HashSet<String>;

/// Whether an explicit `GROUP BY` key list names exactly the variables a
/// `*` item expands to (ADR-0198): non-empty, every key a bare variable,
/// and the key set equal to the incoming scope's name set.
fn explicit_keys_cover_star_scope(keys: &[Expr], outer_names: &crate::binding::Env) -> bool {
    let names: Option<std::collections::HashSet<String>> = keys
        .iter()
        .map(|k| match k {
            Expr::Variable(v) => Some(v.to_string()),
            _ => None,
        })
        .collect();
    !keys.is_empty()
        && names.is_some_and(|names| {
            names.len() == outer_names.len() && outer_names.keys().all(|s| names.contains(s))
        })
}

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
            let covers_scope = items.iter().any(|i| is_agg_expr(&i.expr))
                && explicit_keys_cover_star_scope(keys, outer_names);
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
/// (`COUNT`/`COLLECT`/`EXISTS { … }`) at any position — derived from the
/// one subquery accessor ([`Expr::subquery_body`]), so the three-variant
/// set is spelled nowhere else.
fn contains_subquery_expression(expr: &Expr) -> bool {
    expr.any(&mut |node| node.subquery_body().is_some())
}

/// GBN1.5 alias resolution: the aliased projection item becomes the base of
/// the resolved grouping key (`a.p AS a` with `GROUP BY a.p` lowers to the
/// key `(a.p).p`), so the item is that key's own sub-expression and stays
/// projectable — GroupByAcceptance [46] evaluates the key and reports the
/// type error, not a grouping-key rejection.
fn key_is_property_of(key: &Expr, item: &Expr) -> bool {
    // A static property key's base: under implicit grouping, projecting
    // the base beside its property key rides the key. The dynamic
    // spelling is deliberately absent — a dynamic key (`a[$k]`) exempts
    // nothing: its value is the keyed property, and the base variable
    // beside it is a fresh grouping decision the walk must actually
    // make, not one the key's shape answers.
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
    collect_non_agg_leaves(expr, &mut leaves);
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
    if let Some(name) = leaf_root_name(leaf) {
        let name = name.to_string();
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
    let leaves = collect_nested_scope_leaves(item);
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
        // A property — static or dynamic — reads its base variable.
        Expr::Property { expr, .. } | Expr::DynamicProperty { expr, .. } => match expr.as_ref() {
            Expr::Variable(v) => Some(v),
            _ => None,
        },
        _ => None,
    }
}

/// The leaves of [`check_nested_scope_refs`]: every variable or property
/// read `expr` makes outside its aggregate calls, in the same shape
/// [`collect_non_agg_leaves`] produces, with the identifiers its own
/// nested scopes bind subtracted from the scopes they govern — including
/// the reads inside subquery-expression bodies, merged uniformly: the
/// caller's `outer_names` filter drops each body's own bindings. The
/// entry owns the traversal's scope accumulator; the recursive body
/// ([`nested_scope_leaves`]) carries it.
fn collect_nested_scope_leaves(expr: &Expr) -> Vec<Expr> {
    let mut out = Vec::new();
    nested_scope_leaves(expr, &mut Vec::new(), SubqueryBodies::Merged, &mut out);
    out
}

/// The row-scope leaves of [`check_post_projection_leaves`]: the same
/// walk with subquery-expression bodies left uncollected. A body's
/// pattern binds its names afresh — a name the enclosing projection
/// dropped is a fresh declaration there, never a read of this clause's
/// row, and a body *expression* reading a dropped name is the front
/// end's undefined-name rejection — while a correlation on a name the
/// projection still exposes is admitted by that same projection, so the
/// subclause discipline gains no verdict from a body read.
fn collect_row_scope_leaves(expr: &Expr) -> Vec<Expr> {
    let mut out = Vec::new();
    nested_scope_leaves(expr, &mut Vec::new(), SubqueryBodies::Excluded, &mut out);
    out
}

/// The two entries' policy split on subquery-expression bodies: whether
/// an `EXISTS`/`COUNT`/`COLLECT` body's reads join the walk's output or
/// stay uncollected.
#[derive(Clone, Copy)]
enum SubqueryBodies {
    Merged,
    Excluded,
}

/// The two entries' shared recursive body: a pre-order walk of `expr`
/// carrying the traversal's own scope accumulator (the nested scopes'
/// binders, subtracted from the reads they govern) and the body policy
/// the entry selected.
fn nested_scope_leaves(
    expr: &Expr,
    bound: &mut Vec<SmallName>,
    bodies: SubqueryBodies,
    out: &mut Vec<Expr>,
) {
    // Aggregate calls fold the group; their arguments are the aggregation's
    // input, not enclosing-row reads.
    if is_agg_call(expr) {
        return;
    }
    fn is_bound(name: &SmallName, bound: &[SmallName]) -> bool {
        bound.iter().any(|b| b == name)
    }

    fn read(name: &SmallName, bound: &[SmallName], out: &mut Vec<Expr>) {
        if !is_bound(name, bound) {
            out.push(Expr::Variable(name.clone()));
        }
    }
    // Inner-scope leaves keep the direct walker's shape through the one
    // classification (`is_row_leaf`) and the one base-name extractor
    // (`leaf_root_name`): a leaf whose base is bound in this scope is
    // its own binding and reads nothing; a leaf whose base reads the
    // enclosing row is pushed whole, so it resolves against a property
    // grouping key (`{foo: a.name = 'Andres', kids: collect(..)}` beside
    // a projected `a.name`, TCK Return6 [6]). A shape `is_row_leaf`
    // calls atomic but whose base no name extracts (a chained property)
    // falls through and descends — the direct walker keeps a chained
    // property whole as one leaf (its own pin), while this walk resolves
    // the inner scopes through their bases: the two verdicts differ by
    // design for that one shape, and both read the same predicate.
    if is_row_leaf(expr) {
        if let Some(base) = leaf_root_name(expr) {
            if !is_bound(base, bound) {
                out.push(expr.clone());
            }
            return;
        }
    }
    match expr {
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
                nested_scope_leaves(predicate, bound, bodies, out);
            }
            nested_scope_leaves(projection, bound, bodies, out);
            bound.truncate(mark);
            return;
        }
        // A subquery body's own bindings are not enumerable here: the
        // all-leaves walk reports every name the body mentions and the
        // caller's `outer_names` filter drops the body's own; the
        // row-scope walk leaves the body uncollected entirely (its
        // entry's doc).
        Expr::ExistsSubquery { query }
        | Expr::CountSubquery { query }
        | Expr::CollectSubquery { query } => {
            if matches!(bodies, SubqueryBodies::Excluded) {
                return;
            }
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
        ChildScope::Enclosing => nested_scope_leaves(child, bound, bodies, out),
        ChildScope::Element(names) => {
            let mark = bound.len();
            bound.extend(names.iter().cloned());
            nested_scope_leaves(child, bound, bodies, out);
            bound.truncate(mark);
        }
    });
}

/// The atomic-leaf classification the grouping walks share: a bare
/// variable, a static property whose target carries no aggregation
/// (chains stay whole — `a.p.q` is one leaf), or a dynamic property
/// whose target is a bare variable AND whose key is a per-row constant
/// (`is_seek_const`'s literal-and-parameter set, the one home of that
/// vocabulary). The dynamic arm's variable-target gate is the
/// resolution contract: a dynamic leaf resolves through its base
/// variable (`leaf_root_name`), so a dynamic property of a chained or
/// computed target (`a.meta[$k]`, `f(a)[$k]`) is NOT an atomic leaf —
/// it descends, and its base leaf resolves through the base-variable
/// allowance exactly as it did before the dynamic shape landed. A
/// property whose target carries an aggregation is not a row leaf
/// either — its value comes from the aggregate's own result
/// (`max(a.p).q`, `collect(n)[0].q`) — and a dynamic property with a
/// row-expression key is not one: the key is a row read the grouping
/// discipline must see as its own leaf (`a[k]` under `GROUP BY a` with
/// a per-row `k` stays rejected, so the base-variable allowance can
/// never admit a group-varying key).
fn is_row_leaf(expr: &Expr) -> bool {
    match expr {
        Expr::Variable(_) => true,
        Expr::Property { expr: target, .. } => !is_agg_expr(target),
        Expr::DynamicProperty { expr: target, key } => {
            matches!(target.as_ref(), Expr::Variable(_)) && key.is_seek_const()
        }
        _ => false,
    }
}

fn collect_non_agg_leaves(expr: &Expr, out: &mut Vec<Expr>) {
    // An aggregate call's whole subtree is its own input: the walk never
    // enters it — the sibling walker's prune
    // (`collect_nested_scope_leaves`), in place of a carried `inside_agg`
    // mode no caller ever set.
    if is_agg_call(expr) {
        return;
    }
    if is_row_leaf(expr) {
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
    expr.each_child(&mut |child| collect_non_agg_leaves(child, out));
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
    // A property — static or dynamic — of a bare-variable grouping key
    // or import resolves like the key itself (`a.p` under `GROUP BY a`,
    // `a[$k]` under `GROUP BY a`).
    if matches!(leaf, Expr::Property { .. } | Expr::DynamicProperty { .. }) {
        if let Some(v) = leaf_root_name(leaf) {
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

#[cfg(test)]
mod tests;
