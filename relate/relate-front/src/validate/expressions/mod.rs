//! Expression-position rules: pattern predicates in WHERE, patterns in
//! RETURN/WITH projections and SET values, `labels()`/`type()` and
//! property access on statically-known path/node variables (one
//! static-kind rule family, run at every expression position this walk
//! validates), and undefined-variable checks for the expression
//! positions they appear in. The DELETE-target admission policy lives in
//! its own concern module, [`delete`].

use crate::ident_hash::IdentHashSet;
use relate_names::SmallName;

use relate_error::{EngineError, ErrorKind};
use relate_parser::{Clause, Expr, PatternElement, Statement};

use super::delete::check_delete_expr;
use super::scope::{leading_import_form, ExprScope};
use super::writes::{walk_write_elements, WriteClause};
use crate::agg::contains_aggregation;
use crate::ast::collect::{
    check_expr_vars_defined, check_name_defined, clause_introduced_names, interp_embeds,
    union_chain, walk_expr_scoped, BinderStack, ScopedExprVisitor,
};
use crate::ast::tree::{walk_clause_expr_roots, walk_expr, walk_with_expr_positions};

/// Validate expression-position constraints:
/// - Pattern predicates in WHERE must not introduce new (unbound) variables for relationships
///   or nodes that aren't already in scope.
/// - A single-node pattern predicate like `(n)` is not valid (InvalidArgumentType).
/// - Pattern predicates are legal only in the homes the pattern-position
///   policy names (predicate positions, a comprehension's WHERE filter,
///   the legacy `exists(<pattern>)` argument) and rejected in every value
///   position — a RETURN/WITH projection item, a SET value (the policy
///   lives in [`super::pattern`]).
/// - size() on a pattern predicate is invalid (UnexpectedSyntax, owned by
///   [`super::pattern`]).
/// - The static-kind rule family ([`check_static_kind_rules`]: property
///   access and `labels()` on a statically-known path variable,
///   `type()` on a statically-known node variable) runs at every
///   expression position this walk validates — the WITH/RETURN item
///   lists, the MATCH's element-pattern, path-pattern and own WHEREs, and
///   the WITH's WHERE against its projection view — never a per-position
///   subset.
/// - DELETE targets are admitted by [`super::delete::check_delete_expr`]'s
///   one stated rule, and MERGE/CREATE patterns reject element-pattern
///   predicates.
pub fn validate_expression_constraints(stmt: &Statement) -> Result<(), EngineError> {
    let mut scope = ExprScope::default();
    validate_expression_constraints_in_clauses(&stmt.clauses, &mut scope)
}

/// The clause-list walk behind [`validate_expression_constraints`], factored
/// out so a CALL-subquery body and an expression subquery (`COUNT`/`EXISTS`/
/// `COLLECT`) are checked against their own enclosing scope with the same
/// rules — a reference to an outer variable the body does not import is the
/// front end's `UndefinedVariable`, not a silently bound null (the CALL
/// boundary rides the carrier's outer names).
fn validate_expression_constraints_in_clauses(
    clauses: &[Clause],
    scope: &mut ExprScope,
) -> Result<(), EngineError> {
    for clause in clauses {
        // A clause's pattern variables bind here — names and kinds
        // together, the one pattern walk, plus a MATCH's SEARCH score
        // variable — before the arm below runs, so a MATCH's own WHERE
        // check, every later clause's, and the nested bodies inside the
        // clause's expressions all see the classification. A clause with
        // no pattern binds nothing.
        scope.bind_clause(clause)?;
        validate_subquery_expressions(clause, scope)?;
        match clause {
            Clause::Match(mc) => {
                validate_match_clause(mc, scope)?;
            }
            Clause::With(wc) => {
                validate_with_clause(wc, scope)?;
            }
            Clause::Return(rc) => {
                validate_return_clause(rc, scope)?;
            }
            Clause::Unwind(uw) => {
                validate_unwind_clause(uw, scope.names_mut())?;
            }
            Clause::LoadCSV(lc) => {
                validate_loadcsv_clause(lc, scope.names_mut())?;
            }
            Clause::Create(cc) => {
                validate_create_clause(cc, scope.names_mut())?;
            }
            Clause::Set(sc) => {
                validate_set_clause(sc, scope.names_mut())?;
            }
            Clause::Remove(rc) => {
                validate_remove_clause(rc, scope.names())?;
            }
            Clause::Delete(dc) => {
                validate_delete_clause(dc, scope.names_mut())?;
            }
            Clause::CallProcedure(cp) => {
                validate_callprocedure_clause(cp, scope.names_mut())?;
            }
            // The row-producing catalog commands, through the one catalog
            // classification (the same guard the scope walkers use): each
            // introduces its effective output columns into scope.
            c if crate::show_columns::is_catalog_clause(c) => {
                scope.names_mut().extend(clause_introduced_names(c));
            }
            Clause::CallSubquery(csq) => {
                // The body is checked against the scope *before* the CALL's
                // own exports join it.
                validate_call_body(csq, scope)?;
                validate_callsubquery_clause(csq, scope.names_mut())?;
            }
            Clause::Merge(mc) => {
                validate_merge_clause(mc, scope.names_mut())?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// The scope-aware expression visitor behind [`validate_subquery_expressions`]:
/// it validates each subquery body inline, against the seed scope its
/// expression position actually has plus the comprehension/quantifier/
/// `reduce` binders open where it sits (REL8-2028). The binder events carry
/// no rule of their own, so the shared [`BinderStack`] only records them.
struct SubqueryExprVisitor {
    seed: ExprScope,
    binders: BinderStack,
}

impl SubqueryExprVisitor {
    fn new(seed: ExprScope) -> Self {
        Self {
            seed,
            binders: BinderStack::default(),
        }
    }
}

impl ScopedExprVisitor for SubqueryExprVisitor {
    // `enter_expr` is the one required event; this consumer's rule has no
    // shape inside an expression, only the bodies it validates.
    fn enter_expr(&mut self, _expr: &Expr) -> Result<(), EngineError> {
        Ok(())
    }

    fn bind_name(&mut self, name: &SmallName) -> Result<(), EngineError> {
        self.binders.bind(name);
        Ok(())
    }

    fn unbind_name(&mut self) -> Result<(), EngineError> {
        self.binders.unbind();
        Ok(())
    }

    fn subquery_body(&mut self, statement: &Statement) -> Result<(), EngineError> {
        let mut inner = self.seed.clone();
        // The binders open at the body (a comprehension's element, a
        // quantifier's or `reduce`'s variables) are in scope for it.
        inner
            .names_mut()
            .extend(self.binders.names().iter().cloned());
        validate_expression_constraints_in_clauses(&statement.clauses, &mut inner)
    }
}

/// Walk one clause's expression roots with the scope-aware visitor, which
/// validates every subquery body it meets from the scope that expression
/// position actually sees: the clause's expression view (the input plus the
/// clause's own bindings, plus a WITH's projection for its WHERE and ORDER
/// BY) — except the one input-sensitive position, a WITH's projection items,
/// whose bodies seed from the clause's input (the items evaluate against it,
/// never against the projection their sibling items define). The CALL
/// boundary is cleared either way: an expression subquery sees the enclosing
/// row, no boundary to enforce.
fn validate_subquery_expressions(clause: &Clause, scope: &ExprScope) -> Result<(), EngineError> {
    match clause {
        // The position-sensitive clause: the projection items under the
        // input, the WHERE/ORDER BY/SKIP/LIMIT expressions under the
        // projection view.
        Clause::With(wc) => {
            let mut items = SubqueryExprVisitor::new(scope.subquery_item_seed());
            let mut rest = SubqueryExprVisitor::new(scope.subquery_body_seed(clause));
            walk_with_expr_positions(
                wc,
                &mut |expr| walk_expr_scoped(expr, &mut items),
                &mut |expr| walk_expr_scoped(expr, &mut rest),
            )?;
        }
        // Every other clause's expressions all read one scope: the
        // post-bind carrier (its expression view).
        _ => {
            let mut visitor = SubqueryExprVisitor::new(scope.subquery_body_seed(clause));
            let mut roots =
                |expr: &Expr| -> Result<(), EngineError> { walk_expr_scoped(expr, &mut visitor) };
            walk_clause_expr_roots(clause, &mut roots)?;
        }
    }
    Ok(())
}

/// The CALL-subquery body's expression rules under live 2026.10's import
/// semantics: a scope-clause body sees exactly its imported names; a naked
/// `CALL { … }` body sees only what each UNION arm's leading importing
/// `WITH` names (nothing at all when the arm has none). Without this, an
/// outer name the body never imported resolved to the outer row and a query
/// live Neo4j rejects (`Variable `x` not defined`) ran silently.
fn validate_call_body(
    csq: &relate_parser::CallSubqueryClause,
    caller: &ExprScope,
) -> Result<(), EngineError> {
    use relate_parser::CallScope;
    for arm in union_chain(&csq.statement) {
        // The scope clause decides the arm's whole visibility mode in one
        // match: which names it starts from, and whether they stay visible
        // past a body `WITH` (a scope clause's imports do, REL8-2023 issue
        // 51; the naked form's importing `WITH` is an ordinary projection,
        // so its imports end at the next `WITH`).
        let (seed_names, keep_visible): (IdentHashSet<SmallName>, bool) = match &csq.scope {
            // A scope clause's imports stay visible for the whole body,
            // so a later WITH dropping one is not a leak; the planner's
            // binder owns unimported names there.
            CallScope::Vars(vars) => (vars.iter().cloned().collect(), true),
            CallScope::Wildcard => (caller.names().clone(), true),
            // The importing WITH is evaluated against the outer row and
            // then replaces the scope; an arm without one imports
            // nothing, and an outer name read past either boundary is
            // the leak the carrier's boundary rule rejects. The leading
            // import is the one USE-skipping probe
            // ([`leading_import_form`]), the same value the CIP-218
            // collision check reads.
            CallScope::None => {
                let names = if leading_import_form(&arm.clauses).is_some() {
                    caller.names().clone()
                } else {
                    IdentHashSet::default()
                };
                (names, false)
            }
        };
        // One carrier for the whole arm: the stage chain (`NEXT` stages)
        // threads it exactly as the name scope threads, so a name an
        // earlier stage classified — a WITH's carried alias — keeps its
        // classification in every later stage's checks. The seed derives
        // from one source: the arm's visible names, with the caller's
        // kinds for exactly those names. Only a naked body's arms arm
        // the import boundary (a scope clause's imports stay visible for
        // the whole body; the planner's binder owns unimported names
        // there).
        let mut scope = ExprScope::seeded(seed_names, caller);
        if keep_visible {
            scope.keep_visible();
        } else {
            scope.carry_outer_boundary(caller.names().clone());
        }
        let mut stage = arm;
        loop {
            validate_expression_constraints_in_clauses(&stage.clauses, &mut scope)?;
            match &stage.next {
                Some(next) => stage = next,
                None => break,
            }
        }
    }
    Ok(())
}

/// The Match clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope carrier. The pattern's names and
/// kinds (and the SEARCH score variable) are already bound — the
/// dispatch's pre-arm step — so the arm only checks.
fn validate_match_clause(
    mc: &relate_parser::MatchClause,
    scope: &mut ExprScope,
) -> Result<(), EngineError> {
    // Collect bound variables from match pattern
    //
    // The same path variable cannot be assigned to more than one
    // top-level path pattern of a single MATCH clause's
    // comma-separated graph pattern: a path variable is always an
    // unconditional singleton and cannot appear in a later constituent
    // of the conjunction.
    let mut path_vars_this_match: IdentHashSet<SmallName> = IdentHashSet::default();
    for path in &mc.pattern {
        for name in path.path_names() {
            if !path_vars_this_match.insert(name.clone()) {
                return Err(name.with_str(EngineError::duplicate_variable));
            }
        }
    }
    // Validate element-pattern WHERE clauses, e.g. `(n WHERE
    // n.age > 30)` / `[r WHERE r.p > 0]` (CIP-17/CIP-18). Scope
    // was fully populated above from every path in this MATCH
    // clause before this check runs, so peer element variables
    // are visible -- including forward references to a variable
    // declared later in the same path (per CIP-17/18 5.2.1) --
    // as well as variables from prior clauses.
    for path in &mc.pattern {
        for elem in &path.elements {
            // A `Quantified` element's own per-iteration WHERE
            // (and any element-pattern WHERE inside its operand)
            // is deliberately not scope-checked here: the
            // operand's local group variables are visible only
            // inside it. The planner's quantified-predicate
            // binding (`NonLocalGroupVariableReference`/
            // `UndefinedVariable` in `relate-planner`)
            // checks those on the lowered IR.
            let where_expr = match elem {
                PatternElement::Node(np) => np.where_expr.as_ref(),
                PatternElement::Relationship(rp) => rp.where_expr.as_ref(),
                PatternElement::Quantified(_) => None,
            };
            if let Some(where_expr) = where_expr {
                check_expr_vars_defined(where_expr, scope.names())?;
                check_static_kind_rules(where_expr, scope)?;
            }
        }
        // Validate the parenthesized path pattern's own WHERE,
        // e.g. `((n)-[r]->(m) WHERE n.p > m.q)`.
        // It may reference element variables local to the same
        // path, variables from a prior clause, or unconditional
        // singleton variables of the same MATCH -- all already
        // present in `scope` -- but not an undeclared variable.
        if let Some(where_expr) = &path.where_expr {
            check_expr_vars_defined(where_expr, scope.names())?;
            check_static_kind_rules(where_expr, scope)?;
        }
    }
    // Validate WHERE expression. The carrier arrives with this clause's
    // own pattern entities already classified (the dispatch's pre-arm
    // bind step), so the gate and the kind rules read the full
    // classification.
    if let Some(where_expr) = &mc.where_expr {
        super::pattern::validate_expr_in_where(where_expr, scope)?;
        // Check for undefined variables in WHERE
        check_expr_vars_defined(where_expr, scope.names())?;
        check_static_kind_rules(where_expr, scope)?;
    }
    Ok(())
}

/// The projection-item policy a WITH and a RETURN share, spelled once:
/// no pattern predicate in a value position, no unimported outer
/// reference inside a CALL-subquery body (the carrier's boundary rule),
/// and the static-kind rule family — the same three kind rules as every
/// other expression position. `position` names the value position in the
/// pattern rejection text (`WITH projection`, `RETURN projection`) and
/// is the only per-clause input.
fn check_projection_items(
    items: &[relate_parser::ReturnItem],
    position: &str,
    scope: &ExprScope,
) -> Result<(), EngineError> {
    for item in items {
        if !item.all {
            super::pattern::check_no_pattern_in_value_position(&item.expr, position)?;
            // Inside a CALL-subquery body, an outer name the body never
            // imported must not fall through to the enclosing row.
            scope.reject_unimported_outer_refs(&item.expr)?;
            check_static_kind_rules(&item.expr, scope)?;
        }
    }
    Ok(())
}

/// The With clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope carrier.
fn validate_with_clause(
    wc: &relate_parser::WithClause,
    scope: &mut ExprScope,
) -> Result<(), EngineError> {
    // The projection items, against the input's kinds — the
    // classification the items themselves are read under.
    check_projection_items(&wc.items, "WITH projection", scope)?;
    // The projection's committed carrier, derived once before the WHERE
    // reads it — the WHERE's same-clause-alias view and the tail's commit
    // below share one derivation, so the two can never disagree.
    let projected = scope.projected(&wc.items);
    // WITH WHERE can have pattern predicates, checked against the WHERE's
    // view: the clause's input plus this projection's own output — a
    // same-clause alias is referenceable exactly as it is to every clause
    // after this one (the projection resets the scope only from the next
    // clause on). The kind rules read the same view, and the nested
    // bodies inside the WHERE seed from it
    // ([`ExprScope::expression_view`], the same policy).
    if let Some(where_expr) = &wc.where_expr {
        let where_scope = scope.with_projection(&projected);
        super::pattern::validate_expr_in_where(where_expr, &where_scope)?;
        check_static_kind_rules(where_expr, &where_scope)?;
    }
    // Commit the projection: the scope the following clauses see, names
    // and kinds together.
    *scope = projected;
    Ok(())
}

/// The Return clause's expression constraints: the body of the clause
/// dispatch above, reading the committed scope carrier (RETURN binds
/// nothing).
fn validate_return_clause(
    rc: &relate_parser::ReturnClause,
    scope: &ExprScope,
) -> Result<(), EngineError> {
    check_projection_items(&rc.items, "RETURN projection", scope)
}

/// The Unwind clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_unwind_clause(
    uw: &relate_parser::UnwindClause,
    scope: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    scope.insert(uw.alias.clone());
    Ok(())
}

/// The LoadCSV clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_loadcsv_clause(
    lc: &relate_parser::LoadCSVClause,
    scope: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    scope.insert(lc.alias.clone());
    Ok(())
}

/// The Create clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_create_clause(
    cc: &relate_parser::CreateClause,
    scope: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    for path in &cc.pattern {
        if let Some(pv) = &path.variable {
            scope.insert(pv.clone());
        }
        walk_write_elements(&path.elements, WriteClause::Create, scope)?;
    }
    Ok(())
}

/// The Set clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_set_clause(
    sc: &relate_parser::SetClause,
    scope: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // The one SET-item rule, shared with MERGE's ON CREATE/ON MATCH SET.
    check_set_items(&sc.items, scope)
}

/// The Delete clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_delete_clause(
    dc: &relate_parser::DeleteClause,
    scope: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // Check undefined variables and invalid argument types in DELETE
    for expr in &dc.expressions {
        // Check that DELETE expressions are variables, property access, or subscripts
        // Arithmetic expressions like `1 + 1` are not valid
        check_delete_expr(expr)?;
        // Check for undefined root variable in DELETE expression
        if let Some(root_var) = find_root_variable(expr) {
            check_name_defined(&root_var, scope)?;
        }
    }
    Ok(())
}

/// The CallProcedure clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_callprocedure_clause(
    cp: &relate_parser::CallProcedureClause,
    scope: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // Aggregation functions are not allowed in procedure arguments
    for arg in &cp.args {
        if contains_aggregation(arg) {
            return Err(EngineError::semantic(
                ErrorKind::InvalidAggregation,
                "aggregation functions are not allowed in procedure call arguments".to_string(),
            ));
        }
    }
    // Add yielded variables to scope (the effective post-alias
    // name comes from the one YieldList model)
    for name in cp.yields.effective_names() {
        scope.insert(name.clone());
    }
    Ok(())
}

/// The CallSubquery clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_callsubquery_clause(
    csq: &relate_parser::CallSubqueryClause,
    scope: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // CIP-141/CIP-218: what a CALL clause exports depends on
    // its scope clause (a scope-clause `RETURN *` excludes
    // imported names per the star exclusion); delegate to the shared
    // analysis rather than assuming `RETURN *` always inherits
    // outer scope (only true for a naked `CALL { ... }`).
    let exports = crate::call_scope::call_subquery_exports(csq, scope);
    scope.extend(exports);
    // `REPORT STATUS AS <var>` binds `<var>` into scope
    // for every clause after the CALL (newness of the name
    // itself is checked separately by `validate_variable_types`).
    if let Some(cit) = &csq.in_transactions {
        if let Some(var) = &cit.report_status {
            scope.insert(var.clone());
        }
        // A `DISJOINT BY` key partitions the batch's rows by
        // value, which only holds when the key is deterministic per row —
        // `randomUUID()` would put every row in its own partition (42N7B).
        if let Some(relate_parser::DisjointBy::Expressions(keys)) = &cit.disjoint {
            if keys
                .iter()
                .any(super::aggregation::contains_non_deterministic)
            {
                return Err(EngineError::semantic(
                    ErrorKind::NonDeterministicDisjointBy,
                    "'DISJOINT BY' expressions must be deterministic",
                ));
            }
        }
    }
    Ok(())
}

/// The Merge clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_merge_clause(
    mc: &relate_parser::MergeClause,
    scope: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    reject_merge_subquery_expressions(mc)?;
    // Add MERGE pattern variables to scope
    if let Some(pv) = &mc.pattern.variable {
        scope.insert(pv.clone());
    }
    walk_write_elements(&mc.pattern.elements, WriteClause::Merge, scope)?;
    // The ON CREATE SET / ON MATCH SET checks are one rule
    // over two item lists (the clause's own two fields).
    check_set_items(&mc.on_create, scope)?;
    check_set_items(&mc.on_match, scope)?;
    Ok(())
}

/// 42001: a MERGE clause admits no subquery expression — neither in a
/// pattern element's property map nor in an `ON CREATE`/`ON MATCH SET` value
/// (live 2026.10: "Subquery expressions are not allowed in a MERGE clause.").
/// One walk over both value positions, so the rule cannot drift between them.
fn reject_merge_subquery_expressions(mc: &relate_parser::MergeClause) -> Result<(), EngineError> {
    // A pattern comprehension is a legal property value inside a MERGE
    // pattern (live 2026.10 evaluates it before the merge), but not inside
    // an ON CREATE/ON MATCH SET value; an EXISTS/COUNT/COLLECT subquery is
    // rejected in both positions.
    let check = |expr: &Expr, set_value: bool| -> Result<(), EngineError> {
        let mut found = false;
        walk_expr(expr, &mut |node| {
            found |= matches!(
                node,
                Expr::ExistsSubquery(_) | Expr::CountSubquery(_) | Expr::CollectSubquery(_)
            ) || (set_value && matches!(node, Expr::PatternComprehension(_)));
            Ok(())
        })?;
        if found {
            return Err(EngineError::semantic(
                ErrorKind::UnexpectedSyntax,
                "Subquery expressions are not allowed in a MERGE clause.".to_string(),
            ));
        }
        Ok(())
    };
    for elem in &mc.pattern.elements {
        let (properties, properties_param) = match elem {
            PatternElement::Node(np) => (&np.properties, np.properties_param.as_ref()),
            PatternElement::Relationship(rp) => (&rp.properties, rp.properties_param.as_ref()),
            PatternElement::Quantified(_) => continue,
        };
        for (_, value) in properties {
            check(value, false)?;
        }
        if let Some(param) = properties_param {
            check(param, false)?;
        }
    }
    for item in mc.on_create.iter().chain(&mc.on_match) {
        if let Some(value) = &item.value {
            check(value, true)?;
        }
    }
    Ok(())
}

/// The one SET-item rule, shared by a `SET` clause and MERGE's ON CREATE
/// SET / ON MATCH SET: no pattern predicate in a value, the value
/// expression's variables must be bound, and the item's target — a
/// property access's variable or a bare variable — must be too.
fn check_set_items(
    items: &[relate_parser::SetItem],
    scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    for item in items {
        if let Some(val) = &item.value {
            super::pattern::check_no_pattern_in_value_position(val, "SET values")?;
            check_expr_vars_defined(val, scope)?;
        }
        check_dynamic_label_expressions(&item.labels, scope)?;
        if let Some(prop) = &item.property {
            if let Expr::Variable(var_name) = &prop.expr {
                check_name_defined(var_name, scope)?;
            }
        } else if let Some(var_name) = &item.variable {
            check_name_defined(var_name, scope)?;
        }
    }
    Ok(())
}

/// A REMOVE item's own rules: its dynamic label/type expressions and its
/// property root must be bound (the SET policy's mirror for `REMOVE n:$(x)`).
fn validate_remove_clause(
    rc: &relate_parser::RemoveClause,
    scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    for item in &rc.items {
        check_dynamic_label_expressions(&item.labels, scope)?;
        if let Some(prop) = &item.property {
            if let Expr::Variable(var_name) = &prop.expr {
                check_name_defined(var_name, scope)?;
            }
        } else if let Some(var_name) = &item.variable {
            check_name_defined(var_name, scope)?;
        }
    }
    Ok(())
}

/// The CIP-162 dynamic label/type atom (`SET n:$(expr)`) resolves its
/// expression against the clause's scope: an unbound name is the front
/// end's `UndefinedVariable` (live 2026.10: "Variable `expr` not defined").
fn check_dynamic_label_expressions(
    labels: &[relate_parser::LabelExpr],
    scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    for label in labels {
        if let relate_parser::LabelExpr::Dynamic(dynamic) = label {
            check_expr_vars_defined(&dynamic.expr, scope)?;
        }
    }
    Ok(())
}

/// Find the root variable name from an expression (traversing property access and subscripts).
fn find_root_variable(expr: &Expr) -> Option<SmallName> {
    match expr {
        Expr::Variable(v) => Some(v.clone()),
        Expr::Property(p) => find_root_variable(&p.expr),
        Expr::Subscript(s) => find_root_variable(&s.expr),
        Expr::Paren(inner) => find_root_variable(inner),
        Expr::StringInterp(parts) => interp_embeds(parts).find_map(find_root_variable),
        _ => None,
    }
}

mod kind_rules;
use kind_rules::check_static_kind_rules;

#[cfg(test)]
mod tests;
