//! The generic AST collectors every pass builds on: the variable and
//! pattern collectors, the output-column naming rule, the SEARCH
//! score-variable and distance-metric lookups, the shared expression-scope
//! check pair (`check_expr_vars_defined`/`check_name_defined`), the
//! scope-aware expression walker (`walk_expr_scoped`) and the UNION-chain
//! enumeration. They live below `validate`, `call_scope`,
//! `local_callables` and `lower` (all of which consume them downward) so
//! none of them owns another's helper. Aggregation-specific vocabulary
//! (`contains_aggregation`, the alias table) lives in [`crate::agg`]
//! instead, beside the names it tests.

use relate_names::SmallName;

use crate::ident_hash::{IdentBuildHasher, IdentHashSet};

use relate_error::EngineError;
use relate_parser::{
    Clause, Expr, InterpPart, LiteralValue, MatchClause, PatternElement, PatternPath, ReturnItem,
    Statement, UnionPart,
};

use super::tree::{nesting_depth_error, walk_expr_with};
use super::MAX_NESTING_DEPTH;
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

pub(crate) fn effective_name(item: &ReturnItem) -> Option<SmallName> {
    // The name arms hand the AST's already-classified `SmallName` straight
    // to the scope sets (ADR-0509 §1's wired shape). The `source_text` arm
    // is the one classification point here: an unaliased expression's
    // column name is its verbatim source text, which the parser carries as
    // text, not as a name — classified once, where it becomes a column.
    if let Some(alias) = &item.alias {
        Some(alias.clone())
    } else if let Expr::Variable(v) = &item.expr {
        Some(v.clone())
    } else if let Expr::MapProjection(mp) = &item.expr {
        // A map projection on a variable is a view of that variable and
        // carries the variable's name (matching the executor's
        // `Projection::output_name`), which is what a CALL subquery
        // exports for the `RETURN n {.prop}` shape.
        match &mp.expr {
            Expr::Variable(v) => Some(v.clone()),
            _ => source_text_name(item),
        }
    } else {
        source_text_name(item)
    }
}

/// An unaliased non-variable item's column name: its verbatim source text,
/// classified (see [`effective_name`]).
fn source_text_name(item: &ReturnItem) -> Option<SmallName> {
    item.source_text.as_deref().map(SmallName::from_name)
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
/// The edge filter of the scope-reach expression walk: whether a scope check
/// descends from `parent` into `child` ([`collect_expr_variables`] and the
/// post-projection subclause visibility rule both read the same reach, so a
/// construct that binds its own variables cannot leak them as outer
/// references).
pub(crate) fn expr_child_reaches_scope(parent: &Expr, child: &Expr) -> bool {
    match parent {
        // A dynamic property key (`n[<expr>]`) is not descended into; the
        // static value side always is.
        Expr::Property(p) => std::ptr::eq(child, &p.expr),
        // Skip the bare distance-metric keyword argument of
        // `vector_distance`/`vector_norm` (see `distance_metric_arg_index`).
        Expr::FunctionCall(fc) => match fc.name.with_str(distance_metric_arg_index) {
            Some(metric) => fc.args.get(metric).is_none_or(|a| !std::ptr::eq(child, a)),
            None => true,
        },
        Expr::Binary(_)
        | Expr::Comparison(_)
        | Expr::Unary(_)
        | Expr::List(_)
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
    let result = walk_expr_with(expr, &mut visit, enter_child);
    debug_assert!(
        result.is_ok(),
        "collect_expr_variables walked a tree that already passed the depth cap"
    );
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

/// The end of a statement's `NEXT` chain — its true terminal stage (CIP-213),
/// not its own first stage. This is the front's spelling of the cursor; the
/// parser has a private twin beside its `call_projects` screening (the two
/// should converge onto one `Statement`-level method — tracked with the
/// parser-side consolidation ticket).
pub(crate) fn tail_stage(mut stmt: &Statement) -> &Statement {
    while let Some(next) = &stmt.next {
        stmt = next.as_ref();
    }
    stmt
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
    // Pre-sized (ADR-0505 §4): a `*` passes at most `bound.len()` names
    // through and every other item contributes at most one name.
    let mut out =
        IdentHashSet::with_capacity_and_hasher(bound.len() + items.len(), IdentBuildHasher);
    for item in items {
        if item.all {
            out.extend(bound.iter().cloned());
            continue;
        }
        if let Some(name) = effective_name(item) {
            out.insert(name);
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
/// node-safe. Safe mode's label-less-write guardrail and the default
/// validation walk both thread it instead of [`projected_names`].
pub(crate) fn projected_node_scope(
    items: &[ReturnItem],
    bound: &IdentHashSet<SmallName>,
) -> IdentHashSet<SmallName> {
    // Pre-sized (ADR-0505 §4): same bound as [`projected_names`] — a `*`
    // passes at most the incoming scope through, each other item at most
    // one name. The fold clones the AST's `SmallName`s — no text render,
    // no re-classification (ADR-0509 §1).
    let mut out =
        IdentHashSet::with_capacity_and_hasher(bound.len() + items.len(), IdentBuildHasher);
    for item in items {
        if item.all {
            out.extend(bound.iter().cloned());
        } else if let Expr::Variable(name) = &item.expr {
            if bound.contains(name) {
                out.insert(item.alias.clone().unwrap_or_else(|| name.clone()));
            }
        }
    }
    out
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
    let patterns: &[PatternPath] = match clause {
        Clause::Match(mc) => &mc.pattern,
        Clause::Create(cc) => &cc.pattern,
        Clause::Merge(mc) => std::slice::from_ref(&mc.pattern),
        _ => return,
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

/// The scope-aware expression visit: the consumer owns what a *read* and a
/// *binder* mean for its rule — which reads count, what a binder shadows,
/// how a nested statement's own scope is modeled — while this module owns
/// the one table of which construct binds which name and where each scope
/// begins and ends. The scope-tracking validators implement the visit and
/// ride [`walk_expr_scoped`] instead of re-listing the expression child
/// edges beside the walker family (ADR-0513's 42I58 collector is the
/// first consumer).
pub(crate) trait ScopedExprVisitor {
    /// Pre-order visit of an expression node. An `Err` stops the whole
    /// walk and propagates.
    fn enter_expr(&mut self, expr: &Expr) -> Result<(), EngineError>;
    /// A name read outside expression position: a pattern variable of a
    /// pattern predicate, `shortestPath()` or pattern comprehension, or a
    /// map projection's bare variable selector.
    fn read_name(&mut self, name: &SmallName) -> Result<(), EngineError>;
    /// One binder's scope begins: the name is in scope until the matching
    /// [`unbind_name`](Self::unbind_name).
    fn bind_name(&mut self, name: &SmallName) -> Result<(), EngineError>;
    /// The most recently bound, still-open scope ends. The event stream is
    /// well-nested: this is always the youngest open binder.
    fn unbind_name(&mut self) -> Result<(), EngineError>;
    /// An `EXISTS`/`COUNT`/`COLLECT { … }` body. The consumer owns the
    /// statement-level scope model; the walker does not descend further.
    fn subquery_body(&mut self, statement: &Statement) -> Result<(), EngineError>;
}

/// Walk one expression with [`ScopedExprVisitor`] semantics: every
/// expression child edge the plain expression walker reaches, plus the
/// scope-introducing constructs' binder events — a comprehension's binders
/// scope their WHERE and projection (the list is outside), a map
/// comprehension's key/value binders scope theirs, `reduce`/`allReduce`
/// scope their body (and `allReduce`'s predicate), a quantifier's binder
/// scopes its predicate, and a pattern comprehension's pattern names are
/// read and then bound around its WHERE and projection. A subquery body is
/// handed to [`subquery_body`](ScopedExprVisitor::subquery_body) instead
/// of being descended. Three edges are deliberately not reads of an
/// entity, so they are not walked: the distance-metric keyword argument of
/// `vector_distance`/`vector_norm` (the one
/// [`distance_metric_arg_index`] table every variable reach consults), a
/// property's dynamic-key expression, and `hasLabel`'s label expression.
/// A pattern predicate's and `shortestPath()`'s embedded expressions are
/// likewise the pattern walk's own reach — only their pattern variables
/// are read here. Nesting is capped at [`MAX_NESTING_DEPTH`] from the
/// walk's root with the one shared
/// [`nesting_depth_error`](super::tree::nesting_depth_error); a nested
/// statement's clause roots each start a fresh walk, so a body's
/// expressions restart the count exactly as the per-expression walks
/// always did.
pub(crate) fn walk_expr_scoped(
    expr: &Expr,
    visitor: &mut dyn ScopedExprVisitor,
) -> Result<(), EngineError> {
    walk_expr_scoped_at(expr, 0, visitor)
}

fn walk_expr_scoped_at(
    expr: &Expr,
    depth: usize,
    visitor: &mut dyn ScopedExprVisitor,
) -> Result<(), EngineError> {
    use relate_parser::MapProjectionItem;
    if depth > MAX_NESTING_DEPTH {
        return Err(nesting_depth_error());
    }
    visitor.enter_expr(expr)?;
    match expr {
        Expr::Literal(_)
        | Expr::Variable(_)
        | Expr::Parameter(_)
        | Expr::CountStar
        | Expr::Star
        | Expr::OptionValuePlaceholder => Ok(()),
        Expr::StringInterp(parts) => {
            for part in parts {
                if let InterpPart::Interp(embed) = part {
                    walk_expr_scoped_at(embed, depth + 1, visitor)?;
                }
            }
            Ok(())
        }
        // The projection's base and key/value selectors are ordinary
        // expression children; a bare `Variable` selector pulls a
        // same-named variable from the surrounding scope, so it is a read
        // of that variable.
        Expr::MapProjection(projection) => {
            walk_expr_scoped_at(&projection.expr, depth + 1, visitor)?;
            for item in &projection.items {
                match item {
                    MapProjectionItem::Variable(name) => visitor.read_name(name),
                    MapProjectionItem::KeyValue(_, value) => {
                        walk_expr_scoped_at(value, depth + 1, visitor)
                    }
                    MapProjectionItem::AllProperties | MapProjectionItem::Property(_) => Ok(()),
                }?;
            }
            Ok(())
        }
        // The list is outside the binder's scope; the WHERE and the
        // projection are inside it.
        Expr::ListComprehension(comprehension) => {
            walk_expr_scoped_at(&comprehension.list_expr, depth + 1, visitor)?;
            visitor.bind_name(&comprehension.variable)?;
            if let Some(where_expr) = &comprehension.where_expr {
                walk_expr_scoped_at(where_expr, depth + 1, visitor)?;
            }
            if let Some(map_expr) = &comprehension.map_expr {
                walk_expr_scoped_at(map_expr, depth + 1, visitor)?;
            }
            visitor.unbind_name()
        }
        Expr::MapComprehension(comprehension) => {
            walk_expr_scoped_at(&comprehension.source_expr, depth + 1, visitor)?;
            let binds = 1 + usize::from(comprehension.key_var.is_some());
            if let Some(key_var) = &comprehension.key_var {
                visitor.bind_name(key_var)?;
            }
            visitor.bind_name(&comprehension.value_var)?;
            if let Some(where_expr) = &comprehension.where_expr {
                walk_expr_scoped_at(where_expr, depth + 1, visitor)?;
            }
            walk_expr_scoped_at(&comprehension.key_expr, depth + 1, visitor)?;
            walk_expr_scoped_at(&comprehension.value_expr, depth + 1, visitor)?;
            for _ in 0..binds {
                visitor.unbind_name()?;
            }
            Ok(())
        }
        // The init and the list are outside the binders' scope; the body
        // (and `allReduce`'s predicate) are inside it.
        Expr::Reduce(reduce) => {
            walk_expr_scoped_at(&reduce.init_expr, depth + 1, visitor)?;
            walk_expr_scoped_at(&reduce.list_expr, depth + 1, visitor)?;
            visitor.bind_name(&reduce.accumulator)?;
            visitor.bind_name(&reduce.variable)?;
            walk_expr_scoped_at(&reduce.expression, depth + 1, visitor)?;
            visitor.unbind_name()?;
            visitor.unbind_name()
        }
        Expr::AllReduce(reduce) => {
            walk_expr_scoped_at(&reduce.init_expr, depth + 1, visitor)?;
            walk_expr_scoped_at(&reduce.list_expr, depth + 1, visitor)?;
            visitor.bind_name(&reduce.accumulator)?;
            visitor.bind_name(&reduce.variable)?;
            walk_expr_scoped_at(&reduce.expression, depth + 1, visitor)?;
            walk_expr_scoped_at(&reduce.predicate, depth + 1, visitor)?;
            visitor.unbind_name()?;
            visitor.unbind_name()
        }
        Expr::Quantifier(quantifier) => {
            walk_expr_scoped_at(&quantifier.list, depth + 1, visitor)?;
            visitor.bind_name(&quantifier.variable)?;
            if let Some(predicate) = &quantifier.predicate {
                walk_expr_scoped_at(predicate, depth + 1, visitor)?;
            }
            visitor.unbind_name()
        }
        // A pattern variable that is already bound is a reference, not a
        // fresh binding; the pattern's own names then shadow its WHERE and
        // projection. The pattern's embedded expressions are the pattern
        // walk's own reach, exactly as the scope-aware check pair treats
        // them.
        Expr::PatternComprehension(comprehension) => {
            let names = pattern_vars(std::slice::from_ref(&comprehension.pattern));
            for name in &names {
                visitor.read_name(name)?;
            }
            for name in &names {
                visitor.bind_name(name)?;
            }
            if let Some(where_expr) = &comprehension.where_expr {
                walk_expr_scoped_at(where_expr, depth + 1, visitor)?;
            }
            walk_expr_scoped_at(&comprehension.map_expr, depth + 1, visitor)?;
            for _ in &names {
                visitor.unbind_name()?;
            }
            Ok(())
        }
        Expr::PatternPredicate(predicate) => {
            for name in pattern_vars(std::slice::from_ref(&predicate.pattern)) {
                visitor.read_name(name)?;
            }
            Ok(())
        }
        Expr::ShortestPath(shortest) => {
            for name in pattern_vars(std::slice::from_ref(&shortest.pattern)) {
                visitor.read_name(name)?;
            }
            Ok(())
        }
        // A subquery body reads the enclosing scope: a name it mentions is
        // the outer entity, exactly as a `MATCH` on it inside the subquery
        // would be. The body's own scope model is the visitor's.
        Expr::ExistsSubquery(subquery) => visitor.subquery_body(&subquery.statement),
        Expr::CountSubquery(subquery) => visitor.subquery_body(&subquery.statement),
        Expr::CollectSubquery(subquery) => visitor.subquery_body(&subquery.statement),
        Expr::List(items) => {
            for item in items {
                walk_expr_scoped_at(item, depth + 1, visitor)?;
            }
            Ok(())
        }
        Expr::Map(pairs) => {
            for pair in pairs {
                walk_expr_scoped_at(&pair.value, depth + 1, visitor)?;
            }
            Ok(())
        }
        // The dynamic-key form's key expression is not a read of an entity.
        Expr::Property(access) => walk_expr_scoped_at(&access.expr, depth + 1, visitor),
        Expr::FunctionCall(call) => {
            let skip = call.name.with_str(distance_metric_arg_index);
            for (index, arg) in call.args.iter().enumerate() {
                if skip != Some(index) {
                    walk_expr_scoped_at(arg, depth + 1, visitor)?;
                }
            }
            Ok(())
        }
        Expr::Binary(binary) => {
            walk_expr_scoped_at(&binary.left, depth + 1, visitor)?;
            walk_expr_scoped_at(&binary.right, depth + 1, visitor)
        }
        Expr::Unary(unary) => walk_expr_scoped_at(&unary.expr, depth + 1, visitor),
        Expr::Comparison(comparison) => {
            walk_expr_scoped_at(&comparison.left, depth + 1, visitor)?;
            walk_expr_scoped_at(&comparison.right, depth + 1, visitor)
        }
        Expr::In(in_expr) => {
            walk_expr_scoped_at(&in_expr.expr, depth + 1, visitor)?;
            walk_expr_scoped_at(&in_expr.list, depth + 1, visitor)
        }
        Expr::IsNull(check) => walk_expr_scoped_at(&check.expr, depth + 1, visitor),
        Expr::IsType(check) => walk_expr_scoped_at(&check.expr, depth + 1, visitor),
        Expr::IsNormalized(check) => walk_expr_scoped_at(&check.expr, depth + 1, visitor),
        Expr::ChronologicalCompare(comparison) => {
            walk_expr_scoped_at(&comparison.left, depth + 1, visitor)?;
            walk_expr_scoped_at(&comparison.right, depth + 1, visitor)
        }
        Expr::StringMatch(match_expr) => {
            walk_expr_scoped_at(&match_expr.left, depth + 1, visitor)?;
            walk_expr_scoped_at(&match_expr.right, depth + 1, visitor)
        }
        Expr::Case(case) => {
            for when in &case.whens {
                walk_expr_scoped_at(&when.condition, depth + 1, visitor)?;
                walk_expr_scoped_at(&when.result, depth + 1, visitor)?;
            }
            if let Some(else_expr) = &case.else_expr {
                walk_expr_scoped_at(else_expr, depth + 1, visitor)?;
            }
            Ok(())
        }
        Expr::Subscript(subscript) => {
            walk_expr_scoped_at(&subscript.expr, depth + 1, visitor)?;
            if let Some(index) = &subscript.index {
                walk_expr_scoped_at(index, depth + 1, visitor)?;
            }
            if let Some(from) = &subscript.from {
                walk_expr_scoped_at(from, depth + 1, visitor)?;
            }
            if let Some(to) = &subscript.to {
                walk_expr_scoped_at(to, depth + 1, visitor)?;
            }
            Ok(())
        }
        // The label expression is the dynamic-label half's own walk; only
        // the base expression is a read here.
        Expr::HasLabel(has_label) => walk_expr_scoped_at(&has_label.expr, depth + 1, visitor),
        Expr::Paren(inner) => walk_expr_scoped_at(inner, depth + 1, visitor),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tree::tests::return_expr_of;
    use super::*;
    use crate::agg::contains_aggregation;

    #[test]
    fn collect_expr_variables_matches_historical_reach() {
        // The walker-based collector must visit exactly the node set the
        // hand-rolled recursion did.
        let collected = |query: &str| {
            let e = return_expr_of(query);
            let mut vars = IdentHashSet::default();
            collect_expr_variables(&e, &mut vars);
            vars
        };
        // Plain combinators are descended into.
        let vars = collected("RETURN a.prop + (b IN [c]) AND CASE WHEN d THEN e ELSE f END");
        for v in ["a", "b", "c", "d", "e", "f"] {
            assert!(vars.contains(&SmallName::from(v)), "missing `{v}`");
        }
        // Scope-introducing constructs are NOT descended into at all — not
        // even their list expressions (historical semantics).
        let vars = collected("RETURN [x IN n.list | x + q]");
        assert!(vars.is_empty(), "comprehension leaked: {vars:?}");
        let vars = collected("RETURN reduce(s = i, y IN m.list | s + y)");
        assert!(vars.is_empty(), "reduce leaked: {vars:?}");
        // CIP-214: the bare distance-metric keyword is not a variable.
        let vars = collected("RETURN vector_distance(v1, v2, EUCLIDEAN)");
        assert!(vars.contains(&SmallName::from("v1")) && vars.contains(&SmallName::from("v2")));
        assert!(
            !vars.contains(&SmallName::from("EUCLIDEAN")),
            "metric keyword collected"
        );
        // A dynamic property KEY (`n[<expr>]`) is not descended into; the
        // value side is. (Only SET/REMOVE targets parse key_expr, so build
        // the node directly.)
        let e = Expr::Property(Box::new(relate_parser::PropertyAccess {
            expr: Expr::Variable(SmallName::from_name("n")),
            property: SmallName::from_name(""),
            key_expr: Some(Expr::Variable(SmallName::from_name("k"))),
        }));
        let mut vars = IdentHashSet::default();
        collect_expr_variables(&e, &mut vars);
        assert!(vars.contains(&SmallName::from("n")));
        assert!(
            !vars.contains(&SmallName::from("k")),
            "dynamic property key collected"
        );
    }

    #[test]
    fn contains_aggregation_matches_historical_reach() {
        // The walker-based check must look through exactly the combinators
        // the hand-rolled recursion did: function args, binary/unary/
        // comparison operands, parens, property values — and nothing else.
        assert!(contains_aggregation(&return_expr_of("RETURN count(*)")));
        assert!(contains_aggregation(&return_expr_of("RETURN 1 + count(*)")));
        assert!(contains_aggregation(&return_expr_of(
            "RETURN abs((sum(x)))"
        )));
        assert!(contains_aggregation(&return_expr_of(
            "RETURN n.p < max(m.q)"
        )));
        // Aggregates inside a list literal, CASE arm, IN operand or
        // comprehension are NOT "contained" (historical verdict); a
        // subscript/slice *index or bound* is not walked either, but the
        // postfix *base* is.
        assert!(!contains_aggregation(&return_expr_of("RETURN [count(*)]")));
        assert!(!contains_aggregation(&return_expr_of(
            "RETURN CASE WHEN true THEN count(*) ELSE 0 END"
        )));
        assert!(!contains_aggregation(&return_expr_of("RETURN n[count(*)]")));
        assert!(!contains_aggregation(&return_expr_of(
            "RETURN 1 IN [count(*)]"
        )));
        assert!(!contains_aggregation(&return_expr_of(
            "RETURN [x IN [1] | count(*)]"
        )));
        // Aggregates in the postfix base of a subscript or slice ARE
        // "contained" (the walker descends into the base, not the
        // index/bounds).
        assert!(contains_aggregation(&return_expr_of(
            "RETURN collect(x)[..1]"
        )));
        assert!(contains_aggregation(&return_expr_of(
            "RETURN collect(x)[0]"
        )));
        // Nor one hiding in a dynamic property key.
        let e = Expr::Property(Box::new(relate_parser::PropertyAccess {
            expr: Expr::Variable(SmallName::from_name("n")),
            property: SmallName::from_name(""),
            key_expr: Some(Expr::CountStar),
        }));
        assert!(!contains_aggregation(&e));
    }
}
