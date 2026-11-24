//! The generic AST collectors every pass builds on: the variable and
//! pattern collectors, the SEARCH score-variable and distance-metric
//! lookups, the shared expression-scope check pair
//! (`check_expr_vars_defined`/`check_name_defined`), the scope-aware
//! expression walker (`walk_expr_scoped`) and the UNION-chain
//! enumeration. They live below `validate`, `call_scope`,
//! `local_callables` and `lower` (all of which consume them downward) so
//! none of them owns another's helper. The projection-item naming rule
//! the collectors feed lives in [`super::naming`]; aggregation-specific
//! vocabulary (`contains_aggregation`, the alias table) lives in
//! [`crate::agg`] instead, beside the names it tests.

use relate_names::SmallName;

use crate::ident_hash::{IdentBuildHasher, IdentHashMap, IdentHashSet};

use relate_error::EngineError;
use relate_parser::{
    Clause, Expr, InterpPart, LiteralValue, MatchClause, PatternElement, PatternPath, ReturnItem,
    Statement, UnionPart,
};

use super::naming::{effective_name, item_variable};
use super::tree::{walk_expr_with, SubqueryDescent};
use crate::show_columns::{catalog_output_names, is_catalog_clause};

/// The pattern list a clause carries: `MATCH` and `CREATE` own a list,
/// `MERGE` a single pattern. The one clause→pattern-list mapping — the shape
/// rules and the traversal-bound scan read it instead of re-spelling the
/// match.
pub(crate) fn clause_patterns(clause: &Clause) -> Option<&[PatternPath]> {
    match clause {
        Clause::Match(mc) => Some(&mc.pattern),
        Clause::Create(cc) => Some(&cc.pattern),
        Clause::Merge(mc) => Some(std::slice::from_ref(&mc.pattern)),
        _ => None,
    }
}

/// The pattern a pattern-carrying expression embeds: a `shortestPath()` /
/// `allShortestPaths()` expression, a pattern predicate, or a pattern
/// comprehension. The one expression→pattern mapping the shape rules and
/// the traversal-bound scan read instead of re-spelling the match.
pub(crate) fn pattern_of_expr(expr: &Expr) -> Option<&PatternPath> {
    match expr {
        Expr::ShortestPath(shortest) => Some(&shortest.pattern),
        Expr::PatternPredicate(predicate) => Some(&predicate.pattern),
        Expr::PatternComprehension(comprehension) => Some(&comprehension.pattern),
        _ => None,
    }
}

/// Whether a clause mutates the graph: the updating clauses plus a nested
/// `FOREACH` (its body is writes only). This is the one positive
/// classification — `LOAD CSV` is a read, a procedure call is not statically
/// a write, and everything else is a read/projection — so consumers that
/// additionally treat one of those as a write add it explicitly, and the
/// FOREACH body rule (which admits exactly this set) cannot drift into a
/// third inverted copy.
pub(crate) fn is_updating_clause(clause: &Clause) -> bool {
    matches!(
        clause,
        Clause::Create(_)
            | Clause::Merge(_)
            | Clause::Delete(_)
            | Clause::Set(_)
            | Clause::Remove(_)
            | Clause::Foreach(_)
    )
}

/// The `SCORE AS <var>` variable a `<search clause>` optionally
/// introduces, on top of its `<binding variable reference>` (already one of
/// the MATCH clause's own pattern element variables, so already covered by
/// `collect_pattern_element_vars`). Every "which variables does this MATCH
/// clause bind" scope tracker in this module and in `call_scope` folds this
/// in too, so a later WHERE/WITH/RETURN/ORDER BY can reference the score
/// variable regardless of which side of SEARCH the WHERE was written on.
pub(crate) fn search_score_var(mc: &MatchClause) -> Option<&SmallName> {
    mc.search.as_ref()?.score_var.as_ref()
}

/// Collects every pattern variable name a single `PatternElement` brings
/// into scope: a node's or relationship's own `variable`, or -- for a
/// CIP-28 `Quantified` element -- every named element inside its operand
/// (which become "group variables" in the enclosing scope) plus its own
/// subpath variable, if any. Used by every "which
/// variables does this MATCH/CREATE/MERGE pattern bind" scope-tracking
/// check in this module so a group variable is visible to the rest of the
/// query exactly like any other pattern variable.
pub(crate) fn collect_pattern_element_vars(
    elem: &PatternElement,
    out: &mut IdentHashSet<SmallName>,
) {
    for_each_element_var(elem, &mut |name| {
        out.insert(name.clone());
    });
}

/// The one walk over the variables a pattern element introduces (a node's
/// or relationship's own variable, or a quantified operand's variables plus
/// its subpath variable). Both [`collect_pattern_element_vars`]'s set fold
/// and [`pattern_vars`]'s ordered list consume it.
fn for_each_element_var<'a>(elem: &'a PatternElement, visit: &mut impl FnMut(&'a SmallName)) {
    match elem {
        PatternElement::Node(np) => {
            if let Some(v) = &np.variable {
                visit(v);
            }
        }
        PatternElement::Relationship(rp) => {
            if let Some(v) = &rp.variable {
                visit(v);
            }
        }
        PatternElement::Quantified(qpp) => {
            for inner in &qpp.elements {
                for_each_element_var(inner, visit);
            }
            if let Some(v) = &qpp.subpath_variable {
                visit(v);
            }
        }
    }
}

/// The argument index of `vector_distance`/`vector_norm`'s bare
/// distance-metric keyword, or `None` for every other function — the one
/// home of that fact, consulted by both the variable walker's edge filter
/// ([`collect_expr_variables`]) and the lowering's metric-keyword rewrite
/// (`lower::expr::resolve_vector_metric_arg`).
pub(crate) fn distance_metric_arg_index(name: &str) -> Option<usize> {
    match name.to_lowercase().as_str() {
        "vector_distance" => Some(2),
        "vector_norm" => Some(1),
        _ => None,
    }
}

/// Collect all variable names referenced in an expression.
///
/// Rides the shared self-capping walker ([`walk_expr_with`]) with an edge
/// filter that reproduces the historical hand-rolled recursion's reach
/// exactly: it descends into property *values*, binary/unary/comparison
/// operands, function arguments, list/map items, parentheses,
/// subscripts/slices, `IN`, `IS NULL`/`IS TYPE`/`IS NORMALIZED`, string
/// matches, CASE arms and `HAS LABEL` operands — but not into a dynamic
/// property *key* (`n[<expr>]`), the bare distance-metric keyword of
/// `vector_distance`/`vector_norm` (CIP-214: e.g. EUCLIDEAN parses
/// identically to a variable reference but is resolved as a metric literal
/// in `function.rs`, not looked up in row scope), or any scope-introducing
/// or pattern-bearing construct (list/pattern comprehensions, quantifiers,
/// `reduce`/`allReduce`, map projections, `shortestPath()`, pattern
/// predicates): variables used inside those are bound there, not references
/// in the enclosing scope. Subquery bodies (`EXISTS`/`COUNT`/`COLLECT
/// { … }`) are opaque leaves to the walker, as they were to the hand-rolled
/// recursion.
/// The reach-independent lexical-identity exclusions, one predicate: a
/// dynamic property key (`n[<expr>]`) and the bare distance-metric
/// keyword argument of `vector_distance`/`vector_norm` (see
/// [`distance_metric_arg_index`]) both parse as variable references
/// without *being* reads — every reach (the scope reach and the
/// whole-tree mention reach) excludes them.
fn lexical_identity_excluded(parent: &Expr, child: &Expr) -> bool {
    match parent {
        Expr::Property(p) => !std::ptr::eq(child, &p.expr),
        Expr::FunctionCall(fc) => match fc.name.with_str(distance_metric_arg_index) {
            Some(metric) => fc.args.get(metric).is_some_and(|a| std::ptr::eq(child, a)),
            None => false,
        },
        _ => false,
    }
}

/// The edge filter of the scope-reach expression walk: whether a scope check
/// descends from `parent` into `child` ([`collect_expr_variables`] and the
/// post-projection subclause visibility rule both read the same reach, so a
/// construct that binds its own variables cannot leak them as outer
/// references).
pub(crate) fn expr_child_reaches_scope(parent: &Expr, child: &Expr) -> bool {
    if lexical_identity_excluded(parent, child) {
        return false;
    }
    match parent {
        // The property's value side and a call's non-metric arguments —
        // the children the lexical-identity predicate did not exclude.
        Expr::Property(_)
        | Expr::FunctionCall(_)
        | Expr::Binary(_)
        | Expr::Comparison(_)
        | Expr::Unary(_)
        | Expr::List(_)
        | Expr::PathConstruct(_)
        | Expr::Paren(_)
        | Expr::Subscript(_)
        | Expr::In(_)
        | Expr::IsNull(_)
        | Expr::IsType(_)
        | Expr::IsNormalized(_)
        | Expr::ChronologicalCompare(_)
        | Expr::StringMatch(_)
        | Expr::Case(_)
        | Expr::HasLabel(_)
        | Expr::Map(_) => true,
        // CIP-261: embeds are full expressions and must be walked.
        Expr::StringInterp(parts) => parts
            .iter()
            .any(|part| matches!(part, InterpPart::Interp(e) if std::ptr::eq(child, e.as_ref()))),
        // CIP-272: a map comprehension's source, key, and value expressions.
        Expr::MapComprehension(mc) => {
            std::ptr::eq(child, &mc.source_expr)
                || std::ptr::eq(child, &mc.key_expr)
                || std::ptr::eq(child, &mc.value_expr)
        }
        _ => false,
    }
}

pub(crate) fn collect_expr_variables(expr: &Expr, vars: &mut IdentHashSet<SmallName>) {
    let mut visit = |node: &Expr| {
        if let Expr::Variable(v) = node {
            vars.insert(v.clone());
        }
        Ok(())
    };
    let enter_child = expr_child_reaches_scope;
    // Infallible here: the visitor never fails, and every caller runs after
    // the whole-tree depth cap, so the walker's own cap cannot fire on
    // these trees (the hand-rolled recursion would have overflowed instead).
    let result = walk_expr_with(expr, &mut visit, enter_child, SubqueryDescent::Opaque);
    debug_assert!(
        result.is_ok(),
        "collect_expr_variables walked a tree that already passed the depth cap"
    );
}

/// Whether an expression mentions `name` anywhere — the whole-tree
/// reach over the one find-in-expression scaffolding
/// ([`crate::ast::tree::expr_contains`] with `SubqueryDescent::Enter`,
/// the lexical-identity exclusions kept, everything else entered):
/// subquery bodies (`EXISTS`/`COUNT`/`COLLECT { … }`) and comprehension
/// and quantifier bodies are walked, because a body can *read* an
/// enclosing variable (a correlated reference) even though the
/// variables *bound* there are not outer references — the score-half
/// classification of a searching MATCH's outer WHERE needs exactly that
/// question answered (ADR-0678's split: a score read inside a
/// correlated body still makes the conjunct a post-filter, or the
/// pattern-side probe evaluates it with no score binding at all). A
/// body that *binds* a variable of the same name shadows the enclosing
/// one for its own reads; such a conjunct classifies by the mention and
/// still evaluates correctly (its own binding carries), it
/// post-filters rather than window-restricting.
pub(crate) fn expr_mentions_variable(expr: &Expr, name: &SmallName) -> bool {
    crate::ast::tree::expr_contains(
        expr,
        |parent, child| !lexical_identity_excluded(parent, child),
        |node| matches!(node, Expr::Variable(v) if v == name),
        crate::ast::tree::SubqueryDescent::Enter,
    )
}

/// Bind every variable a pattern introduces into `bound`: each path's own
/// variable, then its elements' node/relationship variables. Shared by the
/// MATCH/CREATE/MERGE arms of the scope walkers in [`crate::validate`].
pub(crate) fn bind_pattern_vars(patterns: &[PatternPath], bound: &mut IdentHashSet<SmallName>) {
    for name in pattern_vars(patterns) {
        bound.insert(name.clone());
    }
}

/// The one node-variable walker: `visit` runs on every node element's
/// variable name — node element variables only, not relationship or path
/// variables (the executor's `create_or_bind_node` reuses only
/// `Value::Node` bindings, so a relationship or path variable does not
/// prove a name is an existing node: `MATCH ()-[r:X]->() CREATE (r)-[:Y]->()`
/// creates a new label-less node) — borrowed from the pattern, descending
/// into quantified operands. Both node-scope folds — the set the default
/// validation walk threads and safe mode's scope walker — ride it, so the
/// node-name enumeration exists once.
fn for_each_node_var<'a>(element: &'a PatternElement, visit: &mut impl FnMut(&'a SmallName)) {
    match element {
        PatternElement::Node(node) => {
            if let Some(name) = &node.variable {
                visit(name);
            }
        }
        PatternElement::Relationship(_) => {}
        PatternElement::Quantified(quantified) => {
            for inner in &quantified.elements {
                for_each_node_var(inner, visit);
            }
        }
    }
}

/// The node-variable fold of a pattern list: every node element variable,
/// borrowed from the pattern, handed to `visit` — the spelling a
/// scope-carrying walk uses when it threads node-ness clause by clause, so
/// no per-pattern table lands in the lowering path.
fn bind_pattern_node_name_refs<'a>(
    patterns: &'a [PatternPath],
    visit: &mut impl FnMut(&'a SmallName),
) {
    for path in patterns {
        for elem in &path.elements {
            for_each_node_var(elem, visit);
        }
    }
}

/// Every variable a pattern introduces, in the canonical order: each path's
/// own variable first, then its elements' variables (quantified operands
/// and subpath variables included). The one enumeration both
/// [`bind_pattern_vars`] and call_scope's checked binder consume.
pub(crate) fn pattern_vars(patterns: &[PatternPath]) -> Vec<&SmallName> {
    let mut out = Vec::new();
    for path in patterns {
        out.extend(path.path_names());
        for elem in &path.elements {
            for_each_element_var(elem, &mut |name| out.push(name));
        }
    }
    out
}

/// Check that every variable `expr` references is in `scope`; the first
/// offender raises the canonical `UndefinedVariable` error. The one
/// expression-scope check: [`crate::validate::expressions`] uses it at every
/// expression position and [`crate::call_scope`] uses it for its strict
/// undefined-variable clause checks.
pub(crate) fn check_expr_vars_defined(
    expr: &Expr,
    scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    let mut vars = IdentHashSet::default();
    walk_expr_with(
        expr,
        &mut |node: &Expr| {
            if let Expr::Variable(v) = node {
                vars.insert(v.clone());
            }
            Ok(())
        },
        |parent, child| {
            expr_child_reaches_scope(parent, child) && !is_dead_boolean_operand(parent, child)
        },
        SubqueryDescent::Opaque,
    )?;
    check_names_allowed(&vars, |v| scope.contains(v))
}

/// Whether `child` is the unreachable operand of `parent` under the
/// reference engine's `x OR true` / `x AND false` rewrite. That rewriter
/// runs before variable resolution, so the dead side's variables are never
/// resolved (`MiscAcceptance` "IN predicate with undefined variable is
/// accepted when it can be rewritten away").
fn is_dead_boolean_operand(parent: &Expr, child: &Expr) -> bool {
    let Expr::Binary(binary) = parent else {
        return false;
    };
    let constant = match binary.op {
        "OR" => true,
        "AND" => false,
        _ => return false,
    };
    let is_constant =
        |e: &Expr| matches!(e, Expr::Literal(LiteralValue::Bool(v)) if *v == constant);
    if is_constant(&binary.right) {
        std::ptr::eq(child, &binary.left)
    } else if is_constant(&binary.left) {
        std::ptr::eq(child, &binary.right)
    } else {
        false
    }
}

/// Whether a pattern is relationship-bearing: a `Relationship` element, or
/// a `Quantified` element (which always wraps a relationship-bearing body —
/// the parser already enforces that). The one test every
/// "is this a single-node pattern" gate consults.
pub(crate) fn has_relationship(pattern: &PatternPath) -> bool {
    pattern.elements.iter().any(|e| {
        matches!(
            e,
            PatternElement::Relationship(_) | PatternElement::Quantified(_)
        )
    })
}

/// [`check_expr_vars_defined`]'s pattern twin: every variable the patterns
/// introduce (quantified operands and subpath variables included) must
/// satisfy `allowed`, else [`EngineError::undefined_variable`] naming the
/// sorted-first offender.
pub(crate) fn check_pattern_vars_allowed(
    patterns: &[PatternPath],
    allowed: impl Fn(&SmallName) -> bool,
) -> Result<(), EngineError> {
    let mut vars = IdentHashSet::default();
    bind_pattern_vars(patterns, &mut vars);
    check_names_allowed(&vars, allowed)
}

/// Check one bare name's scope membership: the leaf of
/// [`check_expr_vars_defined`], for callers holding a bare name (a SET
/// item's target) rather than an expression to walk.
pub(crate) fn check_name_defined(
    name: &SmallName,
    scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    if scope.contains(name) {
        return Ok(());
    }
    Err(name.with_str(EngineError::undefined_variable))
}

/// [`check_expr_vars_defined`]'s names-plus-predicate generalization: every
/// name in `names` must satisfy `allowed`, else the canonical
/// [`undefined_variable_error`] naming the first offender in sorted order
/// (deterministic across builds). The ORDER BY visibility rules use it with
/// their per-mode predicates.
pub(crate) fn check_names_allowed(
    names: &IdentHashSet<SmallName>,
    allowed: impl Fn(&SmallName) -> bool,
) -> Result<(), EngineError> {
    // `SmallName`'s `Ord` is name-text order, so the sorted-first offender
    // is the lexicographically smallest name text (ADR-0505 §4).
    let mut sorted: Vec<&SmallName> = names.iter().collect();
    sorted.sort();
    for name in sorted {
        if !allowed(name) {
            return Err(name.with_str(EngineError::undefined_variable));
        }
    }
    Ok(())
}

/// One embedded expression of a CIP-261 interpolated string literal
/// (`s'...{expr}...'`): yields each `Interp` part's expression, skipping
/// the inert `Text` parts. Shared by every validator that must look inside
/// an interpolation.
pub(crate) fn interp_embeds(parts: &[InterpPart]) -> impl Iterator<Item = &Expr> {
    parts.iter().filter_map(|p| match p {
        InterpPart::Interp(e) => Some(e.as_ref()),
        InterpPart::Text(_) => None,
    })
}

/// Every arm of a statement's UNION chain, with the `all` flag of the link
/// that attached it (`UNION ALL` vs plain `UNION`), in source order — the
/// head statement itself is not an arm. `link` is the head statement's own
/// `union` edge ([`crate::ast::StmtRef::union`] for a walked statement,
/// `Statement::union` for one in hand); `None` yields no arms. The one
/// canonical enumeration of a UNION chain for the validate battery, the
/// lowering's parts fold and [`crate::call_scope`].
pub(crate) fn union_arms<'a>(
    link: Option<&'a UnionPart>,
) -> impl Iterator<Item = (&'a Statement, bool)> + 'a {
    let mut link = link;
    std::iter::from_fn(move || {
        let part = link?;
        let arm = (&part.statement, part.all);
        link = part.statement.union.as_deref();
        Some(arm)
    })
}

/// One statement plus every statement of its `UNION` chain, in order: the
/// chain cursor spelled once for the walkers that must visit a whole
/// chain (`once(stmt)` then [`union_arms`]' own traversal).
pub(crate) fn union_chain(stmt: &Statement) -> impl Iterator<Item = &Statement> {
    std::iter::once(stmt).chain(union_arms(stmt.union.as_deref()).map(|(arm, _)| arm))
}

/// The column names a WITH/RETURN item list projects: a `*` item passes
/// the incoming scope through, every other item contributes its
/// [`effective_name`]. The one project-items-to-names skeleton: both
/// projection arms of [`crate::output_vars`]'s
/// `local_stage_output_vars`, [`crate::validate::search`]'s WITH arm (a WITH
/// replaces the scope), and the WITH arm of [`clause_introduced_names`]
/// (introduction is exactly projection over an empty incoming scope).
pub(crate) fn projected_names(
    items: &[ReturnItem],
    bound: &IdentHashSet<SmallName>,
) -> IdentHashSet<SmallName> {
    project_names(items, bound, effective_name)
}

/// The name-presence wrapper over the one projection skeleton: renders
/// the bound set into [`project_with`]'s `(name, payload)` domain, folds,
/// and collapses the result's keys back — so a name fold's caller spells
/// only its contribution, never the container adapter.
fn project_names(
    items: &[ReturnItem],
    bound: &IdentHashSet<SmallName>,
    contribute: impl Fn(&ReturnItem) -> Option<SmallName>,
) -> IdentHashSet<SmallName> {
    let bound: IdentHashMap<SmallName, ()> = bound.iter().map(|name| (name.clone(), ())).collect();
    project_with(items, &bound, |item| {
        contribute(item).map(|name| (name, ()))
    })
    .into_keys()
    .collect()
}

/// The one projection fold every payload projection shares: a `*` item
/// passes the incoming `(name, payload)` pairs through, every other item
/// contributes the pair its `contribute` derives. Pre-sized (ADR-0505
/// §4): a `*` passes at most `bound.len()` names through and each other
/// item at most one pair, so this is an upper bound. The payload is the
/// projection's per-name fact: `()` for the name-presence folds
/// ([`projected_names`], [`projected_node_scope`]), a variable kind for
/// the kind fold (`validate::scope::projected_var_types`) — one
/// star/pacing policy for every projection.
pub(crate) fn project_with<V: Clone>(
    items: &[ReturnItem],
    bound: &IdentHashMap<SmallName, V>,
    contribute: impl Fn(&ReturnItem) -> Option<(SmallName, V)>,
) -> IdentHashMap<SmallName, V> {
    let mut out =
        IdentHashMap::with_capacity_and_hasher(bound.len() + items.len(), IdentBuildHasher);
    for item in items {
        if item.all {
            out.extend(
                bound
                    .iter()
                    .map(|(name, payload)| (name.clone(), payload.clone())),
            );
            continue;
        }
        if let Some((name, payload)) = contribute(item) {
            out.insert(name, payload);
        }
    }
    out
}

/// The **node-safe** names a projection carries forward: only a bare
/// variable that is already node-safe keeps node-ness (under its alias, if
/// any), and a `*` item passes the whole scope through. Any other projected
/// expression yields a value the executor will not reuse as an existing node
/// (`create_or_bind_node` reuses only `Value::Node` bindings), so a
/// `WITH o` over a may-be-null `OPTIONAL MATCH` binding does not make `o`
/// node-safe. Safe mode's label-less-write guardrail threads it instead of
/// [`projected_names`].
/// Redundant parens around the variable are transparent — the lowered item
/// is the bare variable, so `WITH (n)` carries node-ness exactly like
/// `WITH n` (LABS-1615/1616's paren-transparency family).
pub(crate) fn projected_node_scope(
    items: &[ReturnItem],
    bound: &IdentHashSet<SmallName>,
) -> IdentHashSet<SmallName> {
    // The name-presence wrapper over the one skeleton, with the
    // node-safe contribution: only a bare variable already in `bound`
    // keeps its alias.
    project_names(items, bound, |item| {
        item_variable(item).and_then(|name| {
            bound
                .contains(name)
                .then(|| item.alias.clone().unwrap_or_else(|| name.clone()))
        })
    })
}

/// The **node-safe** names a clause introduces: the pattern node variables of
/// `MATCH`/`CREATE`/`MERGE` only. `UNWIND`/`LOAD CSV` aliases, procedure
/// `YIELD`s and catalog columns bind values the executor will not reuse as a
/// node, so they are not node-safe (safe mode's label-less-write guardrail).
pub(crate) fn clause_node_names(clause: &Clause) -> IdentHashSet<SmallName> {
    let mut names = IdentHashSet::default();
    bind_node_name_refs(clause, &mut names);
    names
}

/// The per-clause node-name fold: the canonical per-clause node-name fact,
/// into a caller's set (a `SmallName` clone per name — no text render).
/// [`clause_node_names`] is the fresh-set form (safe mode's scope walker);
/// a scope-carrying validation walk rides this form directly, so no
/// per-clause table lands in the lowering path.
pub(crate) fn bind_node_name_refs(clause: &Clause, out: &mut IdentHashSet<SmallName>) {
    // The one clause→pattern-list mapping ([`clause_patterns`]) decides
    // which clauses bind node names; the fold below only walks them.
    let Some(patterns) = clause_patterns(clause) else {
        return;
    };
    bind_pattern_node_name_refs(patterns, &mut |name| {
        out.insert(name.clone());
    });
}

/// Names a single clause *introduces* into its body's scope, as one set:
/// pattern variables (plus a SEARCH clause's SCORE variable) for
/// MATCH/CREATE/MERGE, the alias for UNWIND/LOAD CSV, the alias-resolved
/// YIELD names for a procedure call, and the catalog columns for the
/// SHOW/TERMINATE family. The canonical per-clause introduction fact: the
/// composed-SHOW `WHERE` check, [`crate::validate::search`]'s SCORE-conflict
/// scope walk and `validate::expressions`'s catalog arm all derive from it.
/// `call_scope` derives every arm except its catalog arm, which iterates
/// [`crate::show_columns::catalog_output_columns`] directly — its ordered
/// `Vec` fixes the shadow-error order, which this set-typed fold cannot.
/// Consumers that
/// need *more* than introduction — a WITH replaces the scope (a projection,
/// [`crate::validate::search`]'s SCORE-conflict walk) and a `CALL { }` body's
/// exports depend on the enclosing scope — spell those arms themselves.
pub(crate) fn clause_introduced_names(clause: &Clause) -> IdentHashSet<SmallName> {
    let mut names = IdentHashSet::default();
    match clause {
        Clause::Match(mc) => {
            bind_pattern_vars(&mc.pattern, &mut names);
            if let Some(sv) = search_score_var(mc) {
                names.insert(sv.clone());
            }
        }
        Clause::With(wc) => {
            names = projected_names(&wc.items, &IdentHashSet::default());
        }
        Clause::Unwind(uw) => {
            names.insert(uw.alias.clone());
        }
        Clause::LoadCSV(lc) => {
            names.insert(lc.alias.clone());
        }
        Clause::Create(cc) => {
            bind_pattern_vars(&cc.pattern, &mut names);
        }
        Clause::Merge(mc) => {
            bind_pattern_vars(std::slice::from_ref(&mc.pattern), &mut names);
        }
        Clause::CallProcedure(cp) => {
            names.extend(cp.yields.effective_names());
        }
        clause if is_catalog_clause(clause) => {
            names.extend(catalog_output_names(clause));
        }
        _ => {}
    }
    names
}

mod scoped;

pub(crate) use scoped::{walk_expr_scoped, BinderStack, ScopedExprVisitor};

#[cfg(test)]
pub(crate) mod tests;
